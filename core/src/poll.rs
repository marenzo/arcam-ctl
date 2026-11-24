//! When to read an amplifier that speaks only when spoken to: a poll
//! schedule for the [`Link`](crate::link::Link).
//!
//! The amplifier restarts its 20-minute auto-standby timer at every
//! complete control frame, and also on an audio signal and on any key,
//! which a bridge cannot see (`docs/protocol.md`, "Auto-standby"). A bridge
//! that polls at a steady pace keeps it on forever. So the schedule polls
//! fast while something is happening and, in [`Mode::StandbySafe`], leaves
//! gaps longer than the timer once it is quiet:
//!
//! - in standby, or with the power unknown: the power, every
//!   [`Spec::standby_ms`] (the timer runs only while the amplifier is on);
//! - on, within [`Spec::window_ms`] of the last change, command or refresh:
//!   one query every [`Spec::tick_ms`], in the order of [`ROTATION`];
//! - on and quiet (the window over): one round of [`ROUND`], back to back,
//!   every [`Spec::quiet_ms`] from the last frame sent.
//!
//! [`Mode::Live`] never closes the window; [`Mode::Off`] never polls. A
//! change a poll sees reopens the window. While the link wakes the
//! amplifier after a power-on nothing is due: the link asks for the power
//! itself.
//!
//! The [`Scheduler`] keeps no clock and sends nothing. It is told the time
//! and what happened, and says when the next poll is due
//! ([`Scheduler::due_ms`]) and which [`Query`] it is ([`Scheduler::take`]).
//! A poll is a query, so the policy never refuses one. Send it once
//! ([`Link::start_once`](crate::link::Link::start_once)): the next tick
//! asks again.
//!
//! The text form of a [`Spec`] is
//! `mode=standby_safe,tick=1000,standby=5000,window=300,quiet=1260`:
//! `tick` and `standby` in ms, `window` and `quiet` in s.

use core::fmt;
use core::ops::RangeInclusive;

use crate::policy::Query;
use crate::state::{Heard, State};

/// The amplifier's auto-standby timer, in ms.
pub const AUTO_STANDBY_MS: u32 = 20 * 60 * 1000;

/// The queries of the window, one per tick: the volume every other one,
/// since the knob and the remote move it most.
pub const ROTATION: [Query; 6] = [
    Query::Volume,
    Query::Power,
    Query::Volume,
    Query::Mute,
    Query::Volume,
    Query::Source,
];

/// The queries of a quiet round, back to back.
pub const ROUND: [Query; 4] = [Query::Power, Query::Volume, Query::Mute, Query::Source];

/// `tick`, in ms.
pub const TICK_MS: RangeInclusive<u32> = 250..=60_000;
/// `standby`, in ms.
pub const STANDBY_MS: RangeInclusive<u32> = 1_000..=600_000;
/// `window`, in s.
pub const WINDOW_S: RangeInclusive<u32> = 10..=3_600;
/// `quiet`, in s. In [`Mode::StandbySafe`] it must also be longer than the
/// amplifier's timer, [`AUTO_STANDBY_MS`].
pub const QUIET_S: RangeInclusive<u32> = 60..=86_400;

/// The longest text a valid [`Spec`] formats to.
pub const TEXT_MAX: usize = 67;

/// How the schedule polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Never.
    Off,
    /// Fast in a window after each change, then a round every `quiet`,
    /// longer than the amplifier's timer: its auto-standby still works.
    StandbySafe,
    /// Fast whenever the amplifier is on. It never auto-standbys.
    Live,
}

impl Mode {
    /// Every mode.
    pub const ALL: [Mode; 3] = [Mode::Off, Mode::StandbySafe, Mode::Live];

    /// The mode's name in the text form.
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::StandbySafe => "standby_safe",
            Mode::Live => "live",
        }
    }

    /// The mode a name names.
    pub fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// A schedule's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// How it polls.
    pub mode: Mode,
    /// The gap between two polls in the window, in ms.
    pub tick_ms: u32,
    /// The gap between two power polls in standby, in ms.
    pub standby_ms: u32,
    /// How long the fast polls go on after a change, in ms (whole seconds).
    pub window_ms: u32,
    /// The gap before a quiet round, from the last frame sent, in ms (whole
    /// seconds).
    pub quiet_ms: u32,
}

/// Why a spec was refused. A refused spec changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecError {
    /// No field at all.
    Empty,
    /// A field that is not `key=value`.
    NotAField,
    /// A key that is not `mode`, `tick`, `standby`, `window` or `quiet`.
    NoSuchKey,
    /// A key given twice.
    Twice(&'static str),
    /// A mode that is not `off`, `standby_safe` or `live`.
    NoSuchMode,
    /// A value that is not plain decimal digits.
    NotANumber(&'static str),
    /// A value outside its range, in the text's unit.
    OutOfRange {
        /// The field.
        key: &'static str,
        /// Its smallest value.
        min: u32,
        /// Its largest value.
        max: u32,
    },
    /// A window or a quiet gap that is not a whole number of seconds.
    NotWholeSeconds(&'static str),
    /// In [`Mode::StandbySafe`], a quiet gap the amplifier's timer would
    /// not outlast.
    QuietTooShort {
        /// The gap asked for, in s.
        quiet_s: u32,
    },
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SpecError::Empty => f.write_str("an empty spec"),
            SpecError::NotAField => f.write_str("a field is not key=value"),
            SpecError::NoSuchKey => {
                f.write_str("no such key: mode, tick, standby, window or quiet")
            }
            SpecError::Twice(key) => write!(f, "{key} is given twice"),
            SpecError::NoSuchMode => f.write_str("no such mode: off, standby_safe or live"),
            SpecError::NotANumber(key) => write!(f, "{key} is not a number"),
            SpecError::OutOfRange { key, min, max } => {
                write!(f, "{key} is outside {min}..={max}")
            }
            SpecError::NotWholeSeconds(key) => write!(f, "{key} is not whole seconds"),
            SpecError::QuietTooShort { quiet_s } => write!(
                f,
                "quiet {quiet_s} s does not outlast the amplifier's {} s standby timer",
                AUTO_STANDBY_MS / 1000
            ),
        }
    }
}

/// The text's keys, in the order they are formatted.
const KEYS: [&str; 5] = ["mode", "tick", "standby", "window", "quiet"];

/// A plain decimal number, saturating.
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.bytes().fold(0u64, |n, b| {
        n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    }))
}

impl Default for Spec {
    fn default() -> Spec {
        Spec::DEFAULT
    }
}

impl Spec {
    /// `mode=standby_safe,tick=1000,standby=5000,window=300,quiet=1260`.
    pub const DEFAULT: Spec = Spec {
        mode: Mode::StandbySafe,
        tick_ms: 1_000,
        standby_ms: 5_000,
        window_ms: 300_000,
        quiet_ms: 1_260_000,
    };

    /// A spec from its text, over the defaults.
    ///
    /// # Errors
    ///
    /// The first fault of the text, as [`Spec::with`].
    pub fn parse(text: &str) -> Result<Spec, SpecError> {
        Spec::DEFAULT.with(text)
    }

    /// This spec with the fields `text` names changed, checked whole.
    ///
    /// # Errors
    ///
    /// The first fault of the text, or of the spec it makes
    /// ([`Spec::check`]). Nothing of a refused text is taken.
    pub fn with(self, text: &str) -> Result<Spec, SpecError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(SpecError::Empty);
        }
        let mut s = self;
        let mut given = [false; KEYS.len()];
        for field in text.split(',') {
            let (key, value) = field.split_once('=').ok_or(SpecError::NotAField)?;
            let (key, value) = (key.trim(), value.trim());
            let i = KEYS
                .iter()
                .position(|k| *k == key)
                .ok_or(SpecError::NoSuchKey)?;
            let key = KEYS[i];
            if core::mem::replace(&mut given[i], true) {
                return Err(SpecError::Twice(key));
            }
            if i == 0 {
                s.mode = Mode::from_name(value).ok_or(SpecError::NoSuchMode)?;
                continue;
            }
            let n = number(value).ok_or(SpecError::NotANumber(key))?;
            let ms = |unit: u64| u32::try_from(n.saturating_mul(unit)).unwrap_or(u32::MAX);
            match i {
                1 => s.tick_ms = ms(1),
                2 => s.standby_ms = ms(1),
                3 => s.window_ms = ms(1000),
                _ => s.quiet_ms = ms(1000),
            }
        }
        s.check()?;
        Ok(s)
    }

    /// Every field in its range, and in [`Mode::StandbySafe`] a quiet gap
    /// longer than the amplifier's timer.
    ///
    /// # Errors
    ///
    /// The first field out of its range or not in whole seconds, then a
    /// quiet gap too short.
    pub fn check(&self) -> Result<(), SpecError> {
        let fields = [
            (KEYS[1], self.tick_ms, 1, TICK_MS),
            (KEYS[2], self.standby_ms, 1, STANDBY_MS),
            (KEYS[3], self.window_ms, 1000, WINDOW_S),
            (KEYS[4], self.quiet_ms, 1000, QUIET_S),
        ];
        for (key, ms, unit, range) in fields {
            if !range.contains(&(ms / unit)) {
                return Err(SpecError::OutOfRange {
                    key,
                    min: *range.start(),
                    max: *range.end(),
                });
            }
            if ms % unit != 0 {
                return Err(SpecError::NotWholeSeconds(key));
            }
        }
        if self.mode == Mode::StandbySafe && self.quiet_ms <= AUTO_STANDBY_MS {
            return Err(SpecError::QuietTooShort {
                quiet_s: self.quiet_ms / 1000,
            });
        }
        Ok(())
    }
}

impl fmt::Display for Spec {
    /// The text form, every field.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mode={},tick={},standby={},window={},quiet={}",
            self.mode.name(),
            self.tick_ms,
            self.standby_ms,
            self.window_ms / 1000,
            self.quiet_ms / 1000
        )
    }
}

/// The amplifier's power, as the scheduler is told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    /// On.
    On,
    /// In standby.
    Standby,
    /// Not heard yet, or forgotten.
    Unknown,
}

impl Power {
    /// The power `state` holds.
    pub fn of(state: &State) -> Power {
        match state.on() {
            Some(true) => Power::On,
            Some(false) => Power::Standby,
            None => Power::Unknown,
        }
    }
}

/// Which part of the schedule is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// [`Mode::Off`]: nothing is polled.
    Off,
    /// The link is waking the amplifier: nothing is polled.
    Waking,
    /// In standby or unknown: the power every `standby`.
    Standby,
    /// On, inside the window: a query every `tick`.
    Window,
    /// On and quiet: a round every `quiet`.
    Quiet,
}

impl Phase {
    /// The phase's name.
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Off => "off",
            Phase::Waking => "waking",
            Phase::Standby => "standby",
            Phase::Window => "window",
            Phase::Quiet => "quiet",
        }
    }
}

/// Whether a poll saw the amplifier move: power, volume, mute or source
/// heard now and different from before, or heard for the first time. A
/// value forgotten is no change.
pub fn moved(before: &State, after: &State) -> bool {
    fn one<T: PartialEq + Copy>(b: Option<Heard<T>>, a: Option<Heard<T>>) -> bool {
        a.is_some() && b.map(|h| h.value) != a.map(|h| h.value)
    }
    one(before.power, after.power)
        || one(before.volume, after.volume)
        || one(before.mute, after.mute)
        || one(before.source, after.source)
}

/// The schedule: what it was told, and when the next poll is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheduler {
    spec: Spec,
    power: Power,
    waking: bool,
    /// The last change, command or refresh: the window runs from it.
    active_at: Option<u64>,
    /// The last frame sent, a poll's or anyone's.
    sent_at: Option<u64>,
    /// The next query of [`ROTATION`].
    turn: usize,
    /// The next query of [`ROUND`]; 0 when no round is under way.
    round: usize,
    /// Polls in a row with no answer.
    misses: u32,
}

impl Scheduler {
    /// A schedule that knows nothing yet: the power unknown, nothing sent,
    /// so a power poll is due at once.
    pub const fn new(spec: Spec) -> Scheduler {
        Scheduler {
            spec,
            power: Power::Unknown,
            waking: false,
            active_at: None,
            sent_at: None,
            turn: 0,
            round: 0,
            misses: 0,
        }
    }

    /// The spec in force.
    pub fn spec(&self) -> Spec {
        self.spec
    }

    /// A new spec, in force at once. A round under way is dropped.
    pub fn set_spec(&mut self, spec: Spec) {
        self.spec = spec;
        self.round = 0;
    }

    /// The power as last told.
    pub fn power(&self) -> Power {
        self.power
    }

    /// The amplifier's power as now heard.
    pub fn set_power(&mut self, power: Power) {
        if power != Power::On {
            self.round = 0;
        }
        self.power = power;
    }

    /// The link is waking the amplifier, or is done.
    pub fn set_waking(&mut self, waking: bool) {
        self.waking = waking;
    }

    /// A value changed: the window opens again from `now_ms`.
    pub fn changed(&mut self, now_ms: u64) {
        self.active_at = Some(now_ms);
        self.round = 0;
    }

    /// A command or a refresh was asked: the window opens again from
    /// `now_ms`.
    pub fn asked(&mut self, now_ms: u64) {
        self.changed(now_ms);
    }

    /// A frame went out at `now_ms`, a poll's or anyone's: the gaps are
    /// measured from it.
    pub fn sent(&mut self, now_ms: u64) {
        self.sent_at = Some(self.sent_at.map_or(now_ms, |t| t.max(now_ms)));
    }

    /// A poll ended, answered or not.
    pub fn polled(&mut self, answered: bool) {
        self.misses = if answered { 0 } else { self.misses + 1 };
    }

    /// Polls in a row that got no answer.
    pub fn misses(&self) -> u32 {
        self.misses
    }

    /// What a poll heard: the power from `after`, and the window opened
    /// again when the amplifier [`moved`]. Whether it did.
    pub fn heard(&mut self, now_ms: u64, before: &State, after: &State) -> bool {
        self.set_power(Power::of(after));
        let moved = moved(before, after);
        if moved {
            self.changed(now_ms);
        }
        moved
    }

    /// The part of the schedule running at `now_ms`.
    pub fn phase(&self, now_ms: u64) -> Phase {
        if self.spec.mode == Mode::Off {
            return Phase::Off;
        }
        if self.waking {
            return Phase::Waking;
        }
        if self.power != Power::On {
            return Phase::Standby;
        }
        if self.spec.mode == Mode::Live {
            return Phase::Window;
        }
        match self.active_at {
            Some(t) if now_ms < t + u64::from(self.spec.window_ms) => Phase::Window,
            _ => Phase::Quiet,
        }
    }

    /// When the next poll is due, on the caller's clock (at or before
    /// `now_ms`: now). `None` while nothing is: off, or waking.
    pub fn due_ms(&self, now_ms: u64) -> Option<u64> {
        let after = |gap: u32| self.sent_at.map_or(0, |t| t + u64::from(gap));
        let quiet = || {
            if self.round > 0 {
                self.sent_at.unwrap_or(0)
            } else {
                after(self.spec.quiet_ms)
            }
        };
        match self.phase(now_ms) {
            Phase::Off | Phase::Waking => None,
            Phase::Standby => Some(after(self.spec.standby_ms)),
            Phase::Window => {
                let at = after(self.spec.tick_ms);
                Some(if self.phase(at.max(now_ms)) == Phase::Window {
                    at
                } else {
                    quiet()
                })
            }
            Phase::Quiet => Some(quiet()),
        }
    }

    /// The query to send now, when one is due; it counts as sent at
    /// `now_ms`. Send it once and say how it ended ([`Scheduler::polled`],
    /// [`Scheduler::heard`]).
    pub fn take(&mut self, now_ms: u64) -> Option<Query> {
        if now_ms < self.due_ms(now_ms)? {
            return None;
        }
        let q = match self.phase(now_ms) {
            Phase::Off | Phase::Waking => return None,
            Phase::Standby => Query::Power,
            Phase::Window => {
                let q = ROTATION[self.turn];
                self.turn = (self.turn + 1) % ROTATION.len();
                q
            }
            Phase::Quiet => {
                let q = ROUND[self.round];
                self.round = (self.round + 1) % ROUND.len();
                q
            }
        };
        self.sent(now_ms);
        Some(q)
    }
}

#[cfg(test)]
mod tests {

    use alloc::string::ToString;
    use alloc::vec::Vec;

    use super::*;
    use crate::state::Heard;
    use crate::table::{Input, Mute, Power as Pw, Source};

    const S: u64 = 1000;
    const MIN: u64 = 60 * S;

    fn spec(text: &str) -> Spec {
        Spec::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn the_default_and_its_text() {
        let d = Spec::default();
        assert_eq!(d, Spec::DEFAULT);
        assert_eq!(
            (d.mode, d.tick_ms, d.standby_ms, d.window_ms, d.quiet_ms),
            (Mode::StandbySafe, 1_000, 5_000, 300_000, 1_260_000)
        );
        let text = "mode=standby_safe,tick=1000,standby=5000,window=300,quiet=1260";
        assert_eq!(d.to_string(), text);
        assert_eq!(Spec::parse(text), Ok(d));
        assert_eq!(d.check(), Ok(()));
        assert!(d.quiet_ms > AUTO_STANDBY_MS);
    }

    /// Every valid spec formats to a text that parses back to it, and none
    /// is longer than [`TEXT_MAX`].
    #[test]
    fn format_and_parse_round_trip() {
        let mut longest = 0;
        for mode in Mode::ALL {
            for tick in [*TICK_MS.start(), 1_000, *TICK_MS.end()] {
                for standby in [*STANDBY_MS.start(), *STANDBY_MS.end()] {
                    for window in [*WINDOW_S.start(), *WINDOW_S.end()] {
                        for quiet in [*QUIET_S.start(), 1_201, *QUIET_S.end()] {
                            let s = Spec {
                                mode,
                                tick_ms: tick,
                                standby_ms: standby,
                                window_ms: window * 1000,
                                quiet_ms: quiet * 1000,
                            };
                            if s.check().is_err() {
                                assert_eq!(mode, Mode::StandbySafe);
                                assert_eq!(quiet, 60);
                                continue;
                            }
                            let text = s.to_string();
                            longest = longest.max(text.len());
                            assert_eq!(Spec::parse(&text), Ok(s), "{text}");
                        }
                    }
                }
            }
        }
        assert_eq!(longest, TEXT_MAX);
    }

    /// A partial text changes only what it names; spaces around the fields
    /// are let be.
    #[test]
    fn a_partial_spec_changes_what_it_names() {
        assert_eq!(
            Spec::parse("mode=live"),
            Ok(Spec {
                mode: Mode::Live,
                ..Spec::DEFAULT
            })
        );
        let base = spec("mode=off,tick=500");
        assert_eq!(
            base.with(" window = 60 , quiet=1800 "),
            Ok(Spec {
                window_ms: 60_000,
                quiet_ms: 1_800_000,
                ..base
            })
        );
        assert_eq!(base.mode, Mode::Off);
        assert_eq!(base.tick_ms, 500);
    }

    /// A bad field refuses the whole text: nothing of it is taken.
    #[test]
    fn an_invalid_spec_is_refused_whole() {
        let refused = [
            ("", SpecError::Empty),
            ("  ", SpecError::Empty),
            ("live", SpecError::NotAField),
            ("mode=live,", SpecError::NotAField),
            ("mode=live,,tick=500", SpecError::NotAField),
            ("speed=3", SpecError::NoSuchKey),
            ("MODE=live", SpecError::NoSuchKey),
            ("mode=live,mode=off", SpecError::Twice("mode")),
            ("tick=500,tick=600", SpecError::Twice("tick")),
            ("mode=fast", SpecError::NoSuchMode),
            ("mode=", SpecError::NoSuchMode),
            ("tick=", SpecError::NotANumber("tick")),
            ("tick=-1", SpecError::NotANumber("tick")),
            ("tick=+5", SpecError::NotANumber("tick")),
            ("tick=1e3", SpecError::NotANumber("tick")),
            ("window=0x10", SpecError::NotANumber("window")),
            (
                "tick=249",
                SpecError::OutOfRange {
                    key: "tick",
                    min: 250,
                    max: 60_000,
                },
            ),
            (
                "tick=99999999999999999999999",
                SpecError::OutOfRange {
                    key: "tick",
                    min: 250,
                    max: 60_000,
                },
            ),
            (
                "standby=999",
                SpecError::OutOfRange {
                    key: "standby",
                    min: 1_000,
                    max: 600_000,
                },
            ),
            (
                "window=9",
                SpecError::OutOfRange {
                    key: "window",
                    min: 10,
                    max: 3_600,
                },
            ),
            (
                "window=5000000",
                SpecError::OutOfRange {
                    key: "window",
                    min: 10,
                    max: 3_600,
                },
            ),
            (
                "mode=live,quiet=86401",
                SpecError::OutOfRange {
                    key: "quiet",
                    min: 60,
                    max: 86_400,
                },
            ),
            ("quiet=1200", SpecError::QuietTooShort { quiet_s: 1200 }),
            (
                "mode=live,quiet=1200,mode=standby_safe",
                SpecError::Twice("mode"),
            ),
            (
                "quiet=600,tick=500",
                SpecError::QuietTooShort { quiet_s: 600 },
            ),
        ];
        for (text, err) in refused {
            assert_eq!(Spec::parse(text), Err(err), "{text:?}");
        }
        // A quiet gap short of the timer is fine where it is not used to
        // let the amplifier sleep.
        assert_eq!(spec("mode=live,quiet=60").quiet_ms, 60_000);
        assert_eq!(spec("mode=off,quiet=1200").quiet_ms, 1_200_000);
        assert_eq!(spec("quiet=1201").quiet_ms, 1_201_000);
        // A spec made by hand is held to the same rules.
        let odd = Spec {
            window_ms: 15_500,
            ..Spec::DEFAULT
        };
        assert_eq!(odd.check(), Err(SpecError::NotWholeSeconds("window")));
        assert_eq!(
            SpecError::QuietTooShort { quiet_s: 1200 }.to_string(),
            "quiet 1200 s does not outlast the amplifier's 1200 s standby timer"
        );
        assert_eq!(
            SpecError::OutOfRange {
                key: "tick",
                min: 250,
                max: 60_000
            }
            .to_string(),
            "tick is outside 250..=60000"
        );
    }

    /// An amplifier state with every value heard at `at`.
    fn state(on: bool, volume: u8, at: u64) -> State {
        State {
            power: Some(Heard {
                value: if on { Pw::On } else { Pw::Standby },
                at_ms: at,
            }),
            volume: Some(Heard {
                value: volume,
                at_ms: at,
            }),
            mute: Some(Heard {
                value: Mute::NotMuted,
                at_ms: at,
            }),
            source: Some(Heard {
                value: Input {
                    source: Source::Digital3,
                    streamer: false,
                },
                at_ms: at,
            }),
            model: None,
        }
    }

    #[test]
    fn what_counts_as_a_move() {
        let a = state(true, 40, 0);
        assert!(!moved(&a, &state(true, 40, 5_000)), "heard again, the same");
        assert!(moved(&a, &state(true, 41, 5_000)));
        assert!(moved(&a, &state(false, 40, 5_000)));
        assert!(moved(&State::new(), &a), "heard for the first time");
        assert!(!moved(&a, &State::new()), "forgotten");
        let mut model = a;
        model.model = Some(Heard { value: 3, at_ms: 9 });
        assert!(!moved(&a, &model), "the model is no state");
    }

    /// The polls a schedule makes over `ms` with the amplifier told `on`
    /// and nothing changing, each answered at once: when, and what.
    fn run(s: &mut Scheduler, from: u64, ms: u64) -> Vec<(u64, Query)> {
        let mut out = Vec::new();
        let mut now = from;
        while now < from + ms {
            match s.take(now) {
                Some(q) => {
                    out.push((now, q));
                    s.polled(true);
                    // A poll's exchange takes about 27 ms.
                    now += 27;
                }
                None => match s.due_ms(now) {
                    Some(at) => now = at.max(now + 1),
                    None => break,
                },
            }
        }
        out
    }

    fn on(spec: Spec) -> Scheduler {
        let mut s = Scheduler::new(spec);
        s.set_power(Power::On);
        s
    }

    /// Nothing known: a power poll at once, then every `standby`.
    #[test]
    fn in_standby_the_power_every_standby_gap() {
        let mut s = Scheduler::new(Spec::DEFAULT);
        assert_eq!(s.phase(0), Phase::Standby);
        assert_eq!(s.due_ms(0), Some(0));
        let polls = run(&mut s, 0, 30 * S);
        let times: Vec<u64> = polls.iter().map(|p| p.0).collect();
        assert_eq!(times, [0, 5_000, 10_000, 15_000, 20_000, 25_000]);
        assert!(polls.iter().all(|p| p.1 == Query::Power));
        s.set_power(Power::Standby);
        assert_eq!(s.phase(30 * S), Phase::Standby);
        // A command's frame moves the next poll on.
        s.sent(27 * S);
        assert_eq!(s.due_ms(28 * S), Some(32 * S));
    }

    /// On, after a change: one query a tick, the rotation in order, until
    /// the window closes; then a round every `quiet`, back to back.
    #[test]
    fn the_window_then_the_quiet_rounds() {
        let mut s = on(Spec::DEFAULT);
        s.changed(0);
        s.sent(0);
        let polls = run(&mut s, 0, 300 * S);
        assert_eq!(
            polls.len(),
            299,
            "one a second, the window's last tick at 299 s"
        );
        for (i, (at, q)) in polls.iter().enumerate() {
            assert_eq!(*at, (i as u64 + 1) * S);
            assert_eq!(*q, ROTATION[i % ROTATION.len()]);
        }
        assert_eq!(s.phase(300 * S), Phase::Quiet);
        let last = 299 * S;
        assert_eq!(s.due_ms(300 * S), Some(last + 1_260 * S));
        let rounds = run(&mut s, 300 * S, 50 * MIN);
        let at: Vec<u64> = rounds.iter().map(|p| p.0).collect();
        let q: Vec<Query> = rounds.iter().map(|p| p.1).collect();
        let r1 = last + 1_260 * S;
        let r2 = r1 + 3 * 27 + 1_260 * S;
        assert_eq!(
            at,
            [r1, r1 + 27, r1 + 54, r1 + 81, r2, r2 + 27, r2 + 54, r2 + 81]
        );
        assert_eq!(q[..4], ROUND);
        assert_eq!(q[4..], ROUND);
    }

    /// In `standby_safe`, while on and quiet, no gap between two frames is
    /// as short as the amplifier's timer except inside a round: its
    /// auto-standby runs out once the window is over.
    #[test]
    fn the_quiet_gap_outlasts_the_auto_standby_timer() {
        let mut s = on(Spec::DEFAULT);
        s.asked(0);
        s.sent(0);
        let polls = run(&mut s, 0, 6 * 60 * MIN);
        let quiet: Vec<u64> = polls
            .iter()
            .map(|p| p.0)
            .filter(|t| *t >= 300 * S)
            .collect();
        let gaps: Vec<u64> = quiet.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.iter()
                .all(|g| *g <= 27 || *g > u64::from(AUTO_STANDBY_MS))
        );
        let first = quiet[0] - 299 * S;
        assert!(first > u64::from(AUTO_STANDBY_MS), "{first}");
        // Live: never a gap as long as the tick.
        let mut s = on(spec("mode=live"));
        s.sent(0);
        let polls = run(&mut s, 0, 60 * MIN);
        let gaps: Vec<u64> = polls.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.iter().all(|g| *g == S), "a tick, always");
        assert_eq!(s.phase(60 * MIN), Phase::Window);
    }

    /// A change seen in the quiet stretch reopens the window and drops the
    /// round; standby ends it.
    #[test]
    fn a_change_reopens_the_window() {
        let mut s = on(Spec::DEFAULT);
        s.sent(0);
        assert_eq!(s.phase(0), Phase::Quiet, "on, and nothing asked yet");
        assert_eq!(s.due_ms(0), Some(1_260 * S));
        let t = 1_260 * S;
        assert_eq!(s.take(t), Some(Query::Power));
        assert_eq!(s.take(t + 27), Some(Query::Volume));
        let before = state(true, 40, 0);
        assert!(s.heard(t + 27, &before, &state(true, 44, t + 27)));
        assert_eq!(s.phase(t + 27), Phase::Window);
        assert_eq!(s.due_ms(t + 27), Some(t + 27 + S));
        assert_eq!(s.take(t + 27 + S), Some(ROTATION[0]));
        // The window runs from the change, not from the polls.
        assert_eq!(s.phase(t + 27 + 299 * S), Phase::Window);
        assert_eq!(s.phase(t + 27 + 300 * S), Phase::Quiet);
        // Heard in standby: the power every 5 s.
        assert!(s.heard(t + 2 * S, &state(true, 44, 0), &state(false, 44, t)));
        assert_eq!(s.phase(t + 2 * S), Phase::Standby);
        assert_eq!(s.power(), Power::Standby);
        assert_eq!(s.due_ms(t + 2 * S), Some(t + 27 + S + 5 * S));
        // Nothing new: no change, the window stays shut.
        let mut q = on(Spec::DEFAULT);
        assert!(!q.heard(5, &state(true, 40, 0), &state(true, 40, 5)));
        assert_eq!(q.phase(5), Phase::Quiet);
    }

    /// Off never polls; waking polls nothing until it is over; a new spec
    /// is in force at once.
    #[test]
    fn off_and_waking_poll_nothing() {
        let mut s = on(spec("mode=off"));
        s.changed(0);
        assert_eq!(s.phase(0), Phase::Off);
        assert_eq!(s.due_ms(0), None);
        assert_eq!(s.take(10 * MIN), None);
        s.set_spec(Spec::DEFAULT);
        assert_eq!(
            s.take(10 * MIN),
            Some(Query::Power),
            "quiet: a round is due"
        );

        let mut s = on(Spec::DEFAULT);
        s.changed(0);
        s.set_waking(true);
        assert_eq!(s.phase(S), Phase::Waking);
        assert_eq!(s.due_ms(S), None);
        assert_eq!(s.take(S), None);
        s.set_waking(false);
        assert_eq!(s.take(S), Some(ROTATION[0]));
    }

    /// The window's last tick falls in the window; a tick past its end
    /// waits for the quiet round instead.
    #[test]
    fn a_tick_past_the_window_waits_for_the_round() {
        let mut s = on(Spec::DEFAULT);
        s.changed(0);
        s.sent(299_500);
        assert_eq!(s.phase(299_600), Phase::Window);
        assert_eq!(s.due_ms(299_600), Some(299_500 + 1_260 * S));
        assert_eq!(s.take(300_500), None);
    }

    #[test]
    fn misses_count_in_a_row() {
        let mut s = Scheduler::new(Spec::DEFAULT);
        assert_eq!(s.misses(), 0);
        s.polled(false);
        s.polled(false);
        assert_eq!(s.misses(), 2);
        s.polled(true);
        assert_eq!(s.misses(), 0);
    }

    #[test]
    fn names() {
        for m in Mode::ALL {
            assert_eq!(Mode::from_name(m.name()), Some(m));
        }
        assert_eq!(Mode::from_name("Live"), None);
        let phases = [
            Phase::Off,
            Phase::Waking,
            Phase::Standby,
            Phase::Window,
            Phase::Quiet,
        ]
        .map(Phase::name);
        assert_eq!(phases, ["off", "waking", "standby", "window", "quiet"]);
    }
}
