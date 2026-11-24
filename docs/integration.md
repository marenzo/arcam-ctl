# Home Assistant, MQTT and REST

## Broker

Home Assistant needs an MQTT broker; the simplest is the Mosquitto add-on.
Install and start it, add the MQTT integration, and create a user for the
bridge. Enter the broker's host, port, user and password on the bridge's
settings page (`http://<device address>/`). Give an IP address or a DNS name:
the bridge does not resolve `.local` names. The connection uses plaintext
MQTT, including the login. The bridge's account needs to:

- publish to `arcam-radia/<id>/#`, including its own `set/` topics, where it
  clears retained commands;
- subscribe to `arcam-radia/<id>/set/+` and `homeassistant/status`;
- publish to `homeassistant/+/<id>/+/config`, the discovery configs.

## Entities

The device is named `arcam-radia-XXXXXX`, the last three bytes of its MAC
address in hex. Discovery uses Home Assistant's default prefix,
`homeassistant`, and is announced again whenever Home Assistant publishes
`online` on `homeassistant/status`.

| Entity | Type | Notes |
|---|---|---|
| Power | switch | on, standby |
| Mute | switch | |
| Volume | number | 0 to 99; above the ceiling is refused |
| Source | select | the names in [protocol.md](protocol.md#source-codes) |
| Streamer input | binary sensor | the current input is the amplifier's configured Streamer Input |
| Refresh, Volume up, Volume down, Mute toggle | button | |
| Restart | config button | restarts the device; settings and Wi-Fi are kept |
| Link, Reply time, Exchanges, Losses | diagnostic sensor | |

A value the amplifier has not reported yet is unknown. Every value is the last
one the amplifier reported ([polling](#polling)).

## MQTT

Base topic: `arcam-radia/<id>`, with `<id>` the device name above.

| Topic | Direction | Payload |
|---|---|---|
| `<base>/availability` | out, retained, last will | `online` or `offline` |
| `<base>/state` | out, retained | the state JSON, on every change |
| `<base>/diag` | out | the diagnostics JSON, at connect and every 60 s |
| `<base>/result` | out | the result JSON of the last command |
| `<base>/set/power` | in | `on`, `standby` |
| `<base>/set/volume` | in | `0` to `99`, `up`, `down` |
| `<base>/set/mute` | in | `on`, `off`, `toggle` |
| `<base>/set/source` | in | a source name |
| `<base>/set/refresh` | in | any nonempty payload: read everything again |
| `<base>/set/restart` | in | any nonempty payload: restart in 1 s |

Publish commands without retention. On connection the bridge submits an empty
retained publish for each command topic before subscribing at QoS 0. If
client setup fails it retries. A publisher sending a step twice can still
move volume twice. Empty command payloads are ignored.

The state includes the last values reported by the amplifier. Unheard
amplifier fields are `null`. The ceiling comes from the bridge settings.

```json
{"power":"on","volume":59,"muted":false,"source":"digital_3","streamer":false,
 "model":3,"link":"answering","volume_max":60}
```

`link` is `answering`, `no_answer` or `waking`.

Diagnostics: `reply_ms` is the last reply's delay, `exchanges` the requests
sent since boot, polls included, and `losses` those that got no reply. Their
sensors expire after 180 s without a message.

```json
{"reply_ms":27,"exchanges":412,"losses":21}
```

Result, with `command` the key it came under:

```json
{"command":"volume","status":"ok","detail":""}
```

| `status` | Meaning |
|---|---|
| `ok` | the amplifier answered; `detail` says so if the answer differs from the request |
| `refused` | not sent: above the ceiling, unreadable, or the queue (4 deep) is full |
| `no_answer` | no reply, after the [retries](#retries) |
| `not_awake` | a power-on did not come up within 8 s |

## REST

No authentication, firmware updates included: anyone who reaches the
device can control the amplifier or replace the firmware. Keep it on a
trusted network.

A POST that a browser sends for a page on another site is refused with
403: its `Origin` header must be absent, or `http://` plus the `Host` the
device was reached by. curl, scripts and Home Assistant send no `Origin`.
The device's own page passes. This stops a web page from driving the device
through a browser on the network. It does not stop DNS rebinding; many
routers filter rebinding answers.

| Request | Does |
|---|---|
| `GET /` | the settings and status page |
| `GET /api/state` | the state JSON, with the diagnostics' fields |
| `POST /api/command` | one command, `{"<key>": <value>}` with a key and value as on the `set/` topics, except `restart`, which is `POST /api/restart`; waits up to 15 s for the result. HTTP 202 and `status: pending` mean the accepted command may still execute. Avoid resending it. Check the amplifier state or MQTT result for the outcome |
| `GET /api/settings` | `mqtt_host`, `mqtt_port`, `mqtt_user`, `mqtt_password_set`, `volume_max`, `poll`; never the password |
| `POST /api/settings` | writable fields are `mqtt_host`, `mqtt_port`, `mqtt_user`, `mqtt_password`, `volume_max` and `poll`. Accepts a flat JSON object or form. Invalid or duplicate fields reject the whole update (400). Changing broker host, port or user clears the previous password unless a new password accompanies the change. A blank form password keeps the password for the same broker identity. An empty JSON password clears it. The MQTT connection restarts only when the broker host, port, user or password changed |
| `POST /api/restart` | restarts in 1 s, after the answer |
| `POST /api/ota` | a firmware update: the body is the app image, the release's `arcam-ctl-app.bin` (not the merged image). It is written to the app slot that is not running and checked; then the answer is `{"status":"ok","detail":"restarting"}` and the device restarts into it in 1 s. 400 with `{"error": ...}`: the body is empty, larger than the slot, not an app image (its first byte is not `0xE9`), is another project's app (it lacks this firmware's mark) or does not verify, and the running image is left as it is. 409: another update is running. The new image confirms itself once it is on the network and serving HTTP; if it resets before that, the device boots the previous one |
| `GET /metrics` | [metrics](#metrics) for Prometheus |

```sh
# Substitute the address returned by Improv.
bridge=http://192.168.1.42
curl "$bridge/api/state"
curl -X POST "$bridge/api/command" -d '{"volume":40}'
curl -X POST "$bridge/api/settings" -d '{"mqtt_host":"192.168.1.10","mqtt_user":"arcam","mqtt_password":"secret"}'
curl -X POST "$bridge/api/ota" --data-binary @arcam-ctl-app.bin
```

## Metrics

`GET /metrics` serves the Prometheus text format 0.0.4 (`Content-Type:
text/plain; version=0.0.4`). An amplifier series appears once the amplifier
has reported its value.

| Series | Type | Meaning |
|---|---|---|
| `arcam_build_info{version, partition}` | gauge | 1; the firmware version and the app slot it runs from, `ota_0` or `ota_1` |
| `arcam_device_uptime_seconds` | gauge | time since boot |
| `arcam_device_heap_free_bytes`, `arcam_device_heap_min_free_bytes` | gauge | free heap now, and the least since boot |
| `arcam_device_wifi_rssi_dbm` | gauge | the access point's signal, while connected |
| `arcam_link_exchanges_total`, `arcam_link_losses_total` | counter | the diagnostics' `exchanges` and `losses` |
| `arcam_link_reply_seconds` | histogram | reply times; buckets 0.01, 0.025, 0.05, 0.1 s |
| `arcam_link_condition{condition}` | gauge | 1 for the link's condition, `answering`, `no_answer` or `waking`; all 0 before the first request ends |
| `arcam_poll_phase{phase}` | gauge | 1 for the [poll schedule's](#polling) phase: `off`, `waking`, `standby`, `window` or `quiet` |
| `arcam_amp_power_on`, `arcam_amp_muted` | gauge | 1 on, 1 muted |
| `arcam_amp_volume` | gauge | 0 to 99 |
| `arcam_amp_source{source}` | gauge | 1, with the input's name |
| `arcam_amp_streamer` | gauge | 1 when the input is the amplifier's Streamer Input |
| `arcam_amp_volume_max` | gauge | the volume ceiling setting |
| `arcam_amp_age_seconds{field}` | gauge | time since `power`, `volume`, `mute` or `source` was last reported |
| `arcam_mqtt_connected` | gauge | 1 while connected to the broker |
| `arcam_mqtt_connects_total`, `arcam_mqtt_disconnects_total` | counter | connections made, and lost or closed |
| `arcam_mqtt_published_total` | counter | messages handed to the MQTT client |
| `arcam_mqtt_commands_total` | counter | `set/` commands acted on (queued, or a restart) |

```yaml
scrape_configs:
  - job_name: arcam-radia
    static_configs:
      - targets: ["<device address>:80"]
```

An OpenTelemetry Collector scrapes it the same way, with its `prometheus`
receiver.

## Bridge behaviour

### Polling

The bridge hears the amplifier only when it asks ([why](hardware.md#electrical)),
so it asks on a schedule. The schedule is the `poll` setting, by default
`mode=standby_safe,tick=1000,standby=5000,window=300,quiet=1260`.

| Key | Unit | Meaning |
|---|---|---|
| `mode` | | `off`, `standby_safe` or `live` |
| `tick` | ms | the time between queries while polling fast |
| `standby` | ms | the power query interval in standby |
| `window` | s | how long fast polling lasts after a change |
| `quiet` | s | the full-round interval while on and quiet |

- `off`: queries only at boot, on Refresh and after a command.
- `standby_safe`:
  - in standby, query power every `standby` ms, so a power-on by remote is
    seen within seconds;
  - after any change, command or refresh, query once per `tick` for `window`
    seconds, rotating volume, power, volume, mute, volume, source: the knob is
    seen within 2 s and the rest within 6 s;
  - then, while on and quiet, one full round every `quiet` seconds.
- `live`: always fast. The amplifier then never auto-standbys.

`quiet` must exceed 20 minutes in `standby_safe`. Every frame restarts the
amplifier's auto-standby timer ([protocol](protocol.md#auto-standby)), and the
bridge cannot see whether music is playing, so it leaves gaps long enough for
the amplifier's own rule to fire. Playing music keeps the amplifier on by
itself.

### Retries

- A setter (standby, a volume, mute on or off, a source) without a reply is
  sent once more.
- A step or toggle (volume up or down, mute toggle) is never resent: twice
  could be two steps. The bridge reads the state instead and reports
  `no_answer`.
- After a power-on, the bridge queries power every 500 ms for up to 8 s until
  it reads on, then reads volume, mute and source (a wake clears the mute).
  A power-on without a reply is not resent into that silence; it is resent
  only if power then reads standby.
- After a volume change the bridge reads the mute (a volume request unmutes);
  after a source change, the volume and the mute (each input has its own).
  If that volume is above the ceiling, the bridge sets it to the ceiling,
  then mutes again if the amplifier was muted, and the result's `detail`
  says so.

### What is never sent

The firmware's only source of wire bytes is a command table without the DAC
filter (`61`, a persistent setting) and without the power toggle (its result
depends on a state the bridge may not know). A volume above the ceiling is
refused, and so is a step up when the last known volume is at or above it or
unknown.

### Known limitations

- Changes at the amplifier are seen at the next poll, not as they happen.
- The ceiling judges the last volume heard: after an unheard turn of the knob,
  one step can land above it. The next is refused. A level above the ceiling
  that was set at the amplifier stays until the next input change, or until
  someone lowers it.
- The device answers one HTTP request at a time. While `POST /api/command`
  waits for the amplifier, up to 15 s for a power-on, the page, the rest of
  the API and `/metrics` wait too.
- Pins are fixed in the firmware; another board may need a build
  ([hardware](hardware.md#wiring)).

## Examples

A dashboard card; replace `arcam_radia_a1b2c3` with your device's id.

```yaml
type: entities
title: Amplifier
entities:
  - entity: switch.arcam_radia_a1b2c3_power
  - entity: number.arcam_radia_a1b2c3_volume
  - entity: switch.arcam_radia_a1b2c3_mute
  - entity: select.arcam_radia_a1b2c3_source
  - type: buttons
    entities:
      - button.arcam_radia_a1b2c3_volume_down
      - button.arcam_radia_a1b2c3_volume_up
      - button.arcam_radia_a1b2c3_refresh
```

The amplifier follows the TV. A wake takes 4 to 8 s, so wait before the
source:

```yaml
automation:
  - alias: Amplifier follows the TV
    triggers:
      - trigger: state
        entity_id: media_player.tv
        to: "on"
    actions:
      - action: switch.turn_on
        target:
          entity_id: switch.arcam_radia_a1b2c3_power
      - delay: "00:00:08"
      - action: select.select_option
        target:
          entity_id: select.arcam_radia_a1b2c3_source
        data:
          option: digital_1
```
