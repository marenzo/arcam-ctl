# Working on the bridge

An ESP32 bridge to the Arcam Radia A25's CONTROL jack, exposed over MQTT
(with Home Assistant discovery) and HTTP. Read [docs/protocol.md](docs/protocol.md)
for wire behavior and [docs/integration.md](docs/integration.md) for the
public HTTP and MQTT contracts. Read [docs/hardware.md](docs/hardware.md) before
any claim about wiring or levels.

## Layout

Each component owns its entry script.

| Path | Owns | Entry |
|---|---|---|
| `core/` | framing, command table, typed send policy, link (retries, wake), poll schedule. `no_std`, no dependencies, no allocation, tested on the host | `check.sh` |
| `firmware/` | the ESP32 app: UART line, MQTT, REST, `/metrics`, OTA, settings in nvs, Improv Wi-Fi. Its own manifest, lockfile and Xtensa toolchain | `firmware/build.sh` |
| `emulator/` | a virtual A25 plus Espressif's QEMU and Mosquitto, for the end-to-end round | `emulator/test.sh` |
| `docs/` | the reader-facing docs ([docs/README.md](docs/README.md) is the map), the captures, the wiring guide the device serves at `/wiring` | |
| `.github/` | CI (it runs the three entry scripts) and the compatibility issue form | |

The root Cargo workspace is `core` and `emulator`. `firmware/` is excluded
and repeats the workspace's package fields. Keep the two in step.

## Rules

- Protocol behavior lives in `core/`, with tests. The firmware only connects
  it to hardware and the network.
- Power toggle (`00 02`) and DAC filter (`61`) writes must stay unreachable:
  `policy::Wire` is the only source of request bytes, and it cannot express
  either.
- Relative steps and toggles are never retried on the wire. Only an absolute
  setter or a query may be sent a second time (`policy::Request::once`).
- The MQTT topics, the REST API and the send policy are public contracts.
  Change them only when asked, update
  [docs/integration.md](docs/integration.md) in the same change and record the
  change in [CHANGELOG.md](CHANGELOG.md).
- Code and docs change together. Keep changes small, with one change each.

## Evidence

- `docs/captures/` holds test fixtures, read by `core/src/recordings.rs` and
  the emulator's tests. Keep raw exchanges intact. Add a new session as a
  new file ([format](docs/protocol.md#captures)).
- Mark every protocol claim with its basis: **seen** (on the jack, in a
  capture), **bench report** (a maintainer observation outside a capture) or
  **firmware** (read from Arcam's image, with function addresses in
  [docs/protocol.md](docs/protocol.md#firmware)). An emulator run is
  not evidence about the amplifier.
- In `emulator/`, a behavior no source supports is the most conservative
  choice and is marked `Assumption:`. Check it against the capture when
  possible.
- Real Wi-Fi, electrical behavior and physical Home Assistant control each
  need hardware evidence. Never claim them from QEMU.
- Keep Arcam's firmware out of the repository, issues and pull requests.

## Build and check

Use rustup for the host and Docker with BuildKit for the firmware.

```sh
./check.sh                  # host: fmt, clippy, tests, the core on bare metal, rustdoc, typos if installed
./check.sh --firmware       # also the firmware build below
firmware/build.sh --clippy  # production firmware in a container, fmt and clippy first
```

A change under `firmware/` needs the container build: the root checks do not
compile it. The build writes to `dist/`:

- `arcam-radia-esp32-control.bin`: the merged image, flashed at `0x0`;
- `arcam-radia-esp32-control-app.bin`: the app image, for OTA;
- `firmware-notices.tar.gz`: upstream license texts and a dependency
  inventory (`firmware/notices.py`).

A first build copies `Cargo.lock` and `components_esp32.lock` into
`firmware/`. Commit them when they change.

## Emulator

Run this round for device-facing behavior (MQTT, REST, settings, OTA, poll
timing):

```sh
firmware/build.sh --qemu
emulator/test.sh
```

It boots the QEMU image against the virtual amplifier and a real broker.
[emulator/README.md](emulator/README.md) covers what it needs, what is
modeled, what the round checks, the control port and a manual Home Assistant
session. QEMU's UART is a byte pipe, so passing the round is no hardware
test.

## Other Arcam models

Use the `adapt-model` skill (`.claude/skills/adapt-model/SKILL.md`) to support
a model other than the A25. Reports come in through the compatibility issue
form.

## Docs

Write plain, concise English. Each fact has one home, and other places link to
it. [README.md](README.md) is for users: what works, how to set it up and the
evidence status. [docs/protocol.md](docs/protocol.md) is the spec, its
evidence, how it was found and what is open. `docs/wiring/adapter.jpg` is
rendered from `docs/wiring/adapter.html`. The steps are in that file's header.

## Releases

The workspace and firmware package versions must match the tag (`0.1.0` and
`v0.1.0`). Date the changelog entry and update the README's status. On a `v*`
tag, CI waits for the host, spelling, firmware and emulator jobs, then
publishes the merged image, the OTA image, the notices and `SHA256SUMS`.
`core/` is also the `arcam-radia` crate. CI dry-runs its publish on every
push. The maintainer publishes it by hand with `cargo publish -p arcam-radia`
after the tag. A published version cannot be taken back, only yanked.

## License

Code is MIT ([LICENSE](LICENSE)). Docs, measurements and captures are CC BY
4.0 ([docs/LICENSE](docs/LICENSE)).
