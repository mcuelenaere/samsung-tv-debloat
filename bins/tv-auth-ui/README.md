# tv-auth grant manager (`org.tvauth.ui`)

The native Tizen `type="capp"` app that lists, re-modes, and revokes
`tv-authd`'s persistent **Always allow** grants. Approval prompts and toasts
moved to the persistent [`tv-auth-popupd`](../tv-auth-popupd/README.md) on
2026-11-09; before that, this app did all four jobs and the daemon launched it
on demand.

The package also defines the identity both UIs run as: UID 5001 with the exact
SMACK label `User::Pkg::org.tvauth.ui` and the `window.priority.set`
privilege. It never runs as root and has no network privilege. It talks
directly to `/run/tv-auth/broker.sock`.

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
registration, and Samsung Home entry are unchanged. `../tv-authd/src/ui_alias.rs`
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

Opening the app icon shows the persistent-grant list. Each row has a toast
toggle that cycles **Toast: every** → **Toast: first use** (first command after
30 min idle) → **Toast: never**, and a **Revoke** button. Focus starts on the
toggle, not Revoke. The list refreshes when the manager opens or is relaunched,
after each change, on reconnect, and via **Refresh**. The first 5 grants are
shown. BACK exits.

The manager never sends `ui.hello`, so it does not take prompts or toasts from
`tv-auth-popupd`; `tv-authd` serves `grants.*` to the UI identity without
registration.

## Files

| File | Purpose |
|---|---|
| `src/main.rs` | EFL window, manager view, timer dispatch |
| `../../crates/tv-proto` | broker client (`client`), markup (`markup`), message types (`ui`) |
| `../../crates/tv-efl` | EFL FFI, widgets, bounded `/tmp/tvauth-ui.log` writer (`log`) |
| `tizen-manifest.xml` | `org.tvauth.ui`, `exec="tv-auth-ui"`, `type="capp"` |
| `icon.png` | package icon |
| `build-ui.sh` | ARMv7 soft-float cross-build and unsigned TPK assembly |

## Build

```sh
../../sysroot/fetch-tv-sysroot.sh   # read-only TV access; needs authorization
./build-ui.sh
```

`build-ui.sh` runs `../../cross-build.sh gnueabi`, which uses
`rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a`,
target `armv7-unknown-linux-gnueabi`, and Debian's soft-float
`arm-linux-gnueabi-gcc`. It links against the TV's seven direct EFL libraries
(`capi-appfw-application`, `capi-ui-efl-util`, `efl-extension`, `elementary`,
`evas`, `ecore`, `ecore_wl2`) and passes `--allow-shlib-undefined` for their
transitive dependencies.

Host checks:

```sh
cargo fmt --all --check
cargo clippy -p tv-auth-ui -- -D warnings
cargo test -p tv-proto            # client parsing and markup helpers
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
alias setup. The pre-migration managed UI was Partner-signed, so a switch from
it requires a one-time uninstall/reinstall (Tizen refuses cross-author
upgrades).

Order matters when deploying to a system that still has the managed UI:

1. deploy a `tv-authd` build containing `ui_alias.rs` and the root-only UI
   package guard, and restart it;
2. `/run/tvauth-cli uninstall org.tvauth.ui` (root role);
3. `/run/tvauth-cli install tpk /tmp/org.tvauth.ui-0.1.0.tpk`;
4. confirm the daemon log contains `ui_alias=mounted` and the UI launches with
   `/proc/<pid>/maps` pointing at `/run/tv-auth/ui-bin/tv-auth-ui`.

The managed `.NET` implementation was removed from the tree after the native UI
passed acceptance. It remains in git history at commit `7b39883`; a
Partner-signed rollback TPK produced before removal was kept locally under
`out/` (build products are not committed). To rebuild the old UI from source,
check out that commit and use its `ui/build-ui-dotnet.sh`.

## Validation status

- **TV (manager-only split, 2026-11-09):** the rebuilt TPK installed through
  the root role (`re_signed=true`, `backend=was`); launched via
  `app_launcher -s`, it mapped the `/run` alias, showed the manager, and the
  daemon logged `ui_grants_list` with no `ui_attached`.

Earlier results (the combined prompt + manager UI):


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
- **Rollback:** the managed UI source remains at git commit `7b39883`, and the
  previous daemon is at `/opt/tv-auth/tv-authd.m4-20261108`. A Partner-signed
  managed TPK produced before removal was kept locally under `out/` (not
  committed).
