#!/usr/bin/env bash
# Build the firmware in a container and write to dist/:
#   arcam-radia-esp32-control.bin      the merged image (bootloader, partition
#                                      table, app): flash it at 0x0 over USB
#   arcam-radia-esp32-control-app.bin  the app image alone, for OTA
# Needs only Docker with BuildKit.
#
#   firmware/build.sh            # build
#   firmware/build.sh --clippy   # clippy first, then build
#   firmware/build.sh --qemu     # the QEMU images instead (emulator/README.md):
#                                # arcam-radia-esp32-control-qemu.bin, 4 MB,
#                                # and arcam-radia-esp32-control-qemu-app.bin
set -euo pipefail

cd "$(dirname "$0")/.."

clippy=0 qemu=0
for arg in "$@"; do
    case "$arg" in
        --clippy) clippy=1 ;;
        --qemu) qemu=1 ;;
        *) echo "usage: $0 [--clippy] [--qemu]" >&2; exit 2 ;;
    esac
done

docker build -f firmware/Dockerfile --build-arg CLIPPY="$clippy" --build-arg QEMU="$qemu" \
    --output type=local,dest=dist .

# The lock files the build resolved (crates, ESP-IDF components), kept with
# the source. The QEMU build's components differ: its lock is not kept.
for lock in Cargo.lock components_esp32.lock; do
    [ "$qemu" = 1 ] || [ -f "firmware/$lock" ] || cp "dist/$lock" "firmware/$lock"
    rm -f "dist/$lock"
done

image=dist/arcam-radia-esp32-control
[ "$qemu" = 1 ] && image=$image-qemu
app=$(wc -c < "$image-app.bin" | tr -d " ")
merged=$(wc -c < "$image.bin" | tr -d " ")
slot=$((0x1F0000))  # ota_0 and ota_1, partitions.csv
nvs=$((0x3F0000))   # where nvs begins: the merged image must end before it
[ "$app" -le "$slot" ] || { echo "the app does not fit its slot ($app bytes)" >&2; exit 1; }
if [ "$qemu" = 1 ]; then
    [ "$merged" = $((0x400000)) ] || { echo "the QEMU image is not 4 MB ($merged bytes)" >&2; exit 1; }
else
    [ "$merged" -lt "$nvs" ] || { echo "the image reaches nvs ($merged bytes)" >&2; exit 1; }
fi

echo "app: $app bytes, $((app * 1000 / slot / 10)).$((app * 1000 / slot % 10)) % of the $slot-byte slot"
echo "image: $image.bin, $merged bytes; app: $image-app.bin"
