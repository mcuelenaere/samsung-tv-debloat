#!/usr/bin/env bash
# Build a static ARMv7 musl tv-authd using Docker/QEMU.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=rust:1.85.0-slim-bookworm@sha256:1829c432be4a592f3021501334d3fcca24f238432b13306a4e62669dec538e52
TARGET=armv7-unknown-linux-musleabihf

mkdir -p "$SCRIPT_DIR/out"
docker run --rm --platform linux/arm/v7 \
    -v "$SCRIPT_DIR:/src" -w /src \
    -e CARGO_TARGET_DIR=/src/target \
    "$IMAGE" sh -eu -c '
        rustup target add "$1"
        rustc --version
        cargo --version
        cargo build --locked --release --target "$1"
        cp "target/$1/release/tv-authd" out/tv-authd
        chmod 0755 out/tv-authd
    ' sh "$TARGET"

file "$SCRIPT_DIR/out/tv-authd"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tv-authd" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tv-authd"
docker run --rm --platform linux/arm/v7 \
    -v "$SCRIPT_DIR/out:/out:ro" alpine:3.20 /out/tv-authd --help >/dev/null
printf 'ARMv7 emulated --help: OK\n'
