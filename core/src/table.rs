//! The command table: the six command codes, their data bytes, and what a
//! reply's data byte means. Every conversion from a byte returns `None` or
//! an error for a value the table does not hold.
//!
//! | code | command | request data | reply data |
//! |---|---|---|---|
//! | `00` | power | `00` standby, `01` on, `F0` query | `00` standby, `01` on |
//! | `0D` | volume | `0..=99` set, `F0` query, `F1` up, `F2` down | `0..=99` |
//! | `0E` | mute | `00` mute, `01` unmute, `02` toggle, `F0` query | `00` muted, `01` not muted |
//! | `1D` | source | `1..=9` select, `F1` Streamer Input, `F0` query | `1..=9`, bit `0x10` if the Streamer Input |
//! | `5E` | model | `F0` query | `03` on the A25 |
//! | `61` | DAC filter | never sent | `00` / `01` |
//!
//! Power also has a toggle, `02`, that this crate never sends. See
//! `docs/protocol.md` for where each entry comes from.
//!
//! The request data bytes are private: [`crate::policy::Wire`] is the only
//! way to make request bytes. What a request's byte means, read back, is
//! [`Asked`].

use crate::frame::Frame;

/// Data byte of a query, "what is it now?", for every command.
pub(crate) const QUERY: u8 = 0xF0;
/// Data byte of the power and mute toggles. The power toggle is never sent:
/// its result depends on a state the caller may not know. The mute toggle is
/// [`crate::policy::Request::MuteToggle`], sent at most once.
pub(crate) const TOGGLE: u8 = 0x02;
/// Power data: go to standby; in a reply, in standby.
pub(crate) const POWER_STANDBY: u8 = 0x00;
/// Power data: power up; in a reply, on.
pub(crate) const POWER_ON: u8 = 0x01;
/// The highest volume the amplifier accepts. It has no lower ceiling of its
/// own; [`crate::policy::Policy`] provides one.
pub const VOLUME_TOP: u8 = 99;
/// Volume data: one step up.
pub(crate) const VOLUME_UP: u8 = 0xF1;
/// Volume data: one step down.
pub(crate) const VOLUME_DOWN: u8 = 0xF2;
/// Mute data: `00` means **muted**, as a command and as a reply.
pub(crate) const MUTE_MUTED: u8 = 0x00;
/// Mute data: `01` means not muted.
pub(crate) const MUTE_NOT_MUTED: u8 = 0x01;
/// Source data: select whichever input is set as System Settings, Streamer
/// Input.
pub(crate) const SOURCE_STREAMER: u8 = 0xF1;
/// Bit of a source reply: the current input is the configured Streamer
/// Input.
pub(crate) const SOURCE_IS_STREAMER: u8 = 0x10;
/// The A25's answer to the model query.
pub const MODEL_A25: u8 = 0x03;

/// The six command codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `0x00`: standby or on.
    Power,
    /// `0x0D`: the volume, 0 to 99.
    Volume,
    /// `0x0E`: the mute.
    Mute,
    /// `0x1D`: the input.
    Source,
    /// `0x5E`: the model byte.
    Model,
    /// `0x61`: hands DAC filter control to an external device, and the
    /// setting persists. In the table because the amplifier reports it after
    /// a mains power-up; no request carries it.
    DacFilter,
}

impl Command {
    /// Every command, in code order.
    pub const ALL: [Command; 6] = [
        Command::Power,
        Command::Volume,
        Command::Mute,
        Command::Source,
        Command::Model,
        Command::DacFilter,
    ];

    /// The command's code, the frame's second byte.
    pub const fn code(self) -> u8 {
        match self {
            Command::Power => 0x00,
            Command::Volume => 0x0D,
            Command::Mute => 0x0E,
            Command::Source => 0x1D,
            Command::Model => 0x5E,
            Command::DacFilter => 0x61,
        }
    }

    /// The command with this code, if any.
    pub const fn from_code(code: u8) -> Option<Command> {
        Some(match code {
            0x00 => Command::Power,
            0x0D => Command::Volume,
            0x0E => Command::Mute,
            0x1D => Command::Source,
            0x5E => Command::Model,
            0x61 => Command::DacFilter,
            _ => return None,
        })
    }

    /// A short lower-case name, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            Command::Power => "power",
            Command::Volume => "volume",
            Command::Mute => "mute",
            Command::Source => "source",
            Command::Model => "model",
            Command::DacFilter => "DAC filter",
        }
    }
}

/// The amplifier's power, as a request and as a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    /// `00`: standby.
    Standby,
    /// `01`: on.
    On,
}

impl Power {
    /// The data byte.
    pub const fn data(self) -> u8 {
        match self {
            Power::Standby => POWER_STANDBY,
            Power::On => POWER_ON,
        }
    }

    /// The power state this data byte means, if any.
    pub const fn from_data(data: u8) -> Option<Power> {
        match data {
            POWER_STANDBY => Some(Power::Standby),
            POWER_ON => Some(Power::On),
            _ => None,
        }
    }
}

/// The mute, as a request and as a reply. Note the wire's sense:
/// [`Mute::Muted`] is `00`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mute {
    /// `00`: muted.
    Muted,
    /// `01`: not muted.
    NotMuted,
}

impl Mute {
    /// The data byte.
    pub const fn data(self) -> u8 {
        match self {
            Mute::Muted => MUTE_MUTED,
            Mute::NotMuted => MUTE_NOT_MUTED,
        }
    }

    /// The mute state this data byte means, if any.
    pub const fn from_data(data: u8) -> Option<Mute> {
        match data {
            MUTE_MUTED => Some(Mute::Muted),
            MUTE_NOT_MUTED => Some(Mute::NotMuted),
            _ => None,
        }
    }

    /// `true` for [`Mute::Muted`].
    pub const fn is_muted(self) -> bool {
        matches!(self, Mute::Muted)
    }
}

/// The nine inputs, by their wire codes 1 to 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 1: PHONO.
    Phono,
    /// 2: ANALOGUE 1.
    Analogue1,
    /// 3: ANALOGUE 2.
    Analogue2,
    /// 4: ANALOGUE 3.
    Analogue3,
    /// 5: DIGITAL 1.
    Digital1,
    /// 6: DIGITAL 2.
    Digital2,
    /// 7: DIGITAL 3.
    Digital3,
    /// 8: USB-C.
    UsbC,
    /// 9: BLUETOOTH. Refused by the amplifier while it is a Bluetooth sink.
    Bluetooth,
}

impl Source {
    /// Every input, in code order.
    pub const ALL: [Source; 9] = [
        Source::Phono,
        Source::Analogue1,
        Source::Analogue2,
        Source::Analogue3,
        Source::Digital1,
        Source::Digital2,
        Source::Digital3,
        Source::UsbC,
        Source::Bluetooth,
    ];

    /// The data byte, 1 to 9.
    pub const fn data(self) -> u8 {
        match self {
            Source::Phono => 1,
            Source::Analogue1 => 2,
            Source::Analogue2 => 3,
            Source::Analogue3 => 4,
            Source::Digital1 => 5,
            Source::Digital2 => 6,
            Source::Digital3 => 7,
            Source::UsbC => 8,
            Source::Bluetooth => 9,
        }
    }

    /// The input with this data byte, if any. The Streamer Input bit is not
    /// accepted here; see [`Input::from_data`].
    pub const fn from_data(data: u8) -> Option<Source> {
        Some(match data {
            1 => Source::Phono,
            2 => Source::Analogue1,
            3 => Source::Analogue2,
            4 => Source::Analogue3,
            5 => Source::Digital1,
            6 => Source::Digital2,
            7 => Source::Digital3,
            8 => Source::UsbC,
            9 => Source::Bluetooth,
            _ => return None,
        })
    }

    /// The input's name in this project's API, in snake case: `digital_3`,
    /// `usb_c`. The Streamer Input, a selection and not an input, is
    /// `streamer` beside these.
    pub const fn key(self) -> &'static str {
        match self {
            Source::Phono => "phono",
            Source::Analogue1 => "analogue_1",
            Source::Analogue2 => "analogue_2",
            Source::Analogue3 => "analogue_3",
            Source::Digital1 => "digital_1",
            Source::Digital2 => "digital_2",
            Source::Digital3 => "digital_3",
            Source::UsbC => "usb_c",
            Source::Bluetooth => "bluetooth",
        }
    }

    /// The input's name as the amplifier's display shows it.
    pub const fn name(self) -> &'static str {
        match self {
            Source::Phono => "PHONO",
            Source::Analogue1 => "ANALOGUE 1",
            Source::Analogue2 => "ANALOGUE 2",
            Source::Analogue3 => "ANALOGUE 3",
            Source::Digital1 => "DIGITAL 1",
            Source::Digital2 => "DIGITAL 2",
            Source::Digital3 => "DIGITAL 3",
            Source::UsbC => "USB-C",
            Source::Bluetooth => "BLUETOOTH",
        }
    }
}

/// A source reply: the input, and whether it is the configured Streamer
/// Input (bit `0x10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    /// The current input.
    pub source: Source,
    /// The current input is the one set as the Streamer Input.
    pub streamer: bool,
}

impl Input {
    /// The reply's data byte.
    pub const fn data(self) -> u8 {
        self.source.data() | if self.streamer { SOURCE_IS_STREAMER } else { 0 }
    }

    /// The input this reply data byte means, if any.
    pub const fn from_data(data: u8) -> Option<Input> {
        match Source::from_data(data & !SOURCE_IS_STREAMER) {
            Some(source) => Some(Input {
                source,
                streamer: data & SOURCE_IS_STREAMER != 0,
            }),
            None => None,
        }
    }
}

/// What a request's data byte asks of its command, read back from the
/// wire, as the amplifier reads it. For a reader of requests, such as a
/// model of the amplifier; nothing here makes request bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// `F0`, any command: what is it now?
    Query,
    /// `02`, power or mute: flip it. The power toggle is never sent.
    Toggle,
    /// `F1`, volume: one step up.
    Up,
    /// `F2`, volume: one step down.
    Down,
    /// `F1`, source: whichever input is the Streamer Input.
    Streamer,
    /// Any other byte: a value to set, for the command's own reading
    /// ([`Power::from_data`], [`Mute::from_data`], [`Source::from_data`], a
    /// volume up to [`VOLUME_TOP`]).
    Set(u8),
}

impl Asked {
    /// What `data` asks of `command`.
    pub const fn of(command: Command, data: u8) -> Asked {
        match (command, data) {
            (_, QUERY) => Asked::Query,
            (Command::Power | Command::Mute, TOGGLE) => Asked::Toggle,
            (Command::Volume, VOLUME_UP) => Asked::Up,
            (Command::Volume, VOLUME_DOWN) => Asked::Down,
            (Command::Source, SOURCE_STREAMER) => Asked::Streamer,
            (_, data) => Asked::Set(data),
        }
    }
}

/// One reply, interpreted: the command and what its data byte says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Standby or on.
    Power(Power),
    /// 0 to 99. On an input in processor mode it is that mode's volume; on
    /// Bluetooth, the Bluetooth volume.
    Volume(u8),
    /// Muted or not.
    Mute(Mute),
    /// The current input.
    Source(Input),
    /// The model byte; the A25 answers [`MODEL_A25`].
    Model(u8),
    /// Who controls the DAC filter. Sent unsolicited after a mains power-up
    /// only.
    DacFilter {
        /// `true` when the amplifier's own filter setting is in use.
        own: bool,
    },
}

/// A frame the table cannot interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// No command has this code.
    Code(u8),
    /// The command has no such reply.
    Data {
        /// The frame's command.
        command: Command,
        /// The data byte it does not define.
        data: u8,
    },
}

impl core::fmt::Display for Unread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Unread::Code(code) => write!(f, "no command has code 0x{code:02X}"),
            Unread::Data { command, data } => {
                write!(f, "{} has no reply 0x{data:02X}", command.name())
            }
        }
    }
}

impl Reply {
    /// The command this reply belongs to.
    pub const fn command(self) -> Command {
        match self {
            Reply::Power(_) => Command::Power,
            Reply::Volume(_) => Command::Volume,
            Reply::Mute(_) => Command::Mute,
            Reply::Source(_) => Command::Source,
            Reply::Model(_) => Command::Model,
            Reply::DacFilter { .. } => Command::DacFilter,
        }
    }

    /// Interpret a frame against the table.
    ///
    /// # Errors
    ///
    /// [`Unread::Code`] for an unknown command code, [`Unread::Data`] for a
    /// data byte the command's replies do not define.
    pub fn from_frame(frame: Frame) -> Result<Reply, Unread> {
        let command = Command::from_code(frame.code).ok_or(Unread::Code(frame.code))?;
        let data = frame.data;
        let reply = match command {
            Command::Power => Power::from_data(data).map(Reply::Power),
            Command::Volume => (data <= VOLUME_TOP).then_some(Reply::Volume(data)),
            Command::Mute => Mute::from_data(data).map(Reply::Mute),
            Command::Source => Input::from_data(data).map(Reply::Source),
            Command::Model => Some(Reply::Model(data)),
            Command::DacFilter => match data {
                0x00 => Some(Reply::DacFilter { own: false }),
                0x01 => Some(Reply::DacFilter { own: true }),
                _ => None,
            },
        };
        reply.ok_or(Unread::Data { command, data })
    }

    /// The frame that carries this reply.
    pub const fn frame(self) -> Frame {
        let data = match self {
            Reply::Power(p) => p.data(),
            Reply::Volume(v) => v,
            Reply::Mute(m) => m.data(),
            Reply::Source(i) => i.data(),
            Reply::Model(m) => m,
            Reply::DacFilter { own } => own as u8,
        };
        Frame {
            code: self.command().code(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::frame::Decoder;
    use crate::recordings;

    #[test]
    fn the_six_codes_both_ways() {
        let codes = [0x00, 0x0D, 0x0E, 0x1D, 0x5E, 0x61];
        for (c, code) in Command::ALL.into_iter().zip(codes) {
            assert_eq!(c.code(), code);
            assert_eq!(Command::from_code(code), Some(c));
        }
        let known = (0..=255u8).filter(|&b| Command::from_code(b).is_some());
        assert_eq!(known.count(), 6);
    }

    #[test]
    fn the_sources_both_ways_with_their_names() {
        let names = [
            "PHONO",
            "ANALOGUE 1",
            "ANALOGUE 2",
            "ANALOGUE 3",
            "DIGITAL 1",
            "DIGITAL 2",
            "DIGITAL 3",
            "USB-C",
            "BLUETOOTH",
        ];
        for ((s, data), name) in Source::ALL.into_iter().zip(1u8..).zip(names) {
            assert_eq!(s.data(), data);
            assert_eq!(Source::from_data(data), Some(s));
            assert_eq!(s.name(), name);
        }
        for outside in [0, 10, 0x10, 0xF0, SOURCE_STREAMER] {
            assert_eq!(Source::from_data(outside), None);
        }
    }

    /// The API's names are the display's, in snake case.
    #[test]
    fn the_source_keys_are_the_names_in_snake_case() {
        for s in Source::ALL {
            let derived = s.name().to_lowercase().replace([' ', '-'], "_");
            assert_eq!(s.key(), derived, "{s:?}");
        }
    }

    /// Every request data byte reads as what it asks, for every command;
    /// a toggle only for power and mute, a step only for the volume.
    #[test]
    fn every_request_byte_reads_as_what_it_asks() {
        for c in Command::ALL {
            for data in 0..=255u8 {
                let want = match (c, data) {
                    (_, 0xF0) => Asked::Query,
                    (Command::Power | Command::Mute, 0x02) => Asked::Toggle,
                    (Command::Volume, 0xF1) => Asked::Up,
                    (Command::Volume, 0xF2) => Asked::Down,
                    (Command::Source, 0xF1) => Asked::Streamer,
                    _ => Asked::Set(data),
                };
                assert_eq!(Asked::of(c, data), want, "{c:?} 0x{data:02X}");
            }
        }
    }

    /// The reply's `0x10` bit: `15` is DIGITAL 1 as the Streamer Input. No
    /// other bit is in the table.
    #[test]
    fn the_streamer_bit_of_a_source_reply() {
        let plain = Input::from_data(0x05).expect("DIGITAL 1");
        assert_eq!((plain.source, plain.streamer), (Source::Digital1, false));
        let streamer = Input::from_data(0x15).expect("DIGITAL 1 as the Streamer Input");
        assert_eq!(
            (streamer.source, streamer.streamer),
            (Source::Digital1, true)
        );
        assert_eq!(streamer.data(), 0x15);
        for outside in [0x00, 0x10, 0x0A, 0x1A, 0x25, 0x85, 0xF1] {
            assert_eq!(Input::from_data(outside), None, "0x{outside:02X}");
        }
    }

    #[test]
    fn power_and_mute_both_ways_without_the_toggle() {
        assert_eq!(Power::from_data(0), Some(Power::Standby));
        assert_eq!(Power::from_data(1), Some(Power::On));
        assert_eq!(Mute::from_data(0), Some(Mute::Muted));
        assert_eq!(Mute::from_data(1), Some(Mute::NotMuted));
        assert!(Mute::Muted.is_muted() && !Mute::NotMuted.is_muted());
        for outside in [TOGGLE, QUERY, 0x21, 0xFF] {
            assert_eq!(Power::from_data(outside), None);
            assert_eq!(Mute::from_data(outside), None);
        }
        for p in [Power::Standby, Power::On] {
            assert_eq!(Power::from_data(p.data()), Some(p));
        }
        for m in [Mute::Muted, Mute::NotMuted] {
            assert_eq!(Mute::from_data(m.data()), Some(m));
        }
    }

    /// Every reply of the capture reads, says what its label asked, and
    /// writes back to the bytes it came from.
    #[test]
    fn every_reply_of_the_capture_reads_and_writes_back() {
        let mut read = 0;
        for b in recordings::bursts() {
            let mut d = Decoder::new();
            let mut frames = alloc::vec::Vec::new();
            d.feed(&b.rx, &mut |f| frames.push(f));
            for f in frames {
                let reply = Reply::from_frame(f).unwrap_or_else(|e| panic!("{}: {e}", b.label));
                assert_eq!(reply, recordings::answer(&b), "{}", b.label);
                assert_eq!(reply.command().code(), b.tx[1], "{}", b.label);
                assert_eq!(reply.frame(), f);
                let back = [0x21, f.code, 0x00, f.data, 0x0D];
                assert_eq!(b.rx, back, "{}", b.label);
                read += 1;
            }
        }
        assert_eq!(read, 108);
    }

    #[test]
    fn a_frame_outside_the_table_is_refused_with_a_reason() {
        let read = |code, data| Reply::from_frame(Frame { code, data });
        assert_eq!(read(0x0D, 99), Ok(Reply::Volume(99)));
        assert_eq!(
            read(0x0D, 100),
            Err(Unread::Data {
                command: Command::Volume,
                data: 100
            })
        );
        assert!(read(0x0D, QUERY).is_err());
        // The false frame a cut reply makes (see `frame`): power 0x21.
        let e = read(0x00, 0x21).expect_err("power has no 0x21");
        assert_eq!(e.to_string(), "power has no reply 0x21");
        assert!(read(0x00, TOGGLE).is_err());
        assert!(read(0x0E, TOGGLE).is_err());
        assert_eq!(read(0xFE, 0x21), Err(Unread::Code(0xFE)));
        assert_eq!(
            read(0xFE, 0x21).expect_err("no code").to_string(),
            "no command has code 0xFE"
        );
        assert_eq!(read(0x5E, MODEL_A25), Ok(Reply::Model(3)));
        assert_eq!(read(0x61, 0x01), Ok(Reply::DacFilter { own: true }));
        assert_eq!(read(0x61, 0x00), Ok(Reply::DacFilter { own: false }));
        assert!(read(0x61, 0x02).is_err());
    }
}
