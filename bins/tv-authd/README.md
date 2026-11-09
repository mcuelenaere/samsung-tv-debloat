# tv-authd — root broker with approval UI

`tv-authd` runs as root, listens on a local Unix socket, identifies callers
through `SO_PEERCRED`/`SO_PEERSEC`, and executes a shell command only after an
exact package-level grant. M1 validated the headless broker. M2 added the
interactive approval flow, persistent **Always allow** grants, and the managed
.NET grant manager under `root-broker/ui/`. The live branch also has per-app
execution toasts; M4 binds grants to the installed package signature and adds
failure-path/probe coverage. Protocol v2 adds package management (see
`../../docs/tv-authd-pkg-install-plan.md`): `pkg.install` with staging, manifest
inspection, trust enrollment, and a package-aware prompt (M1), plus
`pkg.uninstall`, `pkg.list`, and the prompt-free uid-0 root role (M2), plus
on-device re-signing of unsigned/foreign-signed packages (M3). All three are
TV-validated for the `pkgcmd` TPK path; WGT installs and Samsung Home
registration remain M4.

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
only use UI methods. Ordinary packages may use `exec` and prompted `pkg.*`;
uid 0 with the `System::Privileged` label may use `pkg.*` without a prompt
(never `exec`). A missing, unexpected, or non-package label is rejected.

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
both streams, EINTR handling, package staging (allowlist, symlinks, size caps,
cleanup), zip hygiene and manifest parsing, identity/privilege policy,
signature labelling, trust material and mount parsing (two CAs), `pkgcmd`
output handling, the full install prompt/approve/deny/failure flows, uninstall
prompting and policy (`protected_package`, `unknown_package`), `pkg.list`
database parsing, the root role (prompt-free install/removal with a notice,
`exec` refused), and re-signing (templates, reference encoding, signature-file
recognition, author-mismatch refusal, archive rewrite), and the WAS backend
(`/etc` symlink overlay without `release`, wascmd invocation, pkgcmd+Home
fallback). The
integrated suite is 130 Linux tests. Linux integration
tests run in Docker:

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
checks its `--help` output under ARM emulation. A full build takes about 8 s
(the previous fully emulated build took about 100 s). With package-install M1
through M4 on top of M4 and the review-fixes hardening the binary is 864,820
bytes, SHA-256
`36ed2cdc1e2e828255b936f0035a12c192309fdfa2e8a9c9a3f810acd74d38b7`
(TV-validated for install/uninstall/list/re-signing, the root role, and Samsung
Home registration; see the package-install plan). The validated M3 build was
855,716 bytes, SHA-256
`ab637c0287f439221a4b545e5a63a1440acaaf45f007ae95176e9f946d0bdcec`; the
validated M2 build was 787,372 bytes, SHA-256
`50e246fa4275dbd2bf6cedf05dc4b0a080a354d4bf9ef1d2f092ec8f0c37e915`; the
validated M1 build was 762,228 bytes, SHA-256
`4b2a51e4d73f498fd0688206bd6b93f55e272029d85e638c847196343efafd62`; the
validated M4 build was 584,452 bytes, SHA-256
`7903c0bac20e27ba3fb1de6b8def5424507e9d9f5c8ab77034c9f5cf5000f464`.

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
| `--staging-dir` | `/opt/tv-auth/staging` | package staging (`0711`, wiped at start) |
| `--max-package-bytes` | `134217728` (128 MiB) | staged package cap (max 256 MiB) |
| `--pkgcmd` | `/usr/bin/pkgcmd` | stock installer |
| `--sqlite3` | `/usr/bin/sqlite3` | read-only package database access |
| `--pkgdb` | `/opt/dbspace/.pkgmgr_parser.db` | pkgmgr package database |
| `--xmlsec1` | `/usr/bin/xmlsec1` | on-device XML-DSig signer |
| `--no-was` | off | use `pkgcmd` only (no Samsung Home registration) |
| `--trust-root` | `/opt/tv-auth/trust` | CA material and fingerprint list |
| `--no-trust` | off | skip trust enrollment (development only) |
| `--no-package-support` | off | serve `exec` only |
| `--enroll-trust` | off | verify/repair the bind mounts and exit |

## Package installation

`pkg.install` stages a wgt/tpk into `/opt/tv-auth/staging/` (`0711`; staged
files `0644`; wiped at startup), parses its manifest, and prompts with
`kind: "install"`. Only after
the user approves does it enroll trust and run `pkgcmd -i -t <type> -p <staged>`.
The source allowlist is `/opt/usr/home/owner/**`, `/opt/media/**`, and `/tmp/**`;
packages are capped at 128 MiB by default (`--max-package-bytes`, up to
256 MiB). Package operations never use the grant store and are always
interactive for app clients; see `PROTOCOL.md`.

Unsigned and foreign-signed packages are re-signed on-device after approval
(M3). Tizen validates two chains with different domains: the author root must be
in `tizen-developer`, and the distributor root in `tizen-public`/`tizen-partner`
but *not* in `tizen-developer`. The daemon therefore enrolls two CAs and writes
`author-signature.xml` (`role-author`, all files) and `signature1.xml`
(`role-distributor`, the author signature plus all files) through `xmlsec1`.
It rewrites the staged archive in place, sets `re_signed: true`, and lets
`pkgcmd` validate the result. An upgrade is refused with
`author_mismatch_requires_reinstall` when the installed package was signed by a
different author, because Tizen rejects cross-author upgrades (`-12`).

Installs and removals go through **`webappservice`**, the same path `sdb
install` uses. The daemon runs the stock `/usr/bin/wascmd` under
`unshare -m` with a top-level symlink overlay of `/etc` that has no `release`
file, because that binary exits immediately on retail firmware. The overlay is
a directory of symlinks (one per `/etc` entry, pointing into a bind mount of
the real `/etc`) created in ~1 ms; no file data is copied:

- TPK installs now create the `widgetinfo` row and cached icon that Samsung
  Home ("My apps") reads, and removals delete them;
- the staged package keeps its `0711`/`0644` layout because `webappservice`
  copies it as UID 5001;
- the overlay is reused; only the top-level symlink list is rebuilt before each
  operation (one directory read), so startup and package operations are not
  delayed by `/etc` contents;
- `wascmd -u` refuses packages that were not installed through the SDK path
  (`is_sdk_app`); the daemon then falls back to `pkgcmd -u` plus an explicit
  Home cleanup (delete the `widgetinfo` row, remove the cached icon, set
  `memory/was/app_change=1`);
- `--no-was` restores the `pkgcmd`-only backend. WGT packages cannot be
  installed on this firmware through any backend (`pkgcmd`, `sdb install`, or
  `wascmd` all return `-4 Load archive info fail`), so WGT remains blocked.

`pkg.uninstall` reads the installed package's metadata from
`/opt/dbspace/.pkgmgr_parser.db` through `sqlite3 -readonly` (the
`package_info` table joined with the "No Locale" label row). Reading the
database directly matters: `pkginfo --listpkg` enumerates every app over D-Bus
and takes about 16 s on this firmware, while the same rows are a ~10 ms query.
Only non-system, non-preload, removable `wgt`/`tpk` packages can be removed, and
platform-reserved package ids are refused first. `pkgcmd -u` takes the *package
id* via `-n` (`-p` is a package path and fails with `realpath fail`). Installs
and removals share one global lock.

`pkg.list` is the same read-only query and returns `pkgid`, `name`,
`pkg_version`, `pkg_type`, and `main_app_id` for every installed user package,
so an app store (for example Vetro) can tell what is installed or upgradable.

The root role (UID 0 with the exact `System::Privileged` label) may call
`pkg.install`/`pkg.uninstall`/`pkg.list` without a prompt, with the same
staging, policy, trust, and audit pipeline. Successful prompt-free operations
queue a `ui.notice` (`kind: "install"`/`"uninstall"`) so the user sees a toast;
`exec` remains package-only. `tvauth-cli` (`../tv-auth-cli`) is a static ARMv7 client for
this role (run it from `/run`: the VDFS in `/opt` rejects unsigned ELFs).

Trust enrollment and M3 signing need `/opt/tv-auth/trust/` containing
`user-root-ca.pem` (distributor root), `author-root-ca.pem` (author root),
`fingerprint_list.xml`, `tizen-ca/` (both roots), and the four signing files
`user-author.key`/`.pem` and `user-distributor.key`/`.pem` (keys `0600`). The daemon verifies the material and
bind-mounts it over the stock `/usr/share/ca-certificates/...` paths **in PID
1's mount namespace** (`nsenter -t 1 -m`). This matters: `tv-authd.service`
uses `BindReadOnlyPaths=`, so the daemon lives in a private mount namespace,
while `pkgcmd` delegates the install to `pkgmgr-server` (`package-manager.service`,
`User=app_fw`, `PrivateMounts=no`), which only sees the root namespace. Mounts in
the daemon's namespace would be invisible to the validator. The mounts are
re-established at daemon start and before each install; `tv-authd
--enroll-trust` runs the same check once and exits.

Host validation covers staging, zip hygiene, manifest parsing, policy refusals,
approval plumbing, and `pkgcmd` invocation with a fake installer. TV validation confirmed the PID-1 root-namespace enrollment, the TPK install
flow, the prompt, deny and error paths, and cold-boot persistence (see
`../../docs/tv-authd-pkg-install-plan.md` §14).

### Native UI staging and self-protection

The approval UI is now a native `type="capp"` package. UEP refuses to map an
ELF under `/opt`, so `ui_alias.rs` bind-mounts the installed binary read-only at
`/run/tv-auth/ui-bin/tv-auth-ui` (in PID 1's mount namespace) and points
`bin/tv-auth-ui` at it with a symlink. It runs at daemon start, restores the
packaged file before an install/upgrade, and re-establishes the alias after a
successful install or a failed upgrade. `--ui-exec` and `--ui-alias-dir` override
the defaults. A missing native UI (for example the managed `.NET` package) is a
no-op.

Because the UI is signed with the on-device CA, Tizen's same-author check would
otherwise let an ordinary package replace or remove it. `pkg.install` and
`pkg.uninstall` for `--ui-package` are therefore root-only; ordinary packages
get `manifest_forbidden` / `protected_package`. Both guards were exercised on
the live TV on 2026-11-08.

The native UI was deployed live on 2026-11-08 (daemon `86a7bcee…`, UI TPK
`0b71a21f…`). A cold boot recreated the `/run` alias (`ui_alias=mounted`), kept
the Home row and Vetro grant, and the UI rendered the manager with the expected
SMACK label and a `/run/tv-auth/ui-bin/tv-auth-ui` mapping. The previous M4
daemon is at `/opt/tv-auth/tv-authd.m4-20261108`. The managed UI source remains
at git commit `7b39883`; a Partner-signed rollback TPK produced before removal
was kept locally under `ui/out/` (not committed).

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

The key is the exact SMACK package label observed by the daemon. `notify`
(`every`, `first` or `never`; see `PROTOCOL.md`) is optional and only written
when it is not `every`. `package_fingerprint` is a SHA-256 of the installed
package's `author-signature.xml`:

- TPK: `/opt/usr/apps/<pkgid>/author-signature.xml` (or `/usr/apps/...`);
- WGT: `/opt/usr/apps/<pkgid>/res/wgt/author-signature.xml` (or `/usr/apps/...`).

The signature covers the manifest and signed files. A package upgrade, changed
reinstall, or different signer produces a different fingerprint; the grant is
removed and the next request prompts again. An identical reinstall keeps the
grant. The signature file and package directory must not be symlinks or
group/world-writable, and missing identity fails closed. Version-1 label-only
grants are migrated on first use by binding them to the package currently
installed under that label; a non-default `notify` mode is preserved.

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

/opt/tv-auth/              # 0711 (traversal for UID 5001)
    tv-authd
    grants.json            # 0600
    staging/               # 0711, 0644 files, wiped at daemon start
    trust/                 # 0700
        user-root-ca.pem
        fingerprint_list.xml
        tizen-ca/

/run/opt/tv-auth/          # per-service read-only bind mount of /opt/tv-auth
    tv-authd

/run/tv-auth/              # 0711
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
startup). The VDFS in `/opt` rejects unsigned ELFs, so the daemon (and any
`tvauth-cli` test client) executes from a copy under `/run`, not from
`/opt/tv-auth` directly.

## Approval UI

The UI is maintained separately under `../tv-auth-ui/`:

```sh
../tv-auth-ui/build-ui.sh             # unsigned native TPK
```

It is a native Rust/EFL `type="capp"` app. UEP rejects ELFs mapped from `/opt`,
so the daemon's `ui_alias.rs` exposes the installed binary through a `/run`
bind mount. The daemon launches it through `app_launcher -s org.tvauth.ui` only
when a prompt or toast is needed. See `../tv-auth-ui/README.md` for the UI's
behavior, build, signing, and limitations.

## Test clients

`test-clients/` contains two managed `.NET` service applications that send a
broker `exec` request and save the response under `/tmp/tvauth-<probe>.response`:

```sh
./test-clients/build-test-clients.sh
```

They were used for M1 with a pre-seeded grant. In M2 they are useful as
untrusted requesters: the daemon should launch the UI instead of returning
`not_granted`. Their hard-coded response handling is unchanged, and the request
file can carry any method, so the same probe drove `pkg.uninstall` and
`pkg.list` for M2 validation.

`../tv-auth-cli` (`tvauth-cli`) is a small static Rust client for the uid-0 role:

```sh
../tv-auth-cli/build-armv7.sh
# on the TV: copy out/tvauth-cli to /run and run
/run/tvauth-cli list
/run/tvauth-cli install tpk /tmp/org.example.app-0.1.0.tpk
/run/tvauth-cli uninstall org.example.app
```

`./test-clients/build-upgrade-client.sh` builds a 0.1.1 `brokerone` with changed
content for the M4 package-upgrade test (same package ID/signer, different
signed DLL/manifest, so the grant fingerprint must change).

## Deployment status

M1 deployed and cold-boot validated on 2026-11-05:

1. `/opt/tv-auth` is root-owned mode `0711` (with `grants.json` `0600`,
   `staging/` `0711`, and `trust/` `0700`); `/opt/tv-units` is `0700`. SMACK
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

The package-install M2 work (uninstall, `pkg.list`, root role) was deployed and
TV-validated on 2026-11-08:

- root `pkg.list` and app `pkg.list` returned 126/127 installed user packages
  in milliseconds;
- root `pkg.install` and `pkg.uninstall` ran without a prompt and produced
  `PACKAGE CHANGE` toasts ("root installed a package", "root removed a
  package");
- an app-role uninstall prompt showed requester, name, id, version, and type;
  Deny returned `user_denied` with the package intact and Remove returned
  `status: ok`;
- `pkgcmd -u -n <pkgid>` is the working uninstall form; `-p` is a package path.

M3 (on-device re-signing) was deployed and TV-validated on 2026-11-08:

- a foreign-signed TPK installed through the broker prompt returned
  `status: ok` with `re_signed: true` and `result=signed files=7`; `pkgcmd`
  accepted both new chains and the package installed;
- the installed `author-signature.xml` carries two certificates and the
  `role-author` property, and the UI badge read "Third-party signature — it
  will be re-signed with this TV's CA";
- stripping the signatures from the Partner-signed UI package and installing it
  over the installed UI was refused with
  `denied/author_mismatch_requires_reinstall` before any prompt;
- the domain rule was found the hard way: a single CA in all three domains
  makes every re-signed package fail with `-12 Invalid certificate chain`,
  because the distributor root may not be in `tizen-developer`.

Uninstalling a package that was installed through `sdb` leaves its Samsung Home
`widgetinfo` row behind, because `pkgcmd` does not contact `webappservice`; M4
owns that cleanup. The stale row from the probe was removed manually after
validation.

M4 (the `webappservice` backend) was deployed and TV-validated on 2026-11-08:

- a broker TPK install returned `backend=was`, and Samsung Home gained the
  `widgetinfo` row (`app_featured=1`, `app_type=2`, `install_type=user`,
  `is_display=1`, `install_state=3`) plus the cached icon;
- a broker uninstall returned `backend=was` and removed the package, the Home
  row, and the icon;
- a cold boot (2026-11-08 22:47) reproduced the whole flow: the AT-HUB hook
  started the daemon (`start units rc=0`), `starting`/`staging_ready`/
  `was_ready`/`listening` happened within one second (symlink overlay), trust re-enrolled in PID 1 with both roots
  (`author-root-ca.pem` + `user-root-ca.pem` in `tizen-ca`, fingerprints 1 + 2),
  the `org.tvauth.ui` Home row survived, Vetro resumed executing, and broker
  install/uninstall still produced and removed the Home row and icon;
- a package installed with `pkgcmd` (the `is_sdk_app` case) uninstalled through
  the fallback (`backend=pkgcmd+home`), which deleted the manual Home row and
  icon;
- WGT remains blocked: a minimal WGT, a previously built test WGT, and a
  repackaged copy of the installed Vetro WGT all fail with
  `-4 Load archive info fail` through `wascmd`/`webappservice`, matching the
  earlier `pkgcmd` and `sdb install` failures.

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
- The manager-refresh UI was installed in place on 2026-11-08 using the
  Apps2Samsung **Partner** certificate (the installed UI's author certificate
  matches exactly). Use the Partner profile for future UI updates.
- Cold boot passed on 2026-11-08 (boot ID `c0b55e8d-22a6-4fb6-8c70-a755bbfcab86`):
  AT-HUB `start units rc=0`, both services active with 0 restarts, the M4 binary
  and Partner-signed UI survived, the v2 Vetro grant loaded with `notify: first`
  and no migration/stale events, Vetro commands resumed `exit_code=0`, and the
  post-boot UI showed the manager with **Toast: first use**.
