//! Improv Serial v1 on the USB console (UART0, 115200), so an Improv client
//! in the browser can give the device its Wi-Fi (<https://www.improv-wifi.com/serial/>).
//!
//! A frame is `IMPROV`, version 1, type, length, data, and a checksum (the
//! sum of every byte before it). The console's log lines share the wire:
//! the flasher's reader looks for a frame at the start of a line, so each
//! frame is written in one driver call, between two newlines, and the
//! console's own writes go through the same driver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use esp_idf_svc::sys::{self, EspError, esp};
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};

use crate::lock;

const UART0: sys::uart_port_t = 0;
const HEADER: &[u8; 6] = b"IMPROV";

// Frame types.
const CURRENT_STATE: u8 = 0x01;
const ERROR_STATE: u8 = 0x02;
const RPC: u8 = 0x03;
const RPC_RESULT: u8 = 0x04;

// Current states.
const READY: u8 = 0x02;
const PROVISIONING: u8 = 0x03;
const PROVISIONED: u8 = 0x04;

// Errors.
const INVALID_RPC: u8 = 0x01;
const UNKNOWN_RPC: u8 = 0x02;
const UNABLE_TO_CONNECT: u8 = 0x03;

// RPC commands.
const WIFI_SETTINGS: u8 = 0x01;
const REQUEST_STATE: u8 = 0x02;
const REQUEST_INFO: u8 = 0x03;

/// How long a new network may take to give an address.
const CONNECT_MS: u64 = 20_000;

type Wifi = Arc<Mutex<EspWifi<'static>>>;

/// Take over UART0's receive side and listen for Improv frames, always.
pub fn start(wifi: Wifi, id: String) -> Result<(), crate::Error> {
    // SAFETY: UART0 is the console; the driver is installed once, here, and
    // the console's writes are moved onto it.
    unsafe {
        esp!(sys::uart_driver_install(
            UART0,
            512,
            0,
            0,
            core::ptr::null_mut(),
            0
        ))?;
        sys::uart_vfs_dev_use_driver(0);
    }
    std::thread::Builder::new()
        .name("improv".into())
        .stack_size(6 * 1024)
        .spawn(move || listen(&wifi, &id))?;
    Ok(())
}

fn listen(wifi: &Wifi, id: &str) {
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 64];
    loop {
        // SAFETY: `chunk` has room for its length; the driver is installed.
        let n = unsafe { sys::uart_read_bytes(UART0, chunk.as_mut_ptr().cast(), 64, 100) };
        let Ok(n) = usize::try_from(n) else { continue };
        buf.extend_from_slice(&chunk[..n]);
        while let Some(frame) = take_frame(&mut buf) {
            match frame {
                Ok((kind, data)) => handle(wifi, id, kind, &data),
                Err(()) => send(ERROR_STATE, &[INVALID_RPC]),
            }
        }
    }
}

/// The next whole frame in `buf`, and what came before it dropped: its
/// type and data, or `Err` for a bad checksum. `None` until one is whole.
fn take_frame(buf: &mut Vec<u8>) -> Option<Result<(u8, Vec<u8>), ()>> {
    let Some(at) = buf.windows(HEADER.len()).position(|w| w == HEADER) else {
        // Keep a tail that may be the start of a header.
        let keep = buf.len().min(HEADER.len() - 1);
        buf.drain(..buf.len() - keep);
        return None;
    };
    buf.drain(..at);
    let len = usize::from(*buf.get(8)?);
    let total = 9 + len + 1;
    if buf.len() < total {
        return None;
    }
    let frame: Vec<u8> = buf.drain(..total).collect();
    let sum = frame[..total - 1]
        .iter()
        .fold(0u8, |s, b| s.wrapping_add(*b));
    if frame[6] != 1 || sum != frame[total - 1] {
        return Some(Err(()));
    }
    Some(Ok((frame[7], frame[9..9 + len].to_vec())))
}

/// One frame out, between newlines, in one write.
fn send(kind: u8, data: &[u8]) {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.push(b'\n');
    out.extend_from_slice(HEADER);
    out.extend_from_slice(&[1, kind, u8::try_from(data.len()).unwrap_or(0)]);
    out.extend_from_slice(data);
    let sum = out[1..].iter().fold(0u8, |s, b| s.wrapping_add(*b));
    out.push(sum);
    out.push(b'\n');
    // SAFETY: the driver is installed; it copies the bytes.
    unsafe { sys::uart_write_bytes(UART0, out.as_ptr().cast(), out.len()) };
}

/// An RPC result: the command and its strings, each prefixed by its length.
fn result(command: u8, strings: &[&str]) {
    let mut body = Vec::new();
    for s in strings {
        body.push(u8::try_from(s.len()).unwrap_or(0));
        body.extend_from_slice(s.as_bytes());
    }
    let mut data = vec![command, u8::try_from(body.len()).unwrap_or(0)];
    data.extend(body);
    send(RPC_RESULT, &data);
}

fn handle(wifi: &Wifi, id: &str, kind: u8, data: &[u8]) {
    if kind != RPC || data.len() < 2 || data.len() < 2 + usize::from(data[1]) {
        send(ERROR_STATE, &[INVALID_RPC]);
        return;
    }
    send(ERROR_STATE, &[0]);
    let args = &data[2..2 + usize::from(data[1])];
    match data[0] {
        WIFI_SETTINGS => provision(wifi, args),
        REQUEST_STATE => match crate::ip() {
            Some(ip) => {
                send(CURRENT_STATE, &[PROVISIONED]);
                result(REQUEST_STATE, &[&format!("http://{ip}/")]);
            }
            None => send(CURRENT_STATE, &[READY]),
        },
        REQUEST_INFO => result(REQUEST_INFO, &[crate::NAME, crate::VERSION, "ESP32", id]),
        _ => send(ERROR_STATE, &[UNKNOWN_RPC]),
    }
}

/// Send Wi-Fi settings: `ssid_len ssid pass_len pass`. Kept by the Wi-Fi
/// driver in nvs.
fn provision(wifi: &Wifi, args: &[u8]) {
    let Some((ssid, pass)) = credentials(args) else {
        send(ERROR_STATE, &[INVALID_RPC]);
        return;
    };
    send(CURRENT_STATE, &[PROVISIONING]);
    log::info!("improv: joining {ssid}");
    if let Ok(Some(ip)) = join(wifi, ssid, pass) {
        send(CURRENT_STATE, &[PROVISIONED]);
        result(WIFI_SETTINGS, &[&format!("http://{ip}/")]);
    } else {
        send(ERROR_STATE, &[UNABLE_TO_CONNECT]);
        send(CURRENT_STATE, &[READY]);
    }
}

/// `ssid_len ssid pass_len pass`, both UTF-8.
fn credentials(args: &[u8]) -> Option<(&str, &str)> {
    let field = |at: usize| -> Option<(&str, usize)> {
        let len = usize::from(*args.get(at)?);
        let s = core::str::from_utf8(args.get(at + 1..at + 1 + len)?).ok()?;
        Some((s, at + 1 + len))
    };
    let (ssid, at) = field(0)?;
    let (pass, _) = field(at)?;
    Some((ssid, pass))
}

/// Store the network, connect, and wait for an address.
fn join(wifi: &Wifi, ssid: &str, pass: &str) -> Result<Option<std::net::Ipv4Addr>, EspError> {
    let invalid = || EspError::from_infallible::<{ sys::ESP_ERR_INVALID_ARG }>();
    let conf = ClientConfiguration {
        ssid: ssid.try_into().map_err(|_| invalid())?,
        password: pass.try_into().map_err(|_| invalid())?,
        auth_method: if pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    };
    {
        let mut w = lock(wifi);
        let _ = w.disconnect();
        w.set_configuration(&Configuration::Client(conf))?;
        w.connect()?;
    }
    let mut waited = 0;
    while waited < CONNECT_MS {
        std::thread::sleep(Duration::from_millis(500));
        waited += 500;
        if let Some(ip) = crate::net::ip(&lock(wifi)) {
            return Ok(Some(ip));
        }
    }
    Ok(None)
}
