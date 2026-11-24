//! What may be asked of the amplifier, and the only source of bytes for the
//! wire.
//!
//! A caller names a [`Request`]; [`Policy::check`] returns a [`Wire`] (the
//! four request bytes) or a [`Refusal`]. A `Wire` has no other constructor,
//! so what the types cannot express cannot be sent:
//!
//! - `0x61`, which hands DAC filter control away and persists: no request
//!   names it;
//! - the power toggle (`00 02`): its result depends on a state the caller
//!   may not know, and the wrong guess switches the amplifier off;
//! - a code or an input outside the table.
//!
//! The mute toggle (`0E 02`) can be expressed, as [`Request::MuteToggle`]:
//! its reply reports the state it landed on. Sent twice it would flip back,
//! so [`Request::once`] marks it, together with the volume steps, and the
//! link never sends it again.
//!
//! What the policy refuses, with a reason, is a volume above the ceiling.
//! The amplifier has no ceiling of its own below 99, so
//! [`Policy::volume_max`] is the only one:
//!
//! - an absolute volume above it is refused;
//! - a step up is refused when the last level heard is at or above the
//!   ceiling, and when no level is known.
//!
//! A level the bridge did not set can still be above the ceiling: another
//! input keeps its own volume. [`Policy::clamp`] names the set that brings
//! it back down.
//!
//! The known level is the last one heard. In burst mode a turn of the
//! amplifier's own knob is not heard, so a step up can add one step to a
//! level someone set above the ceiling by hand; its reply then reports
//! that level, and the next step up is refused.
//!
//! Three carriage returns in a row read as "volume 13" to an amplifier
//! waiting behind a stray `21` (`docs/protocol.md`, the traps), so they are
//! no way to flush the line and nothing here produces them as one. The only
//! wire that contains them is the request for volume 13 itself,
//! `21 0D 0D 0D`, when the ceiling allows 13.

use crate::frame;
use crate::table::{self, Command, Mute, Power, Source};

/// What a query can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// Standby or on.
    Power,
    /// The volume, 0 to 99.
    Volume,
    /// Muted or not.
    Mute,
    /// The current input.
    Source,
    /// The model byte.
    Model,
}

impl Query {
    /// Every query.
    pub const ALL: [Query; 5] = [
        Query::Power,
        Query::Volume,
        Query::Mute,
        Query::Source,
        Query::Model,
    ];

    /// The command queried.
    pub const fn command(self) -> Command {
        match self {
            Query::Power => Command::Power,
            Query::Volume => Command::Volume,
            Query::Mute => Command::Mute,
            Query::Source => Command::Source,
            Query::Model => Command::Model,
        }
    }
}

/// A change of volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    /// An absolute level, 0 to the ceiling.
    Set(u8),
    /// One step up.
    Up,
    /// One step down.
    Down,
}

/// A choice of input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// This input.
    Input(Source),
    /// Whichever input is set as the Streamer Input.
    Streamer,
}

/// Everything that can be asked of the amplifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Ask for a value; changes nothing.
    Query(Query),
    /// Go to standby or power on.
    Power(Power),
    /// Set or step the volume.
    Volume(Volume),
    /// Mute or unmute.
    Mute(Mute),
    /// Flip the mute. The reply reports which way it went.
    MuteToggle,
    /// Select an input.
    Source(Select),
}

impl Request {
    /// The command whose reply answers it.
    pub const fn command(self) -> Command {
        match self {
            Request::Query(q) => q.command(),
            Request::Power(_) => Command::Power,
            Request::Volume(_) => Command::Volume,
            Request::Mute(_) | Request::MuteToggle => Command::Mute,
            Request::Source(_) => Command::Source,
        }
    }

    /// A volume step, up or down.
    pub const fn is_step(self) -> bool {
        matches!(self, Request::Volume(Volume::Up | Volume::Down))
    }

    /// A request that does not land on the same state when sent twice: a
    /// volume step or the mute toggle. It is sent once, whatever comes back.
    pub const fn once(self) -> bool {
        self.is_step() || matches!(self, Request::MuteToggle)
    }
}

/// Why a request was not turned into bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// An absolute volume above the ceiling.
    AboveCeiling {
        /// The level asked for.
        asked: u8,
        /// The ceiling.
        max: u8,
    },
    /// An absolute volume above the amplifier's top, 99.
    NoSuchVolume {
        /// The level asked for.
        asked: u8,
    },
    /// A step up from the ceiling or above it.
    AtCeiling {
        /// The last level heard.
        level: u8,
        /// The ceiling.
        max: u8,
    },
    /// A step up with no level heard yet.
    LevelUnknown,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Refusal::AboveCeiling { asked, max } => {
                write!(f, "volume {asked} is above the ceiling of {max}")
            }
            Refusal::NoSuchVolume { asked } => {
                write!(f, "volume {asked} is past the amplifier's top, 99")
            }
            Refusal::AtCeiling { level, max } => write!(
                f,
                "no step up: the volume is {level} and the ceiling is {max}"
            ),
            Refusal::LevelUnknown => {
                f.write_str("no step up: the volume is not known, ask for it first")
            }
        }
    }
}

/// One request's bytes, checked. Made only by [`Policy::check`] and
/// [`Wire::query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire([u8; frame::REQUEST_LEN]);

impl Wire {
    /// A query's bytes. A query changes nothing, so no policy refuses it.
    pub const fn query(what: Query) -> Wire {
        Wire(frame::encode(what.command().code(), table::QUERY))
    }

    /// The four bytes to write.
    pub const fn bytes(&self) -> &[u8; frame::REQUEST_LEN] {
        &self.0
    }

    /// The command code of the reply that answers it.
    pub const fn code(&self) -> u8 {
        self.0[1]
    }
}

/// Limits on what is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The volume ceiling. Nothing sent sets a level above it.
    pub volume_max: u8,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy::DEFAULT
    }
}

impl Policy {
    /// A ceiling of 60 out of 99.
    pub const DEFAULT: Policy = Policy { volume_max: 60 };

    /// The bytes for `request`. `level` is the last volume heard, `None`
    /// when none is known; only a step up depends on it.
    ///
    /// # Errors
    ///
    /// A [`Refusal`] for a volume above the ceiling or above 99, and for a
    /// step up at or above the ceiling or from an unknown level.
    pub fn check(&self, request: Request, level: Option<u8>) -> Result<Wire, Refusal> {
        let max = self.volume_max;
        let (command, data) = match request {
            Request::Query(q) => return Ok(Wire::query(q)),
            Request::Power(p) => (Command::Power, p.data()),
            Request::Mute(m) => (Command::Mute, m.data()),
            Request::MuteToggle => (Command::Mute, table::TOGGLE),
            Request::Source(Select::Input(s)) => (Command::Source, s.data()),
            Request::Source(Select::Streamer) => (Command::Source, table::SOURCE_STREAMER),
            Request::Volume(Volume::Down) => (Command::Volume, table::VOLUME_DOWN),
            Request::Volume(Volume::Set(asked)) => {
                if asked > table::VOLUME_TOP {
                    return Err(Refusal::NoSuchVolume { asked });
                }
                if asked > max {
                    return Err(Refusal::AboveCeiling { asked, max });
                }
                (Command::Volume, asked)
            }
            Request::Volume(Volume::Up) => match level {
                None => return Err(Refusal::LevelUnknown),
                Some(level) if level >= max => return Err(Refusal::AtCeiling { level, max }),
                Some(_) => (Command::Volume, table::VOLUME_UP),
            },
        };
        Ok(Wire(frame::encode(command.code(), data)))
    }

    /// What brings a level heard above the ceiling back down to it: an
    /// absolute set to the ceiling, or `None` when `level` is unknown or
    /// allowed. For a level the bridge did not set, such as the one read
    /// after a change of input, which keeps its own volume
    /// (`docs/protocol.md`, "Commands"). The set unmutes the amplifier, so a
    /// caller that wants it muted asks for the mute again afterwards.
    pub const fn clamp(&self, level: Option<u8>) -> Option<Request> {
        match level {
            Some(level) if level > self.volume_max => {
                Some(Request::Volume(Volume::Set(self.volume_max)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    use super::*;
    use crate::recordings;

    /// Every request the types can express.
    pub fn every_request() -> Vec<Request> {
        let mut all = Vec::new();
        all.extend(Query::ALL.map(Request::Query));
        all.extend([Power::Standby, Power::On].map(Request::Power));
        all.extend([Mute::Muted, Mute::NotMuted].map(Request::Mute));
        all.push(Request::MuteToggle);
        all.extend(Source::ALL.map(|s| Request::Source(Select::Input(s))));
        all.push(Request::Source(Select::Streamer));
        all.push(Request::Volume(Volume::Up));
        all.push(Request::Volume(Volume::Down));
        all.extend((0..=255).map(|n| Request::Volume(Volume::Set(n))));
        all
    }

    /// The request a captured burst's bytes express, for the replays.
    pub fn request_of(tx: &[u8]) -> Request {
        let &[0x21, code, data, 0x0D] = tx else {
            panic!("not a request: {tx:02x?}")
        };
        let command = Command::from_code(code).expect("a code of the table");
        if data == table::QUERY {
            let q = Query::ALL.into_iter().find(|q| q.command() == command);
            return Request::Query(q.expect("a query"));
        }
        match command {
            Command::Power => Request::Power(Power::from_data(data).expect("power")),
            Command::Mute if data == table::TOGGLE => Request::MuteToggle,
            Command::Mute => Request::Mute(Mute::from_data(data).expect("mute")),
            Command::Volume => Request::Volume(match data {
                table::VOLUME_UP => Volume::Up,
                table::VOLUME_DOWN => Volume::Down,
                n => Volume::Set(n),
            }),
            Command::Source => Request::Source(match data {
                table::SOURCE_STREAMER => Select::Streamer,
                n => Select::Input(Source::from_data(n).expect("a source")),
            }),
            Command::Model | Command::DacFilter => panic!("no such request: {tx:02x?}"),
        }
    }

    /// Every wire any policy gives, over every request and every level.
    fn every_wire(policy: Policy) -> Vec<(Request, Option<u8>, Wire)> {
        let levels = core::iter::once(None).chain((0..=255).map(Some));
        let mut out = Vec::new();
        for level in levels {
            for r in every_request() {
                if let Ok(w) = policy.check(r, level) {
                    out.push((r, level, w));
                }
            }
        }
        out
    }

    /// A ceiling under the capture's level of 59.
    const LOW: Policy = Policy { volume_max: 40 };

    const POLICIES: [Policy; 7] = [
        Policy { volume_max: 0 },
        Policy { volume_max: 12 },
        LOW,
        Policy { volume_max: 59 },
        Policy::DEFAULT,
        Policy { volume_max: 99 },
        Policy { volume_max: 255 },
    ];

    #[test]
    fn the_default_ceiling_is_60() {
        assert_eq!(Policy::default().volume_max, 60);
    }

    /// A level above the ceiling is brought down to it with a set the same
    /// policy allows; a level at or under it, or unknown, is left alone.
    #[test]
    fn clamp_sets_the_ceiling_only_above_it() {
        for policy in POLICIES {
            let max = policy.volume_max;
            assert_eq!(policy.clamp(None), None);
            for level in 0..=table::VOLUME_TOP {
                let clamp = policy.clamp(Some(level));
                if level > max {
                    assert_eq!(clamp, Some(Request::Volume(Volume::Set(max))), "{level}");
                    assert!(policy.check(clamp.unwrap(), Some(level)).is_ok(), "{level}");
                } else {
                    assert_eq!(clamp, None, "{level} under {max}");
                }
            }
        }
    }

    /// Every request of the capture is one the types can express, and the
    /// policy that allows it gives the capture's own bytes.
    #[test]
    fn the_captured_requests_come_out_byte_for_byte() {
        // The capture ran at volume 59: its own ceiling.
        let policy = Policy { volume_max: 59 };
        for b in recordings::bursts() {
            let wire = policy
                .check(request_of(&b.tx), Some(58))
                .unwrap_or_else(|e| panic!("{}: {e}", b.label));
            assert_eq!(wire.bytes()[..], b.tx[..], "{}", b.label);
            assert_eq!(wire.code(), b.tx[1]);
        }
    }

    /// The DAC filter's code, the power toggle and every byte outside the
    /// table are in no wire, whatever the ceiling. The mute toggle is in one
    /// wire only, [`Request::MuteToggle`]'s.
    #[test]
    fn the_filter_the_power_toggle_and_strangers_are_unreachable() {
        for policy in POLICIES {
            for (r, level, w) in every_wire(policy) {
                let [start, code, data, end] = *w.bytes();
                let what = alloc::format!("{r:?} at {level:?} under {policy:?}");
                assert_eq!((start, end), (0x21, 0x0D), "{what}");
                assert_ne!(code, 0x61, "{what}");
                let command = Command::from_code(code).expect("a code of the table");
                assert_eq!(command, r.command(), "{what}");
                match command {
                    Command::Power => {
                        assert!(matches!(data, 0x00 | 0x01 | 0xF0), "{what}");
                        assert_ne!(data, table::TOGGLE, "{what}");
                    }
                    Command::Mute => {
                        assert!(matches!(data, 0x00 | 0x01 | 0x02 | 0xF0), "{what}");
                        assert_eq!(data == table::TOGGLE, r == Request::MuteToggle, "{what}");
                    }
                    Command::Volume => {
                        assert!(data <= 99 || matches!(data, 0xF0..=0xF2), "{what}");
                    }
                    Command::Source => {
                        assert!(matches!(data, 1..=9 | 0xF0 | 0xF1), "{what}");
                    }
                    Command::Model => assert_eq!(data, 0xF0, "{what}"),
                    Command::DacFilter => panic!("{what}"),
                }
            }
        }
    }

    #[test]
    fn a_set_above_the_ceiling_is_refused() {
        for policy in POLICIES {
            let max = policy.volume_max;
            for asked in 0..=255u8 {
                let got = policy.check(Request::Volume(Volume::Set(asked)), Some(0));
                if asked > 99 {
                    assert_eq!(got, Err(Refusal::NoSuchVolume { asked }));
                } else if asked > max {
                    assert_eq!(got, Err(Refusal::AboveCeiling { asked, max }));
                } else {
                    assert_eq!(got.expect("at or under the ceiling").bytes()[2], asked);
                }
            }
        }
        let e = Policy::DEFAULT
            .check(Request::Volume(Volume::Set(61)), None)
            .expect_err("61 over 60");
        assert_eq!(e.to_string(), "volume 61 is above the ceiling of 60");
        assert!(
            Policy::DEFAULT
                .check(Request::Volume(Volume::Set(60)), None)
                .is_ok()
        );
        let e = Policy { volume_max: 255 }
            .check(Request::Volume(Volume::Set(100)), None)
            .expect_err("100");
        assert_eq!(e.to_string(), "volume 100 is past the amplifier's top, 99");
    }

    #[test]
    fn a_step_up_is_refused_at_the_ceiling_above_it_and_unknown() {
        let up = Request::Volume(Volume::Up);
        for policy in POLICIES {
            let max = policy.volume_max;
            assert_eq!(policy.check(up, None), Err(Refusal::LevelUnknown));
            for level in 0..=255u8 {
                let got = policy.check(up, Some(level));
                if level >= max {
                    assert_eq!(got, Err(Refusal::AtCeiling { level, max }));
                } else {
                    assert_eq!(got.expect("under the ceiling").bytes()[2], 0xF1);
                }
            }
            // A step down is never refused.
            for level in [None, Some(0), Some(99)] {
                assert!(policy.check(Request::Volume(Volume::Down), level).is_ok());
            }
        }
        // The capture's own step up, from 58: refused under a ceiling of
        // 40, allowed under the default of 60.
        let b = recordings::burst("volume up try 1");
        assert!(Policy::DEFAULT.check(request_of(&b.tx), Some(58)).is_ok());
        let e = LOW
            .check(request_of(&b.tx), Some(58))
            .expect_err("58 is over 40");
        assert_eq!(
            e.to_string(),
            "no step up: the volume is 58 and the ceiling is 40"
        );
        assert_eq!(
            Refusal::LevelUnknown.to_string(),
            "no step up: the volume is not known, ask for it first"
        );
    }

    /// `0D 0D 0D` reads as "volume 13". Nothing produces it but the request
    /// for volume 13, and under a ceiling below 13 nothing at all; wires
    /// sent back to back cannot produce it either, since each starts with
    /// `21`.
    #[test]
    fn three_returns_are_only_ever_volume_13() {
        let has_three = |bytes: &[u8]| bytes.windows(3).any(|w| w == [0x0D; 3]);
        for policy in POLICIES {
            let wires = every_wire(policy);
            for (r, level, w) in &wires {
                assert_eq!(
                    has_three(w.bytes()),
                    *r == Request::Volume(Volume::Set(13)),
                    "{r:?} at {level:?} under {policy:?}"
                );
                assert_eq!(w.bytes()[0], 0x21);
            }
            let any = wires.iter().any(|(_, _, w)| has_three(w.bytes()));
            assert_eq!(any, policy.volume_max >= 13, "{policy:?}");

            // The worst neighbours: every wire that ends `0D 0D`, ahead of
            // every wire.
            let tails = wires.iter().filter(|(_, _, w)| w.bytes()[2] == 0x0D);
            for (_, _, a) in tails {
                for (r, _, b) in wires.iter().filter(|(_, l, _)| l.is_none()) {
                    let mut row = a.bytes().to_vec();
                    row.extend_from_slice(b.bytes());
                    let inside = has_three(a.bytes()) || has_three(b.bytes());
                    assert_eq!(has_three(&row), inside, "{r:?}");
                }
            }
        }
    }

    /// The mute toggle is `21 0E 02 0D` under every ceiling and at every
    /// level, and is the only request besides the steps that is sent once.
    /// No capture holds it: the bytes come from the command table.
    #[test]
    fn the_mute_toggle_is_its_own_wire_and_sent_once() {
        for policy in POLICIES {
            for level in [None, Some(0), Some(59), Some(99)] {
                let w = policy
                    .check(Request::MuteToggle, level)
                    .expect("never refused");
                assert_eq!(*w.bytes(), [0x21, 0x0E, 0x02, 0x0D]);
                assert_eq!(w.code(), 0x0E);
            }
        }
        assert_eq!(request_of(&[0x21, 0x0E, 0x02, 0x0D]), Request::MuteToggle);
        assert_eq!(Request::MuteToggle.command(), Command::Mute);
        let once: Vec<Request> = every_request().into_iter().filter(|r| r.once()).collect();
        assert_eq!(
            once,
            [
                Request::MuteToggle,
                Request::Volume(Volume::Up),
                Request::Volume(Volume::Down),
            ]
        );
        assert!(!Request::MuteToggle.is_step());
    }

    #[test]
    fn a_query_is_never_refused() {
        for q in Query::ALL {
            let w = Wire::query(q);
            assert_eq!(*w.bytes(), [0x21, q.command().code(), 0xF0, 0x0D]);
            for policy in POLICIES {
                assert_eq!(policy.check(Request::Query(q), None), Ok(w));
            }
        }
    }
}
