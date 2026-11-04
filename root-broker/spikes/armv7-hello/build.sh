#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
IMAGE=rust:1.85.0-slim-bookworm@sha256:1829c432be4a592f3021501334d3fcca24f238432b13306a4e62669dec538e52
TARGET=armv7-unknown-linux-musleabihf

docker run --rm --platform linux/arm/v7 \
    -v "$SCRIPT_DIR:/src" -w /src \
    -e CARGO_TARGET_DIR=/src/target \
    "$IMAGE" sh -eu -c '
        rustup target add "$1"
        rustc --version
        cargo --version
        cargo build --locked --release --target "$1"
        mkdir -p out
        cp "target/$1/release/armv7-hello" out/armv7-hello
        chmod 755 out/armv7-hello
    ' sh "$TARGET"

file "$SCRIPT_DIR/out/armv7-hello"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/armv7-hello" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/armv7-hello"
