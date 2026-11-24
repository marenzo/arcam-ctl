//! The exchange in burst mode: one request written, the line held for the
//! reply, the reply read.
//!
//! The link is a state machine and nothing else. Its inputs are the
//! caller's clock in ms and the bytes the UART read; its outputs are a
//! [`Burst`] to write and [`Action`]s. It owns the [`State`] and folds every
//! reply into it.
//!
//! One request is in flight at a time:
//!
//! - [`Link::start`] checks the request against the policy and returns the
//!   burst: four bytes, and how long to hold the line after them. The reply
//!   comes 0 to 50 ms after the request, so the hold is never shorter than
//!   [`HOLD_MIN_MS`].
//! - [`Link::feed`] takes the time and what was read. The bytes are read
//!   before the clock, so a reply handed over after its hold has ended
//!   still counts. A reply is paired with the request by its command code
//!   ([`Action::Done`]); a reply for another code is
//!   [`Action::Unsolicited`].
//! - When the hold passes with no reply, the link returns the burst once
//!   more, [`Action::Retry`]: the amplifier lost about one request in 17 on the bench
//!   (its parser starts over every 50 ms). A second silence is
//!   [`Action::Failed`].
//! - A volume step is the exception. Sent twice it could land twice, and an
//!   unanswered one has an unknown outcome, so it is never sent again: the
//!   link fails it at once and forgets the level, and the policy then
//!   refuses a step up until the volume has been queried.
//! - The mute toggle is the other ([`Request::once`]): sent twice it would
//!   flip back. Unanswered, it fails at once and the mute is forgotten
//!   until it is queried. Answered, its reply is the state it landed on,
//!   folded in like any mute reply.
//! - [`Link::start_once`] sends any request once, whatever comes back: for
//!   a poll ([`crate::poll`]), which the next one stands in for. A silence
//!   is [`Failure::Unanswered`].
//!
//! # Waking from standby
//!
//! The amplifier answers a power-on with `01` at once, before anything has
//! changed, and then answers nothing for about 4 s. So an answered power-on
//! starts the waking phase: volume, mute and source are forgotten (the wake
//! clears the mute without reporting it), no request is accepted, and the
//! link issues a power query, [`Action::Poll`], every
//! [`Config::wake_poll_ms`]. The first `01` ends it with [`Action::Awake`]
//! and the three requests to send next; [`Config::wake_timeout_ms`] of
//! silence ends it with [`Action::NotAwake`] and the power unknown.
//!
//! A power-on whose `01` is lost is not sent again into that silence: it
//! starts the waking phase too, unconfirmed. A `00` to one of its polls
//! means the power-on never landed, and it is sent once more
//! ([`Action::Retry`]); a second silence fails it.
//!
//! The decoder is reset before every burst: the amplifier's driver sleeps
//! between holds, so whatever an earlier hold left behind is noise.

use crate::frame::{Decoder, Frame, REQUEST_LEN};
use crate::policy::{Policy, Query, Refusal, Request, Wire};
use crate::state::State;
use crate::table::{Power, Reply};

/// The shortest hold. On the bench a 60 ms hold caught 5 of 5 replies,
/// 40 ms 4 of 5.
pub const HOLD_MIN_MS: u32 = 60;
/// A request on the wire, rounded up: four bytes at 19200 baud take 2.1 ms.
/// The hold starts after them.
pub const REQUEST_MS: u64 = 3;
/// What to ask once the amplifier is awake: what a wake may have changed.
pub const AFTER_WAKE: [Request; 3] = [
    Request::Query(Query::Volume),
    Request::Query(Query::Mute),
    Request::Query(Query::Source),
];

/// The link's timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// How long the line is held for a reply. Read through
    /// [`Config::hold`], which keeps it at [`HOLD_MIN_MS`] or more.
    pub hold_ms: u32,
    /// The gap between two power queries while waking.
    pub wake_poll_ms: u32,
    /// How long a wake may take before it is given up.
    pub wake_timeout_ms: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config::DEFAULT
    }
}

impl Config {
    /// A 150 ms hold, a wake poll every 500 ms, and a wake timeout of 8 s,
    /// about twice the wake measured on the bench.
    pub const DEFAULT: Config = Config {
        hold_ms: 150,
        wake_poll_ms: 500,
        wake_timeout_ms: 8000,
    };

    /// The hold in use: [`Config::hold_ms`], at least [`HOLD_MIN_MS`].
    pub const fn hold(&self) -> u32 {
        if self.hold_ms < HOLD_MIN_MS {
            HOLD_MIN_MS
        } else {
            self.hold_ms
        }
    }
}

/// One write: the request's bytes, then the line held at 3.3 V for
/// `hold_ms` while the reply comes.
///
/// The hold ends early: two character times after the last byte of a
/// whole reply ([`Burst::held_ms`]), so the amplifier's driver sleeps again
/// as soon as it has answered and the receive pin is exposed to it for as
/// short a time as possible. `hold_ms` stays the cap: a reply that does not
/// come is waited for the whole hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burst {
    /// The request.
    pub wire: Wire,
    /// How long to hold the line after the request's last stop bit, at most.
    pub hold_ms: u32,
}

impl Burst {
    /// The four bytes to write.
    pub const fn bytes(&self) -> &[u8; REQUEST_LEN] {
        self.wire.bytes()
    }

    /// How long after the hold began it ends, at `baud`: two character
    /// times after a whole reply was in (`reply_in_ms` after the hold
    /// began), at most [`Burst::hold_ms`]; the whole hold with no reply.
    pub fn held_ms(&self, reply_in_ms: Option<u32>, baud: u32) -> u32 {
        match reply_in_ms {
            Some(t) => {
                let end = u64::from(t) + two_chars_ms(baud);
                u32::try_from(end).unwrap_or(u32::MAX).min(self.hold_ms)
            }
            None => self.hold_ms,
        }
    }
}

/// Two character times at `baud`, in ms, never under 2: what a line needs
/// at idle before a byte's start bit can be told from a break, and what an
/// early release leaves after a reply for a byte still on its way.
fn two_chars_ms(baud: u32) -> u64 {
    (20_000 / u64::from(baud.max(1))).max(2)
}

/// Why [`Link::start`] returned no burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// A request is in flight.
    Busy,
    /// The amplifier is waking and answers nothing.
    Waking,
    /// The policy refused the request.
    Refused(Refusal),
}

impl core::fmt::Display for Rejected {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Rejected::Busy => f.write_str("a request is still in flight"),
            Rejected::Waking => f.write_str("the amplifier is waking and answers nothing yet"),
            Rejected::Refused(r) => r.fmt(f),
        }
    }
}

/// Why an exchange ended without its reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Sent twice, answered neither time.
    NoAnswer,
    /// A volume step got no answer. It was not sent again, and the level is
    /// unknown until the volume is queried.
    StepUnanswered,
    /// The mute toggle got no answer. It was not sent again, and the mute is
    /// unknown until it is queried.
    ToggleUnanswered,
    /// A request started with [`Link::start_once`] got no answer.
    Unanswered,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Failure::NoAnswer => "no answer, after one retry",
            Failure::StepUnanswered => {
                "no answer to a volume step; it is not sent twice, ask for the volume"
            }
            Failure::ToggleUnanswered => {
                "no answer to the mute toggle; it is not sent twice, ask for the mute"
            }
            Failure::Unanswered => "no answer; it was sent once",
        })
    }
}

/// What a [`Link::feed`] reports, and what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The request in flight was answered.
    Done(Reply),
    /// No reply came: write this burst, the same request once more.
    Retry(Burst),
    /// The request in flight is given up.
    Failed(Failure),
    /// A reply nobody asked for, already folded into the state.
    Unsolicited(Reply),
    /// A frame the table cannot interpret. It answers nothing and changes
    /// nothing.
    Unread(Frame),
    /// Waking: write this burst, a power query.
    Poll(Burst),
    /// The amplifier answered `01`: it is up. Send these next.
    Awake {
        /// The queries to start, in order: [`AFTER_WAKE`].
        ask: [Request; 3],
    },
    /// The wake timed out with no `01`; the power is unknown.
    NotAwake,
}

/// What the link is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing in flight; [`Link::start`] accepts a request.
    Idle,
    /// A request is in flight.
    Exchange,
    /// A power-on was answered; the amplifier is coming up.
    Waking,
}

#[derive(Debug, Clone, Copy)]
enum Inner {
    Idle,
    Exchange {
        request: Request,
        wire: Wire,
        /// When the hold in progress ends.
        deadline: u64,
        retried: bool,
        /// Sent once, whatever comes back ([`Link::start_once`]).
        single: bool,
    },
    Waking {
        /// When the wake is given up.
        until: u64,
        /// When the next power query is due.
        next_poll: u64,
        /// The end of a poll's hold, while one is out.
        poll_until: Option<u64>,
        /// The power-on, while its `01` is unheard: sent again on a `00`.
        unconfirmed: Option<Wire>,
    },
}

/// The link to one amplifier.
#[derive(Debug, Clone)]
pub struct Link {
    policy: Policy,
    config: Config,
    state: State,
    decoder: Decoder,
    inner: Inner,
}

impl Link {
    /// An idle link that knows nothing of the amplifier yet.
    pub const fn new(policy: Policy, config: Config) -> Link {
        Link {
            policy,
            config,
            state: State::new(),
            decoder: Decoder::new(),
            inner: Inner::Idle,
        }
    }

    /// The amplifier as last heard.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The policy requests are judged by.
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Replace the policy: every request started from now on is judged by
    /// `policy`. A request in flight is already on the wire.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    /// The link's timing.
    pub fn config(&self) -> Config {
        self.config
    }

    /// What the link is doing.
    pub fn phase(&self) -> Phase {
        match self.inner {
            Inner::Idle => Phase::Idle,
            Inner::Exchange { .. } => Phase::Exchange,
            Inner::Waking { .. } => Phase::Waking,
        }
    }

    /// Bytes heard that were part of no frame, since the link was made.
    pub fn noise(&self) -> u32 {
        self.decoder.dropped()
    }

    /// When the link next wants a [`Link::feed`], even with nothing read:
    /// the end of the hold in progress, or the next poll of a wake. `None`
    /// when idle.
    pub fn deadline_ms(&self) -> Option<u64> {
        match self.inner {
            Inner::Idle => None,
            Inner::Exchange { deadline, .. } => Some(deadline),
            Inner::Waking {
                until,
                next_poll,
                poll_until,
                ..
            } => Some(poll_until.unwrap_or(next_poll.min(until))),
        }
    }

    /// Begin an exchange: the burst to write now. A step up is judged by
    /// the level in [`Link::state`].
    ///
    /// # Errors
    ///
    /// [`Rejected::Busy`] while a request is in flight,
    /// [`Rejected::Waking`] while the amplifier wakes, and
    /// [`Rejected::Refused`] when the policy refuses the request. The link
    /// is unchanged in every case.
    pub fn start(&mut self, request: Request, now_ms: u64) -> Result<Burst, Rejected> {
        self.begin(request, now_ms, false)
    }

    /// Begin an exchange that is never sent again: a silence fails it with
    /// [`Failure::Unanswered`] (a step or the toggle with their own
    /// failures). For a poll, which the next one stands in for.
    ///
    /// # Errors
    ///
    /// As [`Link::start`].
    pub fn start_once(&mut self, request: Request, now_ms: u64) -> Result<Burst, Rejected> {
        self.begin(request, now_ms, true)
    }

    fn begin(&mut self, request: Request, now_ms: u64, single: bool) -> Result<Burst, Rejected> {
        match self.inner {
            Inner::Idle => {}
            Inner::Exchange { .. } => return Err(Rejected::Busy),
            Inner::Waking { .. } => return Err(Rejected::Waking),
        }
        let wire = self
            .policy
            .check(request, self.state.level())
            .map_err(Rejected::Refused)?;
        self.inner = Inner::Exchange {
            request,
            wire,
            deadline: self.hold_end(now_ms),
            retried: false,
            single,
        };
        Ok(self.burst(wire))
    }

    /// Hand over what the UART read (possibly nothing) at `now_ms`. The
    /// actions come out in order: first what the bytes held, then what the
    /// clock says. Dropping the iterator early still consumes every byte and
    /// reads the clock.
    #[must_use = "the actions say what to write next"]
    pub fn feed<'a>(&'a mut self, now_ms: u64, bytes: &'a [u8]) -> Feed<'a> {
        Feed {
            link: self,
            bytes,
            now_ms,
            clock_read: false,
        }
    }

    /// A burst going out now: the decoder starts clean.
    fn burst(&mut self, wire: Wire) -> Burst {
        self.decoder.reset();
        Burst {
            wire,
            hold_ms: self.config.hold(),
        }
    }

    /// When the hold of a burst written at `now_ms` ends.
    fn hold_end(&self, now_ms: u64) -> u64 {
        now_ms + REQUEST_MS + u64::from(self.config.hold())
    }

    fn frame(&mut self, frame: Frame, now_ms: u64) -> Action {
        let Ok(reply) = Reply::from_frame(frame) else {
            return Action::Unread(frame);
        };
        self.state.apply(reply, now_ms);
        match self.inner {
            Inner::Exchange { request, .. } if reply.command() == request.command() => {
                self.inner = Inner::Idle;
                if request == Request::Power(Power::On) && reply == Reply::Power(Power::On) {
                    self.wake(now_ms, None);
                }
                Action::Done(reply)
            }
            Inner::Waking { .. } if reply == Reply::Power(Power::On) => {
                self.inner = Inner::Idle;
                Action::Awake { ask: AFTER_WAKE }
            }
            Inner::Waking {
                unconfirmed: Some(wire),
                ..
            } if reply == Reply::Power(Power::Standby) => {
                self.inner = Inner::Exchange {
                    request: Request::Power(Power::On),
                    wire,
                    deadline: self.hold_end(now_ms),
                    retried: true,
                    single: false,
                };
                Action::Retry(self.burst(wire))
            }
            _ => Action::Unsolicited(reply),
        }
    }

    /// The waking phase from `now_ms`: what the amplifier was set to is
    /// forgotten, and its power is polled. `unconfirmed`: the power-on,
    /// while its `01` is unheard.
    fn wake(&mut self, now_ms: u64, unconfirmed: Option<Wire>) {
        self.state.forget_settings();
        self.inner = Inner::Waking {
            until: now_ms + u64::from(self.config.wake_timeout_ms),
            next_poll: now_ms + u64::from(self.config.wake_poll_ms),
            poll_until: None,
            unconfirmed,
        };
    }

    fn clock(&mut self, now_ms: u64) -> Option<Action> {
        match self.inner {
            Inner::Idle => None,
            Inner::Exchange { deadline, .. } if now_ms < deadline => None,
            Inner::Exchange { request, .. } if request.once() => {
                self.inner = Inner::Idle;
                Some(Action::Failed(if request.is_step() {
                    self.state.volume = None;
                    Failure::StepUnanswered
                } else {
                    self.state.mute = None;
                    Failure::ToggleUnanswered
                }))
            }
            Inner::Exchange { single: true, .. } => {
                self.inner = Inner::Idle;
                Some(Action::Failed(Failure::Unanswered))
            }
            Inner::Exchange { retried: true, .. } => {
                self.inner = Inner::Idle;
                Some(Action::Failed(Failure::NoAnswer))
            }
            // Its `01` lost, or the power-on itself: the polls tell which.
            Inner::Exchange {
                request: Request::Power(Power::On),
                wire,
                ..
            } => {
                self.wake(now_ms, Some(wire));
                None
            }
            Inner::Exchange { request, wire, .. } => {
                self.inner = Inner::Exchange {
                    request,
                    wire,
                    deadline: self.hold_end(now_ms),
                    retried: true,
                    single: false,
                };
                Some(Action::Retry(self.burst(wire)))
            }
            Inner::Waking {
                poll_until: Some(t),
                ..
            } if now_ms < t => None,
            Inner::Waking { until, .. } if now_ms >= until => {
                self.inner = Inner::Idle;
                self.state.forget_power();
                Some(Action::NotAwake)
            }
            Inner::Waking {
                until,
                next_poll,
                unconfirmed,
                ..
            } => {
                if now_ms < next_poll {
                    self.inner = Inner::Waking {
                        until,
                        next_poll,
                        poll_until: None,
                        unconfirmed,
                    };
                    return None;
                }
                self.inner = Inner::Waking {
                    until,
                    next_poll: now_ms + u64::from(self.config.wake_poll_ms),
                    poll_until: Some(self.hold_end(now_ms)),
                    unconfirmed,
                };
                Some(Action::Poll(self.burst(Wire::query(Query::Power))))
            }
        }
    }
}

/// The actions of one [`Link::feed`]: first what the bytes held, then what
/// the clock says.
#[derive(Debug)]
pub struct Feed<'a> {
    link: &'a mut Link,
    bytes: &'a [u8],
    now_ms: u64,
    clock_read: bool,
}

impl Iterator for Feed<'_> {
    type Item = Action;

    fn next(&mut self) -> Option<Action> {
        while let Some((&byte, rest)) = self.bytes.split_first() {
            self.bytes = rest;
            if let Some(frame) = self.link.decoder.push(byte) {
                return Some(self.link.frame(frame, self.now_ms));
            }
        }
        if self.clock_read {
            return None;
        }
        self.clock_read = true;
        self.link.clock(self.now_ms)
    }
}

impl Drop for Feed<'_> {
    /// An iterator dropped early still moves the link through all of its
    /// bytes and its clock.
    fn drop(&mut self) {
        while self.next().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    use super::*;
    use crate::policy::Volume;
    use crate::policy::tests::request_of;
    use crate::recordings::{self, Burst as Line};
    use crate::table::{Input, Mute, Source};

    /// The hold's end for a burst written at `t`, default timing.
    const HOLD: u64 = REQUEST_MS + 150;
    /// The capture's volume was 59: its ceiling for the replays.
    const CAPTURE: Policy = Policy { volume_max: 59 };
    /// A ceiling under the capture's level.
    const LOW: Policy = Policy { volume_max: 40 };

    fn feed(link: &mut Link, now_ms: u64, bytes: &[u8]) -> Vec<Action> {
        link.feed(now_ms, bytes).collect()
    }

    fn rx(label: &str) -> Vec<u8> {
        recordings::burst(label).rx
    }

    /// Start `line`'s request at its own time; the burst is the line's
    /// bytes.
    fn start(link: &mut Link, line: &Line) -> Burst {
        let burst = link
            .start(request_of(&line.tx), line.at_ms)
            .unwrap_or_else(|e| panic!("{}: {e}", line.label));
        assert_eq!(burst.bytes()[..], line.tx[..], "{}", line.label);
        burst
    }

    /// The whole capture through one link, on the capture's clock. Every
    /// line with a hold the link would use (60 ms and up) is one burst of
    /// the link: a request, the retry of the lost request before it, or a
    /// poll of the wake. The bytes the link writes are the capture's, and
    /// every answer is what the line's label asked.
    #[test]
    fn the_whole_capture_through_the_link() {
        let lines: Vec<Line> = recordings::bursts()
            .into_iter()
            .filter(|b| b.hold_ms >= HOLD_MIN_MS)
            .collect();
        assert_eq!(lines.len(), 110);
        let mut link = Link::new(CAPTURE, Config::DEFAULT);
        let (mut done, mut retries, mut polls, mut silent_polls) = (0, 0, 0, 0);
        let mut awake_after_ms = None;
        let mut wake_at = 0;
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];
            i += 1;
            let l = line.label;

            if link.phase() == Phase::Waking {
                // The link's own poll, when the capture sent its own.
                let burst = Burst {
                    wire: Wire::query(Query::Power),
                    hold_ms: 150,
                };
                assert_eq!(
                    feed(&mut link, line.at_ms, &[]),
                    [Action::Poll(burst)],
                    "{l}"
                );
                assert_eq!(burst.bytes()[..], line.tx[..], "{l}");
                polls += 1;
                let heard = feed(&mut link, line.at_ms + HOLD, &line.rx);
                if line.rx.is_empty() {
                    assert_eq!(heard, [], "{l}");
                    assert_eq!(link.phase(), Phase::Waking, "{l}");
                    silent_polls += 1;
                } else {
                    assert_eq!(heard, [Action::Awake { ask: AFTER_WAKE }], "{l}");
                    assert_eq!(link.phase(), Phase::Idle, "{l}");
                    assert_eq!(link.state().on(), Some(true));
                    awake_after_ms = Some(line.at_ms - wake_at);
                }
                continue;
            }

            let burst = start(&mut link, line);
            assert_eq!(
                link.start(request_of(&line.tx), line.at_ms),
                Err(Rejected::Busy)
            );
            let mut t = line.at_ms + HOLD;
            let mut answer = line;
            if line.rx.is_empty() {
                // Lost. One retry, and the capture's next line is that
                // request sent again.
                assert_eq!(feed(&mut link, t, &[]), [Action::Retry(burst)], "{l}");
                retries += 1;
                answer = &lines[i];
                i += 1;
                assert_eq!(answer.tx, line.tx, "{l}");
                t += HOLD;
            }
            let want = recordings::answer(answer);
            assert_eq!(feed(&mut link, t, &answer.rx), [Action::Done(want)], "{l}");
            done += 1;

            let state = link.state();
            match want {
                Reply::Power(power) => {
                    assert_eq!(
                        state.power.map(|h| (h.value, h.at_ms)),
                        Some((power, t)),
                        "{l}"
                    );
                }
                Reply::Volume(volume) => assert_eq!(state.level(), Some(volume), "{l}"),
                Reply::Mute(mute) => assert_eq!(state.mute.map(|h| h.value), Some(mute), "{l}"),
                Reply::Source(input) => {
                    assert_eq!(state.source.map(|h| h.value), Some(input), "{l}");
                }
                Reply::Model(model) => assert_eq!(state.model.map(|h| h.value), Some(model), "{l}"),
                Reply::DacFilter { .. } => panic!("{l}"),
            }
            if l == "wake request try 1" {
                // The amplifier said on; it is not up, and what it was set
                // to is no longer known.
                assert_eq!(link.phase(), Phase::Waking);
                assert_eq!((state.volume, state.mute, state.source), (None, None, None));
                assert_eq!(link.start(AFTER_WAKE[0], t), Err(Rejected::Waking));
                wake_at = line.at_ms;
            } else {
                assert_eq!(link.phase(), Phase::Idle, "{l}");
            }
        }

        // 110 lines: 5 were the wake's polls, 6 lost requests took a
        // second line each.
        assert_eq!((done, retries, polls, silent_polls), (99, 6, 5, 4));
        // The capture's fifth poll, 3.9 s after its wake request, read on.
        assert_eq!(awake_after_ms, Some(3943));
        // After the wake the capture asked again, and nothing had moved.
        let s = link.state();
        assert_eq!(
            (s.on(), s.level(), s.muted()),
            (Some(true), Some(59), Some(false))
        );
        let input = Input {
            source: Source::Digital3,
            streamer: false,
        };
        assert_eq!(s.source.map(|h| h.value), Some(input));
        assert_eq!(s.model.map(|h| h.value), Some(3));
        assert_eq!(link.noise(), 0);
    }

    /// The three lost queries of the loss test, and every other lost
    /// request of the capture: one retry at the hold's end, the same
    /// bytes, and no second one.
    #[test]
    fn a_lost_request_gets_exactly_one_retry() {
        let lost: Vec<Line> = recordings::bursts()
            .into_iter()
            .filter(|b| b.rx.is_empty() && !b.label.starts_with("wake poll"))
            .collect();
        let of_the_loss_test = lost.iter().filter(|b| b.label.starts_with("loss "));
        assert_eq!(of_the_loss_test.count(), 3);
        assert_eq!(lost.len(), 22);
        for line in &lost {
            let l = line.label;
            let mut link = Link::new(CAPTURE, Config::DEFAULT);
            let burst = start(&mut link, line);
            let t0 = line.at_ms;
            let mut actions = Vec::new();
            // Nothing is heard, a millisecond at a time, for a second.
            for t in t0..t0 + 1000 {
                for a in link.feed(t, &line.rx) {
                    actions.push((t - t0, a));
                }
            }
            assert_eq!(
                actions,
                [
                    (HOLD, Action::Retry(burst)),
                    (2 * HOLD, Action::Failed(Failure::NoAnswer)),
                ],
                "{l}"
            );
            assert_eq!(link.phase(), Phase::Idle, "{l}");
            assert_eq!(link.deadline_ms(), None, "{l}");
            assert_eq!(*link.state(), State::new(), "{l}");
        }
        assert_eq!(Failure::NoAnswer.to_string(), "no answer, after one retry");
    }

    /// Started once, every lost request of the capture ends at its hold
    /// with no retry; answered, it is done as any other. A step and the
    /// toggle keep their own failures.
    #[test]
    fn a_request_started_once_is_never_sent_again() {
        let lost: Vec<Line> = recordings::bursts()
            .into_iter()
            .filter(|b| b.rx.is_empty() && !b.label.starts_with("wake poll"))
            .collect();
        for line in &lost {
            let mut link = Link::new(CAPTURE, Config::DEFAULT);
            let burst = link
                .start_once(request_of(&line.tx), line.at_ms)
                .expect("the capture's request");
            assert_eq!(burst.bytes()[..], line.tx[..], "{}", line.label);
            let t0 = line.at_ms;
            let mut actions = Vec::new();
            for t in t0..t0 + 1000 {
                for a in link.feed(t, &[]) {
                    actions.push((t - t0, a));
                }
            }
            let want = match request_of(&line.tx) {
                r if r.is_step() => Failure::StepUnanswered,
                Request::MuteToggle => Failure::ToggleUnanswered,
                _ => Failure::Unanswered,
            };
            assert_eq!(actions, [(HOLD, Action::Failed(want))], "{}", line.label);
            assert_eq!(link.phase(), Phase::Idle);
        }
        let line = recordings::burst("state volume try 1");
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        link.start_once(Request::Query(Query::Volume), line.at_ms)
            .expect("a query");
        assert_eq!(
            feed(&mut link, line.at_ms + 25, &line.rx),
            [Action::Done(Reply::Volume(59))]
        );
        assert_eq!(link.phase(), Phase::Idle);
        // The policy judges it as any other.
        let mut link = Link::new(LOW, Config::DEFAULT);
        assert_eq!(
            link.start_once(Request::Volume(Volume::Set(41)), 0),
            Err(Rejected::Refused(Refusal::AboveCeiling {
                asked: 41,
                max: 40
            }))
        );
        assert_eq!(
            Failure::Unanswered.to_string(),
            "no answer; it was sent once"
        );
    }

    /// The retry answered: "loss 5" was lost, "loss 6" is the same query
    /// answered.
    #[test]
    fn the_retry_is_answered() {
        let (lost, again) = (recordings::burst("loss 5"), recordings::burst("loss 6"));
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        let burst = start(&mut link, &lost);
        let t = lost.at_ms;
        assert_eq!(link.deadline_ms(), Some(t + HOLD));
        assert_eq!(feed(&mut link, t + HOLD - 1, &lost.rx), []);
        assert_eq!(feed(&mut link, t + HOLD, &lost.rx), [Action::Retry(burst)]);
        assert_eq!(link.deadline_ms(), Some(t + 2 * HOLD));
        assert_eq!(
            feed(&mut link, t + HOLD + 30, &again.rx),
            [Action::Done(Reply::Volume(59))]
        );
        assert_eq!(link.deadline_ms(), None);
        // The hold's end comes and goes: the exchange is over.
        assert_eq!(feed(&mut link, t + 2 * HOLD, &[]), []);
    }

    /// The bytes are read before the clock: a reply handed over long after
    /// its hold ended is the reply, not a retry. And a reply in two reads.
    #[test]
    fn a_reply_handed_over_late_or_in_pieces_still_counts() {
        let line = recordings::burst("state volume try 1");
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        start(&mut link, &line);
        assert_eq!(
            feed(&mut link, line.at_ms + 5000, &line.rx),
            [Action::Done(Reply::Volume(59))]
        );

        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        start(&mut link, &line);
        assert_eq!(feed(&mut link, line.at_ms + 20, &line.rx[..2]), []);
        assert_eq!(
            feed(&mut link, line.at_ms + 22, &line.rx[2..]),
            [Action::Done(Reply::Volume(59))]
        );
        assert_eq!(link.state().volume.map(|h| h.at_ms), Some(line.at_ms + 22));
    }

    /// The hold is the caller's to set and never under 60 ms: the
    /// capture's short holds (40, 25, 15, 8 and 4 ms) lost replies.
    #[test]
    fn the_hold_is_never_under_60_ms() {
        assert_eq!(Config::default().hold(), 150);
        let mut holds = Vec::new();
        let up = Request::Volume(Volume::Up);
        // The one step up needs a level first; its hold is every line's.
        let lines = recordings::bursts().into_iter();
        for line in lines.filter(|b| request_of(&b.tx) != up) {
            let config = Config {
                hold_ms: line.hold_ms,
                ..Config::DEFAULT
            };
            let mut link = Link::new(CAPTURE, config);
            let burst = start(&mut link, &line);
            assert_eq!(burst.hold_ms, line.hold_ms.max(60), "{}", line.label);
            assert_eq!(
                link.deadline_ms(),
                Some(line.at_ms + REQUEST_MS + u64::from(burst.hold_ms))
            );
            if !holds.contains(&line.hold_ms) {
                holds.push(line.hold_ms);
            }
        }
        holds.sort_unstable();
        assert_eq!(holds, [4, 8, 15, 25, 40, 60, 80, 100, 150, 300]);
    }

    /// The reply a 4 ms hold cut to `21 00 00` (see `frame`): through the
    /// link it is no answer, and it does not join the next request's
    /// reply.
    #[test]
    fn a_cut_reply_is_no_answer_and_stays_out_of_the_next() {
        let lines = recordings::bursts();
        let at = lines.iter().position(|b| b.rx_hex == "210000").expect("it");
        let (cut, next) = (&lines[at], &lines[at + 1]);
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        let burst = start(&mut link, cut);
        let t = cut.at_ms;
        assert_eq!(feed(&mut link, t + HOLD, &cut.rx), [Action::Retry(burst)]);
        assert_eq!(
            feed(&mut link, t + 2 * HOLD, &[]),
            [Action::Failed(Failure::NoAnswer)]
        );
        start(&mut link, next);
        assert_eq!(
            feed(&mut link, next.at_ms + HOLD, &next.rx),
            [Action::Done(Reply::Volume(59))]
        );
        assert_eq!(link.state().power, None);
        assert_eq!(link.noise(), 3);
    }

    /// Replies for other codes are the caller's as unsolicited and are in
    /// the state; the request in flight still gets its own.
    #[test]
    fn a_reply_for_another_code_is_unsolicited() {
        let line = recordings::burst("state volume try 1");
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        start(&mut link, &line);
        let mut heard = rx("mute on try 1");
        heard.extend(rx("standby poll 0"));
        heard.extend(&line.rx);
        heard.extend(rx("state source try 1"));
        let t = line.at_ms + 40;
        let source = Input {
            source: Source::Digital3,
            streamer: false,
        };
        assert_eq!(
            feed(&mut link, t, &heard),
            [
                Action::Unsolicited(Reply::Mute(Mute::Muted)),
                Action::Unsolicited(Reply::Power(Power::Standby)),
                Action::Done(Reply::Volume(59)),
                Action::Unsolicited(Reply::Source(source)),
            ]
        );
        let s = link.state();
        assert_eq!(
            (s.muted(), s.on(), s.level()),
            (Some(true), Some(false), Some(59))
        );
        assert_eq!(s.source.map(|h| (h.value, h.at_ms)), Some((source, t)));

        // With nothing in flight, everything is unsolicited.
        assert_eq!(
            feed(&mut link, t + 1000, &line.rx),
            [Action::Unsolicited(Reply::Volume(59))]
        );
    }

    /// The burst the amplifier sends after a mains power-up, as
    /// `docs/protocol.md` describes it: power, model, volume, source, the DAC
    /// filter, mute. Five are bytes the capture heard; the filter frame has
    /// never been seen, so that one follows the reply template. All six are
    /// unsolicited, and the filter reply is in no field.
    #[test]
    fn the_mains_burst_is_six_unsolicited_replies() {
        let mut heard = rx("state power try 1");
        heard.extend(rx("probe3 model try 1"));
        heard.extend(rx("state volume try 1"));
        heard.extend(rx("state source try 1"));
        heard.extend([0x21, 0x61, 0x00, 0x01, 0x0D]);
        heard.extend(rx("state mute try 2"));
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        let actions = feed(&mut link, 7, &heard);
        assert_eq!(actions.len(), 6);
        assert!(actions.iter().all(|a| matches!(a, Action::Unsolicited(_))));
        assert_eq!(
            actions[4],
            Action::Unsolicited(Reply::DacFilter { own: true })
        );
        let s = link.state();
        assert_eq!(
            (s.on(), s.level(), s.muted(), s.model.map(|h| h.value)),
            (Some(true), Some(59), Some(false), Some(3))
        );
    }

    /// A frame the table cannot read answers nothing: junk shaped like a
    /// reply's head ahead of the volume reply (see `frame`) costs the reply,
    /// and the retry gets it.
    #[test]
    fn an_unreadable_frame_answers_nothing() {
        let line = recordings::burst("state volume try 1");
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        let burst = start(&mut link, &line);
        let mut heard = alloc::vec![0x21, 0xFE, 0x00];
        heard.extend(&line.rx);
        let t = line.at_ms + HOLD;
        let stranger = Frame {
            code: 0xFE,
            data: 0x21,
        };
        assert_eq!(
            feed(&mut link, t, &heard),
            [Action::Unread(stranger), Action::Retry(burst)]
        );
        assert_eq!(*link.state(), State::new());
        assert_eq!(
            feed(&mut link, t + HOLD, &line.rx),
            [Action::Done(Reply::Volume(59))]
        );
    }

    /// The link refuses what the policy refuses, and stays idle. The
    /// level it judges by is the one it heard.
    #[test]
    fn the_policy_stands_in_front_of_the_wire() {
        let mut link = Link::new(LOW, Config::DEFAULT);
        let up = Request::Volume(Volume::Up);
        assert_eq!(
            link.start(up, 0),
            Err(Rejected::Refused(Refusal::LevelUnknown))
        );
        assert_eq!(link.phase(), Phase::Idle);

        // The capture's level, 59, is over a ceiling of 40.
        let line = recordings::burst("before volume try 1");
        start(&mut link, &line);
        let t = line.at_ms + HOLD;
        assert_eq!(
            feed(&mut link, t, &line.rx),
            [Action::Done(Reply::Volume(59))]
        );
        let refused = link.start(up, t).expect_err("59 is over 40");
        assert_eq!(
            refused,
            Rejected::Refused(Refusal::AtCeiling { level: 59, max: 40 })
        );
        assert_eq!(
            refused.to_string(),
            "no step up: the volume is 59 and the ceiling is 40"
        );
        assert_eq!(
            link.start(Request::Volume(Volume::Set(41)), t),
            Err(Rejected::Refused(Refusal::AboveCeiling {
                asked: 41,
                max: 40
            }))
        );
        assert_eq!(link.phase(), Phase::Idle);
        // Down is allowed: the capture's own step, 59 to 58.
        let down = recordings::burst("volume down try 1");
        start(&mut link, &down);
        assert_eq!(
            feed(&mut link, down.at_ms + HOLD, &down.rx),
            [Action::Done(Reply::Volume(58))]
        );
        assert_eq!(Rejected::Busy.to_string(), "a request is still in flight");
        assert_eq!(
            Rejected::Waking.to_string(),
            "the amplifier is waking and answers nothing yet"
        );
    }

    /// The capture's step up, 58 to 59, under a ceiling of 59: allowed
    /// once, and the reply closes the door.
    #[test]
    fn a_step_up_stops_at_the_ceiling() {
        let mut link = Link::new(CAPTURE, Config::DEFAULT);
        for label in ["volume down try 1", "volume up try 1"] {
            let line = recordings::burst(label);
            if label.starts_with("volume up") {
                assert_eq!(link.state().level(), Some(58));
            }
            // The step down needs no level; the step up has the 58 the
            // step down's reply gave.
            start(&mut link, &line);
            let want = recordings::answer(&line);
            assert_eq!(
                feed(&mut link, line.at_ms + HOLD, &line.rx),
                [Action::Done(want)]
            );
        }
        assert_eq!(link.state().level(), Some(59));
        assert_eq!(
            link.start(Request::Volume(Volume::Up), 0),
            Err(Rejected::Refused(Refusal::AtCeiling { level: 59, max: 59 }))
        );
    }

    /// A volume step that gets no answer is not sent again: its outcome
    /// is unknown. The level is forgotten, so a step up is refused until
    /// the volume has been asked for.
    #[test]
    fn an_unanswered_step_is_not_sent_twice() {
        for label in ["volume down try 1", "volume up try 1"] {
            let step = recordings::burst(label);
            let mut link = Link::new(CAPTURE, Config::DEFAULT);
            // Learn the level the capture had before the step.
            let before = recordings::burst("volume after down try 1");
            start(&mut link, &before);
            feed(&mut link, before.at_ms + HOLD, &before.rx);
            assert_eq!(link.state().level(), Some(58));

            let t0 = before.at_ms + 1000;
            let burst = link.start(request_of(&step.tx), t0).expect("a step");
            assert_eq!(burst.bytes()[..], step.tx[..]);
            let mut actions = Vec::new();
            for t in t0..t0 + 1000 {
                for a in link.feed(t, &[]) {
                    actions.push((t - t0, a));
                }
            }
            assert_eq!(
                actions,
                [(HOLD, Action::Failed(Failure::StepUnanswered))],
                "{label}"
            );
            assert_eq!(link.state().level(), None);
            assert_eq!(
                link.start(Request::Volume(Volume::Up), t0 + 1000),
                Err(Rejected::Refused(Refusal::LevelUnknown))
            );
        }
        assert_eq!(
            Failure::StepUnanswered.to_string(),
            "no answer to a volume step; it is not sent twice, ask for the volume"
        );
    }

    /// The mute toggle, answered: the reply is the state it landed on, in
    /// the state like any mute reply. No capture holds a toggle, so the
    /// request is the command table's `21 0E 02 0D` and each reply is the
    /// document's template `21 0E 00 dd 0D` (the bytes the capture's mute
    /// and unmute were answered with).
    #[test]
    fn an_answered_mute_toggle_says_where_it_landed() {
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        for (t, data, mute) in [(0, 0x00, Mute::Muted), (1000, 0x01, Mute::NotMuted)] {
            let burst = link.start(Request::MuteToggle, t).expect("a toggle");
            assert_eq!(*burst.bytes(), [0x21, 0x0E, 0x02, 0x0D]);
            let template = [0x21, 0x0E, 0x00, data, 0x0D];
            assert_eq!(
                feed(&mut link, t + 30, &template),
                [Action::Done(Reply::Mute(mute))]
            );
            assert_eq!(link.phase(), Phase::Idle);
            assert_eq!(
                link.state().mute.map(|h| (h.value, h.at_ms)),
                Some((mute, t + 30))
            );
        }
    }

    /// A mute toggle that gets no answer is not sent again: a second one
    /// could flip the mute back. The mute is forgotten; the level, which
    /// no toggle moves, stays. The reply bytes that teach the link the
    /// mute first are the capture's own unmute.
    #[test]
    fn an_unanswered_mute_toggle_is_not_sent_twice() {
        let mut link = Link::new(CAPTURE, Config::DEFAULT);
        let mut t0 = 0;
        for label in ["state volume try 1", "mute off try 1"] {
            let line = recordings::burst(label);
            start(&mut link, &line);
            let want = recordings::answer(&line);
            assert_eq!(
                feed(&mut link, line.at_ms + HOLD, &line.rx),
                [Action::Done(want)]
            );
            t0 = line.at_ms + 1000;
        }
        assert_eq!(link.state().muted(), Some(false));

        let burst = link.start(Request::MuteToggle, t0).expect("a toggle");
        assert_eq!(*burst.bytes(), [0x21, 0x0E, 0x02, 0x0D]);
        let mut actions = Vec::new();
        for t in t0..t0 + 1000 {
            for a in link.feed(t, &[]) {
                actions.push((t - t0, a));
            }
        }
        assert_eq!(actions, [(HOLD, Action::Failed(Failure::ToggleUnanswered))]);
        assert_eq!(link.phase(), Phase::Idle);
        assert_eq!(link.state().muted(), None);
        assert_eq!(link.state().level(), Some(59));
        assert_eq!(
            Failure::ToggleUnanswered.to_string(),
            "no answer to the mute toggle; it is not sent twice, ask for the mute"
        );
    }

    /// The ceiling moved while the link runs: the next request is judged
    /// by the new one.
    #[test]
    fn a_new_policy_judges_the_next_request() {
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        let set = Request::Volume(Volume::Set(50));
        link.set_policy(LOW);
        assert_eq!(link.policy(), LOW);
        assert_eq!(
            link.start(set, 0),
            Err(Rejected::Refused(Refusal::AboveCeiling {
                asked: 50,
                max: 40
            }))
        );
        link.set_policy(Policy::DEFAULT);
        assert!(link.start(set, 0).is_ok());
    }

    /// The amplifier during the capture's wake, as far as the capture saw
    /// it: silent from the wake request until the fifth poll, 3943 ms
    /// later, which it answered. A poll written at `t` hears that poll's
    /// bytes from then on and nothing before.
    struct Waking {
        wake: Line,
        answered: Line,
    }

    impl Waking {
        fn new() -> Waking {
            let w = Waking {
                wake: recordings::burst("wake request try 1"),
                answered: recordings::burst("wake poll 4"),
            };
            for n in 0..4 {
                let silent = recordings::burst(&alloc::format!("wake poll {n}"));
                assert!(silent.rx.is_empty() && silent.at_ms < w.answered.at_ms);
            }
            assert_eq!(w.answered.at_ms - w.wake.at_ms, 3943);
            w
        }

        /// A link whose power-on has just been answered, and when.
        fn link(&self, config: Config) -> (Link, u64) {
            let mut link = Link::new(Policy::DEFAULT, config);
            start(&mut link, &self.wake);
            let t = self.wake.at_ms + HOLD;
            assert_eq!(
                feed(&mut link, t, &self.wake.rx),
                [Action::Done(Reply::Power(Power::On))]
            );
            assert_eq!(link.phase(), Phase::Waking);
            (link, t)
        }

        fn heard(&self, written_at: u64) -> &[u8] {
            if written_at >= self.answered.at_ms {
                &self.answered.rx
            } else {
                &[]
            }
        }
    }

    /// The capture's wake at the link's own pace: a poll every 500 ms
    /// from the answer to the power-on. Seven go unanswered, the eighth is
    /// the first written after the amplifier came back, and the link is
    /// awake 4.2 s after the wake request with the three questions to ask.
    #[test]
    fn the_captured_wake_at_the_links_own_pace() {
        let amp = Waking::new();
        let (mut link, acked) = amp.link(Config::DEFAULT);
        assert_eq!(link.deadline_ms(), Some(acked + 500));
        let poll = Burst {
            wire: Wire::query(Query::Power),
            hold_ms: 150,
        };
        let mut polls = Vec::new();
        let mut awake = None;
        // What the poll in the air will hear, and when its reply lands:
        // 50 ms after it, the slowest the amplifier answers.
        let mut pending: Option<(u64, &[u8])> = None;
        let mut t = acked;
        while awake.is_none() && link.phase() == Phase::Waking {
            t += 1;
            let bytes: &[u8] = match pending {
                Some((at, bytes)) if at == t => {
                    pending = None;
                    bytes
                }
                _ => &[],
            };
            for a in link.feed(t, bytes) {
                match a {
                    Action::Poll(b) => {
                        assert_eq!(b, poll);
                        polls.push(t - acked);
                        pending = Some((t + 50, amp.heard(t)));
                    }
                    Action::Awake { ask } => awake = Some((t, ask)),
                    other => panic!("{other:?}"),
                }
            }
        }
        assert_eq!(polls, [500, 1000, 1500, 2000, 2500, 3000, 3500, 4000]);
        let (at, ask) = awake.expect("awake");
        assert_eq!(at - amp.wake.at_ms, HOLD + 4000 + 50);
        assert_eq!(ask, AFTER_WAKE);
        assert_eq!(link.phase(), Phase::Idle);
        assert_eq!(link.deadline_ms(), None);
        let s = *link.state();
        assert_eq!(s.power.map(|h| (h.value, h.at_ms)), Some((Power::On, at)));
        assert_eq!((s.volume, s.mute, s.source), (None, None, None));

        // The three questions, answered by the capture's post-wake lines;
        // its source query was lost once.
        let lines = [
            ("post-wake volume try 1", None),
            ("post-wake mute try 1", None),
            ("post-wake source try 1", Some("post-wake source try 2")),
        ];
        let mut t = at;
        for (request, (label, again)) in ask.into_iter().zip(lines) {
            let line = recordings::burst(label);
            let burst = link.start(request, t).expect("idle and allowed");
            assert_eq!(burst.bytes()[..], line.tx[..]);
            t += HOLD;
            let mut answer = line.clone();
            if let Some(again) = again {
                assert_eq!(feed(&mut link, t, &line.rx), [Action::Retry(burst)]);
                answer = recordings::burst(again);
                t += HOLD;
            }
            let want = recordings::answer(&answer);
            assert_eq!(feed(&mut link, t, &answer.rx), [Action::Done(want)]);
        }
        let s = link.state();
        assert_eq!((s.level(), s.muted()), (Some(59), Some(false)));
        assert_eq!(s.source.map(|h| h.value.source), Some(Source::Digital3));
    }

    /// A wake that never answers: a poll every 500 ms, fifteen of them,
    /// and at 8 s the link gives up with the power unknown.
    #[test]
    fn a_wake_that_never_answers_is_given_up_at_8_s() {
        let amp = Waking::new();
        let (mut link, acked) = amp.link(Config::DEFAULT);
        let mut actions = Vec::new();
        for t in acked..acked + 20_000 {
            for a in link.feed(t, &[]) {
                actions.push((t - acked, a));
            }
        }
        let (last, polls) = actions.split_last().expect("actions");
        assert_eq!(*last, (8000, Action::NotAwake));
        assert_eq!(polls.len(), 15);
        for (n, (at, a)) in polls.iter().enumerate() {
            assert_eq!(*at, 500 * (n as u64 + 1));
            assert!(matches!(a, Action::Poll(_)));
        }
        assert_eq!(link.phase(), Phase::Idle);
        assert_eq!(link.state().power, None);
        // Idle again: a request is taken.
        assert!(
            link.start(Request::Query(Query::Power), acked + 20_000)
                .is_ok()
        );
    }

    /// While waking, a `00` to a poll is not awake, the wake's last poll
    /// is given its hold, and a late caller gets one poll a call.
    #[test]
    fn waking_edges() {
        let amp = Waking::new();
        let standby = rx("standby poll 0");
        let (mut link, acked) = amp.link(Config::DEFAULT);
        assert!(matches!(
            feed(&mut link, acked + 500, &[])[..],
            [Action::Poll(_)]
        ));
        assert_eq!(link.deadline_ms(), Some(acked + 500 + HOLD));
        assert_eq!(
            feed(&mut link, acked + 550, &standby),
            [Action::Unsolicited(Reply::Power(Power::Standby))]
        );
        assert_eq!(link.phase(), Phase::Waking);
        assert_eq!(link.deadline_ms(), Some(acked + 500 + HOLD));
        assert_eq!(feed(&mut link, acked + 500 + HOLD, &[]), []);
        assert_eq!(link.deadline_ms(), Some(acked + 1000));

        // A caller that comes late: one poll, and the next 500 ms on.
        assert!(matches!(
            feed(&mut link, acked + 3000, &[])[..],
            [Action::Poll(_)]
        ));
        assert_eq!(feed(&mut link, acked + 3200, &[]), []);
        assert_eq!(link.deadline_ms(), Some(acked + 3500));

        // A poll written just before the 8 s: its answer still counts.
        assert!(matches!(
            feed(&mut link, acked + 7990, &[])[..],
            [Action::Poll(_)]
        ));
        assert_eq!(feed(&mut link, acked + 8001, &[]), []);
        assert_eq!(
            feed(&mut link, acked + 8040, &amp.answered.rx),
            [Action::Awake { ask: AFTER_WAKE }]
        );
        assert_eq!(link.state().on(), Some(true));
    }

    /// A power-on whose `01` is lost is not sent again into the wake's
    /// silence: the link waits as for an answered one, unconfirmed. A `00`
    /// to a poll means it never landed: it is sent once more, and a second
    /// silence fails it.
    #[test]
    fn an_unanswered_power_on_waits_for_the_wake() {
        let standby = recordings::burst("standby request try 1");
        let wake = recordings::burst("wake request try 1");
        let off = rx("standby poll 0");
        let in_standby = || {
            let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
            start(&mut link, &standby);
            assert_eq!(
                feed(&mut link, standby.at_ms + HOLD, &standby.rx),
                [Action::Done(Reply::Power(Power::Standby))]
            );
            assert_eq!(link.state().on(), Some(false));
            link
        };
        let poll = Burst {
            wire: Wire::query(Query::Power),
            hold_ms: 150,
        };

        // Its `01` lost, the amplifier waking: polls, then awake.
        let mut link = in_standby();
        let burst = start(&mut link, &wake);
        let t = wake.at_ms + HOLD;
        assert_eq!(feed(&mut link, t, &[]), []);
        assert_eq!(link.phase(), Phase::Waking);
        let s = *link.state();
        assert_eq!((s.volume, s.mute, s.source), (None, None, None));
        assert_eq!(feed(&mut link, t + 500, &[]), [Action::Poll(poll)]);
        assert_eq!(feed(&mut link, t + 500 + HOLD, &[]), []);
        assert_eq!(feed(&mut link, t + 1000, &[]), [Action::Poll(poll)]);
        assert_eq!(
            feed(&mut link, t + 1030, &wake.rx),
            [Action::Awake { ask: AFTER_WAKE }]
        );
        assert_eq!(link.state().on(), Some(true));

        // The power-on itself lost: a `00` to the poll, sent once more,
        // answered, and an ordinary wake.
        let mut link = in_standby();
        start(&mut link, &wake);
        assert_eq!(feed(&mut link, t, &[]), []);
        assert_eq!(feed(&mut link, t + 500, &[]), [Action::Poll(poll)]);
        assert_eq!(feed(&mut link, t + 530, &off), [Action::Retry(burst)]);
        assert_eq!(link.phase(), Phase::Exchange);
        assert_eq!(
            feed(&mut link, t + 560, &wake.rx),
            [Action::Done(Reply::Power(Power::On))]
        );
        assert_eq!(link.phase(), Phase::Waking);

        // Lost again: failed.
        let mut link = in_standby();
        start(&mut link, &wake);
        assert_eq!(feed(&mut link, t, &[]), []);
        assert_eq!(feed(&mut link, t + 500, &[]), [Action::Poll(poll)]);
        assert_eq!(feed(&mut link, t + 530, &off), [Action::Retry(burst)]);
        assert_eq!(
            feed(&mut link, t + 530 + HOLD, &[]),
            [Action::Failed(Failure::NoAnswer)]
        );
        assert_eq!(link.phase(), Phase::Idle);
    }

    /// An iterator dropped at its first action has still taken every byte
    /// and read the clock.
    #[test]
    fn a_feed_dropped_early_still_takes_its_bytes() {
        let line = recordings::burst("state volume try 1");
        let mut link = Link::new(Policy::DEFAULT, Config::DEFAULT);
        start(&mut link, &line);
        let mut heard = rx("mute on try 1");
        heard.extend(&line.rx);
        let first = link.feed(line.at_ms + 30, &heard).next();
        assert_eq!(first, Some(Action::Unsolicited(Reply::Mute(Mute::Muted))));
        assert_eq!(link.phase(), Phase::Idle);
        assert_eq!(link.state().level(), Some(59));
    }

    /// At 19200 baud two characters are 1.04 ms, rounded up to the 2 ms
    /// floor; at 1200 baud they are 16 ms.
    #[test]
    fn two_characters_at_the_line_rates() {
        assert_eq!(two_chars_ms(19_200), 2);
        assert_eq!(two_chars_ms(115_200), 2);
        assert_eq!(two_chars_ms(9_600), 2);
        assert_eq!(two_chars_ms(1_200), 16);
        assert_eq!(two_chars_ms(0), 20_000, "no rate: one character a second");
    }

    /// A burst with the default 150 ms hold.
    const HELD: Burst = Burst {
        wire: Wire::query(Query::Power),
        hold_ms: 150,
    };

    /// The hold ends two characters after the reply, and never past the
    /// cap; with no whole reply it is the cap.
    #[test]
    fn the_hold_ends_after_the_reply() {
        let held = |in_ms| HELD.held_ms(in_ms, 19_200);
        assert_eq!(held(Some(0)), 2);
        assert_eq!(
            held(Some(22)),
            24,
            "a reply in at 22 ms: the line idle at 24"
        );
        assert_eq!(held(Some(50)), 52, "the amplifier's slowest reply");
        assert_eq!(held(Some(148)), 150);
        assert_eq!(held(Some(149)), 150, "the cap");
        assert_eq!(held(Some(u32::MAX)), 150);
        assert_eq!(held(None), 150, "no reply: the whole hold");
        assert_eq!(HELD.held_ms(Some(22), 1_200), 38);
    }

    /// The amplifier replies 0 to 50 ms after the request (its 50 ms
    /// parser task), so an answered request holds the line for about a
    /// sixth of the 150 ms cap on average: about 27 ms.
    #[test]
    fn the_amplifiers_replies_hold_a_sixth_of_the_cap() {
        let holds: u32 = (0..=50).map(|t| HELD.held_ms(Some(t), 19_200)).sum();
        let mean = holds / 51;
        assert_eq!(mean, 27);
        assert!(mean * 5 < 150);
    }
}
