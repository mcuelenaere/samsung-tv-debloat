#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
target=armv7-unknown-linux-musleabihf
binary="$root/target/$target/release/peer-identity"
artifact="$root/out/peer-identity-$target"

for command in cargo rustup rustc file; do
    command -v "$command" >/dev/null || {
        echo "missing required command: $command" >&2
        exit 1
    }
done

if ! rustup target list --installed | grep -qx "$target"; then
    rustup target add "$target"
fi

# This crate needs no C library beyond musl's self-contained startup objects.
# rust-lld therefore provides an ARMv7 static build without an ARM GCC sysroot.
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS='-C link-self-contained=yes'
cargo build --locked --release --target "$target" --manifest-path "$root/Cargo.toml"

mkdir -p "$root/out"
install -m 0755 "$binary" "$artifact"

file "$artifact"
if command -v sha256sum >/dev/null; then
    sha256sum "$artifact"
else
    shasum -a 256 "$artifact"
fi
wc -c "$artifact"
