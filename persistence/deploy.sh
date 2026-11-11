#!/bin/bash
# Guarded deployment and rollback for the S95B AT-HUB persistence hook.
#
# The TV must be reachable as root via SSH. Required host commands include
# ssh, scp with -O support (OpenSSH >= 9), sha256sum, and ordinary POSIX tools.
# Set TV_HOST explicitly so a typo cannot deploy to another machine:
#
#   TV_HOST=root@tv-living.home ./persistence/deploy.sh check [PRISTINE_HOOK]
#   TV_HOST=root@tv-living.home ./persistence/deploy.sh install [PRISTINE_HOOK]
#   TV_HOST=root@tv-living.home ./persistence/deploy.sh rollback SNAPSHOT
#   TV_HOST=root@tv-living.home ./persistence/deploy.sh disable
#   TV_HOST=root@tv-living.home ./persistence/deploy.sh enable
#
# PRISTINE_HOOK is an operator-held, byte-exact copy of the vendor
# /opt/etc/parseinfo.sh. It is required for an upgrade where the live hook is
# no longer pristine. Installation also writes a private off-device snapshot
# containing the previous hook and all five previous unit files, by default
# under ${XDG_STATE_HOME:-$HOME/.local/state}/neon-lagoon/persistence-backups.
set -euo pipefail

TV_HOST=${TV_HOST:-}
PRISTINE_HOOK=${PRISTINE_HOOK:-}
KEEP_REMOTE_STAGE=${KEEP_REMOTE_STAGE:-0}
PERSISTENCE_BACKUP_ROOT=${PERSISTENCE_BACKUP_ROOT:-}

BUILD=T-PTMDEUC-REL-202602250226
PRISTINE_SIZE=805
PRISTINE_HOOK_SHA=c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61
PREVIOUS_HOOK_SHA=27fd5e7abd20a1178a2f395fa6d8c5ae8e241a0bc57fb58f75315c66ea969b94
HOOK=/opt/etc/parseinfo.sh
UNIT_DIR=/opt/tv-units
REMOTE_STAGE_ROOT=/opt/tv-deploy

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
APPLY_SCRIPT=$SCRIPT_DIR/tv-apply.sh
HOOK_SOURCE=$SCRIPT_DIR/parseinfo.sh
if [ -z "$PERSISTENCE_BACKUP_ROOT" ]; then
    PERSISTENCE_BACKUP_ROOT=${XDG_STATE_HOME:-$HOME/.local/state}/neon-lagoon/persistence-backups
fi
BACKUP_ROOT=$PERSISTENCE_BACKUP_ROOT

UNIT_NAMES=(dropbear.service tv-authd.service tv-auth-popupd.service tv-blockd.service tv-patchd.service)
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

action=${1:-}
if [ -n "$action" ]; then
    shift
fi

usage() {
    cat >&2 <<EOF
usage: TV_HOST=root@HOST $0 <command> [argument]

commands:
  check [PRISTINE_HOOK]       inspect local sources and live TV; no changes
  install [PRISTINE_HOOK]     snapshot, stage, verify and replace hook + units
  rollback SNAPSHOT           restore a snapshot created by install
  disable                     create the root-only /opt/tv-units/disabled file
  enable                      remove the stop file

PRISTINE_HOOK may also be supplied through the environment. An upgrade needs
an operator-held pristine 805-byte parseinfo.sh with SHA-256:
  $PRISTINE_HOOK_SHA

Rollback restores files for the next boot. It never stops Dropbear or the other
services over the current SSH session; reboot to leave the old services off.
EOF
}

die() {
    echo "deploy: $*" >&2
    exit 1
}

need_tv_host() {
    [ -n "$TV_HOST" ] || die "set TV_HOST=root@your-tv (refusing to guess a target)"
}

need_host_tools() {
    for tool in ssh scp sha256sum sed date cut wc tr head mkdir chmod cp rm; do
        command -v "$tool" >/dev/null 2>&1 || die "missing host tool: $tool"
    done
}

sha256_file() {
    sha256sum "$1" | cut -d ' ' -f 1
}

unit_source() {
    case "$1" in
        dropbear.service) echo "$SCRIPT_DIR/dropbear.service" ;;
        tv-authd.service) echo "$REPO_ROOT/bins/tv-authd/systemd/tv-authd.service" ;;
        tv-auth-popupd.service) echo "$REPO_ROOT/bins/tv-auth-popupd/systemd/tv-auth-popupd.service" ;;
        tv-blockd.service) echo "$REPO_ROOT/bins/tv-blockd/systemd/tv-blockd.service" ;;
        tv-patchd.service) echo "$REPO_ROOT/bins/tv-patchd/systemd/tv-patchd.service" ;;
        *) die "unknown unit: $1" ;;
    esac
}

require_local_file() {
    [ -f "$1" ] || die "missing local file: $1"
    [ ! -L "$1" ] || die "refusing local symlink: $1"
    [ -r "$1" ] || die "unreadable local file: $1"
}

validate_pristine() {
    path=$1
    [ -n "$path" ] || die "no pristine hook supplied"
    require_local_file "$path"
    size=$(wc -c < "$path" | tr -d ' ')
    [ "$size" = "$PRISTINE_SIZE" ] ||
        die "pristine hook is $size bytes, expected $PRISTINE_SIZE"
    actual=$(sha256_file "$path")
    [ "$actual" = "$PRISTINE_HOOK_SHA" ] ||
        die "pristine hook hash is $actual, expected $PRISTINE_HOOK_SHA"
}

prepare_local_manifest() {
    need_host_tools
    require_local_file "$HOOK_SOURCE"
    require_local_file "$APPLY_SCRIPT"
    for name in "${UNIT_NAMES[@]}"; do
        require_local_file "$(unit_source "$name")"
    done
    NEW_HOOK_SHA=$(sha256_file "$HOOK_SOURCE")
    UNIT_NAMES_LIST=${UNIT_NAMES[*]}
}

remote_preflight() {
    ssh "${SSH_OPTS[@]}" "$TV_HOST" \
        "TARGET_HOOK_SHA='$NEW_HOOK_SHA' UNIT_NAMES='$UNIT_NAMES_LIST' sh -eu -s" <<'REMOTE'
BUILD=T-PTMDEUC-REL-202602250226
PRISTINE_HOOK_SHA=c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61
PREVIOUS_HOOK_SHA=27fd5e7abd20a1178a2f395fa6d8c5ae8e241a0bc57fb58f75315c66ea969b94
HOOK=/opt/etc/parseinfo.sh
UNIT_DIR=/opt/tv-units
STOP=$UNIT_DIR/disabled

fail() { echo "preflight: $*" >&2; exit 1; }
sha_file() { sha256sum "$1" | cut -d ' ' -f 1; }
smack_label() { chsmack "$1" 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p'; }
require_regular() {
    [ -e "$1" ] || fail "missing $1"
    [ ! -L "$1" ] || fail "refusing symlink $1"
    [ -f "$1" ] || fail "not a regular file: $1"
}
require_meta() {
    actual=$(stat -c '%u %g %a' "$1") || fail "cannot stat $1"
    [ "$actual" = "$2" ] || fail "unexpected ownership/mode for $1: $actual"
}
trusted_unit_label() {
    case "$1" in System::Shared|System::Privileged) return 0 ;; *) return 1 ;; esac
}

[ "$(id -u)" = 0 ] || fail "must run as UID 0"
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = "$BUILD" ] || fail "wrong firmware build"
[ -d "$UNIT_DIR" ] || fail "missing $UNIT_DIR"
[ ! -L "$UNIT_DIR" ] || fail "$UNIT_DIR is a symlink"
require_meta "$UNIT_DIR" "0 0 700"
[ "$(smack_label "$UNIT_DIR")" = System::Shared ] || fail "unexpected SMACK label on $UNIT_DIR"
require_regular "$HOOK"
require_meta "$HOOK" "0 0 755"
[ "$(smack_label "$HOOK")" = _ ] || fail "unexpected SMACK label on $HOOK"

hook_sha=$(sha_file "$HOOK")
case "$hook_sha" in
    "$PRISTINE_HOOK_SHA") hook_state=pristine ;;
    "$PREVIOUS_HOOK_SHA") hook_state=previous-hardening ;;
    "$TARGET_HOOK_SHA") hook_state=target ;;
    *) fail "unknown live hook hash: $hook_sha" ;;
esac
if grep -q 'STOP = "/opt/tv-units/disabled"' "$HOOK"; then
    stop_control=supported
else
    stop_control=not-in-live-hook
fi

if [ -e "$STOP" ] || [ -L "$STOP" ]; then
    require_regular "$STOP"
    require_meta "$STOP" "0 0 600"
    [ "$(smack_label "$STOP")" = System::Shared ] || fail "unexpected stop-file SMACK label"
    stop_state=present
else
    stop_state=absent
fi

for name in $UNIT_NAMES; do
    live=$UNIT_DIR/$name
    require_regular "$live"
    require_meta "$live" "0 0 600"
    trusted_unit_label "$(smack_label "$live")" || fail "unexpected unit SMACK label: $name"
    printf 'unit_%s=%s\n' "$name" "$(sha_file "$live")"
done

printf 'hook_sha256=%s\n' "$hook_sha"
printf 'hook_state=%s\n' "$hook_state"
printf 'stop_control=%s\n' "$stop_control"
printf 'stop_file=%s\n' "$stop_state"
printf 'units=ok\n'
REMOTE
}

read_preflight() {
    output=$1
    LIVE_HOOK_SHA=$(printf '%s\n' "$output" | sed -n 's/^hook_sha256=//p')
    LIVE_HOOK_STATE=$(printf '%s\n' "$output" | sed -n 's/^hook_state=//p')
    LIVE_STOP_CONTROL=$(printf '%s\n' "$output" | sed -n 's/^stop_control=//p')
    LIVE_STOP_STATE=$(printf '%s\n' "$output" | sed -n 's/^stop_file=//p')
    [ -n "$LIVE_HOOK_SHA" ] && [ -n "$LIVE_HOOK_STATE" ] &&
        [ -n "$LIVE_STOP_CONTROL" ] && [ -n "$LIVE_STOP_STATE" ] ||
        die "could not parse remote preflight output"
    LIVE_UNIT_HASHES=()
    for name in "${UNIT_NAMES[@]}"; do
        hash=$(printf '%s\n' "$output" | sed -n "s/^unit_$name=//p")
        case "$hash" in
            *[!0-9a-f]*|'') die "could not parse live hash for $name" ;;
        esac
        [ "${#hash}" -eq 64 ] || die "bad live hash length for $name"
        LIVE_UNIT_HASHES+=("$hash")
    done
}

prepare_remote_stage() {
    stage=$1
    ssh "${SSH_OPTS[@]}" "$TV_HOST" "STAGE='$stage' sh -eu -s" <<'REMOTE'
STAGE=${STAGE:?}
[ "$(id -u)" = 0 ] || { echo "stage: must run as UID 0" >&2; exit 1; }
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = T-PTMDEUC-REL-202602250226 ] ||
    { echo "stage: wrong firmware build" >&2; exit 1; }

case "$STAGE" in
    /opt/tv-deploy/*) ;;
    *) echo "stage: unexpected path" >&2; exit 1 ;;
esac
[ "$STAGE" = "${STAGE%/}" ] || { echo "stage: trailing slash" >&2; exit 1; }
case "/$STAGE/" in
    *"/../"*|*"/./"*) echo "stage: dot path component" >&2; exit 1 ;;
esac

umask 077
if [ ! -e /opt/tv-deploy ]; then
    mkdir -m 700 /opt/tv-deploy
    chown 0:0 /opt/tv-deploy
    chsmack -a System::Shared /opt/tv-deploy
fi
[ -d /opt/tv-deploy ] && [ ! -L /opt/tv-deploy ] ||
    { echo "stage: unsafe /opt/tv-deploy" >&2; exit 1; }
[ "$(stat -c '%u %g %a' /opt/tv-deploy)" = "0 0 700" ] ||
    { echo "stage: unexpected /opt/tv-deploy metadata" >&2; exit 1; }
[ "$(chsmack /opt/tv-deploy 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p')" = System::Shared ] ||
    { echo "stage: unexpected /opt/tv-deploy SMACK label" >&2; exit 1; }

[ ! -e "$STAGE" ] || { echo "stage: path already exists: $STAGE" >&2; exit 1; }
mkdir -m 700 "$STAGE"
chown 0:0 "$STAGE"
chsmack -a System::Shared "$STAGE"
REMOTE
}

cleanup_remote_stage() {
    stage=$1
    ssh "${SSH_OPTS[@]}" "$TV_HOST" "STAGE='$stage' sh -eu -s" <<'REMOTE'
case "$STAGE" in
    /opt/tv-deploy/*) ;;
    *) echo "cleanup: refusing unexpected path" >&2; exit 1 ;;
esac
[ "$STAGE" = "${STAGE%/}" ] || exit 1
case "/$STAGE/" in
    *"/../"*|*"/./"*) echo "cleanup: dot path component" >&2; exit 1 ;;
esac
case "$STAGE" in
    /opt/tv-deploy/*/*) echo "cleanup: unexpected nested path" >&2; exit 1 ;;
esac
rm -rf "$STAGE"
REMOTE
}

cleanup_created_stage() {
    status=$?
    trap - EXIT
    if [ -n "${REMOTE_STAGE_CREATED:-}" ] && [ "$KEEP_REMOTE_STAGE" != 1 ]; then
        cleanup_remote_stage "$REMOTE_STAGE_CREATED" >/dev/null 2>&1 || :
    fi
    exit "$status"
}

stage_and_apply() {
    phase=$1
    accepted_hash=$2
    target_hash=$3
    rows=$4
    staged_hook=$5
    shift 5
    stage_name="$(date -u +%Y%m%dT%H%M%SZ)-$$"
    case "$stage_name" in
        *[!0-9A-Za-z-]*) die "unsafe generated stage name" ;;
    esac
    stage=$REMOTE_STAGE_ROOT/$stage_name

    prepare_remote_stage "$stage"
    REMOTE_STAGE_CREATED=$stage
    trap cleanup_created_stage EXIT
    # This firmware has no sftp-server, so force the legacy SCP protocol.
    scp -q -O -o BatchMode=yes -o ConnectTimeout=10 "$@" "$TV_HOST:$stage/"
    ssh "${SSH_OPTS[@]}" "$TV_HOST" \
        "ACTION='$phase' STAGE='$stage' ACCEPTED_HOOK_SHA='$accepted_hash' TARGET_HOOK_SHA='$target_hash' STAGED_HOOK='$staged_hook' UNIT_ROWS='$rows' sh -eu -s" \
        < "$APPLY_SCRIPT"
    if [ "$KEEP_REMOTE_STAGE" = 1 ]; then
        echo "remote stage kept at $stage"
    else
        cleanup_remote_stage "$stage"
    fi
    REMOTE_STAGE_CREATED=
    trap - EXIT
}

manifest_get() {
    key=$1
    file=$2
    sed -n "s/^$key=//p" "$file" | head -n 1
}

remote_apply_preflight() {
    ssh "${SSH_OPTS[@]}" "$TV_HOST" sh -eu -s <<'REMOTE'
[ "$(id -u)" = 0 ] || { echo "preflight: must run as UID 0" >&2; exit 1; }
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = T-PTMDEUC-REL-202602250226 ] ||
    { echo "preflight: wrong firmware build" >&2; exit 1; }
[ -d /opt/tv-units ] && [ ! -L /opt/tv-units ] ||
    { echo "preflight: unsafe /opt/tv-units" >&2; exit 1; }
[ "$(stat -c '%u %g %a' /opt/tv-units)" = "0 0 700" ] ||
    { echo "preflight: unexpected /opt/tv-units metadata" >&2; exit 1; }
[ -f /opt/etc/parseinfo.sh ] && [ ! -L /opt/etc/parseinfo.sh ] ||
    { echo "preflight: unsafe /opt/etc/parseinfo.sh" >&2; exit 1; }
[ "$(stat -c '%u %g %a' /opt/etc/parseinfo.sh)" = "0 0 755" ] ||
    { echo "preflight: unexpected hook metadata" >&2; exit 1; }
REMOTE
}

make_snapshot() {
    pristine=$1
    current_hook_sha=$2
    snapshot_base=$(date -u +%Y%m%dT%H%M%SZ)-${current_hook_sha:0:8}
    snapshot=$BACKUP_ROOT/$snapshot_base
    suffix=1
    while [ -e "$snapshot" ] || [ -e "$snapshot.partial" ]; do
        snapshot=$BACKUP_ROOT/$snapshot_base-$suffix
        suffix=$((suffix + 1))
    done
    if [ -L "$BACKUP_ROOT" ] || { [ -e "$BACKUP_ROOT" ] && [ ! -d "$BACKUP_ROOT" ]; }; then
        die "unsafe backup root: $BACKUP_ROOT"
    fi
    if [ ! -d "$BACKUP_ROOT" ]; then
        mkdir -p "$BACKUP_ROOT"
        chmod 700 "$BACKUP_ROOT"
    fi
    [ -d "$BACKUP_ROOT" ] && [ -w "$BACKUP_ROOT" ] || die "backup root is not writable: $BACKUP_ROOT"
    partial=$snapshot.partial
    mkdir -m 700 "$partial"
    mkdir -m 700 "$partial/units"

    scp -q -O -o BatchMode=yes -o ConnectTimeout=10 \
        "$TV_HOST:$HOOK" "$partial/parseinfo.sh.previous"
    chmod 600 "$partial/parseinfo.sh.previous"
    [ "$(sha256_file "$partial/parseinfo.sh.previous")" = "$current_hook_sha" ] ||
        die "pulled hook changed during snapshot"

    index=0
    for name in "${UNIT_NAMES[@]}"; do
        expected=${LIVE_UNIT_HASHES[$index]}
        scp -q -O -o BatchMode=yes -o ConnectTimeout=10 \
            "$TV_HOST:$UNIT_DIR/$name" "$partial/units/$name"
        chmod 600 "$partial/units/$name"
        [ "$(sha256_file "$partial/units/$name")" = "$expected" ] ||
            die "pulled unit changed during snapshot: $name"
        index=$((index + 1))
    done

    if [ "$current_hook_sha" = "$PRISTINE_HOOK_SHA" ]; then
        if [ -n "$pristine" ]; then
            validate_pristine "$pristine"
            [ "$(sha256_file "$pristine")" = "$current_hook_sha" ] ||
                die "live hook is pristine but differs from PRISTINE_HOOK"
        fi
        cp "$partial/parseinfo.sh.previous" "$partial/parseinfo.sh.pristine"
    else
        validate_pristine "$pristine"
        cp "$pristine" "$partial/parseinfo.sh.pristine"
    fi
    chmod 600 "$partial/parseinfo.sh.pristine"

    {
        printf 'format=1\n'
        printf 'created=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'target=%s\n' "$TV_HOST"
        printf 'build=%s\n' "$BUILD"
        printf 'hook_previous_sha256=%s\n' "$current_hook_sha"
        printf 'hook_installed_sha256=%s\n' "$NEW_HOOK_SHA"
        printf 'hook_pristine_sha256=%s\n' "$PRISTINE_HOOK_SHA"
        for name in "${UNIT_NAMES[@]}"; do
            previous=$(sha256_file "$partial/units/$name")
            installed=$(sha256_file "$(unit_source "$name")")
            printf 'unit_previous_%s=%s\n' "$name" "$previous"
            printf 'unit_installed_%s=%s\n' "$name" "$installed"
        done
    } > "$partial/manifest"
    chmod 600 "$partial/manifest"

    cat > "$partial/README.txt" <<EOF
Private pre-deployment snapshot. It contains Samsung's writable hook script;
do not commit, publish, or copy back to a TV except for an intentional rollback.

Rollback:
  TV_HOST=$TV_HOST $0 rollback $snapshot

Pristine copy: parseinfo.sh.pristine
`sha256sum`:
  $(sha256_file "$partial/parseinfo.sh.pristine")  parseinfo.sh.pristine
EOF
    chmod 600 "$partial/README.txt"
    mv "$partial" "$snapshot"
    SNAPSHOT_PATH=$snapshot
}

check_action() {
    pristine=${1:-$PRISTINE_HOOK}
    prepare_local_manifest
    need_tv_host
    if [ -n "$pristine" ]; then
        validate_pristine "$pristine"
    fi
    read_preflight "$(remote_preflight)"
    echo "target=$TV_HOST build=$BUILD"
    echo "hook_sha256=$LIVE_HOOK_SHA state=$LIVE_HOOK_STATE"
    echo "source_hook_sha256=$NEW_HOOK_SHA"
    echo "stop_control=$LIVE_STOP_CONTROL stop_file=$LIVE_STOP_STATE"
    echo "units=ok"
    if [ "$LIVE_HOOK_STATE" = pristine ]; then
        echo "pristine=current live hook (no external copy needed)"
    elif [ -n "$pristine" ]; then
        echo "pristine=ok $PRISTINE_HOOK_SHA"
    else
        echo "pristine=MISSING; set PRISTINE_HOOK to an operator-held copy before install"
    fi
}

install_action() {
    pristine=${1:-$PRISTINE_HOOK}
    prepare_local_manifest
    need_tv_host
    read_preflight "$(remote_preflight)"
    if [ "$LIVE_HOOK_SHA" != "$PRISTINE_HOOK_SHA" ] && [ -z "$pristine" ]; then
        die "live hook is not pristine; supply PRISTINE_HOOK from off-device storage"
    fi
    make_snapshot "$pristine" "$LIVE_HOOK_SHA"

    rows=
    sources=("$HOOK_SOURCE")
    index=0
    for name in "${UNIT_NAMES[@]}"; do
        accepted=${LIVE_UNIT_HASHES[$index]}
        target=$(sha256_file "$(unit_source "$name")")
        row="$name:$accepted:$target"
        if [ -z "$rows" ]; then rows=$row; else rows="$rows $row"; fi
        sources+=("$(unit_source "$name")")
        index=$((index + 1))
    done
    stage_and_apply install "$LIVE_HOOK_SHA" "$NEW_HOOK_SHA" "$rows" parseinfo.sh "${sources[@]}"

    echo "installed hook=$NEW_HOOK_SHA"
    echo "snapshot=$SNAPSHOT_PATH"
    echo "reboot required to verify the hook, stop file and all five services"
}

rollback_action() {
    need_host_tools
    need_tv_host
    snapshot=${1:-}
    [ -n "$snapshot" ] || die "rollback needs a snapshot directory"
    [ -d "$snapshot" ] || die "missing snapshot: $snapshot"
    snapshot=$(cd -- "$snapshot" && pwd -P)
    manifest=$snapshot/manifest
    previous_hook=$snapshot/parseinfo.sh.previous
    require_local_file "$APPLY_SCRIPT"
    require_local_file "$manifest"
    require_local_file "$previous_hook"
    [ "$(manifest_get format "$manifest")" = 1 ] || die "unsupported snapshot manifest"
    [ "$(manifest_get build "$manifest")" = "$BUILD" ] || die "snapshot build mismatch"
    [ "$(manifest_get target "$manifest")" = "$TV_HOST" ] || die "snapshot target mismatch"
    hook_current=$(manifest_get hook_installed_sha256 "$manifest")
    hook_previous=$(manifest_get hook_previous_sha256 "$manifest")
    [ "$hook_current" != "$hook_previous" ] || die "snapshot records no hook change"
    [ "$(sha256_file "$previous_hook")" = "$hook_previous" ] ||
        die "snapshot previous hook hash mismatch"
    require_local_file "$snapshot/parseinfo.sh.pristine"
    [ "$(sha256_file "$snapshot/parseinfo.sh.pristine")" = "$PRISTINE_HOOK_SHA" ] ||
        die "snapshot pristine hook hash mismatch"

    rows=
    sources=("$previous_hook")
    for name in "${UNIT_NAMES[@]}"; do
        current=$(manifest_get "unit_installed_$name" "$manifest")
        previous=$(manifest_get "unit_previous_$name" "$manifest")
        case "$current" in *[!0-9a-f]*|'') die "bad installed hash for $name" ;; esac
        case "$previous" in *[!0-9a-f]*|'') die "bad previous hash for $name" ;; esac
        [ "${#current}" -eq 64 ] && [ "${#previous}" -eq 64 ] ||
            die "bad snapshot hash length for $name"
        file=$snapshot/units/$name
        require_local_file "$file"
        [ "$(sha256_file "$file")" = "$previous" ] ||
            die "snapshot unit hash mismatch: $name"
        row="$name:$current:$previous"
        if [ -z "$rows" ]; then rows=$row; else rows="$rows $row"; fi
        sources+=("$file")
    done

    remote_apply_preflight
    stage_and_apply rollback "$hook_current" "$hook_previous" "$rows" \
        parseinfo.sh.previous "${sources[@]}"
    echo "rollback restored hook=$hook_previous"
    echo "snapshot=$snapshot"
    echo "running services were not stopped; reboot to complete the rollback"
}

disable_action() {
    need_tv_host
    ssh "${SSH_OPTS[@]}" "$TV_HOST" sh -eu -s <<'REMOTE'
BUILD=T-PTMDEUC-REL-202602250226
UNIT_DIR=/opt/tv-units
STOP=$UNIT_DIR/disabled
[ "$(id -u)" = 0 ] || { echo "disable: must run as UID 0" >&2; exit 1; }
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = "$BUILD" ] ||
    { echo "disable: wrong firmware build" >&2; exit 1; }
[ -d "$UNIT_DIR" ] && [ ! -L "$UNIT_DIR" ] ||
    { echo "disable: unsafe $UNIT_DIR" >&2; exit 1; }
[ "$(stat -c '%u %g %a' "$UNIT_DIR")" = "0 0 700" ] ||
    { echo "disable: unexpected unit-directory metadata" >&2; exit 1; }
[ "$(chsmack "$UNIT_DIR" 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p')" = System::Shared ] ||
    { echo "disable: unexpected unit-directory SMACK label" >&2; exit 1; }
[ -f /opt/etc/parseinfo.sh ] && [ ! -L /opt/etc/parseinfo.sh ] ||
    { echo "disable: unsafe live hook" >&2; exit 1; }
grep -q 'STOP = "/opt/tv-units/disabled"' /opt/etc/parseinfo.sh ||
    { echo "disable: live hook does not implement the stop file; install first" >&2; exit 1; }
if [ -e "$STOP" ] || [ -L "$STOP" ]; then
    [ ! -L "$STOP" ] && [ -f "$STOP" ] ||
        { echo "disable: unsafe existing stop file" >&2; exit 1; }
    [ "$(stat -c '%u %g %a' "$STOP")" = "0 0 600" ] ||
        { echo "disable: unexpected stop-file metadata" >&2; exit 1; }
    [ "$(chsmack "$STOP" 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p')" = System::Shared ] ||
        { echo "disable: unexpected stop-file SMACK label" >&2; exit 1; }
    echo "optional services already disabled"
    exit 0
fi
umask 077
tmp=$UNIT_DIR/.disabled.new.$$
: > "$tmp"
chown 0:0 "$tmp"
chmod 600 "$tmp"
chsmack -a System::Shared "$tmp"
sync
mv -f "$tmp" "$STOP"
sync
[ ! -L "$STOP" ] && [ -f "$STOP" ] && [ "$(stat -c '%u %g %a' "$STOP")" = "0 0 600" ] ||
    { echo "disable: post-create verification failed" >&2; exit 1; }
echo "optional services disabled for subsequent boots"
REMOTE
}

enable_action() {
    need_tv_host
    ssh "${SSH_OPTS[@]}" "$TV_HOST" sh -eu -s <<'REMOTE'
BUILD=T-PTMDEUC-REL-202602250226
UNIT_DIR=/opt/tv-units
STOP=$UNIT_DIR/disabled
[ "$(id -u)" = 0 ] || { echo "enable: must run as UID 0" >&2; exit 1; }
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = "$BUILD" ] ||
    { echo "enable: wrong firmware build" >&2; exit 1; }
[ -d "$UNIT_DIR" ] && [ ! -L "$UNIT_DIR" ] ||
    { echo "enable: unsafe $UNIT_DIR" >&2; exit 1; }
if [ ! -e "$STOP" ] && [ ! -L "$STOP" ]; then
    echo "optional services already enabled"
    exit 0
fi
[ ! -L "$STOP" ] && [ -f "$STOP" ] ||
    { echo "enable: unsafe stop file" >&2; exit 1; }
[ "$(stat -c '%u %g %a' "$STOP")" = "0 0 600" ] ||
    { echo "enable: unexpected stop-file metadata" >&2; exit 1; }
[ "$(chsmack "$STOP" 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p')" = System::Shared ] ||
    { echo "enable: unexpected stop-file SMACK label" >&2; exit 1; }
rm -f "$STOP"
sync
[ ! -e "$STOP" ] || { echo "enable: stop file still present" >&2; exit 1; }
echo "optional services enabled for subsequent boots"
REMOTE
}

case "$action" in
    check)
        [ "$#" -le 1 ] || { usage; exit 2; }
        check_action "${1:-}"
        ;;
    install)
        [ "$#" -le 1 ] || { usage; exit 2; }
        install_action "${1:-}"
        ;;
    rollback)
        [ "$#" -eq 1 ] || { usage; exit 2; }
        rollback_action "$1"
        ;;
    disable|enable)
        [ "$#" -eq 0 ] || { usage; exit 2; }
        "${action}_action"
        ;;
    help|-h|--help)
        usage
        ;;
    *)
        usage
        exit 2
        ;;
esac
