#!/usr/bin/env bash
# Build and assemble the two unsigned packaged peer-identity probes.
# Signing/installation are separate, explicitly authorized device operations.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SPIKE_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
ROOT_BROKER=$(cd -- "$SPIKE_DIR/../.." && pwd)
SYSROOT="$ROOT_BROKER/../sysroot/tv-6.5"
IMAGE=rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a

if [[ ! -f "$SYSROOT/usr/lib/libcapi-appfw-application.so" ]]; then
    printf 'missing TV sysroot; run ../../../sysroot/fetch-tv-sysroot.sh first\n' >&2
    exit 1
fi

docker run --rm --platform linux/amd64 \
    -v "$SPIKE_DIR:/src" \
    -v "$SYSROOT:/tv-sysroot:ro" \
    -w /src/apps/peer-client-app "$IMAGE" sh -eu -c '
        apt-get update >/dev/null
        apt-get install -y --no-install-recommends \
            gcc-arm-linux-gnueabi \
            libc6-dev-armel-cross \
            binutils-arm-linux-gnueabi \
            file zip >/dev/null

        rustup target add armv7-unknown-linux-gnueabi
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER=arm-linux-gnueabi-gcc
        export RUSTFLAGS="-L native=/tv-sysroot/usr/lib -C link-arg=-Wl,--allow-shlib-undefined"

        for item in peerone:peer-one peertwo:peer-two; do
            package=${item%%:*}
            probe=${item##*:}
            target_dir="/src/apps/peer-client-app/target/$package"
            PEER_PROBE="$probe" CARGO_TARGET_DIR="$target_dir" \
                cargo build --release --target armv7-unknown-linux-gnueabi

            pkgroot="/src/apps/out/$package/pkgroot"
            rm -rf "/src/apps/out/$package"
            mkdir -p "$pkgroot/bin"
            cp "$target_dir/armv7-unknown-linux-gnueabi/release/peer-client-app" \
                "$pkgroot/bin/peer-client-app"
            arm-linux-gnueabi-strip "$pkgroot/bin/peer-client-app"
            cp "/src/apps/$probe-tizen-manifest.xml" "$pkgroot/tizen-manifest.xml"
            mkdir -p "$pkgroot/shared/res"
            cp "/src/apps/peer-icon.png" \
                "$pkgroot/shared/res/org.tvauth.spike.$package.png"
            chmod 0755 "$pkgroot/bin/peer-client-app"

            ( cd "$pkgroot" && zip -q -X -r "../org.tvauth.spike.$package-0.1.3.tpk" tizen-manifest.xml bin shared )
            file "$pkgroot/bin/peer-client-app"
            wc -c "$pkgroot/bin/peer-client-app"
        done
    '
