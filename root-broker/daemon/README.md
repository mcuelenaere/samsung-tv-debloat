# tv-authd — root broker with approval UI

`tv-authd` runs as root, listens on a local Unix socket, identifies callers
through `SO_PEERCRED`/`SO_PEERSEC`, and executes a shell command only after an
exact package-level grant. M1 validated the headless broker. M2 added the
interactive approval flow, persistent **Always allow** grants, and the managed
.NET grant manager under `root-broker/ui/`. The live branch also has per-app
execution toasts; M4 binds grants to the installed package signature and adds
failure-path/probe coverage.

The wire contract is in [`PROTOCOL.md`](PROTOCOL.md).

## How a request is decided

```text
ordinary package --exec--> tv-authd
                              │
                    exact persistent grant?
                    yes ──────┴──────> execute /bin/sh -c
                    no
                              │
                              ▼
                    ui.prompt over the same socket
                              │
              ┌───────────────┼───────────────┐
           deny            allow once      always allow
              │               │               │
          no execute       execute once    persist grant
                                          then execute
```

The approval UI is `User::Pkg::org.tvauth.ui` (configurable), UID 5001. It may
only use UI methods; ordinary packages may only use `exec`. A missing, unexpected,
or non-package label is rejected.

Only one prompt is visible globally, and only one request per package label may
be pending. Prompt decisions are bound to a fresh 128-bit nonce, the prompt ID,
the registered UI connection, and a timeout. Timeouts, UI disconnects, launcher
failures, and stale/nonce-mismatched decisions all deny without executing.

## Host tests

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover request parsing, framing limits, oversized frames, strict
grant-file parsing and corruption, package-signature binding and legacy
migration (including preserving `notify`), per-app notify modes, role
separation, approval queueing and queue exhaustion, deny/once/always, nonce
rejection, timeouts, UI disconnect, requester hangup, grant
persistence/revocation, execution, process-group timeouts, output truncation in
both streams, and EINTR handling. The integrated suite is 64 Linux tests. Linux
integration tests run in Docker:

```sh
docker run --rm --platform linux/amd64 -v "$PWD:/src" -w /src rust:1-bookworm \
  sh -eu -c 'cargo test --locked'
```

## ARMv7 build

```sh
./build-armv7.sh
```

This cross-compiles a static stripped ARMv7 musl binary into `out/tv-authd`
from a native (amd64/arm64) Rust 1.85 container, linking with `rust-lld`, then
checks its `--help` output under ARM emulation. A full build takes about 11 s
(the previous fully emulated build took about 100 s). This main-branch M4 port
preserves the earlier toast implementation and builds as 586,316 bytes, SHA-256
`a5e4094044c142e97a0a54cd893b1d81940edc05104a15fcf18e6e4c9c44ff74`
(the live TV currently runs the per-app toast branch build: 584,452 bytes,
`7903c0ba…`). M2 was 546,388 bytes; M1 was 461,940 bytes.

## CLI defaults

| Option | Default | Purpose |
|---|---|---|
| `--socket` | `/run/tv-auth/broker.sock` | Unix socket |
| `--grants` | `/opt/tv-auth/grants.json` | persistent grants |
| `--log` | `/run/tv-auth/tv-authd.log` | bounded log |
| `--socket-gid` | `100` | `root:users` socket group |
| `--ui-package` | `org.tvauth.ui` | approved UI package |
| `--ui-uid` | `5001` | approved UI UID |
| `--ui-app-id` | same as `--ui-package` | AUL app ID |
| `--app-launcher` | `/usr/bin/app_launcher` | fixed launcher |
| `--prompt-timeout-ms` | `60000` | decision timeout |
| `--ui-launch-timeout-ms` | `10000` | wait for UI hello |
| `--launch-cooldown-ms` | `1000` | relaunch rate limit |
| `--prompt-queue-max` | `8` | pending requests |
| `--max-launch-attempts` | `3` | UI launches per request burst |

The launcher inherits the daemon's systemd environment. On this TV that includes
`XDG_RUNTIME_DIR=/run`, which AUL's launcher path uses. Do not clear it.

## Grant file and package identity

The default path is `/opt/tv-auth/grants.json`. It is root-only mode `0600` and
must not be group- or world-writable. The approval UI creates **Always allow**
entries and removes them on revoke:

```json
{
  "version": 2,
  "grants": {
    "User::Pkg::org.example.consumer": {
      "granted_at": 1790000000,
      "notify": "first",
      "package_fingerprint": "sha256:..."
    }
  }
}
```

The key is the exact SMACK package label observed by the daemon. `notify` is
optional and preserved for compatibility with grants written by the live per-app
toast branch; this branch's simpler toast code does not interpret it.
`package_fingerprint` is a SHA-256 of the installed package's
`author-signature.xml`:

- TPK: `/opt/usr/apps/<pkgid>/author-signature.xml` (or `/usr/apps/...`);
- WGT: `/opt/usr/apps/<pkgid>/res/wgt/author-signature.xml` (or `/usr/apps/...`).

The signature covers the manifest and signed files. A package upgrade, changed
reinstall, or different signer produces a different fingerprint; the grant is
removed and the next request prompts again. An identical reinstall keeps the
grant. The signature file and package directory must not be symlinks or
group/world-writable, and missing identity fails closed. Version-1 label-only
grants are migrated on first use by binding them to the package currently
installed under that label; a preserved `notify` value is retained.

Daemons from before `notify` existed reject grant files that contain it, so roll
back only after resetting it to `every`. Updates use a temporary file plus
`rename`, so a crash cannot leave a partial JSON document. A corrupt or untrusted
grants file prevents startup (fail closed); move it aside to recover, then
re-approve applications. A root administrator may still pre-seed the file
manually; the running daemon only reads it at startup, so restart the service
after external edits.

## Runtime layout

```text
/opt/tv-units/
    tv-authd.service

/opt/tv-auth/
    tv-authd
    grants.json

/run/opt/tv-auth/          # per-service read-only bind mount of /opt/tv-auth
    tv-authd

/run/tv-auth/
    broker.sock
    tv-authd.log
```

The systemd unit **bind-mounts** the root-only binary at `/run/opt/tv-auth`
(a private read-only mount, outside UEP's checked prefixes) and runs it from
there — nothing is copied into `/run`. `/run/tv-auth` is mode `0711`: apps can
traverse it to connect to the known socket path but cannot list runtime entries.
The socket is mode `0660`, owner `root:users` (gid 100 on this TV), so UID-5001
apps in the `users` group can connect; package identity is still enforced by the
daemon. The daemon log is mode `0600` (and repaired from older `0644` files on
startup).

## Approval UI

The UI is maintained separately under `root-broker/ui/`:

```sh
./root-broker/ui/build-ui.sh          # unsigned managed TPK
```

It is a managed `netcoreapp3.1` Tizen .NET app (`type="dotnet"`), not a native
ELF, because UEP rejects unsigned native binaries from writable app storage. It
uses the validated notification-window path and a small direct P/Invoke layer.
The daemon launches it through `app_launcher -s org.tvauth.ui` only when a
prompt is needed. See `root-broker/ui/README.md` for the UI's behavior, build,
signing, and limitations.

## Test clients

`test-clients/` contains two managed `.NET` service applications that send a
broker `exec` request and save the response under `/tmp/tvauth-<probe>.response`:

```sh
./test-clients/build-test-clients.sh
```

They were used for M1 with a pre-seeded grant. In M2 they are useful as
untrusted requesters: the daemon should launch the UI instead of returning
`not_granted`. Their hard-coded response handling is unchanged.

`./test-clients/build-upgrade-client.sh` builds a 0.1.1 `brokerone` with changed
content for the M4 package-upgrade test (same package ID/signer, different
signed DLL/manifest, so the grant fingerprint must change).

## Deployment status

M1 deployed and cold-boot validated on 2026-11-05:

1. `/opt/tv-auth` and `/opt/tv-units` are root-owned mode `0700`, SMACK
   `System::Shared`.
2. `/opt/tv-auth/tv-authd` is installed mode `0755`; grants file is `0600`.
3. The dynamic AT-HUB hook discovers `/opt/tv-units/*.service`; it starts the
   broker in its own systemd cgroup without putting it in AT-HUB's cgroup.
4. Live tests confirmed granted execution, denied execution, correct
   package-label attribution, socket mode `0660 root:users`, and daemon restart.
5. Test packages, grants, and temporary files were removed after validation.

M2 is deployed and TV-validated. The signed `org.tvauth.ui` package is installed,
the broker runs the M2 binary, and the grants file is empty after test cleanup.
Validated on the target:

- Deny returned `denied`/`user_denied` without execution.
- Allow once returned a root `id` result, persisted no grant, and prompted again
  on the next request.
- Always allow persisted the exact label and executed; later requests ran with no
  prompt, including after a daemon restart and a cold boot.
- Manager revoke returned `revoked=true`, emptied the grant file, and the next
  request prompted.
- Prompt timeout returned `denied`/`prompt_timeout`; killing the UI during a
  pending prompt returned `ui_unavailable`/`ui_disconnected`. Neither executed.
- `input_keyevent` verified RETURN, Right/Left focus movement, and BACK. The
  manager list renders about two seconds after launch, so scripts must wait for
  the grants response before injecting keys.

Cold boot on 2026-11-05 used boot ID
`7687b045-4e4b-4a7e-9416-68ecc3fc6efc`; AT-HUB logged `start units rc=0`, both
services were active, the unit environment included
`XDG_RUNTIME_DIR=/run HOME=/root`, and the persisted grant allowed an execution
without launching the UI. The test requester package, test grant, `/tmp` files,
and `/run/tvauth-test` were removed afterwards.

M3/Vetro is live on the toast branch (`1471212`), and M4 was deployed on top of
it on 2026-11-08 with a short restart:

- Binary `7903c0ba…` (584,452 bytes). Vetro's label-only grant migrated on its
  next command to `sha256:ed44a7da…` with `notify: first` preserved; Vetro
  commands and toasts continued, and `notice_sent` was suppressed after the
  first-use notice.
- `fsprobe`/`uiprobe` passed; post-M4 `/run/tv-auth` is `0711`, the daemon log is
  `0600` and app-read denied.
- `brokerone` 0.1.0 was granted Always; the changed 0.1.1 upgrade logged
  `grant_stale reason=package_identity_changed` and prompted, while an identical
  0.1.1 reinstall kept the grant. The test package/grant were removed.
- Normal operation: `tv-authd` 328 kB RSS / 356 kB HWM, 3 threads, ~0.4% CPU
  during Vetro polling. Under 64 MiB tmpfs pressure it stayed at 304 kB RSS /
  384 kB HWM; the UI stayed at 50,288 kB RSS / 53,036 kB HWM; no restart/OOM.
- The UI manager-refresh fix is in source but not installed: the live
  `org.tvauth.ui` is signed with a different author certificate and `pkgcmd -i`
  rejects the update (`Author certificate not match`). Re-sign with the installed
  profile or uninstall/reinstall.
- This main-branch port builds as 586,316 bytes (`a5e40940…`) and preserves the
  earlier toast code. It is not the binary currently deployed. The TV runs the
  toast-branch build `7903c0ba…` (584,452 bytes). Do not deploy the main binary
  over the live toast UI without merging that branch.
- Optional remaining check: a cold boot with the live toast-branch M4 binary and
  v2 grant.
