# tv-auth approval and grant-manager UI

This is the native Tizen `type="capp"` UI for `tv-authd`. It has four jobs:

1. show the daemon-generated approval prompt when an application requests root;
2. show a package-aware prompt for `pkg.install` (`kind: "install"`) and
   `pkg.uninstall` (`kind: "uninstall"`);
3. list and revoke persistent **Always allow** grants;
4. show a small toast when an **Always allow** app runs a root command, and when
   a uid-0 client installs or removes a package without a prompt.

The UI is a normal UID-5001 package with the exact SMACK label
`User::Pkg::org.tvauth.ui`. It never runs as root and has no network privilege.
It talks directly to `/run/tv-auth/broker.sock`; there is no helper process.

## Why a `/run` alias

Samsung's kernel UEP module (`SfdUepVerifyExecutableBinary`) refuses to `exec`
or `dlopen` an ELF whose `d_path()` starts with `/opt`, `/tmp`, `/var`, or
`/home`. A Tizen `type="capp"` package installs its executable under
`/opt/usr/apps/<pkgid>/bin/`, so launchpad cannot map a custom native UI from
there even when the TPK is validly signed.

The verifier takes the "UEP disabled" branch when the opened file resolves
through a mount outside those roots. `tv-authd` therefore:

1. bind-mounts the installed UI executable read-only at
   `/run/tv-auth/ui-bin/tv-auth-ui` in PID 1's mount namespace;
2. keeps the real bytes at `bin/tv-auth-ui.real` and replaces
   `bin/tv-auth-ui` with a symlink to the `/run` path.

Launchpad opens the packaged path, the VFS follows the symlink into the `/run`
mount, and `d_path()` reports the `/run` path. The package, SMACK domain, AUL
registration, and Samsung Home entry are unchanged. `daemon/src/ui_alias.rs`
performs this idempotently at daemon startup and after a root install of the UI
package.

This path was validated on the TV on 2026-11-08 with the harmless
`org.tvauth.spike.popup` EFL binary:

```text
before alias: /proc/<pid>/maps contained only launchpad-loader/liblaunchpad
after alias:  00466000-004ad000 r-xp /run/tv-auth/spike-popup-bin/efl-popup
label:        User::Pkg::org.tvauth.spike.popup
RSS:          ~22.6 MB (the spike's EFL window; the prompt UI is smaller)
```

The test package and mounts were removed afterwards.

**Why not the AT-HUB gawk hook?** The `#!/usr/bin/gawk -f` hook bypasses UEP for
*scripts* because gawk does not link `libkUEPUser.so`. It does not change the
path launchpad opens for a `type="capp"` package, so it would still have to
rewrite `app_info.exec` or replace the packaged binary, and it would couple UI
correctness to boot-hook ordering. The `/run` alias stays inside `tv-authd` and
is re-established on every daemon start.

## Behavior

### Approval prompt

When an app without a persistent grant connects, the daemon launches
`app_launcher -s org.tvauth.ui` and sends a `ui.prompt` frame. The UI:

- raises a full-screen notification-type EFL window (notification level TOP,
  then HIGH, MEDIUM, DEFAULT) with a dimmed scrim and a solid centered card;
- displays the exact package ID and SMACK label from the daemon, plus the
  daemon-generated **FULL ROOT ACCESS** warning;
- displays a sanitized display copy of the command (long commands are
  truncated and marked);
- offers **Deny**, **Allow once**, and **Always allow**, focusing **Deny** by
  default;
- shows a live "Denied automatically in N s" countdown for the daemon timeout;
- sends `ui.decision` with the prompt ID and nonce;
- exits shortly after the daemon acknowledges the decision.

**Always allow** is bound to a SHA-256 of the installed package's
`author-signature.xml`. A package upgrade, changed reinstall, or signer change
invalidates the grant and the next request prompts again. If the daemon cannot
verify the package identity, it fails closed and reports
`package_identity_unavailable`.

### Package prompt

For `pkg.install` the daemon sends `kind: "install"` with an `install` object.
The UI shows the requester, package name/id/version/type/size, an upgrade note,
a signature badge, and the requested privileges (up to 7; the rest are
summarised). It offers **Deny** and **Install** only; there is deliberately no
**Always allow** for package operations.

For `pkg.uninstall` the daemon sends `kind: "uninstall"` with an `uninstall`
object and a `remove <pkgid>` command. The same layout shows **REMOVE PACKAGE**,
the package facts, **Deny**, and **Remove**. A package request without the
matching object renders "Malformed package request" with only **Deny**.

### Execution toast

A granted app's executions arrive as `ui.notice` within 250 ms. A notice-only
launch (`"launch": "notice"`) never shows the full-screen window or grabs BACK.
A separate focus-skip notification window in the top-right corner shows up to
three apps, e.g. "**org.example.app** ran 3 commands as root", plus the last
command's first line. New notices update the counters in place and keep the
toast up for 6 s after the last command. The UI then stays connected but
invisible for 60 s so a burst of notices does not relaunch it, and exits.

Prompt-free root package operations also arrive as `ui.notice` with
`kind: "install"`/`"uninstall"` and `package: "root"`, rendered as
**PACKAGE CHANGE**.

### Grant manager

Opening the app icon with no pending prompt shows the persistent-grant list.
Each row has a toast toggle that cycles **Toast: every** → **Toast: first use**
(first command after 30 min idle) → **Toast: never**, and a **Revoke** button.
Focus starts on the toggle, not Revoke. The list refreshes when the manager
opens, after each change, and via **Refresh**. The first 5 grants are shown.

### Failure behavior

- UI timeout: the UI enforces the daemon's `timeout_ms` locally, logs
  `prompt expired`, shows a timeout message, and exits after two seconds; the
  daemon independently denies.
- UI crash or socket loss: the daemon sees EOF and denies the active request;
  the UI clears the dead prompt and reconnects in manager mode.
- Broker restart: the UI reconnects every two seconds, re-sends `ui.hello`, and
  refreshes grants.
- After sending a decision, the UI has a five-second safety exit even if the
  broker stops responding.

The UI is a **best-effort consent interface**, not a proven secure-attention
path. Notification priority and key grab do not prevent another app from
imitating or obscuring the prompt. The daemon-side nonce prevents a different
app from forging the decision message, but visual spoofing is outside this
milestone.

## Files

| File | Purpose |
|---|---|
| `rust/src/main.rs` | EFL window, prompt/package/manager views, timer dispatch |
| `rust/src/client.rs` | direct Unix-socket NDJSON client and reconnect loop |
| `rust/src/efl.rs` | the complete raw EFL/Tizen FFI surface |
| `rust/src/model.rs` | protocol v2 prompt/notice/grant types |
| `rust/src/log.rs` | bounded `/tmp/tvauth-ui.log` writer |
| `rust/tizen-manifest.xml` | `org.tvauth.ui`, `exec="tv-auth-ui"`, `type="capp"` |
| `build-ui.sh` | ARMv7 soft-float cross-build and unsigned TPK assembly |
| `dotnet/` | legacy managed implementation, kept for reference |
| `build-ui-dotnet.sh` | legacy managed build |

## Build

```sh
../sysroot/fetch-tv-sysroot.sh   # read-only TV access; needs authorization
./build-ui.sh
```

`build-ui.sh` uses
`rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a`,
target `armv7-unknown-linux-gnueabi`, and Debian's soft-float
`arm-linux-gnueabi-gcc`. It links against the TV's seven direct EFL libraries
(`capi-appfw-application`, `capi-ui-efl-util`, `efl-extension`, `elementary`,
`evas`, `ecore`, `ecore_wl2`) and passes `--allow-shlib-undefined` for their
transitive dependencies.

Host checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo check --all-targets
cargo test          # library-only: pure formatting/protocol helpers
```

Observed artifact (2026-11-08, host cross-build):

```text
ELF 32-bit LSB pie executable, ARM, EABI5, stripped
ELF flags:       0x5000200 (Version5 EABI, soft-float ABI)
Size:            476,888 bytes
Unsigned TPK:    ~258 KB (zip timestamps make the hash differ per build)
NEEDED:          capi-appfw-application, elementary, evas, capi-ui-efl-util,
                 efl-extension, ecore, ecore_wl2
```

## Signing and installation

`tv-authd` re-signs unsigned TPKs on-device with its author/distributor CAs.
Install with the root-role client:

```sh
/run/tvauth-cli install tpk /tmp/org.tvauth.ui-0.1.0.tpk
```

The package is `org.tvauth.ui`; after install the daemon re-runs the `/run`
alias setup. The old managed UI is Partner-signed, so switching to the on-device
CA requires a one-time uninstall/reinstall (Tizen refuses cross-author
upgrades).

Order matters during the migration:

1. deploy a `tv-authd` build containing `ui_alias.rs` and the root-only UI
   package guard (the current live daemon predates both), and restart it;
2. `/run/tvauth-cli uninstall org.tvauth.ui` (root role);
3. `/run/tvauth-cli install tpk /tmp/org.tvauth.ui-0.1.0.tpk`;
4. confirm the daemon log contains `ui_alias=mounted` and the UI launches with
   `/proc/<pid>/maps` pointing at `/run/tv-auth/ui-bin/tv-auth-ui`.

For rollback, signing `out/tv-auth-ui-dotnet/org.tvauth.ui-0.1.0.dotnet.tpk`
with the `tvauth-partner` profile and installing it restores the managed UI
(uninstall the Rust build first because the author differs).

The legacy managed build can still be produced with `./build-ui-dotnet.sh` and
signed with the `tvauth-partner` profile for rollback:

```sh
tizen package -t tpk -s tvauth-partner -- org.tvauth.ui-0.1.0.dotnet.tpk
```

## Validation status

- **Host:** `cargo check`, `cargo clippy -D warnings`, and `cargo test` pass.
  The ARMv7 soft-float artifact links against the seven TV libraries.
- **TV (spike):** the native ELF loads from the `/run` alias, carries
  `User::Pkg::org.tvauth.spike.popup`, and maps the aliased binary. Without the
  alias, only launchpad happens.
- **TV (Rust UI smoke test, 2026-11-08):** the built `tv-auth-ui` was packaged
  as `org.tvauth.spike.ui` (so the live UI was untouched), installed through the
  daemon's root-role client (`rootcli install tpk`:
  `re_signed=true`, `backend=was`, `result=installed`), staged through a `/run`
  alias, and launched through AUL. It created a 1920x1080 notification window
  titled "Root access", logged `notification level TOP accepted`, and rendered
  the manager card, empty-grant state, and Refresh button. RSS was 16–30 MB
  across runs (well below the `.NET` UI's ~50 MB). The daemon rejected
  `ui.hello` with `not_ui` as expected for the smoke package id, and the UI
  displayed that status. `rootcli uninstall` removed the package, its Home row,
  and the directory.
- **TV (full UI, 2026-11-08):** the daemon with `ui_alias.rs` and the
  root-only UI-package guard was deployed, the managed `.NET` UI was replaced by
  this Rust build (`rootcli install tpk`: `re_signed=true`, `backend=was`,
  `result=installed`), and the daemon logged `ui_alias=mounted`. The running
  process maps `/run/tv-auth/ui-bin/tv-auth-ui`, carries
  `User::Pkg::org.tvauth.ui`, and used 25–31 MB RSS.
- **Exec prompt:** Deny returned `denied/user_denied`; Allow once returned a
  root `id` and did not persist; Always allow persisted a grant and later
  executions ran without a prompt.
- **Package prompts:** install rendered name/id/version/type/size, signature
  badge and privileges; Deny left nothing installed, Install returned
  `status: ok` and created the Home row; uninstall rendered **REMOVE PACKAGE**
  and removed the package with `backend=was`.
- **Toasts and manager:** the execution toast appeared at 696×147 +1176+48 in
  the top notification layer with `TOP` accepted; the manager listed both
  grants, the toast-mode toggle changed `every` → `first`, and **Revoke**
  removed the selected grant.
- **Protection:** an ordinary package's `pkg.install` of `org.tvauth.ui` was
  denied with `manifest_forbidden`; `pkg.uninstall` was denied with
  `protected_package`; no prompt was shown and the UI stayed installed.
- **Cold boot (boot ID `48ee2d01…`):** `tv-authd` restarted from
  `/opt/tv-auth/tv-authd`, logged `ui_alias=mounted`, recreated the read-only
  `/run` mount and symlink, kept the `org.tvauth.ui` Home row, loaded the Vetro
  grant, and Vetro resumed executing without a prompt. The UI launched and
  rendered the manager post-boot.
- **Rollback:** `out/tv-auth-ui-dotnet/org.tvauth.ui-0.1.0.dotnet-signed.tpk`
  (Partner profile) restores the managed UI; the previous daemon is at
  `/opt/tv-auth/tv-authd.m4-20261108`.
