# Emulator

The emulator runs the real firmware against a virtual Arcam Radia A25, with no
board and no amplifier. It is the end-to-end test of everything the bridge
does above the wire: MQTT and Home Assistant discovery, REST, settings in
flash, the poll schedule, `/metrics` and OTA. CI runs it on every push to `main` and on every pull request.

```
virtual-amp  ◄── UART1, TCP 5555 ──►  QEMU ESP32  ◄── MQTT ──►  Mosquitto  (◄── Home Assistant)
    ▲                                    ▲
    └─ control, 15556      HTTP, 18080 ──┘
                emulator/test.sh
```

## The pieces

| Service | What it is |
|---|---|
| `qemu` | Espressif's QEMU for the ESP32, booting `dist/arcam-radia-esp32-control-qemu.bin`. This is the release firmware built with the `qemu` feature: Wi-Fi is replaced by emulated Ethernet and Improv is left out. UART0 is the log. UART1, the jack, connects to `amp:5555`. The flash image is written in place, so settings persist across restarts |
| `amp` | `virtual-amp` (`src/`), the amplifier's side of the CONTROL jack, built on the core's command table |
| `mosquitto` | a real broker, with anonymous access |
| `homeassistant` | only with `--profile ha`, for a manual look |

`Dockerfile` builds one image with QEMU (pinned and checksummed) and
`virtual-amp`. `compose.yml` wires the services together.

## The virtual amplifier

`src/lib.rs` models the A25 as [docs/protocol.md](../docs/protocol.md) and
the [capture](../docs/captures/) describe it. It starts as the bench found
it: on, volume 59, not muted, Digital 3.

What it imitates:

- the parser: everything before a `21` is skipped, and a request needs
  exactly one data byte;
- the reply delay, at the parser's next 50 ms run;
- power, volume (0-99, steps), mute with its inverted data, source and the
  Streamer Input bit, and the model byte `03`;
- standby, and a wake that answers `01` at once, then stays deaf for 3.8 s
  and clears the mute;
- the 20-minute auto-standby, restarted by every frame.

It counts every power toggle and `61` it receives as `forbidden`, and the
test requires that count to stay at zero.

What it does not imitate: lost requests, unsolicited frames, the electrical
layer (QEMU's UART is a byte pipe), the Bluetooth-sink rule and `bt rst`.
Where no source says what the amplifier does, the model takes the most
conservative choice, and the code marks it `Assumption:`.

Its own tests replay the capture and require every reply the capture heard,
byte for byte.

### Control port

`virtual-amp` takes one line on TCP 5556 (host port 15556) and answers one
line:

| Line | Does |
|---|---|
| `state` | nothing; returns the state |
| `knob +N`, `knob -N` | turns the volume knob, as a hand on the front panel would |
| `standby` | standby by remote |
| `power on` | power-on by remote |

The answer is one line of `key=value`: `power` (`on`, `standby`, `waking`),
`volume`, `muted`, `source`, `frames` (requests taken), `forbidden` and
`idle_ms` (time since the auto-standby timer restarted).

```sh
exec 3<>/dev/tcp/127.0.0.1/15556; echo "knob +2" >&3; read -r l <&3; echo "$l"
```

Flags: `--uart ADDR`, `--control ADDR`, `--auto-standby-ms N` (`0` turns Eco
Standby off). Every exchange is logged on stderr (`docker compose logs amp`).

## The automated round

```sh
firmware/build.sh --qemu
emulator/test.sh
```

It needs Docker, curl and python3, plus free local ports 14444, 15556 and
18080. Stop any manual session first. The round copies the image into a
temporary flash file under `target/`, starts its own Compose project, and
removes both on exit. It prints `PASS` or `FAIL` per check and exits non-zero
on any failure. A failure prints the last log lines of `qemu` and `amp`.

It checks, in order:

1. the device answers REST and serves `/wiring`;
2. settings over REST, including the broker password rules, then the broker
   connection and availability;
3. Home Assistant discovery: exactly 14 configs, each well formed;
4. every command over MQTT and over REST, with the amplifier's state after
   each: steps, absolute volume, the ceiling refusing 61, mute, standby,
   wake and sources;
5. a turn of the knob reaching the state within 2.5 s;
6. `/metrics`;
7. edge cases: malformed or oversized requests, and a retained MQTT step
   that is acted on only once;
8. settings surviving a reset and a QEMU restart, and the restart commands;
9. OTA: refused uploads, an accepted one, the boot into the other slot, its
   confirmation, and rollback after a reset before confirmation;
10. quiet `standby_safe` polling sending nothing, and no forbidden frame;
11. a command stuck behind a silent UART returning HTTP 202 `pending`.

Passing it shows that the firmware's logic is right against the model. It is
no hardware test: Wi-Fi, Improv, the electrical layer and anything the
amplifier has not been seen to do are not covered.

## A manual session with Home Assistant

```sh
firmware/build.sh --qemu
mkdir -p target/emulator
cp dist/arcam-radia-esp32-control-qemu.bin target/emulator/flash.bin   # resets this session's settings
docker compose -f emulator/compose.yml --profile ha up -d
```

Set the broker host to `mosquitto` in both the bridge
(<http://127.0.0.1:18080/>) and Home Assistant (<http://127.0.0.1:18123/>),
then use the control port to play the amplifier's side. Stop it with
`docker compose -f emulator/compose.yml --profile ha down`.
