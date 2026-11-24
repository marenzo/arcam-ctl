# CONTROL jack protocol

The serial protocol of the Arcam Radia A25's RJ12 CONTROL jack, Arcam
software V1.34. Arcam does not document it. It was recovered from Arcam's
public software, the amplifier's firmware, and checked on an A25. Other
Radia models and Arcam software versions are unverified. On this page,
"firmware" always means Arcam's.

The page has four parts: the [specification](#physical-layer), the
[evidence](#evidence) behind it, [how it was found](#how-it-was-found) and
what is still [open](#open).

Each claim is marked:

- **seen**: observed on the jack, in a [capture](#captures);
- **bench report**: observed by the maintainer in a session outside a capture;
- **firmware**: read from the [firmware](#firmware), not yet observed.

## Physical layer

RS-232 levels through a transceiver with auto-powerdown: the amplifier's
driver is silent until its receiver sees a valid level (**seen**). How a 3.3 V
pin talks to it: [hardware.md](hardware.md#electrical).

| | |
|---|---|
| Format | 19200 baud, 8 data bits, no parity, 1 stop bit (**seen**) |
| Contacts | 6P4C: ground, amplifier receive, amplifier transmit, ground ([wiring](hardware.md#wiring)) |

## Framing

```
request:  21 cc dd 0D       '!', command, one data byte, '\r'
reply:    21 cc 00 dd 0D    the third byte is always 00
```

The amplifier discards everything before a `21`. A request with more than one
data byte is dropped whole (**firmware**).

## Commands

| cc | Name | dd in a request | dd in the reply | Basis |
|---|---|---|---|---|
| `00` | power | `00` standby, `01` on, `02` toggle, `F0` query | `00` standby, `01` on | seen; toggle: firmware |
| `0D` | volume | `00`..`63` set (0-99), `F0` query, `F1` up one step, `F2` down one step | the volume, 0-99 | query and steps seen; set: bench report |
| `0E` | mute | `00` mute on, `01` mute off, `02` toggle, `F0` query | `00` muted, `01` not muted | seen; toggle: firmware |
| `1D` | source | `01`..`09` select, `F0` query, `F1` the configured Streamer Input | the source; bit `10` set when it is the Streamer Input | query seen, input change confirmed by bench report |
| `5E` | model | `F0` | `03` on an A25 | seen |
| `61` | DAC filter | never send it ([traps](#traps)) | | firmware |

There are no other commands: no key simulation and no version query.

- Mute's data is inverted against intuition: `00` means muted, in requests and
  replies (**seen**).
- A volume set or step while muted unmutes first, and the reply does not say so
  (**firmware**).
- The volume is that of the current input's mode: the processor-mode volume
  for an input in processor mode, Bluetooth's own on the Bluetooth input
  (**firmware**).
- Bluetooth cannot be selected while the amplifier is a Bluetooth sink
  (**firmware**).

### Source codes

| dd | Input | Name in this project |
|---|---|---|
| `01` | Phono | `phono` |
| `02` | Analogue 1 | `analogue_1` |
| `03` | Analogue 2 | `analogue_2` |
| `04` | Analogue 3 | `analogue_3` |
| `05` | Digital 1 | `digital_1` |
| `06` | Digital 2 | `digital_2` |
| `07` | Digital 3 | `digital_3` |
| `08` | USB-C | `usb_c` |
| `09` | Bluetooth | `bluetooth` |
| `F1` | the configured Streamer Input (request only) | `streamer` |

If Digital 1 is the Streamer Input, the reply reads `15` instead of `05`.

### Examples

Built from the table. The capture has the queries, standby, the steps and
mute; `volume 40` and `Digital 1` are illustrations.

```
21 00 F0 0D   power?        ->  21 00 00 01 0D   on
21 00 00 0D   standby       ->  21 00 00 00 0D
21 0D 28 0D   volume 40     ->  21 0D 00 28 0D
21 0D F1 0D   volume up     ->  21 0D 00 29 0D
21 0E 00 0D   mute          ->  21 0E 00 00 0D   muted
21 0E F0 0D   muted?        ->  21 0E 00 01 0D   not muted
21 1D 05 0D   Digital 1     ->  21 1D 00 05 0D
21 5E F0 0D   model         ->  21 5E 00 03 0D
```

## Timing

| | Value | Basis |
|---|---|---|
| Parser period | the receive buffer is parsed every 50 ms | firmware |
| Reply delay | 0 to 50 ms after the request, by that period | seen |
| Lost requests | a request split across two parser runs is lost: about 6 % got no reply | seen |
| Wake | a power-on is answered `01` at once, then nothing is answered for about 4 s | seen |
| Power-up burst spacing | 10 ms between frames | firmware |
| Auto-standby | 20 minutes, see [below](#auto-standby) | firmware |

## Unsolicited frames

The amplifier also sends replies unasked when something changes locally:
front panel, remote, Bluetooth or the standby timer (**firmware**).

After a mains power-up it sends one frame per status, in this order: power,
model, volume, source, `61`, mute. A wake from standby sends only a power frame
(`21 00 00 01 0D`) once it is up (**firmware**).

None of these has been seen: a sleeping driver cannot send them, and a bare
pin wakes it only inside its own holds.

## Traps

| Trap | Detail | Basis |
|---|---|---|
| Lost requests | wait for the reply; send a setter once more. Never resend a step or toggle: both could land | seen |
| The power-on reply comes early | the amplifier says `01` before it is up; query power until it answers `01` again | seen |
| A wake clears mute | and sends no volume, mute or source frame. Read them again after a wake | firmware |
| Every frame restarts auto-standby | see [below](#auto-standby) | firmware |
| Resync | three bytes that are neither `21` nor `0D` return the parser to a known state | firmware |
| `0D 0D 0D` is not filler | it reads as "volume 13" | firmware |
| No volume ceiling | the amplifier accepted an absolute set to 99 | bench report |
| `61` | hands the DAC filter to an external controller, and the setting persists | firmware |
| Text in the buffer | the six bytes `bt rst` reset the Bluetooth module | firmware |

The power toggle (`00 02`) is unsafe for a controller too: its result depends
on a state the controller may not know. The bridge can send neither it nor
`61` ([what is never sent](integration.md#what-is-never-sent)).

## Auto-standby

With the amplifier on and Eco Standby enabled in its menu, a 20-minute timer
puts it in standby. The timer restarts on any of (**firmware**):

- an audio signal on the current input;
- a key press or a remote code;
- every control frame, before the frame is acted on.

A controller cannot see the audio, so it cannot imitate this rule, and one
that queries more often than every 20 minutes keeps the amplifier on forever.
This project's answer: [polling](integration.md#polling).

## Evidence

### Captures

Raw exchanges with a real amplifier live in [captures/](captures/), one file
per session, named `<model>-<firmware>-<session>.jsonl`. They are test
fixtures: the core's tests (`core/src/recordings.rs`) decode every reply in
them and replay them through the link, and the emulator's tests check that
the virtual amplifier answers each request byte for byte. Never edit a
capture. Add a new session as a new file.

Each line is one JSON object:

- `"kind": "burst"`: one request written and the line held for the reply:
  `label` (what was asked), `tx_hex`, `hold_ms`, `rx_hex` (the reply, empty
  when none came) and `t_ms`;
- `"kind": "rx"`: bytes a floating pin produced before the wiring was right
  (`hex`).

`t_ms` is milliseconds since the session's first line.

**`a25-v1.34-bringup.jsonl`**: the first bring-up, on an A25 V1.34, from a
bare 3.3 V ESP32 pin ([hardware](hardware.md#electrical)). 135 exchanges:

- At the start: power `01` (on), volume `3B` (59), mute `01` (not muted),
  source `07` (Digital 3, not the Streamer Input), model `03`. No framing
  error in any reply.
- Volume down answered `3A`, and up answered `3B`. Mute on (`00`) answered
  `00` and the query then read `00`. Mute off (`01`) answered `01`.
- Standby (`21 00 00 0D`) answered `00`. In standby the amplifier kept
  answering: ten power queries read `00`, and volume read `3B`.
- Wake (`21 00 01 0D`) answered `01` at once. Power queries starting 0.9,
  1.6, 2.4 and 3.2 s after it got no answer. The one at 3.9 s read `01`.
  Volume, mute and source then read as before the standby.
- Holds: of the 25 exchanges with holds of 4 to 40 ms, 16 got no reply and
  one a partial one (`21 00 00` at 4 ms), which is why a hold must cover the
  50 ms parser period. Outside the wake, 6 of the 104
  exchanges with a hold of 60 ms or more got no reply, about 1 in 17. In the
  50-query loss test, 3 went unanswered (`loss 5`, `loss 20`, `loss 37`).

The source change (`1D 05` and others) and the absolute volume of 99 were
bench reports in later sessions, not in this capture.

### Firmware

The analysed build is Arcam's A25 V1.34 download,
<https://www.arcam.co.uk/ugc/tor/A25/A25%20Software%20V1.34/ARCAM%20A25%20software_V1.34.zip>
(`A25.bin`, sha256
`f8d6e080eb24d84ffcc971abedf420e786e07593c147329703a35e3487d4bbcf`). It was
analysed only to make an independently written program work with the A25,
and this page publishes only what that needs: the wire protocol and how the
amplifier behaves on it. This repository contains none of Arcam's software,
and no addresses, disassembly or code from it. Anyone with the download can
check each **firmware** claim against it.

## How it was found

The notes below are for anyone working on this amplifier family or a similar
undocumented port.

**Where the protocol was not.** SERVICE USB on the A25 (not the A25+) is a
USB host port for firmware updates from a stick, not a console. The A25 has
no RS-232 or Ethernet port. No pinout, levels or protocol for the CONTROL
jack had been published, not on code hosts, forums or Arcam's integration
pages. The FCC filing belongs to the Bluetooth module, so it has no internal
photos.

**What pointed the right way.**

- The jack is what Arcam's ST5 and CD5 use to drive the amplifier's power,
  input and volume, so it carries a control protocol.
- Arcam's own cable crosses the middle two of its four wires (jack contacts
  3 and 4, terminals 3 and 4 in [the wiring](hardware.md#wiring)): the data
  is on that pair, with transmit and receive swapped between the two ends.
- The amplifier's firmware is a public download. The command codes it handles
  (`00` power, `0D` volume, `0E` mute, `1D` source) are Arcam's SA10/SA20/SA30
  RS-232 codes without the zone and length bytes, at 19200 baud instead of
  38400.

**The method.** The jack's levels were unknown, so the ESP32 measured them
before driving any pin ([hardware](hardware.md#probing-with-an-esp32)). The
firmware then gave the framing, the codes and the timing, and each finding
was asked of the amplifier.

**What it taught.**

- One periodic parser explains the timing. The 50 ms parse period accounts
  for both the 0-50 ms reply delay and the requests lost when they straddle
  two runs.
- Behind an auto-powerdown transceiver, nothing is heard while the driver
  sleeps. A controller on a bare pin can only ask and listen, even though the
  amplifier reports changes on its own.
- Polling changes the amplifier's behavior, because every control frame
  restarts its auto-standby timer. This shaped the bridge's poll schedule.
- Some commands are unsafe. `61` changes a persistent setting, and the power
  toggle's result depends on a state the controller may not know. A bridge
  should be unable to send either, not merely avoid them.
- The firmware is evidence, not proof. Several first readings were corrected
  on the jack: the mute polarity, which frames form the power-up burst, and
  which text commands are reachable. Mark each claim by what supports it.

## Open

Help is wanted on these:

- **Unsolicited frames**, including the power-up burst. A real RS-232
  transceiver (a MAX3232, for example) keeps the amplifier's driver awake and
  should hear the knob, the remote and the burst. It is unknown whether the
  amplifier tolerates a standing level on its receive line.
- What volume and mute read after a source change.
- Commands other than power and volume queries while in standby.
- Bluetooth selection while the amplifier is a Bluetooth sink.
- **Other models and firmware versions**: model code, source numbering, and
  anything else that differs.
  [Report one](https://github.com/marenzo/arcam-ctl/issues/new?template=compatibility.yml).
- **Ground hum** ([hardware.md](hardware.md#ground-hum)).
