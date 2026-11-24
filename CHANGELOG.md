# Changelog

Format: [Keep a Changelog](https://keepachangelog.com). Versions follow
[Semantic Versioning](https://semver.org); before 1.0 anything may change,
including MQTT topics and the REST API.

## 0.1.1 (2026-11-24)

### Changed

- The protocol library is published as `arcam-ctl`. The 0.1.0 crate,
  `arcam-radia`, was a name made only of Arcam's marks and has been removed
  from crates.io. Update a dependency to `arcam-ctl = "0.1.1"` and
  `use arcam_ctl::…`; the API is unchanged.
- The README warns that Arcam's warranty is unlikely to cover damage from
  connecting the bridge, and names Harman as the owner of the ARCAM and
  Radia marks.
- `docs/protocol.md` no longer lists function addresses in Arcam's software.
  It identifies the analysed build by its hash and states that the analysis
  was only for interoperability.

## 0.1.0 (2026-11-24)

The first release.

### Added

- The protocol library (`core/`, the `arcam-radia` crate): framing, the
  command table, a send policy that cannot express the power toggle or `61`
  and enforces a volume ceiling, the amplifier's state, a link with retry and
  wake rules, and the poll schedule.
- ESP32 firmware: the jack on a bare pin, poll modes `off`, `standby_safe` and
  `live`, settings in flash.
- MQTT with Home Assistant discovery: switches, number, select, binary sensor,
  buttons, diagnostics. MQTT retries connection setup after a client error
  and republishes the state after a failed send.
- REST: state, commands, settings and restart, and a settings page that
  reports stale readings and the outcome of Refresh. A queued command that
  outlasts the wait returns HTTP 202 with `pending`. Settings reject a
  malformed request as a whole, and clear an old broker password when the
  broker identity changes.
- A volume ceiling, also applied after an input change: a level read above
  it is set to the ceiling, and the mute is restored.
- Cross-site POSTs, sent by a browser for a page on another site, are
  refused with HTTP 403.
- `GET /metrics`: the device, the link, the amplifier and MQTT for
  Prometheus.
- Over-the-air updates: `POST /api/ota` and the settings page's Update
  firmware write the app image to a second app slot. A new image confirms
  itself once it is on the network; a reset before that boots the previous
  one.
- Wi-Fi setup over Improv Serial, and an offline 3D wiring guide served by
  the device.
- A container build, an emulator for end-to-end tests in CI, and releases
  with the merged image, the OTA image, SHA-256 checksums and a firmware
  notices archive.
