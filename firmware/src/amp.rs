//! The amplifier: one thread owns the UART, the core's [`Link`] and its
//! poll [`Scheduler`] (`docs/integration.md#polling`).
//!
//! Commands from MQTT and HTTP are checked at the door against the volume
//! ceiling and the level last heard ([`Bridge::submit`]), then wait in a
//! queue of [`DEPTH`]. The thread carries each to its end before the next,
//! and always before a poll: a poll goes out only when no command waits.
//!
//! - A setter is sent once more when its hold passes with no reply; a
//!   volume step and the mute toggle never (the core's rules). After a lost
//!   step or toggle, what it would have moved is read instead.
//! - After a volume request the mute is read (a volume request unmutes);
//!   after a source, the volume and the mute. An input keeps its own
//!   volume: one read above the ceiling is set to the ceiling, and muted
//!   again if it was muted ([`Policy::clamp`]).
//! - A power-on is answered at once and then the amplifier is silent for
//!   about 4 s: the link polls the power until it reads on (8 s at most),
//!   then volume, mute and source are read.
//! - Between commands the schedule polls, each poll sent once.
//!
//! Every burst updates the shared [`Snapshot`] and tells the MQTT thread,
//! which publishes the state when it changed.

use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arcam_radia::link::{Action, Config, Failure, Link, Phase, Rejected};
use arcam_radia::policy::{Policy, Query, Refusal, Request, Select, Volume};
use arcam_radia::poll::Scheduler;
use arcam_radia::state::State;
use arcam_radia::table::{Mute, Reply};

use crate::api::{Ask, Command, Condition, Outcome, Snapshot, Status, query_name, reply_words};
use crate::line::{self, Port};
use crate::metrics::Replies;
use crate::mqtt::Event;
use crate::settings::Settings;
use crate::{BAUD, RX_GPIO, TX_GPIO, UART, lock};

/// Commands waiting behind the one on the wire.
pub const DEPTH: usize = 4;
/// The longest the thread waits for a command before it looks at the
/// schedule and the settings again.
const IDLE_MS: u64 = 500;
/// Unanswered polls in a row before the link reads `no_answer`; one is the
/// amplifier's usual loss (about 1 in 17).
const POLL_MISSES: u32 = 2;
/// Everything read on boot and on refresh. Power first: an amplifier that
/// does not answer it is not asked the rest.
const ROUND: [Query; 5] = [
    Query::Power,
    Query::Volume,
    Query::Mute,
    Query::Source,
    Query::Model,
];

/// A command in the queue, and where its end goes besides MQTT.
struct Job {
    command: Command,
    reply: Option<SyncSender<Outcome>>,
}

/// What the thread shares.
pub struct Shared {
    /// The amplifier as last heard.
    pub snapshot: Mutex<Snapshot>,
    /// The settings in force.
    pub settings: Mutex<Settings>,
    /// The poll schedule as last left, for its phase in `/metrics`.
    pub schedule: Mutex<Scheduler>,
}

/// The way in for MQTT and HTTP.
#[derive(Clone)]
pub struct Bridge {
    jobs: SyncSender<Job>,
    events: Sender<Event>,
    /// The snapshot and the settings.
    pub shared: Arc<Shared>,
}

impl Bridge {
    /// The state JSON now.
    pub fn state_json(&self) -> String {
        let max = lock(&self.shared.settings).volume_max;
        lock(&self.shared.snapshot).json(max)
    }

    /// The diagnostics JSON now.
    pub fn diag_json(&self) -> String {
        lock(&self.shared.snapshot).diag_json()
    }

    /// Check `command` at the door and queue it. Its end is published on
    /// MQTT, and also sent to `reply` when given.
    ///
    /// # Errors
    ///
    /// The end at once, already published, when it is not queued: refused
    /// by the ceiling, or the queue is full.
    pub fn submit(
        &self,
        command: Command,
        reply: Option<SyncSender<Outcome>>,
    ) -> Result<(), Outcome> {
        let refused = |detail: String| {
            let outcome = Outcome::new(Status::Refused, detail);
            self.ended(command.key, &outcome);
            Err(outcome)
        };
        if let Ask::One(request) = command.ask {
            let policy = Policy {
                volume_max: lock(&self.shared.settings).volume_max,
            };
            let level = lock(&self.shared.snapshot).state.level();
            if let Err(r) = policy.check(request, level) {
                return refused(r.to_string());
            }
        }
        match self.jobs.try_send(Job { command, reply }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => refused("the command queue is full".into()),
            Err(TrySendError::Disconnected(_)) => refused("the amplifier thread is gone".into()),
        }
    }

    fn ended(&self, key: &str, outcome: &Outcome) {
        log::info!("{key}: {}", outcome.json(key));
        let _ = self.events.send(Event::Result(outcome.json(key)));
    }
}

/// Open the UART, start the thread and queue the boot's reading.
pub fn start(settings: Settings, events: Sender<Event>) -> Result<Bridge, crate::Error> {
    let port = Port::open(UART, RX_GPIO, TX_GPIO, BAUD)?;
    let (jobs, queue) = mpsc::sync_channel(DEPTH);
    let snapshot = Snapshot {
        state: State::new(),
        link: None,
        reply_ms: None,
        exchanges: 0,
        losses: 0,
        replies: Replies::default(),
    };
    let schedule = Scheduler::new(settings.poll);
    let runner = Runner {
        port,
        link: Link::new(
            Policy {
                volume_max: settings.volume_max,
            },
            Config::DEFAULT,
        ),
        schedule,
        snapshot,
        bridge: Bridge {
            jobs,
            events,
            shared: Arc::new(Shared {
                snapshot: Mutex::new(snapshot),
                settings: Mutex::new(settings),
                schedule: Mutex::new(schedule),
            }),
        },
    };
    let bridge = runner.bridge.clone();
    std::thread::Builder::new()
        .name("amp".into())
        .stack_size(8 * 1024)
        .spawn(move || runner.run(&queue))?;
    let _ = bridge.submit(Command::REFRESH, None);
    Ok(bridge)
}

/// How one request ended on the wire.
#[derive(Debug, Clone, Copy)]
enum End {
    Done(Reply),
    /// A power-on, answered, and the amplifier came up: ask these.
    Awake([Request; 3]),
    Failed(Failure),
    NotAwake,
    Refused(Refusal),
    Busy,
}

struct Runner {
    port: Port,
    link: Link,
    schedule: Scheduler,
    snapshot: Snapshot,
    bridge: Bridge,
}

impl Runner {
    fn run(mut self, queue: &Receiver<Job>) {
        loop {
            self.sync_settings();
            *lock(&self.bridge.shared.schedule) = self.schedule;
            let now = line::now_ms();
            let wait = self
                .schedule
                .due_ms(now)
                .map_or(IDLE_MS, |t| t.saturating_sub(now).min(IDLE_MS));
            if let Ok(job) = queue.recv_timeout(Duration::from_millis(wait)) {
                // The settings may have changed during the wait: the job was
                // let in by the new ones.
                self.sync_settings();
                let outcome = self.carry(job.command.ask);
                self.bridge.ended(job.command.key, &outcome);
                if let Some(reply) = job.reply {
                    let _ = reply.try_send(outcome);
                }
            } else if let Some(q) = self.schedule.take(line::now_ms()) {
                self.request(Request::Query(q), true);
            }
        }
    }

    /// The ceiling and the poll spec, as now set.
    fn sync_settings(&mut self) {
        let (volume_max, spec) = {
            let s = lock(&self.bridge.shared.settings);
            (s.volume_max, s.poll)
        };
        if self.link.policy().volume_max != volume_max {
            self.link.set_policy(Policy { volume_max });
        }
        if self.schedule.spec() != spec {
            self.schedule.set_spec(spec);
        }
    }

    /// One command to its end. A command that sent anything opens the
    /// schedule's window.
    fn carry(&mut self, ask: Ask) -> Outcome {
        let before = self.snapshot.exchanges;
        let outcome = match ask {
            Ask::Refresh => self.round(),
            Ask::One(request) => self.one(request),
        };
        if self.snapshot.exchanges != before {
            self.schedule.asked(line::now_ms());
        }
        outcome
    }

    /// Power, and if it answers the rest. The first query left unanswered
    /// is the outcome; the others are still asked.
    fn round(&mut self) -> Outcome {
        let mut unread = None;
        for q in ROUND {
            match self.request(Request::Query(q), false) {
                End::Done(_) | End::Awake(_) => {}
                End::Busy => return Outcome::new(Status::NoAnswer, "the link was not idle"),
                _ => {
                    unread.get_or_insert(q);
                    if q == Query::Power {
                        break;
                    }
                }
            }
        }
        match unread {
            None => Outcome::new(Status::Ok, ""),
            Some(q) => Outcome::new(
                Status::NoAnswer,
                format!("no answer to the {} query, after one retry", query_name(q)),
            ),
        }
    }

    fn one(&mut self, request: Request) -> Outcome {
        let (mut detail, then): (Option<String>, Vec<Request>) = match self.request(request, false)
        {
            End::Refused(r) => return Outcome::new(Status::Refused, r.to_string()),
            End::Busy => return Outcome::new(Status::NoAnswer, "the link was not idle"),
            End::NotAwake => {
                return Outcome::new(
                    Status::NotAwake,
                    "the amplifier did not come up in 8 s after the power-on",
                );
            }
            // Twice unanswered: the amplifier is not there to be asked more.
            End::Failed(f @ Failure::NoAnswer) => {
                return Outcome::new(Status::NoAnswer, f.to_string());
            }
            // Sent once, outcome unknown: read what it would have moved.
            End::Failed(f) => {
                let moved: &[Query] = match f {
                    Failure::StepUnanswered => &[Query::Volume, Query::Mute],
                    _ => &[Query::Mute],
                };
                for &q in moved {
                    self.request(Request::Query(q), false);
                }
                return Outcome::new(Status::NoAnswer, f.to_string());
            }
            End::Awake(then) => (None, then.to_vec()),
            End::Done(reply) => (
                (!landed(request, reply))
                    .then(|| format!("the amplifier answered {}", reply_words(reply))),
                after(request).iter().map(|&q| Request::Query(q)).collect(),
            ),
        };
        for r in then {
            let end = self.request(r, false);
            if !matches!(end, End::Done(_))
                && detail.is_none()
                && let Request::Query(q) = r
            {
                detail = Some(format!(
                    "done; the {} query after it got no answer",
                    query_name(q)
                ));
            }
        }
        if matches!(request, Request::Source(_))
            && let Some(lower) = self.clamp()
        {
            detail = Some(lower);
        }
        Outcome::new(Status::Ok, detail.unwrap_or_default())
    }

    /// A level heard above the ceiling, set to the ceiling; the mute put
    /// back, since the set unmutes. What was done, if anything.
    fn clamp(&mut self) -> Option<String> {
        let state = *self.link.state();
        let level = state.level()?;
        let max = self.link.policy().volume_max;
        let set = self.link.policy().clamp(Some(level))?;
        if !matches!(self.request(set, false), End::Done(_)) {
            return Some(format!(
                "done; the input's volume, {level}, is above the ceiling of {max}, and setting it got no answer"
            ));
        }
        if state.muted() == Some(true) {
            self.request(Request::Mute(Mute::Muted), false);
        }
        Some(format!(
            "done; the input's volume, {level}, was above the ceiling and is now {max}"
        ))
    }

    /// One request from the policy's check to an idle link: its bursts, its
    /// one retry (none for a `poll`), and for a power-on the wait while the
    /// amplifier comes up. The schedule hears every burst.
    fn request(&mut self, request: Request, poll: bool) -> End {
        let started = line::now_ms();
        let begun = if poll {
            self.link.start_once(request, started)
        } else {
            self.link.start(request, started)
        };
        let mut next = match begun {
            Ok(burst) => Some(burst),
            Err(Rejected::Refused(r)) => return End::Refused(r),
            Err(Rejected::Busy | Rejected::Waking) => return End::Busy,
        };
        self.snapshot.exchanges += 1;
        let mut end = None;
        loop {
            let before = *self.link.state();
            let (mut heard, mut n, mut in_ms) = ([0u8; 32], 0, None);
            if let Some(burst) = next.take() {
                self.schedule.sent(line::now_ms());
                let asked = self.port.ask(&burst);
                (heard, n, in_ms) = (asked.heard, asked.n, asked.in_ms);
            } else if let Some(at) = self.link.deadline_ms() {
                let left = at.saturating_sub(line::now_ms());
                std::thread::sleep(Duration::from_millis(left));
            } else {
                break;
            }
            let now = line::now_ms();
            let mut replied = false;
            for action in self.link.feed(now, &heard[..n]) {
                match action {
                    Action::Done(reply) => {
                        replied = true;
                        end = Some(End::Done(reply));
                    }
                    Action::Retry(burst) | Action::Poll(burst) => next = Some(burst),
                    Action::Failed(f) => end = Some(End::Failed(f)),
                    Action::Awake { ask } => {
                        replied = true;
                        end = Some(End::Awake(ask));
                    }
                    Action::NotAwake => end = Some(End::NotAwake),
                    Action::Unsolicited(_) | Action::Unread(_) => {}
                }
            }
            if replied && let Some(ms) = in_ms {
                self.snapshot.reply_ms = Some(ms);
                self.snapshot.replies.add(ms);
            }
            let after = *self.link.state();
            self.schedule.heard(now, &before, &after);
            let phase = self.link.phase();
            self.schedule.set_waking(phase == Phase::Waking);
            if poll && phase == Phase::Idle {
                self.schedule.polled(replied);
            }
            self.snapshot.link = match (phase, end) {
                (Phase::Waking, _) => Some(Condition::Waking),
                (Phase::Exchange, _) => self.snapshot.link,
                (Phase::Idle, Some(End::Done(_) | End::Awake(_))) => Some(Condition::Answering),
                (Phase::Idle, _) if poll && self.schedule.misses() < POLL_MISSES => {
                    self.snapshot.link
                }
                (Phase::Idle, _) => Some(Condition::NoAnswer),
            };
            self.publish();
            if next.is_none() && phase == Phase::Idle {
                break;
            }
        }
        let end = end.unwrap_or(End::Busy);
        if matches!(end, End::Failed(_) | End::NotAwake) {
            self.snapshot.losses += 1;
            self.publish();
        }
        end
    }

    /// The snapshot out to MQTT and HTTP.
    fn publish(&mut self) {
        self.snapshot.state = *self.link.state();
        *lock(&self.bridge.shared.snapshot) = self.snapshot;
        *lock(&self.bridge.shared.schedule) = self.schedule;
        let _ = self.bridge.events.send(Event::State);
    }
}

/// What a request may have moved beside its own reply: a volume request
/// unmutes first and says nothing of it; another input has its own level.
/// A power-on is the link's (`AFTER_WAKE`).
fn after(request: Request) -> &'static [Query] {
    match request {
        Request::Volume(_) => &[Query::Mute],
        Request::Source(_) => &[Query::Volume, Query::Mute],
        Request::Query(_) | Request::Power(_) | Request::Mute(_) | Request::MuteToggle => &[],
    }
}

/// Whether `reply` says the amplifier is where `request` asked. A step, the
/// toggle, the Streamer Input and a query ask for no particular value.
fn landed(request: Request, reply: Reply) -> bool {
    match (request, reply) {
        (Request::Power(p), Reply::Power(got)) => p == got,
        (Request::Volume(Volume::Set(n)), Reply::Volume(got)) => n == got,
        (Request::Mute(m), Reply::Mute(got)) => m == got,
        (Request::Source(Select::Input(s)), Reply::Source(got)) => s == got.source,
        _ => true,
    }
}
