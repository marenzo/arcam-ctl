//! The words MQTT and the REST API share (`docs/integration.md`): a command
//! as its key and value spell it, how it ended, and the state and
//! diagnostics as JSON. [`crate::amp`] carries the commands; this module
//! only names them.

use arcam_radia::policy::{Query, Request, Select, Volume};
use arcam_radia::state::State;
use arcam_radia::table::{Mute, Power, Reply, Source};

use crate::json;
use crate::metrics::Replies;

/// The link's condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The last request was answered.
    Answering,
    /// It was not (or two polls in a row were not).
    NoAnswer,
    /// A power-on was answered; the amplifier is coming up.
    Waking,
}

impl Condition {
    /// As the state JSON and `/metrics` spell it.
    pub fn name(self) -> &'static str {
        match self {
            Condition::Answering => "answering",
            Condition::NoAnswer => "no_answer",
            Condition::Waking => "waking",
        }
    }
}

/// What MQTT and HTTP show of the amplifier.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// What it last said.
    pub state: State,
    /// `None` until the first request has ended.
    pub link: Option<Condition>,
    /// How long the last reply took from the start of its hold, in ms.
    pub reply_ms: Option<u32>,
    /// Requests sent since boot, polls included, retries not.
    pub exchanges: u32,
    /// Requests that ended with no answer.
    pub losses: u32,
    /// Every reply's time, for `/metrics`.
    pub replies: Replies,
}

impl Snapshot {
    /// The state JSON (`docs/integration.md`): the amplifier's own
    /// values.
    pub fn json(&self, volume_max: u8) -> String {
        let s = &self.state;
        let power = s.on().map(|on| if on { "on" } else { "standby" });
        let source = s.source.map(|h| h.value.source.key());
        format!(
            "{{\"power\":{},\"volume\":{},\"muted\":{},\"source\":{},\"streamer\":{},\"model\":{},\"link\":{},\"volume_max\":{volume_max}}}",
            json::opt_str(power),
            json::opt(s.level()),
            json::opt(s.muted()),
            json::opt_str(source),
            json::opt(s.source.map(|h| h.value.streamer)),
            json::opt(s.model.map(|h| h.value)),
            json::opt_str(self.link.map(Condition::name)),
        )
    }

    /// The diagnostics JSON: the link's counts.
    pub fn diag_json(&self) -> String {
        format!(
            "{{\"reply_ms\":{},\"exchanges\":{},\"losses\":{}}}",
            json::opt(self.reply_ms),
            self.exchanges,
            self.losses
        )
    }
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Accepted, but the HTTP wait ended before the outcome was available.
    Pending,
    /// The amplifier answered.
    Ok,
    /// The bridge did not send it.
    Refused,
    /// No reply, after the retry rules.
    NoAnswer,
    /// A power-on did not come up in 8 s.
    NotAwake,
}

/// A command's end: the result JSON's `status` and `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// How it ended.
    pub status: Status,
    /// Why, in words; empty for a plain `ok`.
    pub detail: String,
}

impl Outcome {
    /// An end with its detail.
    pub fn new(status: Status, detail: impl Into<String>) -> Outcome {
        Outcome {
            status,
            detail: detail.into(),
        }
    }

    /// The result JSON for `command`.
    pub fn json(&self, command: &str) -> String {
        let status = match self.status {
            Status::Pending => "pending",
            Status::Ok => "ok",
            Status::Refused => "refused",
            Status::NoAnswer => "no_answer",
            Status::NotAwake => "not_awake",
        };
        format!(
            "{{\"command\":{},\"status\":\"{status}\",\"detail\":{}}}",
            json::string(command),
            json::string(&self.detail)
        )
    }
}

/// What a command asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// One request, and what it may have moved read after it.
    One(Request),
    /// Everything read again.
    Refresh,
}

/// One command: its key (`power`, `volume`, `mute`, `source`, `refresh`)
/// and what it asks.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    /// The key it came under.
    pub key: &'static str,
    /// What it asks.
    pub ask: Ask,
}

impl Command {
    /// Read everything again: the boot's reading, and the `refresh` key.
    pub const REFRESH: Command = Command {
        key: "refresh",
        ask: Ask::Refresh,
    };

    /// A command as the `set/` topics and the REST API spell it.
    ///
    /// # Errors
    ///
    /// Why `key` and `value` are no command, in words for a result.
    pub fn parse(key: &str, value: &str) -> Result<Command, String> {
        let value = value.trim();
        let (key, request) = match key {
            "power" => (
                "power",
                match value {
                    "on" => Request::Power(Power::On),
                    "standby" => Request::Power(Power::Standby),
                    _ => return Err("power: on or standby".into()),
                },
            ),
            "volume" => (
                "volume",
                // A number may come as `40.0` (Home Assistant's slider).
                Request::Volume(match value.strip_suffix(".0").unwrap_or(value) {
                    "up" => Volume::Up,
                    "down" => Volume::Down,
                    n if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                        Volume::Set(n.parse().map_err(|_| "volume: 0 to 99, up or down")?)
                    }
                    _ => return Err("volume: 0 to 99, up or down".into()),
                }),
            ),
            "mute" => (
                "mute",
                match value {
                    "on" => Request::Mute(Mute::Muted),
                    "off" => Request::Mute(Mute::NotMuted),
                    "toggle" => Request::MuteToggle,
                    _ => return Err("mute: on, off or toggle".into()),
                },
            ),
            "source" => (
                "source",
                Request::Source(match value {
                    "streamer" => Select::Streamer,
                    name => Select::Input(
                        Source::ALL
                            .into_iter()
                            .find(|s| s.key() == name)
                            .ok_or("source: one of the ten names in docs/protocol.md")?,
                    ),
                }),
            ),
            "refresh" => return Ok(Command::REFRESH),
            _ => return Err(format!("no such command: {key}")),
        };
        Ok(Command {
            key,
            ask: Ask::One(request),
        })
    }
}

/// A query's name, as a command's key spells it.
pub fn query_name(q: Query) -> &'static str {
    match q {
        Query::Power => "power",
        Query::Volume => "volume",
        Query::Mute => "mute",
        Query::Source => "source",
        Query::Model => "model",
    }
}

/// A reply in words, for a result's detail.
pub fn reply_words(reply: Reply) -> String {
    match reply {
        Reply::Power(Power::On) => "on".into(),
        Reply::Power(Power::Standby) => "standby".into(),
        Reply::Volume(v) => format!("volume {v}"),
        Reply::Mute(m) => if m.is_muted() { "muted" } else { "not muted" }.into(),
        Reply::Source(i) => format!(
            "source {}{}",
            i.source.key(),
            if i.streamer {
                " (the Streamer Input)"
            } else {
                ""
            }
        ),
        Reply::Model(m) => format!("model {m}"),
        Reply::DacFilter { own } => format!("DAC filter own {own}"),
    }
}
