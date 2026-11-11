#!/usr/bin/env bash
# Build the native Tizen `type="capp"` approval and grant-manager UI.
#
# The executable is cross-linked against the TV's soft-float ARMv7 EFL stack
# (see ../../cross-build.sh). It is packaged unsigned; tv-authd re-signs it
# on-device when installed with `tvauth-cli install tpk`.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
OUTPUT="$SCRIPT_DIR/out"
TPK="$OUTPUT/org.tvauth.ui-0.1.0.tpk"

"$ROOT/toolchain/cross-build.sh" gnueabi build --locked --release -p tv-auth-ui

rm -rf "$OUTPUT/pkgroot"
mkdir -p "$OUTPUT/pkgroot/bin" "$OUTPUT/pkgroot/shared/res"
install -m 0755 "$ROOT/target/cross-gnueabi/armv7-unknown-linux-gnueabi/release/tv-auth-ui" \
    "$OUTPUT/pkgroot/bin/tv-auth-ui"
cp "$SCRIPT_DIR/tizen-manifest.xml" "$OUTPUT/pkgroot/tizen-manifest.xml"
cp "$SCRIPT_DIR/icon.png" "$OUTPUT/pkgroot/shared/res/org.tvauth.ui.png"
file "$OUTPUT/pkgroot/bin/tv-auth-ui"

rm -f "$TPK"
( cd "$OUTPUT/pkgroot" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$OUTPUT/pkgroot/bin" "$TPK"
shasum -a 256 "$TPK"
