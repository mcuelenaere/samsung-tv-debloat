#!/bin/sh
# Thin on-TV setup driver, run once by the managed FDET payload.
#
# It is suite orchestration, not a package manager: every native change is
# delegated to tv-pkg. The guarded AT-HUB persistence hook is applied last.
set -eu
umask 077
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

ROOT=
RUN=
PLAN=
ADOPT=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --root) ROOT=${2:?--root needs a value}; shift 2 ;;
        --run) RUN=${2:?--run needs a value}; shift 2 ;;
        --plan) PLAN=${2:?--plan needs a value}; shift 2 ;;
        --adopt-existing) ADOPT=--adopt-existing; shift ;;
        *) echo "setup-driver: unknown argument $1" >&2; exit 2 ;;
    esac
done
[ -n "$ROOT" ] && [ -n "$RUN" ] && [ -n "$PLAN" ] ||
    { echo "setup-driver: --root, --run and --plan are required" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { echo "setup-driver: must run as UID 0" >&2; exit 1; }

cleanup() {
    status=$?
    trap - EXIT
    [ "$status" -eq 0 ] || echo "setup-status=failed"
    rm -rf "$RUN" || true
    [ -z "${stage:-}" ] || rm -rf "$stage" || true
    exit "$status"
}
trap cleanup EXIT

fail() {
    echo "setup-driver: $*" >&2
    exit 1
}

sha_file() {
    sha256sum "$1" | cut -d ' ' -f 1
}

plan_value() {
    sed -n "s/^$1=//p" "$PLAN" | head -n 1
}

TARGET_BUILD=$(plan_value target_build)
BUNDLE_SHA=$(plan_value bundle_sha256)
RUNTIME=$(plan_value runtime)
STAGING=$(plan_value staging)
KEY=$(plan_value key)
[ -n "$TARGET_BUILD" ] && [ -n "$BUNDLE_SHA" ] && [ -n "$RUNTIME" ] ||
    { echo "setup-driver: incomplete plan" >&2; exit 1; }
[ "$STAGING" = "$ROOT" ] || { echo "setup-driver: plan root mismatch" >&2; exit 1; }

[ "$(sed -n 's/^BUILD_ID=//p' /etc/tizen-release)" = "$TARGET_BUILD" ] ||
    fail "target build is not $TARGET_BUILD"

# Recompute the whole-bundle digest from the plan's digest rows. The host
# verified every byte before upload; this catches a partial or mixed upload.
digest_rows=$(mktemp)
trap 'rm -f "$digest_rows"; cleanup' EXIT
sed -n 's/^digest_row=//p' "$PLAN" | sort > "$digest_rows"
digest_body=$(cat "$digest_rows")
actual_bundle_sha=$({ printf '%s\n' "$TARGET_BUILD"; printf '%s' "$digest_body"; } | sha256sum | cut -d ' ' -f 1)
[ "$actual_bundle_sha" = "$BUNDLE_SHA" ] || fail "setup bundle digest mismatch"

# Verify every file the driver will use before changing anything.
for path in \
    "FdetProbe.dll" \
    "FdetProbe.runtimeconfig.json" \
    "tv-pkg" \
    "setup-driver.sh"
do
    expected=$(sed -n "s/^verify $path //p" "$PLAN" | head -n 1)
    [ -n "$expected" ] || fail "missing plan hash for $path"
    [ "$(sha_file "$ROOT/$path")" = "$expected" ] || fail "staged hash mismatch: $path"
done
for expected_line in $(sed -n 's/^verify //p' "$PLAN"); do
    path=${expected_line% *}
    expected=${expected_line##* }
    [ -f "$ROOT/$path" ] || fail "missing staged file: $path"
    [ "$(sha_file "$ROOT/$path")" = "$expected" ] || fail "staged hash mismatch: $path"
done

mkdir -p /opt/tv-suite/bin /opt/tv-suite/etc /opt/tv-suite/var
mkdir -p /opt/tv-auth /opt/tv-sshd /opt/tv-units
chmod 711 /opt/tv-auth
chmod 700 /opt/tv-sshd /opt/tv-units
chown 0:0 /opt/tv-auth /opt/tv-sshd /opt/tv-units
chsmack -a System::Shared /opt/tv-auth 2>/dev/null || true
chsmack -a System::Shared /opt/tv-sshd 2>/dev/null || true
chsmack -a System::Shared /opt/tv-units 2>/dev/null || true

mkdir -p "$RUN"
chmod 700 "$RUN"
cp "$ROOT/tv-pkg" "$RUN/tv-pkg"
chmod 700 "$RUN/tv-pkg"

# Initial SSH key input: merge without overwriting existing keys.
if [ -n "$KEY" ] && [ "$KEY" != "-" ]; then
    [ -f "$ROOT/$KEY" ] || fail "missing initial SSH key"
    touch /opt/tv-sshd/authorized_keys
    chown 0:0 /opt/tv-sshd/authorized_keys
    chmod 600 /opt/tv-sshd/authorized_keys
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        grep -Fqx "$line" /opt/tv-sshd/authorized_keys || printf '%s\n' "$line" >> /opt/tv-sshd/authorized_keys
    done < "$ROOT/$KEY"
fi

# Install components in the fixed reviewed order, using the same tv-pkg path
# later updates use. The first package is Dropbear and tv-pkg support files.
for line in $(sed -n 's/^package //p' "$PLAN"); do
    set -- $line
    id=$1
    path=$2
    "$RUN/tv-pkg" install "$ROOT/$path" $ADOPT || fail "tv-pkg install failed: $id"
    "$RUN/tv-pkg" status "$id" >/dev/null || fail "installed component unhealthy: $id"
    # The UI TPK is installed through tv-authd, so enroll trust as soon as the
    # daemon exists. Existing material is reused, never regenerated.
    if [ "$id" = tv-authd ]; then
        [ -x /opt/tv-auth/tv-authd ] || fail "tv-authd binary missing after install"
        cp /opt/tv-auth/tv-authd "$RUN/tv-authd"
        chmod 700 "$RUN/tv-authd"
        "$RUN/tv-authd" --enroll-trust >/dev/null || fail "trust enrollment failed"
        systemctl try-restart tv-authd.service >/dev/null 2>&1 || true
    fi
done

# Guarded persistence is deliberately last.
stage=/opt/tv-deploy/setup-$$
mkdir -p /opt/tv-deploy
chmod 700 /opt/tv-deploy
chown 0:0 /opt/tv-deploy
chsmack -a System::Shared /opt/tv-deploy 2>/dev/null || true
mkdir "$stage"
chmod 700 "$stage"
chown 0:0 "$stage"
chsmack -a System::Shared "$stage" 2>/dev/null || true
hook_path=$(sed -n 's/^persistence_hook //p' "$PLAN" | { read -r path sha; echo "$path"; })
apply_path=$(sed -n 's/^persistence_apply //p' "$PLAN" | { read -r path sha; echo "$path"; })
target_hook_sha=$(sha_file "$ROOT/$hook_path")
cp "$ROOT/$apply_path" "$stage/tv-apply.sh"
cp "$ROOT/$hook_path" "$stage/parseinfo.sh"
unit_rows=
for line in $(sed -n 's/^persistence_unit //p' "$PLAN"); do
    set -- $line
    path=$1
    target=$2
    name=${path##*/}
    cp "$ROOT/$path" "$stage/$name"
    live=/opt/tv-units/$name
    [ -f "$live" ] || fail "missing live unit $name; install its component first"
    accepted=$(sha_file "$live")
    if [ -n "$unit_rows" ]; then
        unit_rows="$unit_rows $name:$accepted:$target"
    else
        unit_rows="$name:$accepted:$target"
    fi
done
[ "$(sha_file "$stage/parseinfo.sh")" = "$target_hook_sha" ] || fail "staged hook changed"
accepted_hook_sha=$(sha_file /opt/etc/parseinfo.sh)
ACTION=install \
STAGE="$stage" \
ACCEPTED_HOOK_SHA="$accepted_hook_sha" \
TARGET_HOOK_SHA="$target_hook_sha" \
STAGED_HOOK=parseinfo.sh \
UNIT_ROWS="$unit_rows" \
    sh -s < "$stage/tv-apply.sh" || fail "persistence apply failed"

for unit in tv-authd.service dropbear.service tv-auth-popupd.service tv-blockd.service tv-patchd.service; do
    systemctl is-active --quiet "$unit" || fail "service is not active: $unit"
done
[ -S /run/tv-auth/broker.sock ] || fail "broker socket missing"
[ -d /opt/usr/apps/org.tvauth.ui ] || fail "UI package is not registered"
[ -e /run/tv-auth/ui-bin/tv-auth-ui ] || fail "UI alias is not mounted"
for line in $(sed -n 's/^package //p' "$PLAN"); do
    set -- $line
    "$RUN/tv-pkg" status "$1" >/dev/null || fail "post-install status failed: $1"
done

{
    printf 'setup completed %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'target_build=%s\n' "$TARGET_BUILD"
    printf 'bundle_sha256=%s\n' "$BUNDLE_SHA"
    printf 'runtime=%s\n' "$RUNTIME"
} > /opt/tv-suite/var/setup.log

rm -rf "$stage"
rm -rf "$ROOT"
echo "setup-status=ok"
