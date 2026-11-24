//! `virtual-amp`: the virtual A25 ([`arcam_radia_emulator::Amp`]) on two TCP
//! ports, for the emulator (`emulator/README.md`).
//!
//! ```text
//! virtual-amp [--uart ADDR] [--control ADDR] [--auto-standby-ms N]
//! ```
//!
//! - `--uart` (`0.0.0.0:5555`): the jack, one client at a time (QEMU's
//!   UART1 connects to it). Requests in; each reply goes back after the
//!   amplifier's delay.
//! - `--control` (`0.0.0.0:5556`): one line in, one line out: `state`,
//!   `knob +N`, `knob -N`, `standby`, `power on` (see [`Amp::control`]).
//! - `--auto-standby-ms` (20 minutes): Eco Standby's timer; `0` turns it off.
//!
//! Every exchange is logged on stderr.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use arcam_radia_emulator::{Amp, Config, Parser, reply_delay_ms};

type Shared = Arc<(Mutex<Amp>, Instant)>;

fn now_ms(shared: &Shared) -> u64 {
    u64::try_from(shared.1.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn main() {
    let (mut uart, mut control) = ("0.0.0.0:5555".to_owned(), "0.0.0.0:5556".to_owned());
    let mut config = Config::DEFAULT;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_else(|| usage(&arg));
        match arg.as_str() {
            "--uart" => uart = value,
            "--control" => control = value,
            "--auto-standby-ms" => {
                config.auto_standby_ms = value.parse().unwrap_or_else(|_| usage(&arg));
            }
            _ => usage(&arg),
        }
    }
    let shared: Shared = Arc::new((Mutex::new(Amp::new(config, 0)), Instant::now()));
    let uart = TcpListener::bind(&uart).unwrap_or_else(|e| fail(&uart, &e));
    let control = TcpListener::bind(&control).unwrap_or_else(|e| fail(&control, &e));
    eprintln!("virtual-amp: uart {uart:?}, control {control:?}");
    let s = shared.clone();
    std::thread::spawn(move || {
        for conn in control.incoming().flatten() {
            let s = s.clone();
            std::thread::spawn(move || serve_control(&s, conn));
        }
    });
    for conn in uart.incoming().flatten() {
        eprintln!("uart: {:?} connected", conn.peer_addr());
        serve_uart(&shared, conn);
        eprintln!("uart: closed");
    }
}

fn usage(arg: &str) -> ! {
    eprintln!("virtual-amp: bad argument {arg:?}");
    eprintln!("usage: virtual-amp [--uart ADDR] [--control ADDR] [--auto-standby-ms N]");
    std::process::exit(2)
}

fn fail(addr: &str, e: &std::io::Error) -> ! {
    eprintln!("virtual-amp: {addr}: {e}");
    std::process::exit(1)
}

/// Requests in, replies out, until the client goes.
fn serve_uart(shared: &Shared, mut conn: TcpStream) {
    let _ = conn.set_nodelay(true);
    let mut parser = Parser::default();
    let mut buf = [0u8; 64];
    while let Ok(n @ 1..) = conn.read(&mut buf) {
        for &b in &buf[..n] {
            let Some(req) = parser.push(b) else { continue };
            let at = now_ms(shared);
            let reply = lock(shared).request(req, at);
            let line = format!("{at} ms: 21 {:02x} {:02x} 0d ->", req.code, req.data);
            let Some(reply) = reply else {
                eprintln!("{line} no reply");
                continue;
            };
            let delay = reply_delay_ms(at);
            std::thread::sleep(Duration::from_millis(delay));
            eprintln!("{line} {reply:02x?} after {delay} ms");
            if conn.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

fn serve_control(shared: &Shared, conn: TcpStream) {
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { return };
        let at = now_ms(shared);
        let answer = lock(shared)
            .control(line.trim(), at)
            .unwrap_or_else(|e| format!("error: {e}"));
        eprintln!("{at} ms: control {line:?} -> {answer}");
        if writeln!(out, "{answer}").is_err() {
            return;
        }
    }
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Amp> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}
