#!/usr/bin/env bash
# Build the native Tizen `type="capp"` approval and grant-manager UI.
#
# The executable is cross-linked against the TV's soft-float ARMv7 EFL stack.
# It is packaged unsigned; tv-authd re-signs it on-device when installed with
# `rootcli install tpk`.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_BROKER=$(cd -- "$SCRIPT_DIR/.." && pwd)
SYSROOT="$ROOT_BROKER/sysroot/tv-6.5"
IMAGE=rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a
OUTPUT="$SCRIPT_DIR/out/tv-auth-ui"
TPK="$OUTPUT/org.tvauth.ui-0.1.0.tpk"

for lib in \
    libcapi-appfw-application.so \
    libcapi-ui-efl-util.so \
    libefl-extension.so \
    libelementary.so \
    libevas.so \
    libecore.so \
    libecore_wl2.so; do
    if [[ ! -f "$SYSROOT/usr/lib/$lib" ]]; then
        printf 'missing %s\nRun ../sysroot/fetch-tv-sysroot.sh first.\n' "$SYSROOT/usr/lib/$lib" >&2
        exit 1
    fi
done

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" \
    -v "$SYSROOT:/tv-sysroot:ro" \
    -w /src/rust "$IMAGE" sh -eu -c '
        apt-get update >/dev/null
        apt-get install -y --no-install-recommends \
            gcc-arm-linux-gnueabi \
            libc6-dev-armel-cross \
            binutils-arm-linux-gnueabi \
            file zip >/dev/null

        rustup target add armv7-unknown-linux-gnueabi
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER=arm-linux-gnueabi-gcc
        # The TV provides the transitive EFL dependencies at runtime. Link
        # against the seven direct libraries and let their SONAMEs remain.
        export RUSTFLAGS="-L native=/tv-sysroot/usr/lib -C link-arg=-Wl,--allow-shlib-undefined"

        rm -rf /src/out/tv-auth-ui
        cargo build --locked --release --target armv7-unknown-linux-gnueabi

        mkdir -p /src/out/tv-auth-ui/pkgroot/bin /src/out/tv-auth-ui/pkgroot/shared/res
        cp target/armv7-unknown-linux-gnueabi/release/tv-auth-ui \
            /src/out/tv-auth-ui/pkgroot/bin/tv-auth-ui
        arm-linux-gnueabi-strip /src/out/tv-auth-ui/pkgroot/bin/tv-auth-ui
        cp /src/rust/tizen-manifest.xml \
            /src/out/tv-auth-ui/pkgroot/tizen-manifest.xml
        cp /src/rust/icon.png \
            /src/out/tv-auth-ui/pkgroot/shared/res/org.tvauth.ui.png

        echo "== artifact =="
        file /src/out/tv-auth-ui/pkgroot/bin/tv-auth-ui
        arm-linux-gnueabi-readelf -d /src/out/tv-auth-ui/pkgroot/bin/tv-auth-ui \
            | grep -E "NEEDED|FLAGS"
        wc -c /src/out/tv-auth-ui/pkgroot/bin/tv-auth-ui
    '

rm -f "$TPK"
( cd "$OUTPUT/pkgroot" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$OUTPUT/pkgroot/bin" "$TPK"
shasum -a 256 "$TPK"
