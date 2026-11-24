# The emulator's image (emulator/README.md): Espressif's
# QEMU for the ESP32 and the virtual amplifier. Built by emulator/compose.yml.
FROM rust:1.100.0-slim-trixie@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS amp
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY emulator emulator
RUN cargo build --release --locked -p arcam-radia-emulator

FROM debian:trixie-20261020-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
ARG TARGETARCH
# espressif/qemu esp-develop-9.2.2-20260417, checked against its release's
# checksum file.
ARG QEMU_URL=https://github.com/espressif/qemu/releases/download/esp-develop-9.2.2-20260417/qemu-xtensa-softmmu-esp_develop_9.2.2_20260417
ARG QEMU_SHA256_amd64=0eecb2a34a5586c0e59110f77b9343b7b336e82fdb0e1a30e1dc1bab8a547e35
ARG QEMU_SHA256_arm64=00de5985094c14e47d1b38464a006b8ed64fd0fa7a289c56da24f3a329f65339
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
       libgcrypt20 libglib2.0-0t64 libpixman-1-0 libsdl2-2.0-0 libslirp0 \
    && rm -rf /var/lib/apt/lists/* \
    && case "$TARGETARCH" in amd64) arch=x86_64; sum=$QEMU_SHA256_amd64 ;; \
         arm64) arch=aarch64; sum=$QEMU_SHA256_arm64 ;; *) exit 1 ;; esac \
    && curl -fsSL -o /tmp/qemu.tar.xz "$QEMU_URL-$arch-linux-gnu.tar.xz" \
    && echo "$sum  /tmp/qemu.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/qemu.tar.xz -C /opt && rm /tmp/qemu.tar.xz \
    && /opt/qemu/bin/qemu-system-xtensa --version
ENV PATH=/opt/qemu/bin:$PATH
COPY --from=amp /src/target/release/virtual-amp /usr/local/bin/
