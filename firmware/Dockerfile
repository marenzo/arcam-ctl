# Builds the firmware and writes the merged image (USB, at 0x0) and the app
# image (OTA). Run by firmware/build.sh:
#
#   docker build -f firmware/Dockerfile --output dist .
#
# The base is espressif's Rust image for the ESP32 (rustc 1.95 on the Xtensa
# fork, espup's GCC and clang, espflash, ldproxy). esp-idf-sys fetches
# ESP-IDF v6.1 and its tools on the first build; BuildKit cache mounts keep
# them, the cargo registry and the target directory between builds.
FROM espressif/idf-rust:esp32_1.95.0.0@sha256:1f90203f191cc94d7da1acf4da0e0e87aac128165115c1512f30668359f09dc6 AS build

# What ESP-IDF's own build needs beyond the base image.
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake python3-venv python3-pip pkg-config libusb-1.0-0 \
    && rm -rf /var/lib/apt/lists/*
USER esp

ENV ESP_IDF_TOOLS_INSTALL_DIR=custom:/home/esp/.espressif
# 1: also check the format and run clippy (the crate's lints, pedantic)
# before the build. Every warning is an error either way.
ARG CLIPPY=0
# 1: the QEMU image (the `qemu` feature, sdkconfig.qemu and qemu.toml), merged
# and padded to the 4 MB flash. It has its own target cache: its sdkconfig
# differs, and a shared one would rebuild ESP-IDF at every switch.
ARG QEMU=0

# The host workspace's manifest and members: the core inherits its fields.
COPY --chown=esp:esp Cargo.toml /src/Cargo.toml
COPY --chown=esp:esp core /src/core
COPY --chown=esp:esp emulator /src/emulator
COPY --chown=esp:esp firmware /src/firmware
# The wiring guide and its viewer, which the device serves at /wiring.
COPY --chown=esp:esp docs/wiring/guide.html /src/docs/wiring/guide.html
COPY --chown=esp:esp docs/wiring/three-r128 /src/docs/wiring/three-r128
COPY --chown=esp:esp LICENSE /src/LICENSE
COPY --chown=esp:esp docs/LICENSE /src/docs/LICENSE
WORKDIR /src/firmware

RUN --mount=type=cache,target=/home/esp/.espressif,uid=1000,gid=1000 \
    --mount=type=cache,target=/home/esp/.cargo/registry,uid=1000,gid=1000 \
    --mount=type=cache,id=firmware-target-$QEMU,target=/src/firmware/target,uid=1000,gid=1000 \
    bash -euo pipefail -c '\
      . /home/esp/export-esp.sh; \
      export RUSTFLAGS="--cfg espidf_time64 -D warnings"; \
      lock=; [ -f Cargo.lock ] && lock=--locked; \
      image=arcam-radia-esp32-control; pad=--skip-padding; \
      if [ "$QEMU" = 1 ]; then \
        lock="$lock --features qemu"; image=$image-qemu; pad=; \
        export ESP_IDF_SDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.qemu" \
          ESP_IDF_SYS_EXTRA_COMPONENTS_FILE=qemu.toml; \
      fi; \
      if [ "$CLIPPY" = 1 ]; then cargo fmt --check; cargo clippy --release $lock; fi; \
      cargo build --release $lock --message-format=json-render-diagnostics >/tmp/build.jsonl; \
      rel=target/xtensa-esp32-espidf/release; \
      mkdir -p /home/esp/out; \
      espflash save-image --chip esp32 --merge $pad \
        --flash-size 4mb --flash-mode dio --flash-freq 40mhz \
        --bootloader "$rel/bootloader.bin" --partition-table partitions.csv \
        "$rel/arcam-radia" /home/esp/out/$image.bin; \
      espflash save-image --chip esp32 --flash-size 4mb --flash-mode dio --flash-freq 40mhz \
        --partition-table partitions.csv "$rel/arcam-radia" /home/esp/out/$image-app.bin; \
      if [ "$QEMU" = 0 ]; then \
        cargo metadata --locked --filter-platform xtensa-esp32-espidf --format-version 1 >/tmp/metadata.json; \
        python3 /src/firmware/notices.py /tmp/build.jsonl /tmp/metadata.json /home/esp/out/firmware-notices.tar.gz; \
      fi; \
      cp Cargo.lock components_esp32.lock /home/esp/out/'

FROM scratch
COPY --from=build /home/esp/out/ /
