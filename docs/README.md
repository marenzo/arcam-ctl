# Documentation

Start with the page for what you are doing.

| You want to | Read |
|---|---|
| build the bridge and connect it to the amplifier | [hardware.md](hardware.md): parts, wiring, identifying the terminals, the electrical limits |
| follow the wiring step by step | [wiring/guide.html](wiring/guide.html), a 3D guide. The device also serves it at `http://<device address>/wiring` |
| use it from Home Assistant, MQTT, HTTP or Prometheus | [integration.md](integration.md): entities, topics, the REST API, metrics, polling and retries |
| understand or extend the protocol | [protocol.md](protocol.md): the specification, the evidence behind each claim, how it was found and what is still open |
| work on the code | [AGENTS.md](../AGENTS.md): layout, rules, build, checks, the emulator, releases |

## What is here

```
docs/
  hardware.md       the physical build
  integration.md    the public HTTP and MQTT contracts
  protocol.md       the CONTROL jack protocol and its evidence
  captures/         raw exchanges with real amplifiers, one file per session
  wiring/
    guide.html      the 3D wiring guide (built into the firmware)
    adapter.html    the source of adapter.jpg, the adapter rendering
    installed.jpg   a photo of the bridge on the maintainer's A25
    three-r128/     the vendored three.js viewer, MIT
  LICENSE           CC BY 4.0 for the docs and captures
```

Each fact has one home, and other pages link to it. Protocol claims are
marked **seen**, **bench report** or **firmware**, as
[protocol.md](protocol.md) explains. The captures are test fixtures: the core
and emulator tests read them ([captures](protocol.md#captures)), so never edit
one. Add a new session as a new file.
