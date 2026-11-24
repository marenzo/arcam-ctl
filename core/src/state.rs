//! What the amplifier has said about itself: the fold of its replies.
//!
//! Every field is the last value heard and when, or `None`. Nothing here is
//! guessed from a request: a level is known once the amplifier reports it.
//! In burst mode the amplifier is only heard while a request's hold keeps
//! its driver awake, so a turn of its own knob goes unheard and a field
//! ages; [`Heard::age_ms`] tells how old it is.

use crate::table::{Input, Mute, Power, Reply};

/// A value, and when the amplifier last reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heard<T> {
    /// The value reported.
    pub value: T,
    /// The caller's clock at the reply, in ms.
    pub at_ms: u64,
}

impl<T> Heard<T> {
    /// How long ago it was heard.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.at_ms)
    }

    /// Heard within the last `within_ms`.
    pub fn fresh(&self, now_ms: u64, within_ms: u64) -> bool {
        self.age_ms(now_ms) <= within_ms
    }
}

/// The amplifier as last heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// On or in standby. The reply to a power request comes before the
    /// change: while the link is waking, `On` is the amplifier's word and
    /// not yet its state.
    pub power: Option<Heard<Power>>,
    /// 0 to 99.
    pub volume: Option<Heard<u8>>,
    /// Muted or not.
    pub mute: Option<Heard<Mute>>,
    /// The input, and whether it is the configured Streamer Input.
    pub source: Option<Heard<Input>>,
    /// The model byte; an A25 reports 3.
    pub model: Option<Heard<u8>>,
}

impl State {
    /// Nothing known.
    pub const fn new() -> State {
        State {
            power: None,
            volume: None,
            mute: None,
            source: None,
            model: None,
        }
    }

    /// Take one reply, heard at `now_ms`. The DAC filter reply has no field
    /// here: a caller that cares reads it from the link's actions.
    pub fn apply(&mut self, reply: Reply, now_ms: u64) {
        let at_ms = now_ms;
        match reply {
            Reply::Power(value) => self.power = Some(Heard { value, at_ms }),
            Reply::Volume(value) => self.volume = Some(Heard { value, at_ms }),
            Reply::Mute(value) => self.mute = Some(Heard { value, at_ms }),
            Reply::Source(value) => self.source = Some(Heard { value, at_ms }),
            Reply::Model(value) => self.model = Some(Heard { value, at_ms }),
            Reply::DacFilter { .. } => {}
        }
    }

    /// Forget volume, mute and source, for a wake from standby: the firmware
    /// clears the mute on the way up without sending a frame for it, so what
    /// was heard before the standby cannot be trusted. Power stays, and so
    /// does the model, which no wake changes.
    pub fn forget_settings(&mut self) {
        self.volume = None;
        self.mute = None;
        self.source = None;
    }

    /// Forget the power: the amplifier stopped answering.
    pub fn forget_power(&mut self) {
        self.power = None;
    }

    /// The last volume heard.
    pub fn level(&self) -> Option<u8> {
        self.volume.map(|h| h.value)
    }

    /// Whether it is muted, as last heard.
    pub fn muted(&self) -> Option<bool> {
        self.mute.map(|h| h.value.is_muted())
    }

    /// Whether it is on, as last heard.
    pub fn on(&self) -> Option<bool> {
        self.power.map(|h| h.value == Power::On)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Decoder;
    use crate::recordings::{self, Burst};
    use crate::table::Source;

    /// Fold one captured burst into `state` at the burst's own time.
    fn fold(state: &mut State, b: &Burst) {
        let mut d = Decoder::new();
        d.feed(&b.rx, &mut |f| {
            if let Ok(reply) = Reply::from_frame(f) {
                state.apply(reply, b.at_ms);
            }
        });
    }

    fn after(labels: &[&str]) -> State {
        let mut s = State::new();
        for l in labels {
            fold(&mut s, &recordings::burst(l));
        }
        s
    }

    /// The capture's first round of five queries: on, volume 59, mute byte
    /// 1 (not muted), source 7 (DIGITAL 3, not the Streamer Input), model
    /// 3, each stamped with its own burst's time.
    #[test]
    fn the_first_round_of_queries() {
        let labels = [
            "probe3 power query 0 hold 150",
            "probe3 volume try 1",
            "probe3 mute try 1",
            "probe3 source try 1",
            "probe3 model try 1",
        ];
        let s = after(&labels);
        let at = |l: &str| recordings::burst(l).at_ms;
        assert_eq!(
            s.power,
            Some(Heard {
                value: Power::On,
                at_ms: at(labels[0])
            })
        );
        assert_eq!(s.level(), Some(59));
        assert_eq!(s.volume.map(|h| h.at_ms), Some(at(labels[1])));
        assert_eq!(s.mute.map(|h| h.value), Some(Mute::NotMuted));
        assert_eq!(s.muted(), Some(false));
        let input = s.source.expect("a source").value;
        assert_eq!((input.source, input.streamer), (Source::Digital3, false));
        assert_eq!(input.source.name(), "DIGITAL 3");
        assert_eq!(s.model.map(|h| h.value), Some(3));
        assert_eq!(s.on(), Some(true));
    }

    /// Every burst of the capture, in order, each checked against what its
    /// label asked. A lost request changes nothing.
    #[test]
    fn the_whole_capture_folds_to_what_each_label_says() {
        let mut s = State::new();
        let mut wake_polls = alloc::vec::Vec::new();
        for b in recordings::bursts() {
            let before = s;
            fold(&mut s, &b);
            let l = b.label;
            if b.rx.len() < 5 {
                assert_eq!(s, before, "{l}: nothing whole was heard");
                if l.starts_with("wake poll") {
                    wake_polls.push(None);
                }
                continue;
            }
            let at_ms = b.at_ms;
            match recordings::answer(&b) {
                Reply::Power(value) => {
                    assert_eq!(s.power, Some(Heard { value, at_ms }), "{l}");
                    if l.starts_with("wake poll") {
                        wake_polls.push(s.on());
                    }
                }
                Reply::Volume(value) => assert_eq!(s.volume, Some(Heard { value, at_ms }), "{l}"),
                Reply::Mute(value) => assert_eq!(s.mute, Some(Heard { value, at_ms }), "{l}"),
                Reply::Source(value) => assert_eq!(s.source, Some(Heard { value, at_ms }), "{l}"),
                Reply::Model(value) => assert_eq!(s.model, Some(Heard { value, at_ms }), "{l}"),
                Reply::DacFilter { .. } => panic!("{l}"),
            }
        }
        // Silent four times, then on.
        assert_eq!(wake_polls, [None, None, None, None, Some(true)]);
        // The capture ended as it began.
        assert_eq!(
            (s.on(), s.level(), s.muted()),
            (Some(true), Some(59), Some(false))
        );
    }

    /// The ten polls after the standby request all read standby.
    #[test]
    fn standby_polls_read_standby() {
        let mut polls = 0;
        for b in recordings::bursts() {
            if b.label.starts_with("standby poll") {
                let mut s = State::new();
                fold(&mut s, &b);
                assert_eq!(s.on(), Some(false), "{}", b.label);
                polls += 1;
            }
        }
        assert_eq!(polls, 10);
        // In standby it still reports its volume.
        assert_eq!(after(&["standby volume try 1"]).level(), Some(59));
    }

    #[test]
    fn the_step_down_and_the_mute() {
        let s = after(&["before volume try 1", "volume down try 1"]);
        assert_eq!(s.level(), Some(58));
        let s = after(&["volume down try 1", "volume up try 1"]);
        assert_eq!(s.level(), Some(59));
        let s = after(&["before mute try 1", "mute on try 1"]);
        assert_eq!(s.muted(), Some(true));
        let s = after(&["mute on try 1", "mute off try 1"]);
        assert_eq!(s.muted(), Some(false));
    }

    #[test]
    fn a_wake_forgets_the_settings_and_keeps_power_and_model() {
        let mut s = after(&[
            "probe3 power query 0 hold 150",
            "probe3 volume try 1",
            "probe3 mute try 1",
            "probe3 source try 1",
            "probe3 model try 1",
        ]);
        s.forget_settings();
        assert_eq!((s.volume, s.mute, s.source), (None, None, None));
        assert_eq!((s.on(), s.model.map(|h| h.value)), (Some(true), Some(3)));
        s.forget_power();
        assert_eq!(s.power, None);
        assert_eq!(s.model.map(|h| h.value), Some(3));
    }

    #[test]
    fn a_field_ages() {
        let b = recordings::burst("state volume try 1");
        let mut s = State::new();
        fold(&mut s, &b);
        let v = s.volume.expect("a volume");
        assert_eq!(v.age_ms(b.at_ms + 1500), 1500);
        assert!(v.fresh(b.at_ms + 1500, 2000));
        assert!(!v.fresh(b.at_ms + 2500, 2000));
        assert_eq!(v.age_ms(0), 0);
    }
}
