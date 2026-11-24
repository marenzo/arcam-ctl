//! `GET /metrics`: the device, the link, the amplifier and MQTT in the
//! Prometheus text format 0.0.4 (`docs/integration.md#metrics`). Rendered
//! into one `String` per scrape from the shared snapshot and the counters.
//! A value the amplifier has not reported yet is left out, not zero.

use core::fmt::{self, Display, Write};
use std::sync::atomic::Ordering::Relaxed;

use esp_idf_svc::sys;

use arcam_ctl::poll::Phase;
use arcam_ctl::state::State;

use crate::amp::Bridge;
use crate::api::{Condition, Snapshot};
use crate::{line, lock, mqtt, ota};

/// The reply-time histogram's upper bounds, in ms and as its `le` labels;
/// `+Inf` follows.
const BUCKETS: [(u32, &str); 4] = [(10, "0.01"), (25, "0.025"), (50, "0.05"), (100, "0.1")];

/// The reply times seen since boot, as the histogram counts them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Replies {
    /// Replies at or under each bound of [`BUCKETS`], not cumulative.
    within: [u32; BUCKETS.len()],
    /// All replies.
    count: u32,
    /// Their sum, in ms.
    sum_ms: u64,
}

impl Replies {
    /// Count one reply that took `ms`.
    pub fn add(&mut self, ms: u32) {
        if let Some(i) = BUCKETS.iter().position(|&(b, _)| ms <= b) {
            self.within[i] += 1;
        }
        self.count += 1;
        self.sum_ms += u64::from(ms);
    }
}

/// The metrics now.
pub fn render(bridge: &Bridge) -> String {
    let snap = *lock(&bridge.shared.snapshot);
    let schedule = *lock(&bridge.shared.schedule);
    let volume_max = lock(&bridge.shared.settings).volume_max;
    let now = line::now_ms();
    let mut m = Out(String::with_capacity(4096));
    device(&mut m, now);
    link(&mut m, &snap, schedule.phase(now));
    amp(&mut m, &snap.state, volume_max, now);
    broker(&mut m);
    m.0
}

/// The device itself.
fn device(m: &mut Out, now: u64) {
    m.head(
        "arcam_build_info",
        "gauge",
        "The firmware version and the app partition it runs from.",
    );
    m.labeled(
        "arcam_build_info",
        format_args!(
            "version=\"{}\",partition=\"{}\"",
            crate::VERSION,
            ota::running()
        ),
        1,
    );
    m.one(
        "arcam_device_uptime_seconds",
        "gauge",
        "Time since boot.",
        Secs(now),
    );
    // SAFETY: plain reads of the heap's counters.
    let (free, min_free) = unsafe {
        (
            sys::esp_get_free_heap_size(),
            sys::esp_get_minimum_free_heap_size(),
        )
    };
    m.one("arcam_device_heap_free_bytes", "gauge", "Free heap.", free);
    m.one(
        "arcam_device_heap_min_free_bytes",
        "gauge",
        "The least free heap since boot.",
        min_free,
    );
    if let Some(rssi) = rssi() {
        m.one(
            "arcam_device_wifi_rssi_dbm",
            "gauge",
            "The Wi-Fi access point's signal strength.",
            rssi,
        );
    }
}

/// The link and the poll schedule.
fn link(m: &mut Out, snap: &Snapshot, phase: Phase) {
    m.one(
        "arcam_link_exchanges_total",
        "counter",
        "Requests sent to the amplifier, polls included, retries not.",
        snap.exchanges,
    );
    m.one(
        "arcam_link_losses_total",
        "counter",
        "Requests that got no answer.",
        snap.losses,
    );
    let r = snap.replies;
    m.head(
        "arcam_link_reply_seconds",
        "histogram",
        "How long replies took, from the start of the request's hold.",
    );
    let mut within = 0;
    for ((_, le), n) in BUCKETS.iter().zip(r.within) {
        within += n;
        m.labeled(
            "arcam_link_reply_seconds_bucket",
            format_args!("le=\"{le}\""),
            within,
        );
    }
    m.labeled(
        "arcam_link_reply_seconds_bucket",
        format_args!("le=\"+Inf\""),
        r.count,
    );
    m.sample("arcam_link_reply_seconds_sum", Secs(r.sum_ms));
    m.sample("arcam_link_reply_seconds_count", r.count);
    m.head(
        "arcam_link_condition",
        "gauge",
        "The link's condition, one-hot; all 0 before the first request ends.",
    );
    for c in [Condition::Answering, Condition::NoAnswer, Condition::Waking] {
        m.labeled(
            "arcam_link_condition",
            format_args!("condition=\"{}\"", c.name()),
            u8::from(snap.link == Some(c)),
        );
    }
    m.head(
        "arcam_poll_phase",
        "gauge",
        "The poll schedule's phase, one-hot.",
    );
    for p in [
        Phase::Off,
        Phase::Waking,
        Phase::Standby,
        Phase::Window,
        Phase::Quiet,
    ] {
        m.labeled(
            "arcam_poll_phase",
            format_args!("phase=\"{}\"", p.name()),
            u8::from(phase == p),
        );
    }
}

/// What the amplifier reported; each value once it has.
fn amp(m: &mut Out, s: &State, volume_max: u8, now: u64) {
    if let Some(on) = s.on() {
        m.one(
            "arcam_amp_power_on",
            "gauge",
            "1 on, 0 standby.",
            u8::from(on),
        );
    }
    if let Some(v) = s.level() {
        m.one("arcam_amp_volume", "gauge", "The volume, 0 to 99.", v);
    }
    if let Some(muted) = s.muted() {
        m.one("arcam_amp_muted", "gauge", "1 muted.", u8::from(muted));
    }
    if let Some(h) = s.source {
        m.head("arcam_amp_source", "gauge", "The current input.");
        m.labeled(
            "arcam_amp_source",
            format_args!("source=\"{}\"", h.value.source.key()),
            1,
        );
        m.one(
            "arcam_amp_streamer",
            "gauge",
            "1 when the current input is the configured Streamer Input.",
            u8::from(h.value.streamer),
        );
    }
    m.one(
        "arcam_amp_volume_max",
        "gauge",
        "The volume ceiling setting.",
        volume_max,
    );
    let ages = [
        ("power", s.power.map(|h| h.age_ms(now))),
        ("volume", s.volume.map(|h| h.age_ms(now))),
        ("mute", s.mute.map(|h| h.age_ms(now))),
        ("source", s.source.map(|h| h.age_ms(now))),
    ];
    if ages.iter().any(|(_, a)| a.is_some()) {
        m.head(
            "arcam_amp_age_seconds",
            "gauge",
            "Time since the amplifier last reported each field.",
        );
        for (field, age) in ages {
            if let Some(ms) = age {
                m.labeled(
                    "arcam_amp_age_seconds",
                    format_args!("field=\"{field}\""),
                    Secs(ms),
                );
            }
        }
    }
}

/// The MQTT client.
fn broker(m: &mut Out) {
    let q = &mqtt::STATS;
    m.one(
        "arcam_mqtt_connected",
        "gauge",
        "1 while connected to the broker.",
        u8::from(q.connected.load(Relaxed)),
    );
    m.one(
        "arcam_mqtt_connects_total",
        "counter",
        "Connections to the broker.",
        q.connects.load(Relaxed),
    );
    m.one(
        "arcam_mqtt_disconnects_total",
        "counter",
        "Connections to the broker lost or closed.",
        q.disconnects.load(Relaxed),
    );
    m.one(
        "arcam_mqtt_published_total",
        "counter",
        "Messages handed to the MQTT client.",
        q.published.load(Relaxed),
    );
    m.one(
        "arcam_mqtt_commands_total",
        "counter",
        "Commands received on the set/ topics and acted on.",
        q.commands.load(Relaxed),
    );
}

/// The text being written. A `String` takes every write: their results
/// are dropped.
struct Out(String);

impl Out {
    fn head(&mut self, name: &str, kind: &str, help: &str) {
        let _ = write!(self.0, "# HELP {name} {help}\n# TYPE {name} {kind}\n");
    }

    fn sample(&mut self, name: &str, value: impl Display) {
        let _ = writeln!(self.0, "{name} {value}");
    }

    fn labeled(&mut self, name: &str, labels: fmt::Arguments<'_>, value: impl Display) {
        let _ = writeln!(self.0, "{name}{{{labels}}} {value}");
    }

    /// A family of one sample without labels.
    fn one(&mut self, name: &str, kind: &str, help: &str, value: impl Display) {
        self.head(name, kind, help);
        self.sample(name, value);
    }
}

/// Milliseconds written as seconds.
struct Secs(u64);

impl Display for Secs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1000, self.0 % 1000)
    }
}

/// The access point's signal, while connected.
#[cfg(not(feature = "qemu"))]
fn rssi() -> Option<i8> {
    let mut ap = sys::wifi_ap_record_t::default();
    // SAFETY: `ap` is the record the call fills.
    sys::esp!(unsafe { sys::esp_wifi_sta_get_ap_info(&raw mut ap) })
        .ok()
        .map(|()| ap.rssi)
}

/// QEMU has no Wi-Fi.
#[cfg(feature = "qemu")]
fn rssi() -> Option<i8> {
    None
}
