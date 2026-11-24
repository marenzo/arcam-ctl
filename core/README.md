# arcam-radia

A Rust library for the serial protocol of the Arcam Radia A25's CONTROL jack,
the RJ12 port on the back that Arcam does not document. It is `no_std`, has
no dependencies, never allocates and does no I/O. You bring the UART and
the clock, and it gives you the bytes to send and the facts that come back.

This is an unofficial, independent interoperability project. It is not made,
supported or endorsed by Arcam.

## What is in it

- `frame`: the request encoder (`21 cc dd 0D`) and the reply decoder
  (`21 cc 00 dd 0D`), which skips noise.
- `table`: the command codes, and what their data bytes mean.
- `policy`: the only way to make request bytes. The power toggle and the DAC
  filter command `0x61` cannot be expressed at all, and a volume above a
  configurable ceiling is refused.
- `state`: what the amplifier last said about itself, and when.
- `link`: one request in flight, its hold, a single retry for setters only
  (never for a step or a toggle), and the wait while the amplifier wakes.
- `poll`: when to read the amplifier unasked without defeating its
  20-minute auto-standby.

```rust
use arcam_radia::policy::{Policy, Request, Volume};

let policy = Policy { volume_max: 60 };
let wire = policy.check(Request::Volume(Volume::Set(40)), None).unwrap();
assert_eq!(wire.bytes(), &[0x21, 0x0D, 0x28, 0x0D]);
assert!(policy.check(Request::Volume(Volume::Set(80)), None).is_err());
```

The crate documentation has the burst-mode loop that drives `link` from a
3.3 V GPIO.

## Status

Verified against a real A25 on firmware V1.34. Other Radia models and
firmware versions are untested. The protocol, the evidence behind each
claim and the open questions are in
[docs/protocol.md](https://github.com/marenzo/arcam-radia-esp32-control/blob/main/docs/protocol.md).
The ESP32 firmware that uses this crate is in the same
[repository](https://github.com/marenzo/arcam-radia-esp32-control).

## License

MIT. Arcam and Radia are trademarks of their respective owners. Their use
here only describes compatibility.
