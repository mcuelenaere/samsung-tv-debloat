#!/usr/bin/env bash
# Cross-link the Rust/EFL spike for the TV's soft-float ARMv7 ABI.
# The TV is not contacted by this script; the minimal sysroot must already
# exist under root-broker/sysroot/tv-6.5/.
set -euo pipefail

SPIKE_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_BROKER=$(cd -- "$SPIKE_DIR/../.." && pwd)
SYSROOT="$ROOT_BROKER/sysroot/tv-6.5"
IMAGE=rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a

for lib in \
    libcapi-appfw-application.so \
    libcapi-ui-efl-util.so \
    libefl-extension.so \
    libelementary.so \
    libevas.so; do
    if [[ ! -f "$SYSROOT/usr/lib/$lib" ]]; then
        printf 'missing %s\nRun ../../sysroot/fetch-tv-sysroot.sh first.\n' "$SYSROOT/usr/lib/$lib" >&2
        exit 1
    fi
done

docker run --rm --platform linux/amd64 \
    -v "$SPIKE_DIR:/src" \
    -v "$SYSROOT:/tv-sysroot:ro" \
    -w /src "$IMAGE" sh -eu -c '
        apt-get update >/dev/null
        apt-get install -y --no-install-recommends \
            gcc-arm-linux-gnueabi \
            libc6-dev-armel-cross \
            binutils-arm-linux-gnueabi \
            file >/dev/null

        rustup target add armv7-unknown-linux-gnueabi
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER=arm-linux-gnueabi-gcc
        # The TV provides the transitive EFL dependencies at runtime. Link
        # against the five direct libraries and let their SONAMEs remain.
        export RUSTFLAGS="-L native=/tv-sysroot/usr/lib -C link-arg=-Wl,--allow-shlib-undefined"

        cargo build --locked --release --target armv7-unknown-linux-gnueabi
        mkdir -p out
        cp target/armv7-unknown-linux-gnueabi/release/efl-popup \
            out/efl-popup-armv7-softfloat
        arm-linux-gnueabi-strip out/efl-popup-armv7-softfloat

        echo "== artifact =="
        file out/efl-popup-armv7-softfloat
        echo "== dynamic dependencies =="
        arm-linux-gnueabi-readelf -d out/efl-popup-armv7-softfloat \
            | grep -E "NEEDED|FLAGS"
        echo "== maximum GLIBC requirement =="
        arm-linux-gnueabi-readelf --version-info out/efl-popup-armv7-softfloat \
            | grep -oE "GLIBC_[0-9.]+" | sort -Vu | tail -1
        echo "== size =="
        wc -c out/efl-popup-armv7-softfloat
        sha256sum out/efl-popup-armv7-softfloat
    '
