# Unix peer-identity spike

This is a small blocking Linux `AF_UNIX`/`SOCK_STREAM` server and client for checking the two kernel-provided identities needed by `tv-authd`:

- `SO_PEERCRED`: peer PID, UID, and GID;
- `SO_PEERSEC`: peer LSM/security label, which must be a package-specific SMACK label on the TV.

The server acquires both identities immediately after `accept(2)`, before reading any peer bytes. It handles up to a caller-specified number of connections sequentially. Each client sends one harmless newline-terminated probe (at most 128 bytes), and the server replies `ok`. Logs contain escaped, bounded probe/label data so the two connections can be correlated without allowing log injection.

`libc` is the only dependency. There is no async runtime or authorization policy in this spike.

## Result summary

| Validation | Result |
|---|---|
| x86_64 Linux build, unit tests, and two-client host test in Docker | **Passed** for transport and `SO_PEERCRED`; `SO_PEERSEC` returned `ENOPROTOOPT` |
| ARMv7 `armv7-unknown-linux-musleabihf` release build | **Passed**; static, stripped ARM hard-float ELF |
| Two-client test of the ARMv7 artifact in Docker `linux/arm/v7` emulation | **Passed** for transport and `SO_PEERCRED`; `SO_PEERSEC` returned `ENOPROTOOPT` |
| `SO_PEERSEC` on TV | **Passed** for both the SSH root domain and packaged UID-5001 apps |
| SMACK/package identity | **Passed** with two separately installed managed .NET packages: distinct `User::Pkg::*` labels observed |
| Actual Samsung TV/Tizen execution | **Validated** for the server binary under `/run` and for managed packaged clients |

The development containers do not provide SMACK through `SO_PEERSEC`. The test output says `SMACK CHECK: NOT VALIDATED`, rather than treating an unsupported security-label query as success.

### TV `SO_PEERSEC` execution (2026-11-04)

The exact static ARMv7 artifact was staged under `/run/tvauth-spike` and run as
a two-connection server plus two root-shell clients. The artifacts were removed
after the test. Both clients reported the same UID and the kernel returned a
security label:

```text
client_pid=19748 probe="probe-one" reply="ok"
client_pid=19749 probe="probe-two" reply="ok"
connection=1 so_peercred=present pid=19748 uid=0 gid=0 so_peersec=present label="System::Privileged" probe="probe-one"
connection=2 so_peercred=present pid=19749 uid=0 gid=0 so_peersec=present label="System::Privileged" probe="probe-two"
```

This proves `SO_PEERSEC` is implemented and returns SMACK labels on the target,
including for an SSH-root process running under `/run`.

### Packaged UID-5001 validation (2026-11-05)

The first native packaged probes installed correctly but never executed because
UEP blocks unsigned native ELFs from writable app storage; launchpad never
mapped them. The final probes are two managed `netcoreapp3.1` applications
packaged as Tizen `dotnet` service apps. This is the same execution class as
Vetro's managed/web runtime and avoids the native-ELF UEP wall.

After installing both signed TPKs and launching them through `app_launcher`
against the `/run` server, the server observed:

```text
connection=1 so_peercred=present pid=28911 uid=5001 gid=100 so_peersec=present label="User::Pkg::org.tvauth.spike.peerone" probe="dotnet-peer-one"
connection=2 so_peercred=present pid=28925 uid=5001 gid=100 so_peersec=present label="User::Pkg::org.tvauth.spike.peertwo" probe="dotnet-peer-two"
```

This validates the required broker identity model: same shared UID, distinct
PIDs, and distinct package labels. The server process and both test packages
were removed after the test.

Build sources are under `apps/`; `build-dotnet-test-apps.sh` creates the
unsigned managed TPK roots, and signing used the existing TV Public certificate.
No certificate or password is stored in the repository.

## Files and behavior

- `src/main.rs`: CLI, blocking server/client, bounded line reader, and small `getsockopt(2)` wrappers.
- `scripts/host-test.sh`: runs two clients and checks their PIDs, current UID/GID, probes, replies, and explicit `SO_PEERSEC` status.
- `scripts/build-armv7-musl.sh`: builds and copies the static ARMv7 release artifact into `out/`.
- `target/` and `out/`: local build products, excluded by this spike's `.gitignore`.

`SO_PEERSEC` results are deliberately distinguished:

- `so_peersec=present`: a non-empty label was returned (this alone does **not** prove that the LSM is SMACK);
- `so_peersec=unsupported`: the kernel returned `ENOPROTOOPT` or `EOPNOTSUPP`;
- `so_peersec=missing`: a zero-length/empty label or `ENODATA` was returned;
- `so_peersec=error`: any other error, which fails the host test.

A present label is printed with trailing NUL bytes removed. Unexpected/missing labels are not converted into a usable application identity.

## Run on Linux

```sh
cargo build --locked
./target/debug/peer-identity server /tmp/peer-identity.sock 2
```

In two other shells, or sequentially after starting the server in the background:

```sh
./target/debug/peer-identity client /tmp/peer-identity.sock probe-one
./target/debug/peer-identity client /tmp/peer-identity.sock probe-two
```

Example connection record from the actual Linux run:

```text
connection=1 so_peercred=present pid=281 uid=0 gid=0 so_peersec=unsupported errno=92 message="Protocol not available (os error 92)" probe="probe-one"
```

The container ran as root, hence UID/GID 0; the script checks against the actual current UID/GID rather than requiring root.

### Host test

```sh
./scripts/host-test.sh
# Or test an already-built binary without invoking Cargo:
./scripts/host-test.sh ./out/peer-identity-armv7-unknown-linux-musleabihf
```

The script requires Linux and exits 77 with an explicit skip message elsewhere. It uses a temporary socket, starts the server with a two-connection limit, launches each client as a separate process, and compares shell-observed client PIDs with the server's `SO_PEERCRED` records. It returns success only for checks that actually passed. Unsupported/missing security labels produce a separate **SMACK not validated** result; an unexpected `SO_PEERSEC` error fails the test.

## Commands and observed results

All host-test commands below were run on 2026-11-04 from this spike directory on an Apple Silicon macOS host, using Docker Server 29.9.2. No TV command was part of these host-test results; the later on-TV validation is recorded separately above.

### Linux host build and tests

```sh
docker run --rm --platform linux/amd64 \
  -v "$PWD:/spike" -w /spike rust:1-bookworm \
  sh -c 'rustc --version && cargo --version && uname -a && cargo build --locked && cargo test --locked && ./scripts/host-test.sh'
```

Environment and results:

- image digest: `sha256:c9155318d07341301a78b45ecdd81af2e43b8b297d64e8fd4c5e80d7735607c3`;
- `rustc 1.99.1 (58cf8f9c3 2026-10-13)`;
- `cargo 1.99.1 (5f94df478 2026-08-27)`;
- kernel: `Linux 7.0.14-linuxkit`, x86_64;
- locked dependency: `libc 0.2.191`;
- three unit tests passed (line at limit, over-limit rejection, log escaping);
- two distinct client PIDs (281 and 282), expected UID/GID, both probes, and both replies passed;
- both `SO_PEERSEC` calls returned `ENOPROTOOPT` (errno 92), so SMACK was explicitly **not validated**.

A lint pass also completed with no warnings:

```sh
docker run --rm --platform linux/amd64 \
  -v "$PWD:/spike" -w /spike rust:1-bookworm \
  sh -c 'rustup component add clippy && cargo clippy --locked --all-targets -- -D warnings'
```

### ARMv7 static musl build

On a Rustup-managed toolchain, run:

```sh
./scripts/build-armv7-musl.sh
```

The script installs `armv7-unknown-linux-musleabihf` if needed, selects Rust's bundled `rust-lld`, enables self-contained linking, and uses the size-conscious release profile in `Cargo.toml`. This crate needs no external C objects beyond the Rust target's musl startup objects, so no ARM GCC sysroot is required.

The actual build used the same Linux Docker image:

```sh
docker run --rm --platform linux/amd64 \
  -v "$PWD:/spike" -w /spike rust:1-bookworm \
  ./scripts/build-armv7-musl.sh
```

Artifact:

| Property | Observed value |
|---|---|
| Path | `out/peer-identity-armv7-unknown-linux-musleabihf` |
| Size | **393,976 bytes** |
| SHA-256 | `9f0a2ae63a5ad899670d42ca5d931fe51a556da3d21a46bc0880b3ea64eaef8c` |
| `file(1)` | `ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, stripped` |
| ELF flags | EABI version 5, hard-float ABI |
| `readelf -d` | `There is no dynamic section in this file.` |

### ARMv7 emulation smoke test

Docker's ARM emulation was available, so the actual ARM artifact was exercised rather than only cross-compiled:

```sh
docker run --rm --platform linux/arm/v7 \
  -v "$PWD:/spike" -w /spike alpine:3 \
  ./scripts/host-test.sh ./out/peer-identity-armv7-unknown-linux-musleabihf
```

The test passed with client PIDs 25 and 28, UID/GID 0/0, and both expected probes. Again, both `SO_PEERSEC` calls returned errno 92 and the result was **SMACK CHECK: NOT VALIDATED**. This is ARM emulation on the development host, not Tizen or on-TV evidence.

## Why TV validation needs two separately packaged app processes

The host test proves only that stream transport works and that `SO_PEERCRED` distinguishes two processes. Two clients launched from a root shell are not a valid package-identity test:

1. Their `SO_PEERCRED` values normally show root's UID/GID (or at least shell credentials), not Tizen's shared application UID 5001.
2. Their `SO_PEERSEC` label is inherited from the shell/security domain, not two different installed package domains. Even if a label is returned, two shell children do not demonstrate package separation.
3. Different PIDs are ephemeral process identifiers; they do not establish an application grant key.

On the TV, create, install, and launch **two separately packaged test applications with different package IDs**, both running as the shared app UID. Put the client logic in each application's own process and have each process connect directly to the socket and send a unique harmless probe. The server must observe the same UID but different PIDs and distinct exact labels of the form `User::Pkg::<pkgid>`, matching the respective installed package. Missing, unsupported, or unexpected labels must fail closed.

Connecting directly matters because the kernel identifies the process on the socket. A shell/helper/CLI indirection can change the observed PID and security domain and does not reproduce the planned same-process Vetro/UI integrations. Separate TV checks remain necessary for Vetro's actual service process and the approval UI, as listed in the main plan.
