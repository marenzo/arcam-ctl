//! The HTTP routes (`docs/integration.md`, "REST"): the settings page, the
//! REST API, `/metrics` ([`crate::metrics`]) and the firmware upload
//! ([`crate::ota`]). No authentication: the device belongs on a trusted
//! network. A POST that a browser sends for a page on another site is
//! refused ([`cross_site`]).

use std::sync::Mutex;
use std::sync::mpsc::{Sender, sync_channel};
use std::time::Duration;

use esp_idf_svc::http::Method;
use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::io::Write;

use crate::amp::Bridge;
use crate::api::{Command, Outcome, Status};
use crate::mqtt::Event;
use crate::settings::{Settings, Store};
use crate::{Error, json, lock, metrics, ota};

/// The longest body read.
const BODY_MAX: usize = 1024;
/// How long `POST /api/command` waits for the end: a power-on takes about 5 s.
const COMMAND_WAIT: Duration = Duration::from_secs(15);

const PAGE: &str = include_str!("page.html");
/// The 3D wiring guide (docs/wiring/guide.html), so it needs no hosting.
const WIRING: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wiring.html.gz"));

type Req<'a, 'b> = Request<&'a mut EspHttpConnection<'b>>;

/// Start the server; it runs while the returned value lives.
pub fn start(
    bridge: Bridge,
    store: Store,
    events: Sender<Event>,
) -> Result<EspHttpServer<'static>, Error> {
    let mut server = EspHttpServer::new(&Configuration {
        // The OTA upload verifies the image on this stack.
        stack_size: 12 * 1024,
        ..Default::default()
    })?;
    server.fn_handler("/", Method::Get, |req| reply(req, 200, HTML, PAGE))?;
    server.fn_handler("/wiring", Method::Get, |req| -> Result<(), Error> {
        req.into_response(
            200,
            None,
            &[("Content-Type", HTML), ("Content-Encoding", "gzip")],
        )?
        .write_all(WIRING)?;
        Ok(())
    })?;
    let b = bridge.clone();
    server.fn_handler("/api/state", Method::Get, move |req| {
        // The state and the diagnostics in one object.
        let (state, diag) = (b.state_json(), b.diag_json());
        let both = format!("{},{}", &state[..state.len() - 1], &diag[1..]);
        reply(req, 200, JSON, &both)
    })?;
    let b = bridge.clone();
    server.fn_handler("/api/command", Method::Post, move |mut req| {
        if cross_site(&req) {
            return reply(req, 403, JSON, &error(CROSS_SITE));
        }
        let body = match body(&mut req) {
            Ok(body) => body,
            Err((code, why)) => return reply(req, code, JSON, &error(&why)),
        };
        let (code, result) = command(&b, &body);
        reply(req, code, JSON, &result)
    })?;
    let b = bridge.clone();
    server.fn_handler("/metrics", Method::Get, move |req| {
        reply(req, 200, METRICS, &metrics::render(&b))
    })?;
    server.fn_handler("/api/ota", Method::Post, |mut req| {
        if cross_site(&req) {
            return reply(req, 403, JSON, &error(CROSS_SITE));
        }
        let len = req.header("Content-Length").and_then(|v| v.parse().ok());
        match ota::upload(&mut req, len) {
            Ok(()) => {
                let sent = reply(req, 200, JSON, r#"{"status":"ok","detail":"restarting"}"#);
                // The new image boots next either way.
                crate::restart_soon();
                sent
            }
            Err((code, why)) => reply(req, code, JSON, &error(&why)),
        }
    })?;
    server.fn_handler("/api/restart", Method::Post, |req| {
        if cross_site(&req) {
            return reply(req, 403, JSON, &error(CROSS_SITE));
        }
        crate::restart_soon();
        reply(req, 200, JSON, r#"{"status":"ok","detail":"restarting"}"#)
    })?;
    let b = bridge.clone();
    server.fn_handler("/api/settings", Method::Get, move |req| {
        reply(req, 200, JSON, &settings_json(&lock(&b.shared.settings)))
    })?;
    let store = Mutex::new(store);
    server.fn_handler("/api/settings", Method::Post, move |mut req| {
        if cross_site(&req) {
            return reply(req, 403, JSON, &error(CROSS_SITE));
        }
        let body = match body(&mut req) {
            Ok(body) => body,
            Err((code, why)) => return reply(req, code, JSON, &error(&why)),
        };
        match change(&bridge, &store, &body) {
            Ok(s) => {
                let _ = events.send(Event::Reconnect);
                reply(req, 200, JSON, &settings_json(&s))
            }
            Err(e) => reply(req, 400, JSON, &error(&e)),
        }
    })?;
    Ok(server)
}

const JSON: &str = "application/json";
const HTML: &str = "text/html";
/// The Prometheus text format.
const METRICS: &str = "text/plain; version=0.0.4";

/// Why a cross-site POST is refused.
const CROSS_SITE: &str = "refused: a POST from a page on another site";

/// Whether a browser sent this request for a page on another site. A
/// browser names the page's origin in `Origin`, and the device's own page
/// is `http://` plus the `Host` it was opened by. Clients that are not
/// browsers (curl, Home Assistant, scripts) send no `Origin`. So no web page
/// opened in a browser on the network can change the device's settings,
/// command the amplifier or replace the firmware.
fn cross_site(req: &Req<'_, '_>) -> bool {
    let Some(origin) = req.header("Origin") else {
        return false;
    };
    let host = req.header("Host").unwrap_or_default();
    origin
        .strip_prefix("http://")
        .is_none_or(|o| host.is_empty() || !o.eq_ignore_ascii_case(host))
}

/// `{"error": why}`.
fn error(why: &str) -> String {
    format!("{{\"error\":{}}}", json::string(why))
}

fn reply(req: Req<'_, '_>, code: u16, kind: &str, body: &str) -> Result<(), Error> {
    req.into_response(code, None, &[("Content-Type", kind)])?
        .write_all(body.as_bytes())?;
    Ok(())
}

/// Read a complete UTF-8 body before parsing commands or persisting settings.
fn body(req: &mut Req<'_, '_>) -> Result<String, (u16, String)> {
    let mut buf = vec![0u8; BODY_MAX + 1];
    let mut n = 0;
    while n < buf.len() {
        let got = req
            .read(&mut buf[n..])
            .map_err(|_| (400, "body read failed".to_owned()))?;
        if got == 0 {
            break;
        }
        n += got;
    }
    if n > BODY_MAX {
        return Err((413, format!("body exceeds {BODY_MAX} bytes")));
    }
    buf.truncate(n);
    String::from_utf8(buf).map_err(|_| (400, "expected a UTF-8 body".to_owned()))
}

/// `{"<key>": value}`, exactly one key: queued, and its end waited for.
fn command(bridge: &Bridge, body: &str) -> (u16, String) {
    let refused = |key: &str, why: String| {
        let outcome = Outcome {
            status: Status::Refused,
            detail: why,
        };
        (400, outcome.json(key))
    };
    let fields = match json::fields(body) {
        Some(f) if f.len() == 1 => f,
        _ => return refused("", "the body is one JSON object with one key".into()),
    };
    let (key, value) = &fields[0];
    let value = match (key.as_str(), value.as_str()) {
        (_, "true") => "on",
        ("power", "false") => "standby",
        (_, "false") => "off",
        (_, v) => v,
    };
    let cmd = match Command::parse(key, value) {
        Ok(c) => c,
        Err(why) => return refused(key, why),
    };
    let (tx, rx) = sync_channel(1);
    if let Err(outcome) = bridge.submit(cmd, Some(tx)) {
        return (200, outcome.json(key));
    }
    match rx.recv_timeout(COMMAND_WAIT) {
        Ok(outcome) => (200, outcome.json(key)),
        Err(_) => (
            202,
            Outcome::new(Status::Pending, "accepted, outcome unknown; do not resend").json(key),
        ),
    }
}

/// `GET /api/settings`: everything but the password.
fn settings_json(s: &Settings) -> String {
    format!(
        "{{\"mqtt_host\":{},\"mqtt_port\":{},\"mqtt_user\":{},\"mqtt_password_set\":{},\"volume_max\":{},\"poll\":{}}}",
        json::string(&s.mqtt_host),
        s.mqtt_port,
        json::string(&s.mqtt_user),
        !s.mqtt_password.is_empty(),
        s.volume_max,
        json::string(&s.poll.to_string())
    )
}

/// The fields of a JSON or form body over the settings in force, checked
/// whole, then kept and put in force. A form's empty password leaves the
/// password as it is.
fn change(bridge: &Bridge, store: &Mutex<Store>, body: &str) -> Result<Settings, String> {
    let (fields, form) = if body.trim_start().starts_with('{') {
        (json::fields(body).ok_or("not a flat JSON object")?, false)
    } else {
        (
            json::form(body).ok_or("invalid form encoding or duplicate key")?,
            true,
        )
    };
    let mut current = lock(&bridge.shared.settings);
    let mut s = current.clone();
    let mut password_supplied = false;
    for (key, value) in &fields {
        if form && key == "mqtt_password" && value.is_empty() {
            continue;
        }
        s.set(key, value)?;
        password_supplied |= key == "mqtt_password";
    }
    if !password_supplied
        && (s.mqtt_host != current.mqtt_host
            || s.mqtt_port != current.mqtt_port
            || s.mqtt_user != current.mqtt_user)
    {
        s.mqtt_password.clear();
    }
    lock(store)
        .save(&s)
        .map_err(|e| format!("not saved: {e}"))?;
    *current = s.clone();
    log::info!("settings saved");
    Ok(s)
}
