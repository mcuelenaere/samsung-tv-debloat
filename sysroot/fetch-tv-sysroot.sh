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
# tv-tizen: vconf keys, the read-only pkgmgr database, app launch, and HTTP
# through the TV's own TLS stack and CA bundle.
copy_as /usr/lib/libvconf.so.0.3.1 libvconf.so \
    5222e126ad2384f25b486f7a73bd7772bc5d6122003c34509659f1a5a759ea06
copy_as /usr/lib/libsqlite3.so.0.8.6 libsqlite3.so \
    8f174af876be98628d29f752857c1fa7a30bb5af8b10707d6e36723a032995bd
copy_as /usr/lib/libcapi-appfw-app-control.so.0.15.3 libcapi-appfw-app-control.so \
    00891ff0f67c92ff42b3feb6dd9fffb642d4713fd1f5e181ab0a0f78d40cb344
copy_as /usr/lib/libaul.so.0.43.16 libaul.so \
    3f6e0f15f125ffee8750914b6d795275fb56824f9017bb22799498feebf71f7a
copy_as /usr/lib/libcurl.so.4.7.0 libcurl.so \
    6f3dd180cbc10c1a9753bd838eb47b51c04d9ffe76a889b597c8f3f0e075c5c3
copy_as /usr/lib/libcrypto.so.1.1 libcrypto.so \
    d763b8173e8aacf7e78fee2e20265cd8f199a8eed1522c0e7100324b6fb818a4
# tv-launcher: app status events and termination; SVG source icons.
copy_as /usr/lib/libcapi-appfw-app-manager.so.0.12.1 libcapi-appfw-app-manager.so \
    4526b5a9d8f51c30e518e65b52d7ec8e7d5c8a9b6450813dc7ca574e66887cf7
copy_as /usr/lib/libthorvg.so.0.6.99 libthorvg.so \
    4d390a02c047e9429308b44fe8ff785e89085ec0bfa9d44571337fd0f0a63483
# tv-patchd: TLS for the YouTube proxy.
copy_as /usr/lib/libssl.so.1.1 libssl.so \
    ffda6cbac6b90ac18c88e0516b36eb0648d7243c63b43818b3a68e958b32bee6

printf 'Collected minimal sysroot from %s (%s)\n' "$HOST" "$actual_target"
find "$DEST" -maxdepth 1 -type f -print0 | sort -z | xargs -0 shasum -a 256
