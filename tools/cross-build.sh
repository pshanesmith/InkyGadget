#!/usr/bin/env bash
# Cross-build the Pi binary (ARMv6, glibc 2.31 or newer, hardware driver on) on a Mac or Linux machine.
#   tools/cross-build.sh            -> dist/inkyframe-armv6
# Needs: rustup (with the arm-unknown-linux-gnueabihf target), zig and cargo-zigbuild:
#   brew install zig cargo-zigbuild && rustup target add arm-unknown-linux-gnueabihf
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
TARGET=arm-unknown-linux-gnueabihf
cargo zigbuild --release --features hardware --target "$TARGET.2.31"
mkdir -p dist
cp "target/$TARGET/release/inkyframe" dist/inkyframe-armv6
ls -l dist/inkyframe-armv6
