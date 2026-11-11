#!/usr/bin/env bash
# Package the cross-linked EFL binary as an unsigned TPK.
#
# tv-authd (`rootcli install tpk`) re-signs it on-device, so no certificate
# material is needed here. Used by the UEP `/run` alias spike.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
BINARY="$SCRIPT_DIR/out/efl-popup-armv7-softfloat"
PKGROOT="$SCRIPT_DIR/out/pkgroot"
TPK="$SCRIPT_DIR/out/org.tvauth.spike.popup-0.1.0.tpk"

if [[ ! -f "$BINARY" ]]; then
    printf 'missing %s\nRun ./build-armv7-softfloat.sh first.\n' "$BINARY" >&2
    exit 1
fi

rm -rf "$PKGROOT"
mkdir -p "$PKGROOT/bin"
cp "$BINARY" "$PKGROOT/bin/efl-popup"
cp "$SCRIPT_DIR/tizen-manifest.xml" "$PKGROOT/tizen-manifest.xml"

rm -f "$TPK"
( cd "$PKGROOT" && zip -q -X -r "$TPK" tizen-manifest.xml bin )
ls -l "$TPK"
shasum -a 256 "$TPK"
