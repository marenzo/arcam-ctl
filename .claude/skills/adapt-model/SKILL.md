---
name: adapt-model
description: Extend the bridge to an Arcam amplifier other than the Radia A25 (A15, A5, A25+, a newer A25 firmware) from a compatibility report, a capture or the model's public firmware. Use when someone reports another model, sends a capture or a model byte other than 03, or asks to support or check a Radia model. Not for Arcam's SA, AV or HDA series, which use the documented RS-232 protocol with zone and length bytes and need a separate core.
---

# Adapt the bridge to another Arcam model

The A25's protocol is the only verified one. A sibling model probably shares
the framing, but its model byte, inputs, timing and traps may differ. Put
every difference in `core/` with its evidence, and never infer one model's
behavior from another's. Read `AGENTS.md` first. Its rules apply here without
exception.

## 1. Collect the evidence

Fill in this table before you change any code. Leave a row blank if no source
gives a value for it.

| Item | A25 V1.34 | New model | Basis |
|---|---|---|---|
| Model byte (`21 5E F0 0D`) | `03` | | |
| Serial format | 19200 8N1 | | |
| Framing (`21 cc dd 0D` / `21 cc 00 dd 0D`) | as shown | | |
| Command codes | `00 0D 0E 1D 5E 61` | | |
| Volume range | 0-99 | | |
| Mute polarity | `00` = muted | | |
| Source codes and names | `docs/protocol.md#source-codes` | | |
| Streamer Input (`F1` request, `10` reply bit) | yes | | |
| Wake silence after power-on | about 4 s | | |
| Auto-standby | 20 min, every frame restarts it | | |
| Dangerous writes | power toggle `00 02`, `61` | | |
| Text traps in the buffer | `bt rst` | | |
| Contacts and levels | `docs/hardware.md` | | |

Use these sources, from strongest to weakest:

1. **seen**: a capture. Save the bridge's exchanges in the format of
   `docs/protocol.md#captures` as
   `docs/captures/<model>-<firmware>-<session>.jsonl`. Keep the raw bytes
   and never edit them afterwards.
2. **bench report**: a maintainer's or reporter's observation, for example
   `GET /api/state` from the compatibility issue form. Its `model` field is
   the model byte.
3. **firmware**: Arcam's public download for that model. Repeat the A25
   method in `docs/protocol.md#how-it-was-found`: find the UART set to
   19200, the 50 ms parser task, the dispatch on the command byte and each handler. Record the
   download URL, the image's sha256, its base address and the function entry
   points. Addresses from the A25 build do not apply to another build.
   Never commit the image, or any part of it, to the repository.

On hardware, follow `docs/hardware.md#identify-the-terminals` before you drive
any pin. Start with queries only (`F0`). Never send the power toggle or `61`
to explore, and never send a code the table does not list.

If the framing or the serial format differs, stop. Report the difference to
the user, because the change then belongs in a separate core and is beyond
this workflow.

## 2. Change the core

All model knowledge lives in `core/`. The firmware and the emulator read it
from there.

- `core/src/table.rs`: add the model byte beside `MODEL_A25`. If the input
  list differs, add a `Model` type decoded from the model byte with
  `sources() -> &'static [Source]`, and keep `Source` as the union of
  inputs. Add a variant only for an input that has evidence. An unknown model
  byte keeps today's A25 behavior, so existing users see no change.
- `core/src/policy.rs`: refuse a source the reported model lacks, the same
  way it refuses a volume above the ceiling. Any newly found dangerous write
  joins the power toggle and `61` as something `Wire` cannot express.
  Relative steps and toggles stay `Request::once`.
- `core/src/link.rs` and `core/src/poll.rs`: if the wake time or the
  auto-standby rule differs, key the constant to the model rather than
  changing the A25's value.
- Tests: for each difference, add a test that names its basis. Read a new
  capture through `recordings.rs`, as the A25 tests do.

## 3. Connect the firmware and the emulator

- `firmware/src/mqtt.rs`: build the discovery `options` for the source select
  from the model's inputs, and the device's `model` field (now `Radia A25
  bridge`) from the model byte. The source select changes once the first
  model reply arrives, so publish discovery again at that point.
- `firmware/src/api.rs`: the source parser and its error message.
- `emulator/src/lib.rs`: make `Amp` take its model (byte, inputs, wake time),
  and add `--model` to `emulator/src/main.rs`. A behavior no source supports
  gets the most conservative choice and an `Assumption:` comment. Check the
  emulator against the new capture, as the A25 emulator test does.

## 4. Update the docs

- `docs/protocol.md`: add a short section for each model with its own rows
  and their basis. Leave the A25 tables untouched. Add the model's unknowns
  to "Open".
- `docs/protocol.md#firmware`: add an evidence index for the new firmware
  build. Remove the model from "Open" once its questions are answered.
- `docs/integration.md`: per-model source names or select options are a
  public contract change, so record them in `CHANGELOG.md`.
- `docs/hardware.md`: only for contacts or levels that were measured to
  differ.
- `README.md`, the amplifier table: `works` requires exchanges on that
  model. Firmware reading alone is `expected, untested`. A model with no
  evidence stays `unknown`.

## 5. Verify and report

```sh
./check.sh
firmware/build.sh --clippy
firmware/build.sh --qemu && emulator/test.sh
```

Run the emulator round for the A25 and for the new model. In the report to
the user, state for each difference whether it was seen, bench-reported,
read from firmware or assumed, and list what still needs a hardware session.
