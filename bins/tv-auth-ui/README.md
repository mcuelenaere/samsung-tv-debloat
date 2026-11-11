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

A rail of tabs on the left (Store, Permissions, Tweaks, System) and the
tab's page on the right. Up and Down
on the rail switch tabs; Right or OK enters the page; Left at a page's left
edge, or BACK, returns to the rail; BACK on the rail exits.

Drawn without Elementary widgets (`src/screen.rs`), like the dock: every page
is data (`src/page.rs`, host-tested): sections, status tiles, rounded cards
of rows with switches, buttons or a progress bar, chips, a grid of app cards,
a header and wrapped paragraphs. `page.rs` lays them out, moves focus by
geometry (the nearest focusable part in the arrow's direction, so grids,
chips and button pairs all work) and works out the scroll position.
`screen.rs` creates evas objects only for parts near the viewport, and an
ecore animator glides the focus ring (or, on switch rows, a highlight with a
blue edge), the scroll position and the rail's highlight; a new tab fades
and rises in. Each change re-renders the page in place: focus stays on the
same action and the scroll position is kept.

### Store

- Fetches `https://apps2samsung.com/catalog/resolved.json.signed` with the TV's
  libcurl (TLS-validated), checks the RSA-SHA256 signature with the TV's
  libcrypto against the Apps2Samsung key pinned in `src/store.rs` (the one
  their TV-Store pins), and rejects a catalog older than 7 days or dated more
  than an hour ahead.
- A search field, **Reload**, category chips, then a five-column grid of app
  cards (139 today): icon, name, and the installed version, the number of
  builds, or "Update available". Installed apps sort first, then by name;
  the order stays put until the filter changes. Community entries are
  grouped by their `group` (variants become builds), and each release
  provider's latest release is one app whose assets are its builds.
- OK on the search field opens a single-line Elementary entry over it, so
  the TV's keyboard comes up; confirming (or leaving it) filters by name,
  description, build name and package id.
- OK on a card opens its details page: large icon, category, builds,
  description, package id(s), source repository, minimum Tizen version,
  catalog flags (closed-source, self-updating, root, Partner certificate),
  and one row per build with **Install**, **Update**, **Open** and
  **Remove**. BACK returns to the grid, focused on that app.
- Adult entries, non-HTTPS URLs, entries without a SHA-256, and apps needing
  Tizen > 6.5 are left out.
- While a build is installed or removed its row shows "Downloading N%" or
  "Waiting for approval..." with a progress bar instead of its buttons.
- Install: downloads to `/tmp/tv-store/<pkgid>.<wgt|tpk>` on a worker thread,
  keeps it only if size and SHA-256 match the signed catalog, then sends
  `pkg.install`. `tv-authd` stages and re-signs it and asks the user on the
  popup's prompt. Remove sends `pkg.uninstall` (also prompted). Open launches
  the package's main app through `app_control` (needs the
  `appmanager.launch` privilege; without it launches fail with -13).
- Icons are fetched for the listed apps, hash-checked, cached in
  `/tmp/tv-store/icons/`, and shown as they arrive.
- The window is an ordinary app toplevel (`E_LAYER_CLIENT_NORMAL`, layer 200).
  On this firmware `ELM_WIN_BASIC` leaves the Wayland window type at NONE and
  such a surface never gets an xdg toplevel, so the manager sets
  `ECORE_WL2_WINDOW_TYPE_TOPLEVEL` itself. As a real app window it obscures
  (and so pauses) the app underneath; the popup's prompt (800) opens above
  it, and keys arrive by focus with no keygrab.

### Permissions

The persistent-grant list: per app its toast mode button, which cycles
**Toast: every** → **Toast: first use** → **Toast: never**, and **Revoke**.
Below it, **Recent activity** shows the newest `grants.audit` lines with
their age, and **Refresh**.

The manager never sends `ui.hello`, so it does not take prompts or toasts from
the popup daemon.

### Tweaks

Four status tiles (hosts blocked, the auto-update setting, telemetry services
running, what the Home key opens), any daemon error, then sections:
- **Blocking**: `tv-blockd`'s host blocking switch, with each built-in host
  group as a chip under it (dimmed while blocking is off);
- **Updates and telemetry**: the auto-update lock and telemetry;
- **Background services**: AirPlay and the Multi View preload;
- **Permanent changes**: **Disable HbbTV**, **Disable ACR**, **Disable
  targeted ads**, under a warning that they outlive a reboot and TV suite;
  each subtitle starts with "Permanent" and says whether the app is
  disabled, running normally, or not on this TV;
- **YouTube** (`tv-patchd`): **TizenTube** and **Update TizenTube**;
- **Home**: **Custom Home** (`custom_home`; the Home key opens the dock
  below, or Samsung's Home again);
- **Reset**: **Undo all changes** sends `undo` to tv-blockd and tv-patchd:
  everything off, and what they changed put back.

Each switch or chip sends the daemon's whole edited config with `set`
(`/run/tv-suite/blockd.sock`, `/run/tv-suite/patchd.sock`). See
`bins/tv-blockd/README.md` and `bins/tv-patchd/README.md`.

### System

A live view, refreshed every 3 s from tv-blockd's `system` request while the
tab is open:
- tiles: CPU (all cores) and load, memory and swap in use, GPU (Mali
  `dvfs_usage`, averaged); a line with NPU activity, storage and swap IO
  rates and the process count;
- **What the tweaks save**: each service and permanent option with its
  switch, and what its processes use now (memory, swap, CPU), or, once
  stopped, what they used when tv-blockd stopped them;
- **Busiest now** and **Largest in memory**: the top eight processes, named
  by app id where they are apps;
- **Developer access**: **SDB** (tv-blockd's `sdb_off`, shown the other way
  round: on means SDB runs) and, while it is on, **Host**: the one address
  Samsung's developer mode lets connect, edited with the TV keyboard.

### Dock (`org.tvauth.home`)

The same executable, started under the package's second app id (`main`
checks `AUL_APPID`). When tv-patchd's `custom_home` is on, it is the Home
key's target: each Home press arrives as `app_control` and shows the dock,
or hides it if it is already shown.

A rounded bar at the bottom of the screen (`NOTIFICATION_HIGH`, alpha, as
small as the widest row), over whatever is playing, with three rows:
- **apps**, with **Inputs ^** above and **Settings ⌄** below;
- **Up**: the inputs, opened on the current input (`memory/system/source_type`,
  marked with a blue dot); Down returns;
- **Down**: Samsung Home (`com.samsung.tv.csfs`), Settings (`org.tizen.menu`)
  and TV suite; Up returns.

The selection glides between icons in 0.3 s at constant speed. Holding
Left or Right past 0.35 s starts a run that speeds up from 6 to 28 entries
per second and settles on the next entry when the key is released (key-up,
or 0.25 s without key repeats). The focused
icon grows from 92 to 112 px and rises 4 px, a white ring and a glow in the
icon's own colour follow the selection, and its name fades in underneath.
The row scrolls to keep the selection centred. Rows fade and slide in from
the side their arrow points to. OK opens the entry once the bar has faded
out; BACK (from any row) or 20 s idle hides it.

No Elementary widgets (`src/dock.rs`): plain evas images and text placed by
hand. An ecore animator runs only while something moves, and each frame
calls into EFL only for values that changed. Geometry, motion and keys are
in `src/dock_core.rs`; the pictures (bar, shadow, ring, glow, chevrons,
finished icons) are drawn in memory by `src/pixels.rs` (both host-tested).
Icons get rounded corners and are fitted whole onto a square tile;
Samsung's SVGs are rasterised with thorvg (`tv_tizen::svg`) onto a light
tile when they are dark, a dark one when light. An animation that drops
frames (a gap over 25 ms) is logged with its longest gap and longest own
work.

The apps are Samsung Home's own catalog (`src/catalog.rs`), most launched
first (webappservice's `use_count`, which counts launches from anywhere:
Samsung's Home, the remote's app keys, the dock), then in Home's row order,
then by title. A launch changes the count, so the row is rebuilt after it;
finished icons are cached by path and modification time, so that only
creates objects.
- webappservice's `widgetinfo` rows with `is_hidden = 0` (sideloaded apps
  included): title, the app id to launch (`tizen_id`) and the icon folder
  (`152x152.png`, else `224x224.png`, `icon.png`, `104x104.png`);
- Home's row order comes from the `d/webapp@<id>` lines of
  `/run/tv-suite/home-row`, which tv-patchd mirrors because only Home's
  label may read the original.

The inputs are TV, then the HDMI sources in Home's row, in that order, from
Home's `.sourceCache` (mirrored as `/run/tv-suite/home-sources`):
- name: `EditName`, else `DetectedName`, else the source id;
- icon: `LauncherUnfocusedIconPath` (Samsung's launcher art, e.g. Xbox and
  Nintendo), else `IconPath`;
- OK sends tv-viewer Samsung's own source-change request (`change_source`,
  `sourceType`, `sourceName`, `sourceIcon`, `mbrIndex`,
  `__K_EX_LAUNCH_REQUESTOR=user`).

Then the connected DLNA media servers (`DLNA/<UniqueKey>` entries with
`ConnectionState`), by name. `"172.17.2.219 - Sonos Play:3 Media Server -
RINCON_…"` is shown as `Sonos Play:3 (172.17.2.219)`. OK opens the server in
Samsung's media browser, `com.samsung.tv.mycontents`, the way Samsung's
source panel does: extras `launch_type=mycontents`,
`called_app=com.samsung.tv.csfs`, `device_type=DLNA`, `device_id` (the
`UniqueKey`) and `device_name` (the full name). With fewer than three extras
or no device type the browser only preloads, hidden (its
`OnAppControlReceived`).

A thread watches `/opt/dbspace` (`.webappservice.db*`) and `/run/tv-suite`
(`home-row`, `home-sources`) with inotify and reads the catalog again 300 ms
after the last change. Only when apps or inputs differ (webappservice writes
on every launch too) does it wake the main loop through an `ecore_pipe`; the
rows are rebuilt and each keeps its selection. An install or uninstall shows
within about 2–3 s of its start, before `tvauth-cli` returns.

See `docs/tv-launcher-plan.md`.

## Files

| File | Purpose |
|---|---|
| `src/main.rs` | window, the pages built from data, actions, worker jobs |
| `src/screen.rs` | the manager's renderer: rail, pages, focus, motion |
| `src/page.rs` | page model: layout, focus movement, scrolling (host-tested) |
| `src/kit.rs` | evas helpers shared by the manager and the dock |
| `src/store.rs` | catalog envelope, freshness, flattening, version states (host-tested) |
| `src/dock.rs` | the dock (`org.tvauth.home`) |
| `src/catalog.rs` | the dock's app list and order (host-tested) |
| `src/dock_core.rs` | the dock's geometry, motion and keys (host-tested) |
| `src/pixels.rs` | the dock's pictures, drawn in memory (host-tested) |
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
