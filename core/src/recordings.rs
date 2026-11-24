//! The bring-up capture, read for the tests.
//!
//! `docs/captures/a25-v1.34-bringup.jsonl` holds every exchange with a real A25 from
//! the first bring-up session, one JSON object per line:
//!
//! - `"kind": "burst"`: one request written and the line held for the
//!   reply: `label` (what was asked), `tx_hex`, `hold_ms`, `rx_hex` (one
//!   reply; empty when the amplifier lost the request; three bytes once,
//!   when a 4 ms hold cut the reply short) and `t_ms`;
//! - `"kind": "rx"`: what a floating pin produced before the wiring was
//!   right (`hex`): noise for the decoder.
//!
//! `t_ms` is milliseconds since the capture's first line. No JSON crate:
//! the fields are flat and their strings hold no escapes.

use alloc::string::String;
use alloc::vec::Vec;

use crate::table::{Input, Mute, Power, Reply, Source};

const CAPTURE: &str = include_str!("../../docs/captures/a25-v1.34-bringup.jsonl");

/// One burst of the capture.
#[derive(Debug, Clone)]
pub struct Burst {
    pub label: &'static str,
    pub tx: Vec<u8>,
    pub rx_hex: &'static str,
    pub rx: Vec<u8>,
    pub hold_ms: u32,
    /// The capture's clock at the burst, in ms.
    pub at_ms: u64,
}

/// The string after `"key": "` in `line`.
fn text(line: &'static str, key: &str) -> Option<&'static str> {
    let open = alloc::format!("\"{key}\": \"");
    let rest = &line[line.find(&open)? + open.len()..];
    Some(&rest[..rest.find('"')?])
}

/// The unsigned integer after `"key": ` in `line`.
fn number<T: core::str::FromStr>(line: &str, key: &str) -> Option<T> {
    let open = alloc::format!("\"{key}\": ");
    let rest = &line[line.find(&open)? + open.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Bytes from their hex.
pub fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex in whole bytes: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Hex from bytes, lower case as the capture writes it.
pub fn to_hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(2 * bytes.len());
    for b in bytes {
        write!(out, "{b:02x}").expect("a String takes any write");
    }
    out
}

/// Every burst, in the capture's order.
pub fn bursts() -> Vec<Burst> {
    CAPTURE
        .lines()
        .filter(|l| text(l, "kind") == Some("burst"))
        .map(|l| {
            let rx_hex = text(l, "rx_hex").expect("rx_hex");
            Burst {
                label: text(l, "label").expect("label"),
                tx: hex(text(l, "tx_hex").expect("tx_hex")),
                rx_hex,
                rx: hex(rx_hex),
                hold_ms: number(l, "hold_ms").expect("hold_ms"),
                at_ms: number(l, "t_ms").expect("t_ms"),
            }
        })
        .collect()
}

/// The one burst with this label.
pub fn burst(label: &str) -> Burst {
    let all = bursts();
    let mut found = all.iter().filter(|b| b.label == label);
    let b = found.next().unwrap_or_else(|| panic!("no burst {label:?}"));
    assert!(found.next().is_none(), "{label:?} names one burst");
    b.clone()
}

/// What the floating pin produced: every `rx` line's bytes.
pub fn stray_bytes() -> Vec<u8> {
    CAPTURE
        .lines()
        .filter(|l| text(l, "kind") == Some("rx"))
        .flat_map(|l| hex(text(l, "hex").expect("hex")))
        .collect()
}

/// What the amplifier's answer to a burst says, from the burst's label and
/// the session notes alone (`docs/protocol.md#captures`), not from its
/// bytes: on, volume 59, not muted, DIGITAL 3, model 3; in standby after
/// the standby request and at every standby poll; 58 after the step down
/// and until the step up; muted after "mute on" until "mute off".
pub fn answer(b: &Burst) -> Reply {
    let l = b.label;
    match b.tx[1] {
        0x00 => {
            let standby = l.starts_with("standby request") || l.starts_with("standby poll");
            Reply::Power(if standby { Power::Standby } else { Power::On })
        }
        0x0D => {
            let down = l.starts_with("volume down") || l.starts_with("volume after down");
            Reply::Volume(if down { 58 } else { 59 })
        }
        0x0E => {
            let muted = l.starts_with("mute on") || l.starts_with("mute read try");
            Reply::Mute(if muted { Mute::Muted } else { Mute::NotMuted })
        }
        0x1D => Reply::Source(Input {
            source: Source::Digital3,
            streamer: false,
        }),
        0x5E => Reply::Model(3),
        code => panic!("{l}: no request of the capture has code {code:02x}"),
    }
}

/// The frames the regular expression `21(..)00(..)0d` finds in a burst's
/// hex, on byte boundaries, leftmost first and not overlapping: the
/// decoder's reference, written over the text rather than the bytes.
pub fn regex_frames(hex_text: &str) -> Vec<(u8, u8)> {
    let h = hex_text.as_bytes();
    let byte = |i: usize| u8::from_str_radix(&hex_text[i..i + 2], 16).expect("hex");
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= h.len() {
        if &h[i..i + 2] == b"21" && &h[i + 4..i + 6] == b"00" && &h[i + 8..i + 10] == b"0d" {
            out.push((byte(i + 2), byte(i + 6)));
            i += 10;
        } else {
            i += 2;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capture as `docs/protocol.md` counts it: 135 exchanges, 3 of the
    /// 50 loss-test queries unanswered.
    #[test]
    fn the_capture_is_the_one_the_document_describes() {
        let all = bursts();
        assert_eq!(all.len(), 135);
        assert!(all.iter().all(|b| b.tx.len() == 4));
        let loss: Vec<&Burst> = all
            .iter()
            .filter(|b| b.label.starts_with("loss "))
            .collect();
        assert_eq!(loss.len(), 50);
        let lost: Vec<&str> = loss
            .iter()
            .filter(|b| b.rx.is_empty())
            .map(|b| b.label)
            .collect();
        assert_eq!(lost, ["loss 5", "loss 20", "loss 37"]);
        // The first burst came 2199.694 s after the stray byte that opens
        // the capture.
        assert_eq!(all[0].at_ms, 2_199_694);
        assert_eq!(all[134].at_ms - all[0].at_ms, 145_548);
    }

    #[test]
    fn the_reference_reads_a_reply() {
        assert_eq!(regex_frames("210d003b0d"), [(0x0D, 0x3B)]);
        assert_eq!(regex_frames("210000"), []);
        assert_eq!(regex_frames("fe21210e00000d"), [(0x0E, 0x00)]);
        assert_eq!(regex_frames("2100f00d"), []);
    }
}
