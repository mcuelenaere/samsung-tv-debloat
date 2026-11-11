#!/usr/bin/env bash
# Assemble the reproducible setup bundle and every suite component package.
#
# Source artifacts are produced by the existing cross-build scripts plus the
# separately implemented tv-pkg build. This script performs no network access
# and does not contact a TV.
#
#   rooting/build-suite-bundle.sh [--output DIR]
#
# Override artifact locations with:
#   TV_PKG_BIN, FDET_MODERN_DIR, FDET_LEGACY_DIR, CROSS_TARGET
set -euo pipefail

HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd -- "$HERE/.." && pwd)
OUTPUT="$HERE/out/setup-bundle"
if [ "${1:-}" = --output ]; then
    OUTPUT=${2:?--output needs a directory}
fi
WORK=$(mktemp -d "${TMPDIR:-/tmp}/tv-bundle.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
SRC=$WORK/source
BUNDLE=$WORK/bundle
mkdir -p "$SRC" "$BUNDLE/packages" "$BUNDLE/fdet/modern" "$BUNDLE/fdet/legacy" \
    "$BUNDLE/persistence/units"

TV_PKG_BIN=${TV_PKG_BIN:-$REPO/bins/tv-pkg/out/tv-pkg}
FDET_MODERN_DIR=${FDET_MODERN_DIR:-$HERE/payloads/fdet/out/modern}
FDET_LEGACY_DIR=${FDET_LEGACY_DIR:-$HERE/payloads/fdet/out/legacy}

need() {
    [ -f "$1" ] || { echo "build-suite-bundle: missing $1" >&2; exit 1; }
}

need "$TV_PKG_BIN"
need "$FDET_MODERN_DIR/FdetProbe.dll"
need "$FDET_MODERN_DIR/FdetProbe.runtimeconfig.json"
need "$FDET_LEGACY_DIR/FdetProbe.dll"
need "$FDET_LEGACY_DIR/FdetProbe.runtimeconfig.json"
need "$REPO/third_party/dropbear/out/dropbear"
need "$REPO/bins/tv-authd/out/tv-authd"
need "$REPO/bins/tv-auth-cli/out/tvauth-cli"
need "$REPO/bins/tv-auth-ui/out/org.tvauth.ui-0.1.0.tpk"
need "$REPO/bins/tv-auth-popupd/out/tv-auth-popupd"
need "$REPO/bins/tv-blockd/out/tv-blockd"
need "$REPO/bins/tv-patchd/out/tv-patchd"

install -m 0755 "$REPO/third_party/dropbear/out/dropbear" "$SRC/tv-dropbear"
install -m 0755 "$TV_PKG_BIN" "$SRC/tv-pkg"
install -m 0755 "$REPO/bins/tv-authd/out/tv-authd" "$SRC/tv-authd"
install -m 0755 "$REPO/bins/tv-auth-cli/out/tvauth-cli" "$SRC/tvauth-cli"
install -m 0755 "$REPO/bins/tv-auth-popupd/out/tv-auth-popupd" "$SRC/tv-auth-popupd"
install -m 0755 "$REPO/bins/tv-blockd/out/tv-blockd" "$SRC/tv-blockd"
install -m 0755 "$REPO/bins/tv-patchd/out/tv-patchd" "$SRC/tv-patchd"
install -m 0644 "$REPO/bins/tv-blockd/systemd/tv-blockd.service" "$SRC/tv-blockd.service"
install -m 0644 "$REPO/bins/tv-patchd/systemd/tv-patchd.service" "$SRC/tv-patchd.service"
install -m 0644 "$REPO/bins/tv-authd/systemd/tv-authd.service" "$SRC/tv-authd.service"
install -m 0644 "$REPO/bins/tv-auth-popupd/systemd/tv-auth-popupd.service" "$SRC/tv-auth-popupd.service"
install -m 0644 "$HERE/payloads/persistence/dropbear.service" "$SRC/dropbear.service"
[ -f "$REPO/bins/tv-blockd/etc/blockd.json" ] &&
    install -m 0644 "$REPO/bins/tv-blockd/etc/blockd.json" "$SRC/blockd.json" || printf "{}\n" > "$SRC/blockd.json"
[ -f "$REPO/bins/tv-patchd/etc/patchd.json" ] &&
    install -m 0644 "$REPO/bins/tv-patchd/etc/patchd.json" "$SRC/patchd.json" || printf "{}\n" > "$SRC/patchd.json"
install -m 0644 "$REPO/bins/tv-auth-ui/out/org.tvauth.ui-0.1.0.tpk" "$SRC/org.tvauth.ui-0.1.0.tpk"

cargo build --manifest-path "$REPO/Cargo.toml" -p tv-bundle >/dev/null
BUNDLER=$REPO/target/debug/tv-bundle

for spec in "$HERE"/packages/*.json; do
    id=$(sed -n 's/^  "id": "\([^"]*\)",$/\1/p' "$spec" | head -n 1)
    [ -n "$id" ] || { echo "build-suite-bundle: no id in $spec" >&2; exit 1; }
    "$BUNDLER" package "$spec" --source "$SRC" --output "$BUNDLE/packages/$id.tvpkg"
done

install -m 0755 "$FDET_MODERN_DIR/FdetProbe.dll" "$BUNDLE/fdet/modern/FdetProbe.dll"
install -m 0644 "$FDET_MODERN_DIR/FdetProbe.runtimeconfig.json" \
    "$BUNDLE/fdet/modern/FdetProbe.runtimeconfig.json"
install -m 0755 "$FDET_LEGACY_DIR/FdetProbe.dll" "$BUNDLE/fdet/legacy/FdetProbe.dll"
install -m 0644 "$FDET_LEGACY_DIR/FdetProbe.runtimeconfig.json" \
    "$BUNDLE/fdet/legacy/FdetProbe.runtimeconfig.json"
install -m 0755 "$TV_PKG_BIN" "$BUNDLE/tv-pkg"
install -m 0755 "$HERE/payloads/setup/setup-driver.sh" "$BUNDLE/setup-driver.sh"
install -m 0644 "$HERE/payloads/persistence/parseinfo.sh" "$BUNDLE/persistence/parseinfo.sh"
install -m 0755 "$HERE/payloads/persistence/tv-apply.sh" "$BUNDLE/persistence/tv-apply.sh"
for unit in dropbear.service tv-authd.service tv-auth-popupd.service tv-blockd.service tv-patchd.service; do
    install -m 0644 "$SRC/$unit" "$BUNDLE/persistence/units/$unit"
done

# The target build is intentionally explicit; compatibility is checked on the TV.
TARGET_BUILD=${TARGET_BUILD:?set TARGET_BUILD to the exact reviewed build}
"$BUNDLER" setup --source "$BUNDLE" --target-build "$TARGET_BUILD" \
    --order "$HERE/packages/package-order.txt" \
    --output "$BUNDLE/setup-manifest.json"

rm -rf "$OUTPUT"
mkdir -p "$(dirname "$OUTPUT")"
cp -R "$BUNDLE" "$OUTPUT"
echo "setup bundle: $OUTPUT"
sha256sum "$OUTPUT/setup-manifest.json"
