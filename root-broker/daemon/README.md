# tv-authd — root broker with approval UI

`tv-authd` runs as root, listens on a local Unix socket, identifies callers
through `SO_PEERCRED`/`SO_PEERSEC`, and executes a shell command only after an
exact package-level grant. M1 validated the headless broker. M2 adds the
interactive approval flow, persistent **Always allow** grants, and the managed
.NET grant manager under `root-broker/ui/`.

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

Tests cover request parsing, framing limits, strict grant-file parsing, role
separation, approval queueing, deny/once/always, nonce rejection, timeouts, UI
disconnect, grant persistence/revocation, execution, process-group timeouts, and
output truncation. Linux integration tests run in Docker:

```sh
docker run --rm --platform linux/amd64 -v "$PWD:/src" -w /src rust:1-bookworm \
  sh -eu -c 'cargo test --locked'
```

## ARMv7 build

```sh
./build-armv7.sh
```

This produces a static stripped ARMv7 musl binary in `out/tv-authd` and checks
its `--help` output under ARM emulation. The validated M2 build is 546,388 bytes,
SHA-256 `d93890d3f4104fee7fc537e927010dc05b8c06777944a671c2262a558c4a5ae1`
(M1 was 461,940 bytes).

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

## Grant file

The default path is `/opt/tv-auth/grants.json`. It is root-only mode `0600` and
must not be group- or world-writable. The approval UI creates **Always allow**
entries and removes them on revoke:

```json
{
  "version": 1,
  "grants": {
    "User::Pkg::org.example.consumer": {
      "granted_at": 1790000000
    }
  }
}
```

The key is the exact SMACK package label observed by the daemon. Updates use a
temporary file plus `rename`, so a crash cannot leave a partial JSON document.
Corrupt or unreadable grants fail closed. A root administrator may still pre-seed
the file manually; the running daemon only reads it at startup, so restart the
service after external edits.

## Runtime layout

```text
/opt/tv-units/
    tv-authd.service

/opt/tv-auth/
    tv-authd
    grants.json

/run/tv-auth/
    tv-authd
    broker.sock
    tv-authd.log
```

The systemd unit stages the root-only binary into `/run/tv-auth` and runs it
from there. The socket is mode `0660`, owner `root:users` (gid 100 on this TV),
so UID-5001 apps in the `users` group can connect; package identity is still
enforced by the daemon.

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
