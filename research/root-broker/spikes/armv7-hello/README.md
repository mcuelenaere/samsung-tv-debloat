# ARMv7 static musl Rust hello spike

A dependency-free Rust executable for the `armv7-unknown-linux-musleabihf`
target. The build uses Docker's ARMv7 emulation and Rust's bundled `rust-lld`;
it does not need a host Rust installation, a separate C cross-toolchain, or a
Tizen SDK.

**Status (2026-11-04): host build, Docker/QEMU smoke test, direct TV execution,
and TV transient-systemd execution under `/run` passed. The TV has no writable
persistent rootfs; the isolated `/run` staging directory was removed after the
test.**

## Build

Requirements: Docker Desktop (or another Docker installation) with
`linux/arm/v7` emulation, plus the macOS `file`, `wc`, and `shasum` utilities
used for the final report.

```sh
cd root-broker/spikes/armv7-hello
./build.sh
```

The script runs this logical build sequence inside the container (see
`build.sh` for its exact invocation):

```sh
rustup target add armv7-unknown-linux-musleabihf
cargo build --locked --release --target armv7-unknown-linux-musleabihf
```

The container and toolchain are pinned as follows:

- image: `rust:1.85.0-slim-bookworm`
- image index SHA-256:
  `1829c432be4a592f3021501334d3fcca24f238432b13306a4e62669dec538e52`
- platform: `linux/arm/v7`
- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- `cargo 1.85.0 (d73d2caf9 2024-12-31)`
- target: `armv7-unknown-linux-musleabihf`

The release profile enables size optimization, LTO, one codegen unit, aborting
panics, and symbol stripping. The source has a compile-time guard requiring an
ARM/musl target. Output is copied to `out/armv7-hello`; `out/` and `target/`
are ignored build-product directories.

## Validation performed

Host used for these results: Docker Desktop 4.98.0 (245422), Docker Engine and
CLI 29.9.2, on an arm64 macOS host.

### ELF identity and artifact measurements

`./build.sh` ended with these host-side checks (the individual commands are
shown for clarity):

```sh
file out/armv7-hello
wc -c < out/armv7-hello
shasum -a 256 out/armv7-hello
```

Results:

```text
ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, stripped
Size: 323652 bytes
SHA-256: 4e7db0752221e5fc910c8d70be150caa7353463acd47f760683e2d5452d8b8cc
```

The following command provided a second architecture/linkage check:

```sh
docker run --rm --platform linux/arm/v7 \
  -v "$PWD/out:/out:ro" alpine:3.20 sh -eu -c \
  'apk add --no-cache binutils >/dev/null; readelf -h /out/armv7-hello; readelf -l /out/armv7-hello; readelf -d /out/armv7-hello'
```

Relevant `readelf` results:

```text
Class:   ELF32
Data:    2's complement, little endian
Type:    EXEC (Executable file)
Machine: ARM
Flags:   0x5000400, Version5 EABI, hard-float ABI
There is no dynamic section in this file.
```

The program headers contain no `INTERP` entry.

### Docker/QEMU execution smoke test

The exact artifact above was executed without modification:

```sh
docker run --rm --platform linux/arm/v7 \
  -v "$PWD/out:/out:ro" alpine:3.20 /out/armv7-hello
```

Observed output and exit status:

```text
armv7-hello: static ARMv7 musl Rust smoke test
exit status: 0
```

### On-TV execution (2026-11-04)

The exact artifact was staged as `/run/tvauth-spike/armv7-hello` (root-owned,
SMACK `System::Run`, SHA-256 matching the build above) and executed on
`T-PTMDEUC-REL-202602250226`:

```text
== direct execution ==
armv7-hello: static ARMv7 musl Rust smoke test
exit=0

== transient systemd execution ==
armv7-hello: static ARMv7 musl Rust smoke test
Running as unit: tvauth-spike-hello.service
Finished with result: success
Main processes terminated with: code=exited/status=0
Service runtime: 7ms
```

`systemd-run --wait --pipe --collect` created a transient unit without changing
persistent configuration. `/run/tvauth-spike` was removed afterwards.

This proves the static ARMv7 hard-float Rust binary executes from `/run` both
under the SSH root domain and under a PID-1-owned systemd unit on this firmware,
despite UEP/SMACK. It does not prove that a dynamically linked or installed
package binary will pass the same checks.

## Not validated

- Runtime RSS on the TV was not measured.
- This spike does not implement or test the broker, a persistent systemd unit,
  or a TV installation procedure.
- The test used the SSH `System::Privileged` domain, not an application package
  label.
