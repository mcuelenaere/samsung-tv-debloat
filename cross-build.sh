#!/usr/bin/env bash
# Cross-compile workspace members for the TV in a pinned Rust 1.85 container.
#
#   ./cross-build.sh musl    build --locked --release -p tv-authd
#   ./cross-build.sh gnueabi build --locked --release -p tv-auth-ui
#
# musl:    static armv7-unknown-linux-musleabihf, linked with rust-lld
#          (.cargo/config.toml). Headless daemons and CLIs.
# gnueabi: dynamic soft-float armv7-unknown-linux-gnueabi against the TV
#          libraries in sysroot/tv-6.5 (sysroot/fetch-tv-sysroot.sh). EFL UIs
#          and anything using tv-tizen.
#
# Output lands in target/cross-<flavor>/<triple>/.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
flavor=${1:?usage: cross-build.sh musl|gnueabi <cargo args...>}
shift

case "$flavor" in
musl)
    exec docker run --rm \
        -v "$ROOT:/src" -w /src \
        -e CARGO_TARGET_DIR=/src/target/cross-musl \
        rust:1.85.0-slim-bookworm@sha256:1829c432be4a592f3021501334d3fcca24f238432b13306a4e62669dec538e52 \
        sh -eu -c '
            rustup target add armv7-unknown-linux-musleabihf >/dev/null
            rustc --version
            cargo "$@" --target armv7-unknown-linux-musleabihf
        ' sh "$@"
    ;;
gnueabi)
    if [[ ! -d "$ROOT/sysroot/tv-6.5/usr/lib" ]]; then
        echo "missing sysroot/tv-6.5; run sysroot/fetch-tv-sysroot.sh first" >&2
        exit 1
    fi
    exec docker run --rm --platform linux/amd64 \
        -v "$ROOT:/src" -w /src \
        -v "$ROOT/sysroot/tv-6.5:/tv-sysroot:ro" \
        -e CARGO_TARGET_DIR=/src/target/cross-gnueabi \
        rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a \
        sh -eu -c '
            apt-get update >/dev/null
            apt-get install -y --no-install-recommends \
                gcc-arm-linux-gnueabi libc6-dev-armel-cross binutils-arm-linux-gnueabi >/dev/null
            rustup target add armv7-unknown-linux-gnueabi >/dev/null
            rustc --version
            export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER=arm-linux-gnueabi-gcc
            # The TV provides transitive dependencies at runtime; link only
            # against the direct libraries and keep their SONAMEs.
            export RUSTFLAGS="-L native=/tv-sysroot/usr/lib -C link-arg=-Wl,--allow-shlib-undefined"
            cargo "$@" --target armv7-unknown-linux-gnueabi
            # Strip so artifacts match the TV-deployed sizes.
            find "$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabi/release" -maxdepth 2 -type f -perm -u+x \
                ! -name "*.so" ! -name "*.d" -exec arm-linux-gnueabi-strip {} + 2>/dev/null || true
        ' sh "$@"
    ;;
*)
    echo "unknown flavor $flavor (musl|gnueabi)" >&2
    exit 2
    ;;
esac
