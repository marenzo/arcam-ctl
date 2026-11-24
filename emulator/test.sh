#!/usr/bin/env bash
# The emulator, end to end (emulator/README.md): the QEMU
# image (firmware/build.sh --qemu) against the virtual amplifier and
# Mosquitto (emulator/compose.yml), checked over MQTT and REST. Needs Docker,
# curl and python3. Exits non-zero if any check fails.
set -euo pipefail

cd "$(dirname "$0")/.."

image=dist/arcam-radia-esp32-control-qemu.bin
app=dist/arcam-radia-esp32-control-qemu-app.bin
for f in "$image" "$app"; do
    [ -f "$f" ] || { echo "no $f: run firmware/build.sh --qemu" >&2; exit 2; }
done
mkdir -p target
EMULATOR_FLASH=$(mktemp -d "$PWD/target/emulator-test.XXXXXX")
export EMULATOR_FLASH
export COMPOSE_PROJECT_NAME=arcam-radia-test-$$
cp "$image" "$EMULATOR_FLASH/flash.bin"

http=http://127.0.0.1:18080
failed=0 passed=0 results_pid= results= offline_id=

compose() { docker compose -f emulator/compose.yml "$@"; }
pass() { echo "PASS  $1"; passed=$((passed + 1)); }
fail() { echo "FAIL  $1${2:+: $2}"; failed=$((failed + 1)); }
check() { local name=$1; shift; if "$@"; then pass "$name"; else fail "$name"; fi; }

finish() {
    local code=$?
    [ -z "$results_pid" ] || kill "$results_pid" 2>/dev/null || true
    if [ "$failed" != 0 ] || [ "$code" != 0 ]; then
        compose logs --no-color --tail 60 qemu amp >&2 || true
    fi
    if [ -n "$offline_id" ]; then docker stop "$offline_id" >/dev/null 2>&1 || true; fi
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    echo "$passed passed, $failed failed"
    # Cleanup must not replace the round's own result.
    [ -z "$results" ] || rm -f "$results" || true
    rm -rf "$EMULATOR_FLASH" || echo "warning: could not remove $EMULATOR_FLASH" >&2
    if [ "$failed" != 0 ]; then exit 1; fi
    exit "$code"
}
trap finish EXIT

# Up to $1 seconds for the rest of the line to succeed.
within() {
    local end=$((SECONDS + $1))
    shift
    until "$@"; do
        [ "$SECONDS" -lt "$end" ] || return 1
        sleep 0.25
    done
}

# The virtual amplifier's control port: one line out, one line back.
amp() {
    local line
    exec 3<>/dev/tcp/127.0.0.1/15556
    echo "$*" >&3
    read -r line <&3
    exec 3<&-
    echo "$line"
}
amp_field() { amp state | tr ' ' '\n' | sed -n "s/^$1=//p"; }
# Every `key=value` given is in the amplifier's state.
amp_is() {
    local s kv
    s=" $(amp state) "
    for kv in "$@"; do [[ "$s" == *" $kv "* ]] || return 1; done
}

sub() { compose exec -T mosquitto mosquitto_sub "$@"; }
pub() { compose exec -T mosquitto mosquitto_pub "$@"; }
rest() { curl -fsS -m 5 "$http/api/$1"; }
answers() { rest state >/dev/null; }
rest_field() { rest state | python3 -I -c "import json, sys; print(json.load(sys.stdin)['$1'])"; }
online() { [ "$(sub -C 1 -W 3 -t "$base/availability")" = online ]; }

# A state JSON (REST or retained MQTT) says what the amplifier says.
state_matches() {
    python3 -I -c '
import json, sys
state = json.loads(sys.argv[1])
amp = dict(kv.split("=", 1) for kv in sys.argv[2].split())
want = {"power": amp["power"], "volume": int(amp["volume"]),
        "muted": amp["muted"] == "true", "source": amp["source"]}
got = {k: state.get(k) for k in want}
sys.exit(0 if got == want else f"    state {got}, amplifier {want}")' "$1" "$(amp state)"
}
retained_state_matches() { state_matches "$(sub -C 1 -W 3 -t "$base/state")"; }
rest_state_matches() { state_matches "$(rest state)"; }

# `discovery COMPONENT OBJECT`: its config is retained and well formed.
discovery() {
    local payload
    payload=$(grep "^homeassistant/$1/$id/$2/config " <<<"$configs" | cut -d' ' -f2-) || return 1
    python3 -I -c '
import json, sys
c = json.loads(sys.argv[1])
id, obj = sys.argv[2], sys.argv[3]
assert c["unique_id"] == f"{id}_{obj}", c["unique_id"]
assert c["availability_topic"] == f"arcam-radia/{id}/availability"
assert c["device"]["identifiers"] == [id]' "$payload" "$id" "$2"
}

# One command: `send mqtt|rest KEY VALUE` prints its result JSON.
send() {
    local via=$1 key=$2 value=$3 n
    if [ "$via" = mqtt ]; then
        n=$(wc -l <"$results")
        pub -t "$base/set/$key" -m "$value"
        within 20 more_than "$n" || true
        sed -n "$((n + 1))p" "$results"
    elif [[ "$value" =~ ^[0-9]+$ ]]; then
        curl -sS -m 20 -X POST "$http/api/command" -d "{\"$key\":$value}"
    else
        curl -sS -m 20 -X POST "$http/api/command" -d "{\"$key\":\"$value\"}"
    fi
}
more_than() { [ "$(wc -l <"$results")" -gt "$1" ]; }

# `step VIA KEY VALUE STATUS [key=value...]`: the command ends with STATUS,
# and the amplifier is then as given.
step() {
    local via=$1 key=$2 value=$3 status=$4 result
    shift 4
    result=$(send "$via" "$key" "$value")
    if [[ "$result" != *"\"status\":\"$status\""* ]]; then
        fail "$via: $key $value" "${result:-no result}"
    elif ! amp_is "$@"; then
        fail "$via: $key $value" "amplifier: $(amp state)"
    else
        pass "$via: $key $value -> $status${*:+, $*}"
    fi
}

# `rebooted BEFORE`: the device answers again, its exchange count restarted.
rebooted() { local n; n=$(rest_field exchanges 2>/dev/null) && [ "$n" -lt "$1" ]; }
settings_kept() {
    rest settings | grep -q "\"mqtt_host\":\"mosquitto\".*\"poll\":\"$poll\""
}
now_ms() { python3 -I -c 'import time; print(int(time.time() * 1000))'; }
volume_is() { rest state | grep -q "\"volume\":$1,"; }
quiet() { [ "$(amp_field idle_ms)" -ge 3000 ]; }

echo "==> the emulator: mosquitto, the virtual amplifier, QEMU"
compose build --quiet
compose up -d --quiet-pull
check "the device answers REST" within 60 answers
check "GET /wiring serves the wiring guide" grep -q "RJ12 CONTROL jack" <<<"$(curl --compressed -fsS -m 10 "$http/wiring")"

echo "==> settings over REST"
# A blank password in the UI preserves it for the same broker identity.
set_form() { curl -fsS -m 10 "$http/api/settings" -d "$1"; }
saved=$(set_form 'mqtt_user=first&mqtt_password=fixture')
check "a supplied broker password is saved" grep -q '"mqtt_password_set":true' <<<"$saved"
saved=$(set_form 'mqtt_user=first&mqtt_password=')
check "a blank form password keeps the existing login" grep -q '"mqtt_password_set":true' <<<"$saved"
saved=$(set_form 'mqtt_user=second&mqtt_password=')
check "a changed broker identity clears the old password" grep -q '"mqtt_password_set":false' <<<"$saved"
set_form 'mqtt_user=' >/dev/null
settings=$(curl -fsS -m 10 -X POST "$http/api/settings" -d '{"mqtt_host":"mosquitto"}')
check "POST /api/settings takes the broker" grep -q '"mqtt_host":"mosquitto"' <<<"$settings"
topic=$(sub -v -C 1 -W 30 -t 'arcam-radia/+/availability' | cut -d' ' -f1) ||
    { fail "the device connects to the broker"; exit 1; }
base=${topic%/availability}
id=${base#arcam-radia/}
echo "    device: $id"
check "availability goes online" within 15 online

echo "==> discovery"
configs=$(sub -v -W 3 -t "homeassistant/+/$id/+/config" || true)
for e in switch/power switch/mute number/volume select/source binary_sensor/streamer \
    button/refresh button/restart button/volume_up button/volume_down button/mute_toggle \
    sensor/link sensor/reply_time sensor/exchanges sensor/losses; do
    check "discovery $e" discovery "${e%/*}" "${e#*/}"
done
check "no other discovery config" [ "$(grep -c . <<<"$configs")" = 14 ]
check "the retained state matches the amplifier" within 10 retained_state_matches

# Every result, for the MQTT commands to wait on.
results=$(mktemp)
sub -t "$base/result" >"$results" &
results_pid=$!
disown
sleep 1

for via in mqtt rest; do
    echo "==> commands over $via"
    v=$(amp_field volume)
    step "$via" refresh 1 ok "volume=$v"
    step "$via" volume down ok "volume=$((v - 1))"
    step "$via" volume up ok "volume=$v"
    step "$via" volume 45 ok volume=45
    step "$via" volume 61 refused volume=45
    step "$via" mute on ok muted=true
    step "$via" mute off ok muted=false
    step "$via" mute toggle ok muted=true
    step "$via" volume 40 ok volume=40 muted=false
    step "$via" power standby ok power=standby
    step "$via" power on ok power=on muted=false
    step "$via" source analogue_1 ok source=analogue_1
    step "$via" source digital_3 ok source=digital_3
    check "$via: the state matches the amplifier" within 5 rest_state_matches
done
check "the retained state matches the amplifier" within 5 retained_state_matches

echo "==> the knob, through the poll schedule"
# The window's rotation reads the volume every other tick: 2 s, and a reply.
v=$(amp_field volume)
amp "knob +2" >/dev/null
start=$(now_ms)
until volume_is $((v + 2)) || [ $(($(now_ms) - start)) -gt 2500 ]; do sleep 0.1; done
took=$(($(now_ms) - start))
check "knob +2 is in the state in $took ms (2.5 s at most)" [ "$took" -le 2500 ]

echo "==> metrics"
metrics() { curl -fsS -m 5 "$http/metrics"; }
# The value of a series without labels.
metric() { metrics | sed -n "s/^$1 //p"; }
partition() { metrics | sed -n 's/^arcam_build_info{.*partition="\([^"]*\)"} 1$/\1/p'; }
kind=$(curl -sS -m 5 -o /dev/null -w '%{content_type}' "$http/metrics")
check "GET /metrics is the text format 0.0.4 ($kind)" [ "$kind" = "text/plain; version=0.0.4" ]
m=$(metrics)
check "/metrics has arcam_build_info" grep -q '^arcam_build_info{version="[^"]*",partition="ota_[01]"} 1$' <<<"$m"
check "/metrics counts the exchanges" grep -qE '^arcam_link_exchanges_total [1-9][0-9]*$' <<<"$m"
volume_metric() { [ "$(metric arcam_amp_volume)" = "$(amp_field volume)" ]; }
check "/metrics: arcam_amp_volume is the amplifier's" within 5 volume_metric
check "/metrics: arcam_mqtt_connected 1" grep -qx 'arcam_mqtt_connected 1' <<<"$m"

echo "==> edges"
code() { curl -sS -m 20 -o /dev/null -w '%{http_code}' -X POST "$http/api/$1" -d "$2"; }
curl -sS -m 20 -X POST "$http/api/command" -d '{"power":false}' >/dev/null
check "rest: power false is standby" amp_is power=standby
check "rest: power true is on" grep -q '"status":"ok"' <<<"$(curl -sS -m 20 -X POST "$http/api/command" -d '{"power":true}')"
check "a null setting is refused" [ "$(code settings '{"mqtt_password":null}')" = 400 ]
check "unquoted JSON text is refused" [ "$(code command '{"mute":on}')" = 400 ]
check "duplicate JSON keys are refused" [ "$(code settings '{"volume_max":55,"volume_max":50}')" = 400 ]
check "a malformed form field rejects the entire settings update" [ "$(code settings 'volume_max=55&mqtt_user=%ZZ')" = 400 ]
check "the prior ceiling is still in force" [ "$(rest_field volume_max)" = 60 ]
check "a body over 1024 bytes is refused" [ "$(code command "{\"refresh\":\"$(printf '%02000d' 0)\"}")" = 413 ]
# A browser names the sending page in Origin; only the device's own may act.
from() { curl -sS -m 20 -o /dev/null -w '%{http_code}' -H "Origin: $1" "$http/api/$2" -d "$3"; }
check "a cross-site settings POST is refused" [ "$(from http://attacker.example settings '{"volume_max":99}')" = 403 ]
check "a cross-site command is refused" [ "$(from http://attacker.example command '{"volume":61}')" = 403 ]
check "a null Origin is refused" [ "$(from null restart '')" = 403 ]
check "the ceiling is unchanged" [ "$(rest_field volume_max)" = 60 ]
check "a POST from the device's own page is accepted" [ "$(from "$http" command '{"refresh":true}')" = 200 ]

echo "==> an input's own volume above the ceiling"
# Set by hand to 70, muted, then another input: the bridge reads 70 after
# the change, sets the ceiling and mutes again.
step rest volume 40 ok volume=40
amp "knob +30" >/dev/null
step rest mute on ok volume=70 muted=true
step rest source analogue_2 ok source=analogue_2 volume=60 muted=true
v=$(amp_field volume)
n=$(wc -l <"$results")
pub -r -t "$base/set/volume" -m down
within 20 more_than "$n" || true
check "a retained volume step lands once" amp_is "volume=$((v - 1))"
curl -fsS -m 10 -X POST "$http/api/settings" -d '{"mqtt_port":1883}' >/dev/null
sleep 2
check "the device is back on the broker" within 15 online
sleep 2
check "the retained step is not delivered again" amp_is "volume=$((v - 1))"
check "the retained step is cleared" [ -z "$(sub -C 1 -W 2 -t "$base/set/volume" || true)" ]

echo "==> settings survive a reset"
poll=mode=standby_safe,tick=1000,standby=5000,window=10,quiet=1260
settings=$(curl -fsS -m 10 -X POST "$http/api/settings" -d '{"poll":"window=10"}')
check "a partial poll spec keeps the rest" grep -q "\"poll\":\"$poll\"" <<<"$settings"
n=$(rest_field exchanges)
exec 4<>/dev/tcp/127.0.0.1/14444
echo system_reset >&4
sleep 1
exec 4<&-
check "the device comes back after system_reset" within 30 rebooted "$n"
check "the broker and the poll spec are kept" settings_kept
step mqtt refresh 1 ok power=on
n=$(rest_field exchanges)
compose restart qemu >/dev/null 2>&1
check "the device comes back after QEMU restarts" within 30 rebooted "$n"
check "the settings are in the flash image" settings_kept

# The restart commands. Checked to take the device down, not to bring it
# back: in QEMU a software restart boot-loops when an interrupt of the
# emulated Ethernet is pending (nothing resets that device; a real ESP32
# resets its peripherals), so a fresh QEMU process brings it back instead.
down() { ! answers; }
again() {
    compose restart qemu >/dev/null 2>&1
    check "the device answers again in a fresh QEMU" within 30 answers
    check "the device is back on the broker" within 15 online
}
check "POST /api/restart answers" grep -q '"status":"ok"' <<<"$(curl -sS -m 10 -X POST "$http/api/restart")"
check "the device goes down after POST /api/restart" within 10 down
again
# Sent until it lands: just after a boot the device may not have
# subscribed yet, and a command at QoS 0 is not kept.
restart_by_mqtt() { pub -t "$base/set/restart" -m restart; sleep 2; down; }
check "the device goes down after set/restart" within 40 restart_by_mqtt
again

echo "==> OTA"
# `ota BODY`: POST /api/ota with curl's --data-binary BODY; prints the code.
ota() {
    curl -sS -m 120 -o /dev/null -w '%{http_code}' -X POST "$http/api/ota" \
        -H 'Content-Type: application/octet-stream' --data-binary "$1"
}
# The QEMU process writes its flash back into the file it booted from.
in_flash() {
    python3 -I -c '
import sys
flash, app = (open(p, "rb").read() for p in sys.argv[1:3])
at = int(sys.argv[3], 16)
sys.exit(flash[at:at + len(app)] != app)' "$EMULATOR_FLASH/flash.bin" "$app" "$1"
}
# The whole log first: `grep -q` would end the pipe early, and pipefail fail it.
confirmed() { [[ "$(compose logs --no-color qemu 2>/dev/null)" == *"ota: $1 confirmed"* ]]; }
was=$(partition)
check "an empty upload is refused" [ "$(ota "")" = 400 ]
check "an upload that is no app image is refused" [ "$(ota "$(printf '%04096d' 0)")" = 400 ]
cut=$(mktemp)
head -c 300000 "$app" >"$cut"
check "a cut app image is refused" [ "$(ota "@$cut")" = 400 ]
rm -f "$cut"
check "the device answers after the refused uploads" answers
check "it still runs $was" [ "$(partition)" = "$was" ]
# QEMU's warm reset can immediately reset the trial slot a second time
# before Ethernet initializes. Freeze the VM after the HTTP acceptance and
# cold-boot from its persisted flash. Restart routes are tested above.
upload_and_pause() {
    exec 4<>/dev/tcp/127.0.0.1/14444
    local code
    code=$(ota "@$app")
    echo stop >&4
    exec 4<&-
    [ "$code" = 200 ]
}
check "the QEMU app image is accepted" upload_and_pause
again
now=$(partition)
moved() { [ -n "$now" ] && [ "$now" != "$was" ]; }
check "it comes back on the other slot ($was, now ${now:-unknown})" moved
check "the new image confirms itself" within 20 confirmed "$now"
# A fresh image runs ota_0, so the update went to ota_1 (partitions.csv).
check "the new image is in the flash file at ota_1" in_flash 0x200000
again
check "after a fresh boot it stays on $now" [ "$(partition)" = "$now" ]

echo "==> rollback on a reset before the new image has an address"
# Freeze the CPU during the one-second grace period after the HTTP response.
# A separate QEMU process boots the new slot with no IPv4 DHCP, using the
# same flash. Interrupt it only after the bootloader has marked it pending.
check "the next update is accepted" upload_and_pause
compose stop qemu >/dev/null
offline_id=$(compose run -d --rm --no-deps qemu qemu-system-xtensa \
    -machine esp32 -display none -drive file=/flash/flash.bin,if=mtd,format=raw \
    -nic user,model=open_eth,ipv4=off -serial stdio -serial null -monitor none)
pending_boot() { [[ "$(docker logs "$offline_id" 2>&1)" == *"ota: running $was, new:"* ]]; }
if ! within 30 pending_boot; then
    docker logs --tail 40 "$offline_id" >&2 || true
    fail "the uploaded slot did not boot pending confirmation"
    exit 1
fi
pass "the uploaded slot boots pending confirmation"
docker stop "$offline_id" >/dev/null
offline_id=
again
check "reset before confirmation rolls back to $now" [ "$(partition)" = "$now" ]

echo "==> standby_safe, quiet: nothing restarts the auto-standby timer"
# The 10 s window after the boot's refresh closes; the next round is due
# 21 minutes on.
within 40 quiet || true
frames=$(amp_field frames)
sleep 10
check "no frame in 10 s of quiet (idle $(amp_field idle_ms) ms)" [ "$(amp_field frames)" = "$frames" ]
check "no 0x61 and no power toggle reached the amplifier" amp_is forbidden=0


# A silent UART makes each wake consume its full eight-second deadline.
# Three queued MQTT wakes hold the next REST command past its 15-second wait.
compose pause amp >/dev/null
printf 'on\non\non\n' | pub -l -t "$base/set/power"
queued=$(curl -sS -m 40 -w ' %{http_code}' "$http/api/command" -d '{"refresh":true}')
check "an accepted command still in the queue returns pending and HTTP 202" \
    grep -q '"status":"pending".* 202$' <<<"$queued"
compose unpause amp >/dev/null
