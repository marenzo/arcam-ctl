//! The serial protocol of the Arcam Radia A25's CONTROL jack.
//!
//! The A25 has an RJ12 jack marked CONTROL that Arcam does not document. It
//! is an RS-232 link at 19200 baud, 8N1, with two frames:
//!
//! ```text
//! request : 21 cc dd 0D        '!', a command code, one data byte, '\r'
//! reply   : 21 cc 00 dd 0D     the third byte is always 00
//! ```
//!
//! `docs/protocol.md` in the repository has the whole of it: how it was
//! recovered from the amplifier's firmware, what was measured on the jack,
//! and the traps.
//!
//! This crate is pure: bytes and milliseconds in, bytes and facts out. It
//! has no dependencies, does not allocate and does no I/O. The UART is the
//! caller's.
//!
//! - [`frame`]: the request encoder and the reply decoder.
//! - [`table`]: the six command codes and what their data bytes mean.
//! - [`policy`]: what may be asked, and the only source of bytes for the
//!   wire. The power toggle and the DAC filter code `0x61` cannot be
//!   expressed; a volume above a configurable ceiling is refused.
//! - [`state`]: what the amplifier last said about itself, and when.
//! - [`link`]: one request in flight, its hold and when the hold ends
//!   (early, two character times after the reply), its single retry, and
//!   the wait while the amplifier wakes from standby.
//! - [`poll`]: when to read the amplifier unasked, without defeating its
//!   20-minute auto-standby.
//!
//! # Burst mode
//!
//! The amplifier's RS-232 transmitter sits behind an auto-powerdown
//! transceiver: it only drives the line while its receiver sees a valid
//! RS-232 level. A 3.3 V GPIO can still talk to it. Transmit inverted (idle
//! at 0 V), and after each request hold the line at 3.3 V for a while: the
//! hold wakes the amplifier's driver, and the reply arrives within it.
//! Receive inverted too, with a pull-down. Each exchange is one *burst*:
//! four bytes out, then the hold.
//!
//! A caller's loop looks like this:
//!
//! ```text
//! let burst = link.start(request, now)?;        // or why not
//! uart.write_then_hold(burst.bytes(), burst.hold_ms);
//! loop {
//!     let n = uart.read(&mut buf, until: link.deadline_ms());
//!     for action in link.feed(now, &buf[..n]) {
//!         match action {
//!             Action::Retry(b) | Action::Poll(b) => uart.write_then_hold(b.bytes(), b.hold_ms),
//!             Action::Done(reply) => ...,       // also folded into link.state()
//!             Action::Awake { ask } => ...,     // start each of `ask` in turn
//!             ...
//!         }
//!     }
//! }
//! ```
//!
//! Every complete frame restarts the amplifier's 20-minute auto-standby
//! timer, so ask when there is a reason to, not on a fast schedule.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate alloc;

pub mod frame;
pub mod link;
pub mod policy;
pub mod poll;
pub mod state;
pub mod table;

#[cfg(test)]
mod recordings;
