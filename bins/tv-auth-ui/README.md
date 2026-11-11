# TV suite manager (`org.tvauth.ui`)

The user-launched native Tizen `type="capp"` app of the suite (the plan's
`tv-manager`; the binary keeps the `tv-auth-ui` name the daemon's `/run` alias
expects). Approval prompts and toasts live in the persistent
[`tv-auth-popupd`](../tv-auth-popupd/README.md).

The package also defines the identity both UIs run as: UID 5001 with the exact
SMACK label `User::Pkg::org.tvauth.ui`, the `window.priority.set` privilege,
and (for the store) `internet`. It never runs as root.

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

Three tabs, driven with the remote (arrows, OK, BACK to exit).

### Store

- Fetches `https://apps2samsung.com/catalog/resolved.json.signed` with the TV's
  libcurl (TLS-validated), checks the RSA-SHA256 signature with the TV's
  libcrypto against the Apps2Samsung key pinned in `src/store.rs` (the one
  their TV-Store pins), and rejects a catalog older than 7 days or dated more
  than an hour ahead.
- Lists one row per app (135 today): community entries grouped by their
  `group` (variants become builds), and each release provider's latest
  release as one app whose assets are its builds. Each row has **Details**;
  installed apps also have **Open** (or **Update** when the catalog is
  newer). Installing starts from the details page.
- **Details** opens a page with the large icon, full description, category,
  package id(s), source repository, minimum Tizen version, catalog flags
  (closed-source, self-updating, root, Partner certificate), and every build
  with Install/Update/Open/Remove. BACK (there is no on-screen Back button)
  returns to the list exactly as it was: the list is unpacked and hidden
  while the page is open, not rebuilt, so scroll position and focus survive.
  Installed apps sort first, then by name; the order stays put until the
  filter changes. One scrolling list: rows are appended 16 at a time as
  focus nears the end, and the scroller follows focus.
- A search field (`elm_entry`; OK opens the TV keyboard) filters by name,
  description, build name and package id as you type; a category dropdown
  (`elm_hoversel`) filters by category. Adult entries, non-HTTPS URLs, entries without a
  SHA-256, and apps needing Tizen > 6.5 are left out; Partner-level apps are
  labelled.
- Matches `package_id`/`package_version` against `pkg.list`: **Install**,
  **Update** (`vX → vY`), or **Open** + **Remove**.
- While a build is being installed or removed its rows (in the list and on
  the details page) show "Downloading N%" or "Waiting for approval…" over a
  progress bar instead of their buttons. (The TV theme draws nothing for a
  pulsing bar, so the waiting phase is a full bar plus text.)
- After a change only the visible rows are rebuilt in place, so the
  selection, scroll position and the focused button survive.
- Install: downloads to `/tmp/tv-store/<pkgid>.<wgt|tpk>` on a worker thread,
  keeps it only if size and SHA-256 match the signed catalog, then sends
  `pkg.install`. `tv-authd` stages and re-signs it and asks the user on the
  popup's prompt. Remove sends `pkg.uninstall` (also prompted). Open launches
  the package's main app through `app_control` (needs the
  `appmanager.launch` privilege; without it launches fail with -13).
- Icons are fetched as rows are appended, hash-checked, cached in
  `/tmp/tv-store/icons/`, and swapped into their row in place.
- The window is an ordinary app toplevel (`E_LAYER_CLIENT_NORMAL`, layer 200).
  On this firmware `ELM_WIN_BASIC` leaves the Wayland window type at NONE and
  such a surface never gets an xdg toplevel, so the manager sets
  `ECORE_WL2_WINDOW_TYPE_TOPLEVEL` itself. As a real app window it obscures
  (and so pauses) the app underneath, Samsung's Home bar (layer 650) and the
  popup's prompt (800) open above it, and BACK arrives by focus with no
  keygrab.

### Permissions

The persistent-grant list: a toast toggle per app that cycles **Toast: every**
→ **Toast: first use** → **Toast: never**, and **Revoke** (focus starts on the
toggle). Below it, **Recent activity** shows the newest `grants.audit` lines
with their age. The first 5 grants are shown.

The manager never sends `ui.hello`, so it does not take prompts or toasts from
the popup daemon.

### Tweaks

On/Off toggles for `tv-blockd` (`/run/tv-suite/blockd.sock`): host blocking,
each built-in host group with its note, host count and evidence, the
auto-update lock, and telemetry. A summary line shows the blocked host count,
the auto-update setting, running telemetry services and any reconcile error.
Each press sends the whole edited config with `set`; the row's button updates
in place from the answer, so focus stays. **Undo all changes** at the bottom
sends `undo` to tv-blockd and tv-patchd: everything off, and what they
changed put back. See `bins/tv-blockd/README.md`.

**TizenTube in YouTube** switches `tv-patchd` (`/run/tv-suite/patchd.sock`)
on and off, and **Update TizenTube** installs npm's latest release. See
`bins/tv-patchd/README.md`.

### Dock (`org.tvauth.home`)

The same executable, started under the package's second app id (`main`
checks `AUL_APPID`). When tv-patchd's `custom_home` is on, it is the Home
key's target:
- each Home press arrives as `app_control` and shows the dock, or hides it if
  it is already shown;
- it is a `NOTIFICATION_HIGH` window across the bottom of the screen;
- BACK or 20 s idle also hides it;
- tiles so far: **Samsung Home** (`com.samsung.tv.csfs`) and **TV suite**.

See `docs/tv-launcher-plan.md`.

## Files

| File | Purpose |
|---|---|
| `src/main.rs` | EFL window, Store/Permissions views, worker jobs |
| `src/store.rs` | catalog envelope, freshness, flattening, version states (host-tested) |
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
cargo clippy -p tv-auth-ui --all-targets -- -D warnings
cargo test -p tv-auth-ui --lib    # catalog logic
STORE_CATALOG=resolved.json.signed cargo test -p tv-auth-ui --lib -- --ignored
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

- **TV (store, 2026-11-09):** the manager loaded and verified the live catalog
  (234 apps, 39 pages) and the page's icons over the TV's libcurl; `pkg.list`
  matched 126 installed packages. Installing "/dev/null on TV" (`DvNullSh01`
  1.4.0, 728 KB WGT) downloaded and hash-checked the file, staged it, and
  raised the popup's install prompt (captured with
  `enlightenment_info -dump_screen`); approving it re-signed (36 files) and
  installed it through WAS with a Home row, and the row switched to
  Open/Remove. Remove went through a prompt and `pkgcmd+home`. The
  Permissions tab showed the Vetro grant and the audit lines of both changes.
  (That build used a notification window; see the follow-up below.)
- **TV (follow-ups, 2026-11-09):** continuous scrolling past the first chunk
  and the category dropdown worked with injected keys. With the toplevel
  window, YouTube received `visibility.notify(fully obscured)` and
  `iconify_state_changed` when the manager opened, the Home button showed
  Samsung's Home bar (confirmed on the TV), and BACK closed the manager.

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
