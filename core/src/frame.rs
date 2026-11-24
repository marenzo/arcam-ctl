//! The two frames on the wire.
//!
//! ```text
//! request : 21 cc dd 0D        one data byte
//! reply   : 21 cc 00 dd 0D     the third byte is always 00
//! ```
//!
//! Request bytes are made in this crate only, through
//! [`crate::policy::Wire`]. [`Decoder`] finds replies in whatever the UART
//! read.
//!
//! # How the decoder treats noise
//!
//! A reply is five bytes: `21`, any code, `00`, any data, `0D`. The code and
//! the data can be any byte (volume 13 replies `21 0D 00 0D 0D`, volume 33
//! `21 0D 00 21 0D`), so the three fixed bytes are all there is to check.
//! The decoder takes the earliest `21` whose five bytes fit, as the regular
//! expression `21 .. 00 .. 0D` would over the same bytes. When a candidate
//! fails, only its first byte is dropped and the search resumes at the next
//! `21` already held, so a stray `21` ahead of a reply does not cost the
//! reply.
//!
//! Three consequences, each handled by [`crate::link`]:
//!
//! - Noise ending in `21 xx 00` right before a volume reply fits together
//!   with the reply's first two bytes (`21 0D`), and the reply is lost to a
//!   frame with code `xx`. The link's retry asks again. Any other reply's
//!   second byte is not `0D`, so it still reads.
//! - A request's own four bytes never make a frame: there is no fifth. A
//!   request with data `00` followed by a stray `0D` would read as a reply,
//!   but nothing on this link echoes (separate wires each way).
//! - A reply cut short by the end of a hold stays in the decoder, and the
//!   next hold's reply can then complete a false frame: a cut `21 00 00`
//!   followed by `21 0D 00 3B 0D` reads as "power 0x21". The link calls
//!   [`Decoder::reset`] before every request, and the table rejects the
//!   value.

/// A frame's first byte, `!`.
pub const START: u8 = 0x21;
/// A frame's last byte, a carriage return.
pub const END: u8 = 0x0D;
/// A request's length.
pub const REQUEST_LEN: usize = 4;
/// A reply's length.
pub const REPLY_LEN: usize = 5;

/// A request's bytes. Crate-private: [`crate::policy::Wire`] is the only
/// source of bytes for the wire.
pub(crate) const fn encode(code: u8, data: u8) -> [u8; REQUEST_LEN] {
    [START, code, data, END]
}

/// One reply as received: the command code and the data byte, not yet
/// interpreted (see [`crate::table::Reply::from_frame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The command code, the reply's second byte.
    pub code: u8,
    /// The data byte, the reply's fourth byte.
    pub data: u8,
}

/// The reply decoder: bytes in, in any pieces, frames out.
#[derive(Debug, Clone)]
pub struct Decoder {
    /// The candidate: `held[0]` is a `21` whenever `len > 0`.
    held: [u8; REPLY_LEN],
    len: usize,
    /// Bytes discarded since the decoder was made.
    dropped: u32,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

impl Decoder {
    /// An empty decoder.
    pub const fn new() -> Decoder {
        Decoder {
            held: [0; REPLY_LEN],
            len: 0,
            dropped: 0,
        }
    }

    /// Discard a frame in progress. Call it before every request: what an
    /// earlier hold left behind is not part of the next reply.
    pub fn reset(&mut self) {
        self.dropped = self.dropped.saturating_add(count(self.len));
        self.len = 0;
    }

    /// Bytes of a frame in progress.
    pub fn pending(&self) -> usize {
        self.len
    }

    /// Bytes that were part of no frame since the decoder was made: a
    /// measure of line noise.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Take one byte; returns a frame when the byte completes one.
    pub fn push(&mut self, byte: u8) -> Option<Frame> {
        if self.len == 0 && byte != START {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        }
        self.held[self.len] = byte;
        self.len += 1;
        loop {
            if self.len > 2 && self.held[2] != 0 {
                self.next_candidate();
            } else if self.len == REPLY_LEN {
                if self.held[4] == END {
                    self.len = 0;
                    return Some(Frame {
                        code: self.held[1],
                        data: self.held[3],
                    });
                }
                self.next_candidate();
            } else {
                return None;
            }
        }
    }

    /// Take a slice; every frame it completes goes to `out`, in order.
    pub fn feed(&mut self, bytes: &[u8], out: &mut impl FnMut(Frame)) {
        for &b in bytes {
            if let Some(f) = self.push(b) {
                out(f);
            }
        }
    }

    /// The candidate failed: drop its `21` and everything up to the next
    /// `21` held.
    fn next_candidate(&mut self) {
        let next = self.held[1..self.len]
            .iter()
            .position(|&b| b == START)
            .map_or(self.len, |i| i + 1);
        self.held.copy_within(next..self.len, 0);
        self.len -= next;
        self.dropped = self.dropped.saturating_add(count(next));
    }
}

/// A count of held bytes, at most [`REPLY_LEN`], as the noise counter's type.
#[allow(
    clippy::cast_possible_truncation,
    reason = "never more than REPLY_LEN bytes are held"
)]
const fn count(held: usize) -> u32 {
    held as u32
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::recordings::{self, hex};

    fn frames(d: &mut Decoder, bytes: &[u8]) -> Vec<(u8, u8)> {
        let mut out = Vec::new();
        d.feed(bytes, &mut |f| out.push((f.code, f.data)));
        out
    }

    #[test]
    fn a_request_is_four_bytes_between_bang_and_return() {
        assert_eq!(encode(0x00, 0xF0), [0x21, 0x00, 0xF0, 0x0D]);
        assert_eq!(encode(0x0D, 0x28), [0x21, 0x0D, 0x28, 0x0D]);
    }

    /// Every answered burst of the capture, each on a fresh decoder: the
    /// frames are the ones the regular expression finds in the line's hex.
    #[test]
    fn every_answered_burst_decodes_as_the_regex_reads_it() {
        let mut answered = 0;
        let mut with_frame = 0;
        for b in recordings::bursts().iter().filter(|b| !b.rx.is_empty()) {
            answered += 1;
            let got = frames(&mut Decoder::new(), &b.rx);
            assert_eq!(got, recordings::regex_frames(b.rx_hex), "{}", b.label);
            with_frame += got.len();
        }
        // 109 bursts heard something: 108 whole replies, and one cut short
        // by a 4 ms hold.
        assert_eq!(answered, 109);
        assert_eq!(with_frame, 108);
    }

    /// The whole capture through one decoder, at once and in awkward
    /// pieces: the same frames as the regular expression over all of its
    /// hex.
    #[test]
    fn the_capture_as_one_stream_matches_the_regex() {
        let mut all = alloc::string::String::new();
        for b in recordings::bursts() {
            all.push_str(b.rx_hex);
        }
        let bytes = hex(&all);
        let want = recordings::regex_frames(&all);
        assert!(want.len() >= 107);
        assert_eq!(frames(&mut Decoder::new(), &bytes), want);
        for piece in [1, 2, 3, 4, 7] {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for chunk in bytes.chunks(piece) {
                d.feed(chunk, &mut |f| got.push((f.code, f.data)));
            }
            assert_eq!(got, want, "pieces of {piece}");
        }
    }

    /// The capture's own trap: a reply a 4 ms hold cut to `21 00 00`, then
    /// the next burst's volume reply. Without a reset the two read as one
    /// false frame and the volume is lost; with a reset between holds the
    /// cut reply is dropped and the volume reads.
    #[test]
    fn a_cut_reply_must_not_join_the_next_hold() {
        let bursts = recordings::bursts();
        let at = bursts
            .iter()
            .position(|b| b.rx_hex == "210000")
            .expect("the cut reply");
        let (cut, next) = (&bursts[at], &bursts[at + 1]);
        assert_eq!(next.rx_hex, "210d003b0d");

        let mut d = Decoder::new();
        assert_eq!(frames(&mut d, &cut.rx), []);
        assert_eq!(d.pending(), 3);
        assert_eq!(frames(&mut d, &next.rx), [(0x00, 0x21)]);

        let mut d = Decoder::new();
        assert_eq!(frames(&mut d, &cut.rx), []);
        d.reset();
        assert_eq!(frames(&mut d, &next.rx), [(0x0D, 0x3B)]);
        assert_eq!(d.dropped(), 3);
    }

    /// What a floating pin produced before the wiring was right: the
    /// capture's one stray byte, `fe`, and its kin (`f8`, a break's `00`).
    /// No frame, and a real reply behind them still reads.
    #[test]
    fn noise_is_no_frame() {
        let stray = recordings::stray_bytes();
        assert_eq!(stray, [0xFE]);
        let mut d = Decoder::new();
        assert_eq!(frames(&mut d, &stray), []);
        for _ in 0..100 {
            assert_eq!(frames(&mut d, &[0xFE, 0xF8, 0x00, 0xFF, 0x0D]), []);
        }
        assert_eq!(d.pending(), 0);
        assert_eq!(d.dropped(), 501);

        let reply = &recordings::burst("state power try 1").rx;
        assert_eq!(frames(&mut d, reply), [(0x00, 0x01)]);
        assert_eq!(d.dropped(), 501);
    }

    /// A `21` in the noise opens a candidate; it does not cost the reply
    /// behind it.
    #[test]
    fn a_bang_in_the_noise_does_not_cost_the_reply() {
        let reply = recordings::burst("state volume try 1").rx.clone();
        for noise in [
            &[0x21][..],
            &[0x21, 0x21],
            &[0x21, 0xFE],
            &[0x21, 0x00, 0x00, 0xFE],
            &[0xFE, 0x21, 0x0D, 0x21, 0x21],
        ] {
            let mut bytes = noise.to_vec();
            bytes.extend_from_slice(&reply);
            assert_eq!(
                frames(&mut Decoder::new(), &bytes),
                recordings::regex_frames(&recordings::to_hex(&bytes)),
                "{noise:02x?}"
            );
            assert_eq!(
                frames(&mut Decoder::new(), &bytes),
                [(0x0D, 0x3B)],
                "{noise:02x?}"
            );
        }
    }

    /// The one kind of noise that does cost a reply: `21 xx 00` right
    /// before a volume reply is itself the head of a frame, and takes the
    /// reply's `21 0D` as its data and its end. The table knows no code
    /// `FE`, and the link asks again. Before any other reply the same noise
    /// costs nothing.
    #[test]
    fn noise_shaped_like_a_reply_head_takes_the_reply() {
        let mut bytes = alloc::vec![0x21, 0xFE, 0x00];
        bytes.extend_from_slice(&recordings::burst("state volume try 1").rx);
        assert_eq!(frames(&mut Decoder::new(), &bytes), [(0xFE, 0x21)]);
        assert_eq!(
            recordings::regex_frames(&recordings::to_hex(&bytes)),
            [(0xFE, 0x21)]
        );

        let mut bytes = alloc::vec![0x21, 0xFE, 0x00];
        bytes.extend_from_slice(&recordings::burst("state power try 1").rx);
        assert_eq!(frames(&mut Decoder::new(), &bytes), [(0x00, 0x01)]);
    }

    /// A reply split anywhere across two feeds is one frame.
    #[test]
    fn a_reply_split_across_feeds_is_one_frame() {
        for b in recordings::bursts().iter().filter(|b| b.rx.len() == 5) {
            for cut in 0..5 {
                let mut d = Decoder::new();
                let mut got = frames(&mut d, &b.rx[..cut]);
                assert_eq!(got, [], "{} cut at {cut}", b.label);
                got.extend(frames(&mut d, &b.rx[cut..]));
                assert_eq!(got, [(b.rx[1], b.rx[3])], "{} cut at {cut}", b.label);
            }
        }
    }

    /// The capture's requests, each alone and all in a row: four bytes are
    /// never a reply, and neither are a reply's first four.
    #[test]
    fn a_request_echo_and_a_truncated_reply_are_no_frame() {
        let bursts = recordings::bursts();
        let mut row = Decoder::new();
        for b in &bursts {
            assert_eq!(frames(&mut Decoder::new(), &b.tx), [], "{}", b.label);
            assert_eq!(frames(&mut row, &b.tx), [], "{}", b.label);
        }
        for b in bursts.iter().filter(|b| b.rx.len() == 5) {
            for keep in 0..5 {
                assert_eq!(frames(&mut Decoder::new(), &b.rx[..keep]), []);
            }
        }
        // An echo ahead of its reply: the reply still reads.
        let b = recordings::burst("standby request try 1");
        let mut bytes = b.tx.clone();
        bytes.extend_from_slice(&b.rx);
        assert_eq!(frames(&mut Decoder::new(), &bytes), [(0x00, 0x00)]);
    }

    /// The third byte marks a reply: every reply of the capture with
    /// anything but `00` there is no frame. A request followed by a stray
    /// `0D` fits but for that byte; the capture's two requests with data
    /// `00`, standby and mute, are the ones that would read.
    #[test]
    fn the_third_byte_must_be_zero() {
        let bursts = recordings::bursts();
        for b in bursts.iter().filter(|b| b.rx.len() == 5) {
            for third in 1..=255 {
                let mut bytes = b.rx.clone();
                bytes[2] = third;
                assert_eq!(frames(&mut Decoder::new(), &bytes), [], "{}", b.label);
            }
        }
        let mut would_read = Vec::new();
        for b in &bursts {
            let mut bytes = b.tx.clone();
            bytes.push(0x0D);
            let got = frames(&mut Decoder::new(), &bytes);
            assert_eq!(got, recordings::regex_frames(&recordings::to_hex(&bytes)));
            if b.tx[2] == 0x00 {
                assert_eq!(got, [(b.tx[1], 0x0D)], "{}", b.label);
                would_read.push(b.label);
            } else {
                assert_eq!(got, [], "{}", b.label);
            }
        }
        assert_eq!(would_read, ["mute on try 1", "standby request try 1"]);
    }

    /// A reply whose code or data is itself `21` or `0D`.
    #[test]
    fn frame_bytes_as_code_and_data() {
        let mut d = Decoder::new();
        assert_eq!(
            frames(&mut d, &[0x21, 0x0D, 0x00, 0x0D, 0x0D]),
            [(0x0D, 13)]
        );
        assert_eq!(
            frames(&mut d, &[0x21, 0x0D, 0x00, 0x21, 0x0D]),
            [(0x0D, 33)]
        );
        assert_eq!(d.dropped(), 0);
    }
}
