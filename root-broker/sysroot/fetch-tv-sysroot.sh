#!/usr/bin/env bash
# Copy the directly referenced TV libraries into an ignored local sysroot.
# This performs only reads over SSH; it writes only beneath this repository.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
DEST="$SCRIPT_DIR/tv-6.5/usr/lib"
HOST=${TV_HOST:-root@192.0.2.10}
EXPECTED_TARGET='__TIZEN-TRUNK2022-PontusM-RELEASE_20260225.1__T-PTMDEUC'

mkdir -p "$DEST"

actual_target=$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" 'cat /proc/target_version')
if [[ "$actual_target" != "$EXPECTED_TARGET" ]]; then
    printf 'unexpected TV target: %s\nexpected: %s\n' "$actual_target" "$EXPECTED_TARGET" >&2
    exit 1
fi

copy_as() {
    local remote=$1
    local local_name=$2
    local expected_sha256=$3
    local temporary="$DEST/.$local_name.tmp"
    ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "cat '$remote'" >"$temporary"
    mv "$temporary" "$DEST/$local_name"
    chmod 0644 "$DEST/$local_name"
    printf '%s  %s\n' "$expected_sha256" "$DEST/$local_name" | shasum -a 256 -c -
}

# Rust's -l<name> lookup needs unversioned local names. Each file retains its
# original SONAME; transitive dependencies remain the TV runtime's concern.
copy_as /usr/lib/libcapi-appfw-application.so.0.5.27 libcapi-appfw-application.so \
    fe033c6b8eaf2db51a0d8f3514c56b61d6df8339613b74b798c64576b5af8fc3
copy_as /usr/lib/libcapi-ui-efl-util.so.0.7.7 libcapi-ui-efl-util.so \
    f48a2b6bd1d20ec59d4b19a73a6fffa0581c85616eb27c7190622f216905fe3f
copy_as /usr/lib/libefl-extension.so.0.1.0 libefl-extension.so \
    77965ee57ca55f08b7a216bb8383aec485954d8e7e24d468094181878ec16493
copy_as /usr/lib/libelementary.so.1.25.1 libelementary.so \
    4cc002554bf81679f726c48d3e07a9cfa5c6d3880226d5988125b5ebe737d722
copy_as /usr/lib/libevas.so.1.25.1 libevas.so \
    c03f352e860b78fed4fcd2690003d4db96cf77a412d3df98f239ba2c545d2585
# Ecore's timer integrates the broker poll with the EFL main loop;
# ecore_wl2 selects the Wayland notification window type.
copy_as /usr/lib/libecore.so.1.25.1 libecore.so \
    49727cc58a8f15ab472ae84ef14700a5fb4741099dd4fddb12e32e8f9c3c8758
copy_as /usr/lib/libecore_wl2.so.1.25.1 libecore_wl2.so \
    304b5ca9f60c6b7b0d27b36a5c0b26199e600b4087ee49128d2152ff693954f2

printf 'Collected minimal sysroot from %s (%s)\n' "$HOST" "$actual_target"
find "$DEST" -maxdepth 1 -type f -print0 | sort -z | xargs -0 shasum -a 256
