#!/bin/sh
# On-TV half of the guarded AT-HUB persistence deployment.
#
# This script is normally fed to `sh -s` by deploy.sh after the source files
# have been copied into STAGE. It is not a general installer: it only replaces
# the S95B AT-HUB hook and the five unit files owned by this project.
#
# Required environment:
#   ACTION=install|rollback
#   STAGE=/opt/tv-deploy/<id>
#   ACCEPTED_HOOK_SHA=<current live hook, or the target for a repeated apply>
#   TARGET_HOOK_SHA=<hash of the staged hook>
#   STAGED_HOOK=<parseinfo.sh or parseinfo.sh.previous>
#   UNIT_ROWS="<name>:<accepted sha>:<target sha> ..."
#
# Every mutation is checked again immediately before it happens. The previous
# files are copied under STAGE/original and restored on an ordinary failure.
set -eu
umask 077
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

BUILD=T-PTMDEUC-REL-202602250226
PRISTINE_HOOK_SHA=c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61
PREVIOUS_HOOK_SHA=27fd5e7abd20a1178a2f395fa6d8c5ae8e241a0bc57fb58f75315c66ea969b94
HOOK=/opt/etc/parseinfo.sh
UNIT_DIR=/opt/tv-units
STOP=$UNIT_DIR/disabled
UNITS='dropbear.service tv-authd.service tv-auth-popupd.service tv-blockd.service tv-patchd.service'

: "${ACTION:?ACTION is required}"
: "${STAGE:?STAGE is required}"
: "${ACCEPTED_HOOK_SHA:?ACCEPTED_HOOK_SHA is required}"
: "${TARGET_HOOK_SHA:?TARGET_HOOK_SHA is required}"
: "${STAGED_HOOK:?STAGED_HOOK is required}"
: "${UNIT_ROWS:?UNIT_ROWS is required}"

transaction_started=0
tmp_files=

fail() {
    echo "tv-persistence: $*" >&2
    exit 1
}

sha_file() {
    sha256sum "$1" | cut -d ' ' -f 1
}

smack_label() {
    chsmack "$1" 2>/dev/null | sed -n 's/.*access="\([^"]*\)".*/\1/p'
}

is_sha() {
    case "$1" in
        *[!0-9a-f]*|'') return 1 ;;
    esac
    [ "${#1}" -eq 64 ]
}

is_trusted_unit_label() {
    case "$1" in
        System::Shared|System::Privileged) return 0 ;;
        *) return 1 ;;
    esac
}

require_regular() {
    path=$1
    [ -e "$path" ] || fail "missing $path"
    [ ! -L "$path" ] || fail "refusing symlink $path"
    [ -f "$path" ] || fail "not a regular file: $path"
}

require_meta() {
    path=$1
    expected=$2
    actual=$(stat -c '%u %g %a' "$path") || fail "cannot stat $path"
    [ "$actual" = "$expected" ] || fail "unexpected ownership/mode for $path: $actual"
}

install_file() {
    src=$1
    dst=$2
    expected=$3
    mode=$4
    label=$5
    [ "$(sha_file "$src")" = "$expected" ] || fail "staged hash changed: $src"
    tmp="${dst%/*}/.${dst##*/}.tv-deploy.$$"
    tmp_files="$tmp_files $tmp"
    rm -f "$tmp"
    cp "$src" "$tmp" || fail "copy failed: $src"
    chown 0:0 "$tmp" || fail "chown failed: $tmp"
    chmod "$mode" "$tmp" || fail "chmod failed: $tmp"
    chsmack -a "$label" "$tmp" || fail "chsmack failed: $tmp"
    [ "$(sha_file "$tmp")" = "$expected" ] || fail "staged copy hash mismatch: $dst"
    sync
    mv -f "$tmp" "$dst" || fail "atomic replace failed: $dst"
    require_regular "$dst"
    require_meta "$dst" "0 0 $mode"
    [ "$(smack_label "$dst")" = "$label" ] || fail "wrong SMACK label after install: $dst"
}

restore_original_files() {
    [ -d "$STAGE/original" ] || return 0
    if [ -f "$STAGE/original/parseinfo.sh" ]; then
        if ! ( install_file "$STAGE/original/parseinfo.sh" "$HOOK" \
            "$ACCEPTED_HOOK_SHA" 755 _ ) >/dev/null 2>&1; then
            echo "tv-persistence: RESTORE FAILED for $HOOK; use the host snapshot" >&2
        fi
    fi
    if [ -d "$STAGE/original/units" ]; then
        remaining=$UNIT_ROWS
        while [ -n "$remaining" ]; do
            row=${remaining%% *}
            if [ "$row" = "$remaining" ]; then
                remaining=
            else
                remaining=${remaining#* }
            fi
            name=${row%%:*}
            rest=${row#*:}
            accepted=${rest%%:*}
            old=$STAGE/original/units/$name
            [ -f "$old" ] || continue
            if ! ( install_file "$old" "$UNIT_DIR/$name" "$accepted" \
                600 System::Shared ) >/dev/null 2>&1; then
                echo "tv-persistence: RESTORE FAILED for $name; use the host snapshot" >&2
            fi
        done
    fi
}

cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$status" -ne 0 ] && [ "$transaction_started" -eq 1 ]; then
        echo "tv-persistence: failure after mutation; restoring previous files" >&2
        restore_original_files
    fi
    for path in $tmp_files; do
        rm -f "$path" 2>/dev/null || :
    done
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

case "$ACTION" in
    install|rollback) ;;
    *) fail "unknown ACTION: $ACTION" ;;
esac
case "$STAGE" in
    /opt/tv-deploy/*) ;;
    *) fail "STAGE outside /opt/tv-deploy" ;;
esac
[ "$STAGE" = "${STAGE%/}" ] || fail "unexpected STAGE path"
case "$STAGED_HOOK" in
    parseinfo.sh|parseinfo.sh.previous) ;;
    *) fail "unexpected STAGED_HOOK name" ;;
esac
is_sha "$ACCEPTED_HOOK_SHA" || fail "bad ACCEPTED_HOOK_SHA"
is_sha "$TARGET_HOOK_SHA" || fail "bad TARGET_HOOK_SHA"

[ "$(id -u)" = 0 ] || fail "must run as UID 0"
[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = "$BUILD" ] || fail "wrong firmware build"
for tool in chown chmod chsmack sha256sum stat sed cut mv cp sync grep; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing tool: $tool"
done

[ -d "$STAGE" ] || fail "missing stage directory"
[ ! -L "$STAGE" ] || fail "stage directory is a symlink"
require_meta "$STAGE" "0 0 700"
[ "$(smack_label "$STAGE")" = System::Shared ] || fail "unexpected stage SMACK label"

[ -d "$UNIT_DIR" ] || fail "missing $UNIT_DIR"
[ ! -L "$UNIT_DIR" ] || fail "$UNIT_DIR is a symlink"
require_meta "$UNIT_DIR" "0 0 700"
[ "$(smack_label "$UNIT_DIR")" = System::Shared ] || fail "unexpected SMACK label on $UNIT_DIR"

require_regular "$HOOK"
require_meta "$HOOK" "0 0 755"
[ "$(smack_label "$HOOK")" = _ ] || fail "unexpected SMACK label on $HOOK"

if [ -e "$STOP" ] || [ -L "$STOP" ]; then
    require_regular "$STOP"
    require_meta "$STOP" "0 0 600"
    [ "$(smack_label "$STOP")" = System::Shared ] || fail "unexpected stop-file SMACK label"
fi

require_regular "$STAGE/$STAGED_HOOK"
[ "$(sha_file "$STAGE/$STAGED_HOOK")" = "$TARGET_HOOK_SHA" ] || fail "staged hook hash mismatch"

if [ "$ACTION" = install ]; then
    case "$ACCEPTED_HOOK_SHA" in
        "$PRISTINE_HOOK_SHA"|"$PREVIOUS_HOOK_SHA"|"$TARGET_HOOK_SHA") ;;
        *) fail "live hook is not a known pristine/previous/target hash" ;;
    esac
    grep -q 'STOP = "/opt/tv-units/disabled"' "$STAGE/$STAGED_HOOK" ||
        fail "target hook does not contain the opt-in stop check"
fi

current_hook_sha=$(sha_file "$HOOK")
if [ "$current_hook_sha" != "$ACCEPTED_HOOK_SHA" ] &&
   [ "$current_hook_sha" != "$TARGET_HOOK_SHA" ]; then
    fail "live hook changed after snapshot"
fi

# Validate every row and every live unit before copying anything to /opt.
seen=
remaining=$UNIT_ROWS
count=0
while [ -n "$remaining" ]; do
    row=${remaining%% *}
    if [ "$row" = "$remaining" ]; then
        remaining=
    else
        remaining=${remaining#* }
    fi
    name=${row%%:*}
    rest=${row#*:}
    accepted=${rest%%:*}
    target=${rest#*:}
    [ "$name" != "$row" ] || fail "malformed UNIT_ROWS"
    case "$rest" in
        *:*) ;;
        *) fail "malformed UNIT_ROWS entry for $name" ;;
    esac
    case "$rest" in
        *:*:*) fail "too many fields for $name" ;;
    esac
    is_sha "$accepted" || fail "bad accepted hash for $name"
    is_sha "$target" || fail "bad target hash for $name"
    case " $UNITS " in
        *" $name "*) ;;
        *) fail "unexpected unit name: $name" ;;
    esac
    case " $seen " in
        *" $name "*) fail "duplicate unit: $name" ;;
    esac
    seen="$seen $name"
    count=$((count + 1))

    staged=$STAGE/$name
    require_regular "$staged"
    [ "$(sha_file "$staged")" = "$target" ] || fail "staged unit hash mismatch: $name"

    live=$UNIT_DIR/$name
    require_regular "$live"
    require_meta "$live" "0 0 600"
    is_trusted_unit_label "$(smack_label "$live")" || fail "unexpected unit SMACK label: $name"
    live_sha=$(sha_file "$live")
    if [ "$live_sha" != "$accepted" ] && [ "$live_sha" != "$target" ]; then
        fail "live unit changed after snapshot: $name"
    fi
done
[ "$count" -eq 5 ] || fail "expected all five project unit files"

# Keep a private rollback copy for this transaction, then replace units before
# the hook. Until the final hook rename, boot behaviour is still unchanged.
mkdir -m 700 "$STAGE/original"
mkdir -m 700 "$STAGE/original/units"
cp "$HOOK" "$STAGE/original/parseinfo.sh"
chown 0:0 "$STAGE/original/parseinfo.sh"
chmod 600 "$STAGE/original/parseinfo.sh"
chsmack -a _ "$STAGE/original/parseinfo.sh"
for name in $UNITS; do
    cp "$UNIT_DIR/$name" "$STAGE/original/units/$name"
    chown 0:0 "$STAGE/original/units/$name"
    chmod 600 "$STAGE/original/units/$name"
    chsmack -a System::Shared "$STAGE/original/units/$name"
done

transaction_started=1
remaining=$UNIT_ROWS
while [ -n "$remaining" ]; do
    row=${remaining%% *}
    if [ "$row" = "$remaining" ]; then
        remaining=
    else
        remaining=${remaining#* }
    fi
    name=${row%%:*}
    rest=${row#*:}
    target=${rest#*:}
    install_file "$STAGE/$name" "$UNIT_DIR/$name" "$target" 600 System::Shared
done

install_file "$STAGE/$STAGED_HOOK" "$HOOK" "$TARGET_HOOK_SHA" 755 _
sync
[ "$(sha_file "$HOOK")" = "$TARGET_HOOK_SHA" ] || fail "post-install hook hash mismatch"
transaction_started=0

echo "tv-persistence: $ACTION complete"
echo "hook_sha256=$TARGET_HOOK_SHA"
echo "units=$UNITS"
echo "stop_file=$([ -e "$STOP" ] && echo present || echo absent)"
