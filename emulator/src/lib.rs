//! A virtual Arcam Radia A25: the amplifier's side of the CONTROL jack, as
//! `docs/protocol.md` and the bring-up capture describe it, for the
//! firmware's emulator (`emulator/README.md`).
//!
//! [`Parser`] finds requests in the bytes the bridge writes, [`Amp`]
//! answers them and keeps the state, and [`Amp::control`] is the test's
//! hand on the front panel. Bytes and milliseconds in, bytes out: the
//! sockets are the binary's.
//!
//! Where the documents do not say what the amplifier does, the virtual amp does
//! the most conservative thing, and the line says `Assumption:`.

use arcam_radia::frame::{END, Frame, REPLY_LEN, REQUEST_LEN, START};
use arcam_radia::poll::AUTO_STANDBY_MS;
use arcam_radia::table::{self, Asked, Command, Input, Mute, Power, Reply, Source};

/// The amplifier's parser runs every 50 ms (`docs/protocol.md#timing`).
pub const TASK_MS: u64 = 50;
/// A reply on the wire: five bytes of ten bits at 19200 baud, rounded up.
pub const REPLY_WIRE_MS: u64 = 3;
/// How long the amplifier answers nothing after a power-on from standby.
/// In the capture the last silent poll began 3.2 s after the request and
/// the first answered one 3.9 s after it.
pub const WAKE_MS: u64 = 3_800;

/// How long after a request heard at `now_ms` its reply is out: the
/// parser's next run, then the reply's own time on the wire. 3 to 53 ms;
/// the bench measured 0 to 50 ms.
pub fn reply_delay_ms(now_ms: u64) -> u64 {
    TASK_MS - now_ms % TASK_MS + REPLY_WIRE_MS
}

/// The amplifier's parser (`docs/protocol.md#framing`): everything before a `21` is
/// dropped, and a `21` is a request only when its fourth byte is `0D`, so
/// a request with two data bytes is dropped whole.
#[derive(Debug, Default)]
pub struct Parser {
    held: [u8; REQUEST_LEN],
    len: usize,
}

impl Parser {
    /// Take one byte; returns the request's code and data when it ends one.
    pub fn push(&mut self, byte: u8) -> Option<Frame> {
        if self.len == 0 && byte != START {
            return None;
        }
        self.held[self.len] = byte;
        self.len += 1;
        if self.len < REQUEST_LEN {
            return None;
        }
        self.len = 0;
        (byte == END).then_some(Frame {
            code: self.held[1],
            data: self.held[2],
        })
    }
}

/// The amplifier's menu settings.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Eco Standby: on and untouched this long, it stands by. `0`: off.
    pub auto_standby_ms: u64,
    /// The input set as System Settings, Streamer Input.
    pub streamer: Source,
}

impl Config {
    /// Eco Standby on at 20 minutes, the Streamer Input on DIGITAL 1.
    /// Assumption: the docs' example Streamer Input; any input would do.
    pub const DEFAULT: Config = Config {
        auto_standby_ms: AUTO_STANDBY_MS as u64,
        streamer: Source::Digital1,
    };
}

/// The amplifier.
#[derive(Debug, Clone)]
pub struct Amp {
    config: Config,
    power: Power,
    volume: u8,
    muted: bool,
    source: Source,
    /// Coming up from standby, deaf, until then.
    waking_until: Option<u64>,
    /// When the auto-standby timer last restarted.
    touched_ms: u64,
    /// Requests it took.
    frames: u32,
    /// Requests the bridge must never send: `0x61` and the power toggle.
    forbidden: u32,
}

impl Amp {
    /// The amplifier as the bench found it (`docs/protocol.md#captures`):
    /// on, volume 59, not muted, DIGITAL 3.
    pub fn new(config: Config, now_ms: u64) -> Amp {
        Amp {
            config,
            power: Power::On,
            volume: 59,
            muted: false,
            source: Source::Digital3,
            waking_until: None,
            touched_ms: now_ms,
            frames: 0,
            forbidden: 0,
        }
    }

    /// The wire's answer to one request heard at `now_ms`, if any.
    pub fn request(&mut self, req: Frame, now_ms: u64) -> Option<[u8; REPLY_LEN]> {
        self.settle(now_ms);
        if self.waking_until.is_some() {
            return None;
        }
        self.frames += 1;
        // Every frame restarts the timer, before it is acted on
        // (`docs/protocol.md#auto-standby`).
        self.touched_ms = now_ms;
        // Assumption: an unknown code, or a data byte outside the table, gets no reply.
        let command = Command::from_code(req.code)?;
        let asked = Asked::of(command, req.data);
        if command == Command::DacFilter || (command == Command::Power && asked == Asked::Toggle) {
            self.forbidden += 1;
        }
        // Assumption: in standby a setter other than power changes nothing and
        // is answered like a query (only queries were seen in standby).
        let asked = if self.power == Power::Standby && command != Command::Power {
            Asked::Query
        } else {
            asked
        };
        let reply = match command {
            Command::Power => self.power(asked, now_ms)?,
            Command::Volume => self.volume(asked)?,
            Command::Mute => self.mute(asked)?,
            Command::Source => self.source(asked)?,
            Command::Model => (asked == Asked::Query).then_some(Reply::Model(table::MODEL_A25))?,
            // Assumption: never sent by the bridge, so not imitated: no reply.
            Command::DacFilter => return None,
        };
        let Frame { code, data } = reply.frame();
        Some([START, code, 0x00, data, END])
    }

    /// The front panel and the remote, one line: `state`, `knob +N`,
    /// `knob -N`, `standby` or `power on`. Returns [`Amp::status`] after it.
    ///
    /// # Errors
    ///
    /// Why the line is not a control, or cannot act now.
    pub fn control(&mut self, line: &str, now_ms: u64) -> Result<String, String> {
        self.settle(now_ms);
        let on = self.power == Power::On && self.waking_until.is_none();
        match line.split_whitespace().collect::<Vec<_>>()[..] {
            ["state"] => {}
            ["knob", turn] => {
                let steps: i16 = turn.parse().map_err(|_| "knob: +N or -N")?;
                if !on {
                    return Err("knob: the amplifier is not on".into());
                }
                let level = (i16::from(self.volume) + steps).clamp(0, table::VOLUME_TOP.into());
                self.volume = u8::try_from(level).unwrap_or(table::VOLUME_TOP);
                // Assumption: the knob steps as the volume frames do, unmuting.
                self.muted = false;
                self.touched_ms = now_ms;
            }
            ["standby"] => {
                self.power = Power::Standby;
                self.waking_until = None;
            }
            ["power", "on"] => {
                self.power(Asked::Set(Power::On.data()), now_ms);
            }
            _ => return Err(format!("no such control: {line:?}")),
        }
        Ok(self.status(now_ms))
    }

    /// One line of `key=value`: `power` (`on`, `standby` or `waking`),
    /// `volume`, `muted`, `source`, `frames` (requests taken), `forbidden`
    /// (`0x61` or power toggles heard) and `idle_ms` (since the
    /// auto-standby timer last restarted).
    pub fn status(&self, now_ms: u64) -> String {
        let power = match (self.waking_until, self.power) {
            (Some(_), _) => "waking",
            (None, Power::On) => "on",
            (None, Power::Standby) => "standby",
        };
        format!(
            "power={power} volume={} muted={} source={} frames={} forbidden={} idle_ms={}",
            self.volume,
            self.muted,
            self.source.key(),
            self.frames,
            self.forbidden,
            now_ms.saturating_sub(self.touched_ms)
        )
    }

    /// Time passes: a wake ends, the auto-standby timer runs out.
    fn settle(&mut self, now_ms: u64) {
        if let Some(until) = self.waking_until {
            if now_ms < until {
                return;
            }
            // A wake clears the mute (`docs/protocol.md#traps`); the timer starts
            // when it is up.
            self.waking_until = None;
            self.power = Power::On;
            self.muted = false;
            self.touched_ms = until;
        }
        let limit = self.config.auto_standby_ms;
        if self.power == Power::On && limit > 0 && now_ms - self.touched_ms >= limit {
            self.power = Power::Standby;
        }
    }

    fn power(&mut self, asked: Asked, now_ms: u64) -> Option<Reply> {
        let on = match asked {
            Asked::Query => return Some(Reply::Power(self.power)),
            Asked::Set(data) => Power::from_data(data)? == Power::On,
            Asked::Toggle => self.power == Power::Standby,
            Asked::Up | Asked::Down | Asked::Streamer => return None,
        };
        if !on {
            self.power = Power::Standby;
        } else if self.power == Power::Standby {
            // The reply comes before the change, then silence (docs: the traps).
            self.waking_until = Some(now_ms + WAKE_MS);
        }
        Some(Reply::Power(if on { Power::On } else { Power::Standby }))
    }

    fn volume(&mut self, asked: Asked) -> Option<Reply> {
        let level = match asked {
            Asked::Query => return Some(Reply::Volume(self.volume)),
            Asked::Up => (self.volume + 1).min(table::VOLUME_TOP),
            Asked::Down => self.volume.saturating_sub(1),
            Asked::Set(n) if n <= table::VOLUME_TOP => n,
            Asked::Set(_) | Asked::Toggle | Asked::Streamer => return None,
        };
        // A volume set or step while muted unmutes first.
        self.muted = false;
        self.volume = level;
        Some(Reply::Volume(level))
    }

    fn mute(&mut self, asked: Asked) -> Option<Reply> {
        self.muted = match asked {
            Asked::Query => self.muted,
            Asked::Set(data) => Mute::from_data(data)?.is_muted(),
            Asked::Toggle => !self.muted,
            Asked::Up | Asked::Down | Asked::Streamer => return None,
        };
        Some(Reply::Mute(if self.muted {
            Mute::Muted
        } else {
            Mute::NotMuted
        }))
    }

    fn source(&mut self, asked: Asked) -> Option<Reply> {
        match asked {
            Asked::Query => {}
            Asked::Streamer => self.source = self.config.streamer,
            // Assumption: volume and mute stay as they were (never seen).
            Asked::Set(n) => self.source = Source::from_data(n)?,
            Asked::Toggle | Asked::Up | Asked::Down => return None,
        }
        Some(Reply::Source(Input {
            source: self.source,
            streamer: self.source == self.config.streamer,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURE: &str = include_str!("../../docs/captures/a25-v1.34-bringup.jsonl");

    // Request data bytes, written out from `docs/protocol.md` ("Commands"):
    // the core makes request bytes only through its policy, and these
    // tests send the amplifier what the policy never would.
    const QUERY: u8 = 0xF0;
    const TOGGLE: u8 = 0x02;
    const STANDBY: u8 = 0x00;
    const ON: u8 = 0x01;
    const UP: u8 = 0xF1;
    const MUTED: u8 = 0x00;
    const STREAMER: u8 = 0xF1;

    /// The string or number after `"key": ` in one line of the capture.
    fn field<'a>(line: &'a str, key: &str) -> &'a str {
        let open = format!("\"{key}\": ");
        let rest = &line[line.find(&open).expect(key) + open.len()..];
        match rest.strip_prefix('"') {
            Some(s) => &s[..s.find('"').expect("a closing quote")],
            None => &rest[..rest.find([',', '}']).expect("a number's end")],
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn frame(bytes: &[u8]) -> Frame {
        let mut p = Parser::default();
        let mut got = bytes.iter().filter_map(|&b| p.push(b));
        let f = got.next().expect("a request");
        assert!(got.next().is_none());
        f
    }

    /// Every burst of the capture, in order and at its own time, through
    /// one amplifier: each reply heard is the model's, byte for byte. The
    /// four silent wake polls are the model's silence. What the model
    /// answers and the capture did not hear is the amplifier's own loss
    /// (its parser starts over every 50 ms) or a hold that ended before the
    /// reply: neither is a state, so the model cannot tell them apart.
    /// Every one of them is a query, so the state after it is the same.
    #[test]
    fn the_capture_replays_byte_for_byte() {
        let mut amp: Option<Amp> = None;
        let (mut exact, mut silent) = (0, 0);
        let mut lost = Vec::new();
        for line in CAPTURE.lines().filter(|l| field(l, "kind") == "burst") {
            let (label, at) = (
                field(line, "label"),
                field(line, "t_ms").parse().expect("ms"),
            );
            let hold: u32 = field(line, "hold_ms").parse().expect("ms");
            let (tx, rx) = (hex(field(line, "tx_hex")), hex(field(line, "rx_hex")));
            let amp = amp.get_or_insert_with(|| Amp::new(Config::DEFAULT, at));
            let reply = amp.request(frame(&tx), at);
            match (reply, rx.len()) {
                (Some(r), 5) => {
                    assert_eq!(r[..], rx[..], "{label}");
                    exact += 1;
                }
                (None, 0) => silent += 1,
                // A 4 ms hold cut the reply short.
                (Some(r), 3) => assert_eq!(r[..3], rx[..], "{label}"),
                (Some(_), 0) => {
                    assert_eq!(tx[2], QUERY, "{label}");
                    lost.push((label, hold));
                }
                (r, _) => panic!("{label}: the model says {r:02x?}, the capture {rx:02x?}"),
            }
        }
        assert_eq!((exact, silent), (108, 4));
        let short = lost.iter().filter(|(_, hold)| *hold < 60).count();
        let lost_at_60: Vec<&str> = lost
            .iter()
            .filter(|(_, hold)| *hold >= 60)
            .map(|(l, _)| *l)
            .collect();
        assert_eq!(short, 16);
        assert_eq!(
            lost_at_60,
            [
                "state mute try 1",
                "loss 5",
                "loss 20",
                "loss 37",
                "mute read try 1",
                "post-wake source try 1"
            ]
        );
    }

    #[test]
    fn the_parser_wants_one_data_byte_and_skips_noise() {
        let mut p = Parser::default();
        let mut got = Vec::new();
        for &b in &[
            0xFE, 0x0D, 0x21, 0x0D, 0xF0, 0x0D, // volume?
            0x21, 0x0D, 0x28, 0x29, 0x0D, // two data bytes: dropped
            0x21, 0x0D, 0x0D, 0x0D, // a stray 21, then 0D 0D 0D: volume 13
        ] {
            got.extend(p.push(b).map(|f| (f.code, f.data)));
        }
        assert_eq!(got, [(0x0D, 0xF0), (0x0D, 0x0D)]);
    }

    fn ask(amp: &mut Amp, code: u8, data: u8, at: u64) -> Option<u8> {
        amp.request(Frame { code, data }, at).map(|r| r[3])
    }

    #[test]
    fn a_wake_is_answered_at_once_then_deaf_then_unmuted() {
        let mut amp = Amp::new(Config::DEFAULT, 0);
        assert_eq!(ask(&mut amp, 0x0E, MUTED, 0), Some(0x00));
        assert_eq!(ask(&mut amp, 0x00, STANDBY, 10), Some(0x00));
        // In standby a setter changes nothing.
        assert_eq!(ask(&mut amp, 0x0D, 30, 20), Some(59));
        assert_eq!(ask(&mut amp, 0x00, ON, 100), Some(0x01));
        assert_eq!(ask(&mut amp, 0x00, QUERY, 100 + WAKE_MS - 1), None);
        assert_eq!(ask(&mut amp, 0x00, QUERY, 100 + WAKE_MS), Some(0x01));
        assert_eq!(ask(&mut amp, 0x0E, QUERY, 100 + WAKE_MS), Some(0x01));
        assert_eq!(amp.forbidden, 0);
    }

    #[test]
    fn every_frame_restarts_the_auto_standby_timer() {
        let config = Config {
            auto_standby_ms: 1_000,
            ..Config::DEFAULT
        };
        let mut amp = Amp::new(config, 0);
        for at in [900, 1_800, 2_700] {
            assert_eq!(ask(&mut amp, 0x00, QUERY, at), Some(0x01));
        }
        assert!(
            amp.control("knob -2", 3_600)
                .expect("on")
                .contains("volume=57")
        );
        assert!(
            amp.control("state", 4_600)
                .expect("state")
                .starts_with("power=standby")
        );
        assert_eq!(ask(&mut amp, 0x00, QUERY, 4_700), Some(0x00));
    }

    #[test]
    fn volume_mute_source_and_the_streamer_bit() {
        let mut amp = Amp::new(Config::DEFAULT, 0);
        assert_eq!(ask(&mut amp, 0x0E, TOGGLE, 0), Some(0x00));
        assert_eq!(ask(&mut amp, 0x0D, UP, 0), Some(60));
        assert_eq!(ask(&mut amp, 0x0E, QUERY, 0), Some(0x01));
        assert_eq!(ask(&mut amp, 0x0D, 99, 0), Some(99));
        assert_eq!(ask(&mut amp, 0x0D, UP, 0), Some(99));
        assert_eq!(ask(&mut amp, 0x0D, 100, 0), None);
        assert_eq!(ask(&mut amp, 0x1D, STREAMER, 0), Some(0x15));
        assert_eq!(ask(&mut amp, 0x1D, 2, 0), Some(0x02));
        assert_eq!(ask(&mut amp, 0x5E, QUERY, 0), Some(table::MODEL_A25));
        assert_eq!(ask(&mut amp, 0x61, QUERY, 0), None);
        assert_eq!(ask(&mut amp, 0x00, TOGGLE, 0), Some(0x00));
        assert_eq!(amp.forbidden, 2);
    }

    #[test]
    fn the_reply_comes_with_the_next_task_run() {
        assert_eq!(reply_delay_ms(0), 53);
        assert_eq!(reply_delay_ms(49), 4);
        assert!((0..1_000).all(|t| (3..=53).contains(&reply_delay_ms(t))));
    }
}
