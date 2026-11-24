//! MQTT and Home Assistant discovery, as `docs/integration.md` says.
//!
//! One thread owns the client. Its inbox takes the client's own events,
//! the amplifier's (a burst ended, a command ended) and a settings change,
//! which makes a new client. The state is published, retained, when it
//! differs from the last one published; the diagnostics, not retained,
//! once at connect and then every [`DIAG_EVERY`]. Commands are subscribed at most
//! once (`QoS` 0), and a retained one is cleared at connect, before the
//! subscription: a broker never delivers one twice, so a volume step is
//! never doubled. [`STATS`] counts the connections, messages and commands
//! for `/metrics`.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use arcam_radia::table::Source;
use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EventPayload, LwtConfiguration, MqttClientConfiguration, QoS,
};
use esp_idf_svc::sys::EspError;

use crate::amp::Bridge;
use crate::api::{Command, Outcome, Status};
use crate::{json, lock};

/// The `set/` topics.
const COMMANDS: [&str; 6] = ["power", "volume", "mute", "source", "refresh", "restart"];
/// Home Assistant's discovery prefix.
const DISCOVERY: &str = "homeassistant";
/// How often `<base>/diag` is published. Its sensors expire after three.
const DIAG_EVERY: Duration = Duration::from_mins(1);

/// The client's counts since boot, for `/metrics`.
pub struct Stats {
    /// Connected to the broker now.
    pub connected: AtomicBool,
    /// Connections made.
    pub connects: AtomicU32,
    /// Connections lost or closed.
    pub disconnects: AtomicU32,
    /// Messages the client took to send.
    pub published: AtomicU32,
    /// `set/` commands acted on: queued for the amplifier, or a restart.
    pub commands: AtomicU32,
}

/// The one client's counts.
pub static STATS: Stats = Stats {
    connected: AtomicBool::new(false),
    connects: AtomicU32::new(0),
    disconnects: AtomicU32::new(0),
    published: AtomicU32::new(0),
    commands: AtomicU32::new(0),
};

/// What the MQTT thread is told.
pub enum Event {
    /// The amplifier's snapshot may have changed.
    State,
    /// A command ended: its result JSON.
    Result(String),
    /// The settings changed: connect again with them.
    Reconnect,
    /// The client connected to the broker.
    Connected,
    /// It lost the broker.
    Disconnected,
    /// A message on a subscribed topic.
    Message(String, String),
    /// Nothing for a second: time for the diagnostics, maybe.
    Tick,
}

/// Start the thread.
pub fn start(
    id: String,
    bridge: Bridge,
    events: Sender<Event>,
    inbox: Receiver<Event>,
) -> Result<(), crate::Error> {
    std::thread::Builder::new()
        .name("mqtt".into())
        .stack_size(8 * 1024)
        .spawn(move || run(&id, &bridge, &events, &inbox))?;
    Ok(())
}

fn run(id: &str, bridge: &Bridge, events: &Sender<Event>, inbox: &Receiver<Event>) {
    let base = format!("arcam-radia/{id}");
    let mut client = connect(id, &base, bridge, events);
    let mut online = false;
    let mut last = String::new();
    let mut setup_at = None;
    let mut diag_at: Option<Instant> = None;
    loop {
        let event = match inbox.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => Event::Tick,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match event {
            Event::Connected => {
                set_online(&mut online, false);
                setup_at = Some(Instant::now());
                last.clear();
                diag_at = None;
                let _ = events.send(Event::State);
            }
            Event::Disconnected => {
                set_online(&mut online, false);
                setup_at = None;
            }
            Event::Message(topic, payload) => {
                if !online {
                    continue;
                }
                if topic == format!("{DISCOVERY}/status") {
                    if payload == "online"
                        && let Some(c) = client.as_mut()
                    {
                        announce(c, id, &base);
                        last.clear();
                        let _ = events.send(Event::State);
                    }
                } else if let Some(key) = topic.strip_prefix(&format!("{base}/set/")) {
                    command(bridge, events, key, &payload);
                }
            }
            Event::State | Event::Tick => {
                if online && let Some(c) = client.as_mut() {
                    publish_state(c, bridge, &base, &mut last);
                }
            }
            Event::Result(result) => {
                if online && let Some(c) = client.as_mut() {
                    let topic = format!("{base}/result");
                    let _ = send(
                        c,
                        &topic,
                        QoS::AtLeastOnce,
                        false,
                        result.as_bytes(),
                        Via::Now,
                    );
                }
            }
            Event::Reconnect => {
                // The old client goes first: its broker session ends.
                setup_at = None;
                drop(client.take());
                set_online(&mut online, false);
                client = connect(id, &base, bridge, events);
            }
        }
        if setup_at.is_some_and(|t| Instant::now() >= t)
            && let Some(c) = client.as_mut()
        {
            match connected(c, id, &base) {
                Ok(()) => {
                    setup_at = None;
                    set_online(&mut online, true);
                    publish_state(c, bridge, &base, &mut last);
                }
                Err(e) => {
                    log::warn!("mqtt setup failed, retrying: {e}");
                    setup_at = Some(Instant::now() + Duration::from_secs(2));
                }
            }
        }
        if online
            && diag_at.is_none_or(|t| t.elapsed() >= DIAG_EVERY)
            && let Some(c) = client.as_mut()
        {
            let (topic, diag) = (format!("{base}/diag"), bridge.diag_json());
            let _ = send(
                c,
                &topic,
                QoS::AtMostOnce,
                false,
                diag.as_bytes(),
                Via::Queue,
            );
            diag_at = Some(Instant::now());
        }
    }
}

/// A failed publish stays eligible for the next state event or idle tick.
fn publish_state(c: &mut EspMqttClient<'static>, bridge: &Bridge, base: &str, last: &mut String) {
    let state = bridge.state_json();
    if state != *last
        && send(
            c,
            &format!("{base}/state"),
            QoS::AtLeastOnce,
            true,
            state.as_bytes(),
            Via::Now,
        )
        .is_ok()
    {
        *last = state;
    }
}

/// A connection's start: the retained commands cleared, the subscriptions,
/// availability and discovery.
fn connected(c: &mut EspMqttClient<'static>, id: &str, base: &str) -> Result<(), EspError> {
    // A retained command would come again at every connect.
    for key in COMMANDS {
        let topic = format!("{base}/set/{key}");
        send(c, &topic, QoS::AtMostOnce, true, b"", Via::Now)?;
    }
    c.subscribe(&format!("{base}/set/+"), QoS::AtMostOnce)?;
    c.subscribe(&format!("{DISCOVERY}/status"), QoS::AtMostOnce)?;
    let topic = format!("{base}/availability");
    send(c, &topic, QoS::AtLeastOnce, true, b"online", Via::Queue)?;
    announce(c, id, base);
    Ok(())
}

/// Whether the client is connected, and the transitions counted.
fn set_online(online: &mut bool, now: bool) {
    if *online != now {
        let count = if now {
            &STATS.connects
        } else {
            &STATS.disconnects
        };
        count.fetch_add(1, Relaxed);
    }
    *online = now;
    STATS.connected.store(now, Relaxed);
}

/// How [`send`] hands a message to the client.
#[derive(Clone, Copy)]
enum Via {
    /// `publish`: sent now, by the calling thread.
    Now,
    /// `enqueue`: into the client's outbox, sent by its own task.
    Queue,
}

/// One message to the client, counted when the client takes it.
fn send(
    c: &mut EspMqttClient<'static>,
    topic: &str,
    qos: QoS,
    retain: bool,
    payload: &[u8],
    how: Via,
) -> Result<(), EspError> {
    match how {
        Via::Now => c.publish(topic, qos, retain, payload),
        Via::Queue => c.enqueue(topic, qos, retain, payload),
    }?;
    STATS.published.fetch_add(1, Relaxed);
    Ok(())
}

/// A `set/` message into the amplifier's queue; a malformed one is refused
/// on the result topic at once.
fn command(bridge: &Bridge, events: &Sender<Event>, key: &str, payload: &str) {
    // A retained command cleared: no command.
    if payload.is_empty() {
        return;
    }
    if key == "restart" {
        STATS.commands.fetch_add(1, Relaxed);
        crate::restart_soon();
        return;
    }
    match Command::parse(key, payload) {
        Ok(cmd) => {
            if bridge.submit(cmd, None).is_ok() {
                STATS.commands.fetch_add(1, Relaxed);
            }
        }
        Err(why) => {
            let outcome = Outcome {
                status: Status::Refused,
                detail: why,
            };
            let _ = events.send(Event::Result(outcome.json(key)));
        }
    }
}

/// A client for the broker in the settings, or none when no host is set.
fn connect(
    id: &str,
    base: &str,
    bridge: &Bridge,
    events: &Sender<Event>,
) -> Option<EspMqttClient<'static>> {
    let s = lock(&bridge.shared.settings).clone();
    if s.mqtt_host.is_empty() {
        log::info!("mqtt: no broker set");
        return None;
    }
    let url = format!("mqtt://{}:{}", s.mqtt_host, s.mqtt_port);
    let will = format!("{base}/availability");
    let conf = MqttClientConfiguration {
        client_id: Some(id),
        username: (!s.mqtt_user.is_empty()).then_some(s.mqtt_user.as_str()),
        password: (!s.mqtt_password.is_empty()).then_some(s.mqtt_password.as_str()),
        lwt: Some(LwtConfiguration {
            topic: &will,
            payload: b"offline",
            qos: QoS::AtLeastOnce,
            retain: true,
        }),
        buffer_size: 1024,
        out_buffer_size: 1024,
        ..Default::default()
    };
    let tx = events.clone();
    let client = EspMqttClient::new_cb(&url, &conf, move |ev| {
        let event = match ev.payload() {
            EventPayload::Connected(_) => Event::Connected,
            EventPayload::Disconnected => Event::Disconnected,
            EventPayload::Received {
                topic: Some(topic),
                data,
                details: Details::Complete,
                ..
            } => Event::Message(topic.to_owned(), String::from_utf8_lossy(data).into_owned()),
            _ => return,
        };
        let _ = tx.send(event);
    });
    match client {
        Ok(c) => {
            log::info!("mqtt: {url}");
            Some(c)
        }
        Err(e) => {
            log::warn!("mqtt: {url}: {e}");
            None
        }
    }
}

/// Each entity: its component, object id, name and own config fields. In
/// the fields `<base>` is the base topic and `<sources>` the source names.
const ENTITIES: [(&str, &str, &str, &str); 14] = [
    (
        "switch",
        "power",
        "Power",
        r#""state_topic":"<base>/state","command_topic":"<base>/set/power","payload_on":"on","payload_off":"standby","state_on":"on","state_off":"standby","value_template":"{{ value_json.power }}""#,
    ),
    (
        "switch",
        "mute",
        "Mute",
        r#""state_topic":"<base>/state","command_topic":"<base>/set/mute","payload_on":"on","payload_off":"off","state_on":"on","state_off":"off","value_template":"{{ 'None' if value_json.muted is none else ('on' if value_json.muted else 'off') }}""#,
    ),
    (
        "number",
        "volume",
        "Volume",
        r#""state_topic":"<base>/state","command_topic":"<base>/set/volume","min":0,"max":99,"step":1,"mode":"slider","value_template":"{{ value_json.volume }}""#,
    ),
    (
        "select",
        "source",
        "Source",
        r#""state_topic":"<base>/state","command_topic":"<base>/set/source","options":[<sources>],"value_template":"{{ value_json.source }}""#,
    ),
    (
        "binary_sensor",
        "streamer",
        "Streamer input",
        r#""state_topic":"<base>/state","payload_on":"on","payload_off":"off","value_template":"{{ 'None' if value_json.streamer is none else ('on' if value_json.streamer else 'off') }}""#,
    ),
    (
        "button",
        "refresh",
        "Refresh",
        r#""command_topic":"<base>/set/refresh","payload_press":"refresh""#,
    ),
    (
        "button",
        "restart",
        "Restart",
        r#""command_topic":"<base>/set/restart","payload_press":"restart","device_class":"restart","entity_category":"config""#,
    ),
    (
        "button",
        "volume_up",
        "Volume up",
        r#""command_topic":"<base>/set/volume","payload_press":"up""#,
    ),
    (
        "button",
        "volume_down",
        "Volume down",
        r#""command_topic":"<base>/set/volume","payload_press":"down""#,
    ),
    (
        "button",
        "mute_toggle",
        "Mute toggle",
        r#""command_topic":"<base>/set/mute","payload_press":"toggle""#,
    ),
    (
        "sensor",
        "link",
        "Link",
        r#""state_topic":"<base>/state","entity_category":"diagnostic","value_template":"{{ value_json.link }}""#,
    ),
    (
        "sensor",
        "reply_time",
        "Reply time",
        r#""state_topic":"<base>/diag","expire_after":180,"entity_category":"diagnostic","unit_of_measurement":"ms","state_class":"measurement","value_template":"{{ value_json.reply_ms }}""#,
    ),
    (
        "sensor",
        "exchanges",
        "Exchanges",
        r#""state_topic":"<base>/diag","expire_after":180,"entity_category":"diagnostic","state_class":"total_increasing","value_template":"{{ value_json.exchanges }}""#,
    ),
    (
        "sensor",
        "losses",
        "Losses",
        r#""state_topic":"<base>/diag","expire_after":180,"entity_category":"diagnostic","state_class":"total_increasing","value_template":"{{ value_json.losses }}""#,
    ),
];

/// One retained discovery config per entity, with a shared device block.
fn announce(client: &mut EspMqttClient<'static>, id: &str, base: &str) {
    let sources: Vec<String> = Source::ALL
        .into_iter()
        .map(|s| json::string(s.key()))
        .chain([json::string("streamer")])
        .collect();
    let url = crate::ip().map_or(String::new(), |ip| {
        format!(",\"configuration_url\":\"http://{ip}/\"")
    });
    let device = format!(
        "\"device\":{{\"identifiers\":[\"{id}\"],\"name\":\"{id}\",\"manufacturer\":\"arcam-radia-esp32-control (unofficial)\",\"model\":\"Radia A25 bridge\",\"sw_version\":\"{}\"{url}}}",
        crate::VERSION
    );
    for (component, object, name, fields) in ENTITIES {
        let fields = fields
            .replace("<base>", base)
            .replace("<sources>", &sources.join(","));
        let config = format!(
            "{{\"name\":\"{name}\",\"unique_id\":\"{id}_{object}\",\"availability_topic\":\"{base}/availability\",{fields},{device}}}"
        );
        let topic = format!("{DISCOVERY}/{component}/{id}/{object}/config");
        if let Err(e) = send(
            client,
            &topic,
            QoS::AtLeastOnce,
            true,
            config.as_bytes(),
            Via::Queue,
        ) {
            log::warn!("mqtt: {topic}: {e}");
        }
    }
}
