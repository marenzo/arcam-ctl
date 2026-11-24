#!/usr/bin/env bash
# The checks CI runs, for the host workspace (core/, emulator/), and the core
# built for bare metal. With --firmware, also the firmware's container build
# with clippy (firmware/build.sh; needs Docker).
set -euo pipefail

cd "$(dirname "$0")"

firmware=0
[ "${1:-}" = --firmware ] && firmware=1

echo "==> cargo fmt --check"
cargo fmt --all --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test"
cargo test --workspace

# The core is no_std: built for a target with no std, a use of it fails.
# rust-toolchain.toml lists the target; a toolchain installed before it did
# gets it here.
echo "==> cargo build, the core for bare metal"
bare=thumbv7em-none-eabihf
rustup target list --installed | grep -qx "$bare" || rustup target add "$bare"
cargo build -p arcam-ctl --target "$bare"

echo "==> cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

if command -v typos >/dev/null 2>&1; then
    echo "==> typos"
    typos
else
    echo "==> typos: not installed, skipped (cargo install typos-cli)"
fi

if [ "$firmware" = 1 ]; then
    echo "==> firmware: clippy and build, in a container"
    firmware/build.sh --clippy
fi

echo "All checks passed."
