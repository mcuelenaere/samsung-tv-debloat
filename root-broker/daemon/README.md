# tv-authd — M1 headless broker

This is the M1 broker core. It runs as root, listens on a local Unix socket,
identifies callers through `SO_PEERCRED`/`SO_PEERSEC`, checks a pre-seeded
package grant, and executes a shell command with bounded time and output.

The approval UI is not connected yet. M1 exists to validate the daemon and its
wire contract before adding prompts and grants management.

## Host tests

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover request validation, framing limits, strict grant-file parsing,
malformed/denied requests, execution, process-group timeouts, and output
truncation. Linux integration tests run in Docker:

```sh
docker run --rm --platform linux/amd64 -v "$PWD:/src" -w /src rust:1-bookworm \
  sh -eu -c 'cargo test --locked'
```

## ARMv7 build

```sh
./build-armv7.sh
```

This produces a static stripped ARMv7 musl binary in `out/tv-authd` and checks
its `--help` output under ARM emulation.

## Grant file

The default path is `/opt/tv-auth/grants.json`. M1 does not create or modify grants
from requests. A root administrator pre-seeds exact package labels:

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

The file must not be group- or world-writable. The grant key is the exact SMACK
package label observed by the daemon.

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

## Test clients

`test-clients/` contains two managed `.NET` service applications that send a
broker `exec` request and save the response under `/tmp/tvauth-<probe>.response`:

```sh
./test-clients/build-test-clients.sh
```

The resulting unsigned TPKs were signed with the existing Public TV certificate
and installed through `pkgcmd`. Only `org.tvauth.spike.brokerone` was granted.
`brokerone` received a successful response with root `id` output; `brokertwo`
received `{"status":"denied","error":"not_granted"}`. Both packages were removed
after validation.

## Deployment status

Deployed live on 2026-11-05:

1. `/opt/tv-auth` and `/opt/tv-units` are root-owned mode `0700`, SMACK
   `System::Shared`.
2. `/opt/tv-auth/tv-authd` is installed mode `0755`; grants file is `0600`.
3. The dynamic AT-HUB hook discovers `/opt/tv-units/*.service`; it started the
   broker in its own systemd cgroup without putting it in AT-HUB's cgroup.
4. Live tests confirmed granted execution, denied execution, correct
   package-label attribution, socket mode `0660 root:users`, and daemon restart.
5. Test packages, grants, and temporary files were removed after validation.

Cold boot passed on 2026-11-05: the boot ID changed, AT-HUB logged
`start units rc=0`, both Dropbear and `tv-authd` became active from
`/opt/tv-units` without manual help, the broker recreated its socket, and the
hardened root-only Dropbear source was used.
