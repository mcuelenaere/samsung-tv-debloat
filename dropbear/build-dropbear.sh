#!/bin/bash
# Build a statically linked ARMv7 dropbear multi-binary using Docker/QEMU.
# Output: ./out/dropbear (next to this script)
set -euo pipefail

VERSION=2026.94
TARBALL_SHA256=e098034a843699200c8c977a991fff73159735bf795d5f72ef672c41a6b1ae81
ALPINE_IMAGE=alpine:3.20
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
OUT_DIR=$SCRIPT_DIR/out
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dropbear-armv7.XXXXXX")
trap 'rm -rf "$WORK_DIR"' EXIT

TARBALL=$WORK_DIR/dropbear.tar.bz2
curl -fsSL \
    "https://matt.ucc.asn.au/dropbear/releases/dropbear-${VERSION}.tar.bz2" \
    -o "$TARBALL"
printf '%s  %s\n' "$TARBALL_SHA256" "$TARBALL" | shasum -a 256 -c -
tar -xjf "$TARBALL" -C "$WORK_DIR"

# - Alpine arm/v7 supplies an armv7-musl toolchain; Docker Desktop's QEMU
#   support runs it on Apple Silicon and other non-ARM hosts.
# - LDFLAGS=-static overrides dropbear's default -Wl,-pie, which fails when
#   combined with Alpine armv7's static CRT objects.
# - Build all programs into one multi-call binary. The TV launcher selects
#   dropbear/dropbearkey through argv[0] symlinks.
docker run --rm --platform linux/arm/v7 \
    -v "$WORK_DIR:/src" -w /src "$ALPINE_IMAGE" \
    sh -eu -c "
        apk add --no-cache gcc make musl-dev
        cd /src/dropbear-${VERSION}
        ./configure --disable-zlib
        make -j\"\$(nproc)\" \
            PROGRAMS='dropbear dropbearkey dbclient scp' \
            MULTI=1 STATIC=1 STRIP='' LDFLAGS='-static'
        strip dropbearmulti
    "

mkdir -p "$OUT_DIR"
cp "$WORK_DIR/dropbear-${VERSION}/dropbearmulti" "$OUT_DIR/dropbear"
chmod 755 "$OUT_DIR/dropbear"

file "$OUT_DIR/dropbear"
shasum -a 256 "$OUT_DIR/dropbear"
echo "Built $OUT_DIR/dropbear"
