# tv-launcher: our own Home (Phase 6 plan)

A dock that answers the remote's Home key in place of Samsung's Home. It is
modelled on Vetro's launcher (`~/Projects/personal/vetro/dock`), with three
changes asked for on 2026-11-10:

1. installed and uninstalled apps show up **at once**, not at the next start;
2. HDMI sources carry **the name and icon Samsung's Home shows** (PS5, Xbox,
   …);
3. **"Samsung Home" actually opens Samsung's Home.**

This replaces §5.5 of `rust-tv-suite-plan.md`.

## What Vetro does, and where it falls short

Source: a read of Vetro's `dock/` and `app/service/dock*.js`.

| Area | Vetro | Problem |
|---|---|---|
| Home key | Reads the remote's evdev nodes read-only (code 63, never grabbed). The system still sends Home to Samsung's Home, so every press starts a detached shell that runs `app_launcher -k` on csfs, home.sidebar and home.media at about 0.15, 0.45, 0.9, 1.6 and 2.8 s. | Timed kills: Home flashes, a slow Home start survives the chain (Vetro issue #41 is open), and the kills also hit a Samsung Home you asked for. |
| Process | .NET dock started by `systemd-run` **as root**, `ELM_WIN_NOTIFICATION` at level TOP, bar-sized overlay. | Root draws the window and launches apps from outside the owner session. |
| App list | Home's row order from `.appLayerCache`, titles from `.webappservice.db`, icons from `apps_icon`. Built once by a shell script, **read only at dock start**. | Installs show up after a manual rebuild or restart. Sideloaded apps (not in Home's row) are only appended on 2022 Home. |
| Sources | Always TV plus HDMI 1–4. Names are read from `.sourceCache` but **only sent to tv-viewer; the dock draws no labels**. Icons are allowed only from PNGs under fixed roots, so Samsung's SVG device icons are dropped. Both options are off by default. | Generic HDMI tiles. |
| Samsung Home button | `app_launcher -s com.samsung.tv.coba.newhome \|\| … csfs` as root, after bumping a generation file that cancels the pending kills. | Vetro's comments say plain app_control is "ignored"; `app_launcher` returns 0 without opening on CoBA, so the `\|\|` fallback never runs; Home takes 4–5 s; sidebar and media stay killed; "needs a TV" in their own state notes. |

## Findings on tv-living (T-PTMDEUC-1720.7)

- **Home** is `com.samsung.tv.csfs` + `home.sidebar` + `home.media`.
  `com.samsung.tv.coba.newhome` is installed but not running.
- **Home key**:
  - It is not grabbed through e20 (`enlightenment_info -keygrab_status` lists
    no `XF86Home`).
  - `tvctx-manager` (`libapp-context-manager`) handles it:
    `KeyHandlerProduct::OnHomeKeyPressed` calls
    `ContextProduct::LaunchHomeApp`, which launches
    `app_operation_get_current_app("http://tizen.org/appcontrol/operation/main")`.
    That is the Tizen default app for operation `main`, stored in
    `/opt/dbspace/user/5001/.appsvc.db` (`appsvc` table) as
    `com.samsung.tv.csfs`.
  - `libcapi-appfw-app-operation-tv` also exports
    `app_operation_set_current_app` and `app_operation_reset_current_app`
    (through `app_control_set_defapp`), and tvctx-manager registers a callback
    for changes.
- **Install events**: pkgmgr broadcasts on the system bus,
  `org.tizen.pkgmgr.signal` `install`/`update`/`uninstall` with the package
  id, then `"end"`, `"ok"`.
  - Seen while reinstalling `org.tvauth.ui`.
  - `.pkgmgr_parser.db` and `.webappservice.db` are rewritten at the same time.
- **Home's row** is `com.samsung.tv.home.media/data/.appLayerCache.1`:
  - `s/livetv@TVPLUS`, then 11 `d/webapp@<registry id>` entries, then
    `s/source.layer.hdmi@HDMI1/0`, `@HDMI2`, `@HDMI4`, then `EOFCOBA`;
  - csfs has no `.appLayerCache` on this firmware.
- **Sources**: `<home app>/data/.sourceCache`, a JSON object keyed by source
  id (`TV`, `HDMI1/0`, `HDMI2`, `HDMI3`, `HDMI4`, `USBCAM`).
  - Each entry has `EditName`, `DetectedName`, `SourceType` (HDMI1=13 …
    HDMI4=16), `IconPath`, `LauncherFocusedIconPath`,
    `LauncherUnfocusedIconPath`, `ConnectionState`, `PowerState`,
    `MbrActivityIndex`, `SourceCategory`.
  - csfs, home.media, home.sidebar and multiscreen all keep a copy,
    refreshed at boot.
  - On tv-living:

    | Source | Name | Icon |
    |---|---|---|
    | HDMI1/0 | XBOX SeriesX\|S | `launcher_xboxseries_{f,n}.png` (2K) and `source_icon_xboxseries.png` |
    | HDMI2 | PlayStation 5 | `i_icon_device_m_gamepad.svg` |
    | HDMI3 | HW-Q930B | soundbar SVG |
    | HDMI4 | Nintendo Switch | |
- **Autorun Smart Hub** `db/menu/smart_hub/ontvsetting/auto_start` is 1.
- The live source data sits behind Samsung's proprietary TVService
  (`com.samsung.TVService.*` on D-Bus). We use the cache instead.

## Spike results (2026-11-11)

Probe `crates/tv-tizen/examples/home-spike.rs`, run on tv-living through
`systemd-run --uid=5001 -p SmackProcessLabel=User::Pkg::org.tvauth.ui`.

| Question | Answer |
|---|---|
| Can uid 5001 with the manager's label close csfs, home.sidebar and home.media? | **No.** `app_manager_terminate_app` and `app_manager_request_terminate_bg_app` return -13 (permission denied); `aul_terminate_pid` returns -8 (illegal access). `/usr/bin/app_launcher` cannot even be executed under that label (exit 126). Root `app_launcher -k` works. (Moot: see the follow-up below.) |
| Do app-launch events reach an EFL loop? | **Yes**, for uid 5001 and root alike: `ecore_main_loop_glib_integrate()` plus `aul_listen_app_launch_signal_v2` / `aul_listen_app_dead_signal`. After a Home press: csfs launch event at about +40 ms, home.sidebar at +1.25 s, home.media at +1.65 s. |
| Which launch opens Samsung's Home fully? | From uid 5001, app_control with operation `http://tizen.org/appcontrol/operation/main` and no app id, or app id `com.samsung.tv.csfs`, with or without that operation. All three start csfs, home.sidebar and home.media, and Home is visible with its sidebar within 5 s. `com.samsung.tv.coba.newhome` returns 0 and starts nothing (as Vetro saw). |
| Does tv-viewer switch inputs from uid 5001? | The request is accepted (0) with the extras below. The TV was already on HDMI2, so the visible switch is still to be confirmed (step 3). |
| SVG source icons? | Evas has no SVG loader on this firmware (image loaders: bmp, generic without helpers, gif, ico, pmaps, psd, tga, tgv, tiff, wbmp, webp, xpm; PNG/JPEG built in). `libthorvg.so.0` (0.6.99) renders them: `tvg_engine_init(2 = SW)`, `tvg_swcanvas_set_target(…, 1 = ARGB8888)`, `tvg_picture_load`, draw, sync; the gamepad icon drew at 96×96. |
| Settings from uid 5001? | app_control for `org.tizen.menu` opens the settings menu over the current input. |

### Follow-up: changing the Home target (2026-11-11)

| Question | Answer |
|---|---|
| Can the Home key's target be changed? | **Yes.** `app_operation_set_current_app(operation/main, "org.tvauth.ui")` as root writes `/opt/dbspace/user/5001/.appsvc.db`. Pressing Home from YouTube then launched `org.tvauth.ui` and **none** of csfs, home.sidebar or home.media started. `app_operation_reset_current_app` put csfs back, and Home opened Samsung's again. `app_operation_get_default_app` keeps reporting csfs throughout (the factory default). |
| Who may set it? | Root, or uid 5001 with `System::Privileged`. The manager's own label gets -13. |
| Boot dependencies on csfs? | This firmware has no `firstscreen` script, and no systemd unit names csfs. A cold boot with the target changed is still to be tested. |

This makes the guard unnecessary: see *Home key*.

Also seen: Vetro's `wrt-service` (pid 3984) is still running two days after
Vetro was uninstalled, retrying `/home/owner`. It ends at the next reboot.

## Design

### Process

The dock is an ordinary Tizen UI app: a second application, `org.tvauth.home`,
in the existing `org.tvauth.ui` package.
- It has the same executable (`tv-auth-ui`), which branches on its app id, so
  tv-authd's UEP `/run` alias covers it unchanged.
- It runs in the owner session with the package's label and privileges
  (`appmanager.launch`), like the manager.
- AUL starts it, and it stays resident after the first Home press; hiding only
  unmaps the window.

No daemon, no evdev, no root at run time.

### Home key: make the dock the Home app

- tv-patchd (root) sets
  `app_operation_set_current_app("http://tizen.org/appcontrol/operation/main",
  "org.tvauth.home")` while **Custom Home** is on.
  - It checks the value on every 60 s pass and sets it again if a package
    update or Samsung changed it.
  - Switching off, or Undo, calls `app_operation_reset_current_app`.
- tvctx-manager then launches or resumes our dock on every Home press. Each
  press arrives as an app_control event and shows the dock (or hides it if it
  is already shown).
- Samsung's Home is not started at all: nothing to suppress, nothing killed,
  nothing disabled.
- The target is stored in the user's appsvc database, so it also holds from
  boot, before any of our daemons start.

### Samsung Home that works

- The **Samsung Home** tile launches app id `com.samsung.tv.csfs` from the
  dock's session. The spike showed this starts csfs, home.sidebar and
  home.media, and Home is visible with its sidebar.
  - It cannot be operation `main` without an app id: that now resolves to the
    dock.
  - There are no pending kills to race with.
- The next Home press goes to the dock again.
- Acceptance: Home is fully visible (sidebar included) in under 3 s, every
  time, and the next Home press brings our dock back.

### Apps, refreshed at once

- **Catalog** (as built in step 2): webappservice's `widgetinfo`, the list
  Samsung's Home itself shows.
  - Rows with `is_hidden = 0`: 21 apps on tv-living, sideloads (Kodi,
    RetroArch, OmegaTube, Steam Link) included, no system "apps", so no
    exclude list.
  - `id` is the id Home's row uses, `title` the name, `tizen_id` the app to
    launch (the wrapper ids, e.g. `9Ur5IzDKqV.TizenYouTube`), `icon_path` a
    folder of PNG sizes.
  - pkgmgr's database is not needed.
- **Order**: Home's row (`.appLayerCache.1` or `.2` of home.media, the newer)
  first, then the rest by title.
  - Only Home's label may read Home's `data/`, so tv-patchd mirrors the row
    and `.sourceCache` to `/run/tv-suite/home-row` and `home-sources`
    (`User::App::Shared`) while Custom Home is on.
- **Refresh**: the dock watches `/opt/dbspace` (`.webappservice.db*`) and
  `/run/tv-suite` (`home-row`) with inotify, re-queries 300 ms after the last
  change and rebuilds the tiles, keeping focus on the same app. tv-patchd
  watches Home's `data/` the same way for the mirror. No D-Bus or glib.
- Target: an install or uninstall shows in the dock **within 1 s of pkgmgr's
  `end ok`**, including with the dock open.

### Sources with Samsung's names and icons

- **List**: the sources in Home's row (`s/source.layer.hdmi@…`), in that
  order, then any other connected HDMI source from `.sourceCache`
  (`ConnectionState`), then TV.
- **Name**: `EditName`, else `DetectedName`, else `HDMI n`, always drawn.
- **Icon**: `LauncherUnfocusedIconPath`/`LauncherFocusedIconPath` (Samsung's
  2K branded launcher art, e.g. Xbox Series and Nintendo), else `IconPath`,
  else a generic HDMI icon.
  - PNGs load directly.
  - SVGs (e.g. the PlayStation's gamepad icon) are rasterised with thorvg
    into an evas image buffer, once per path and size.
- **Switch**: app_control to `org.tizen.tv-viewer` with Samsung's
  `SourceChangeViaTVViewer` extras: `change_source` and `sourceType` (the
  cache's `SourceType`), `sourceName`, `sourceIcon` (an existing file;
  tv-viewer rejects a missing one), `mbrIndex` (the cache's
  `MbrActivityIndex`), `__K_EX_LAUNCH_REQUESTOR=user`.
- The current source (`memory/system/source_type`) is marked.

### UI

- A bottom dock over the running app (notification window like Vetro's;
  video keeps playing behind).
- One row of tiles: **Samsung Home**, Home-row apps, the remaining apps, a
  divider, sources, **Settings**, **TV suite** (`org.tvauth.ui`). The focused
  tile shows its name.
- BACK or Home hides it, as does a launch or 20 s idle.
- Built with `tv-efl`. Navigation comes from the focused window's own key
  events (no global grabs).

### Control and reversibility

- **Tweaks tab**: a **Custom Home** on/off toggle. It is tv-patchd's new
  `custom_home` setting, beside `tizentube`, with the same `set`/`undo` socket
  methods.
- **Off** resets the Home target, so Samsung's Home answers the next press.
- **Undo all changes** includes it.
- Autorun Smart Hub opens Samsung's Home at power-on even with the dock as
  the Home target (cold boot, step 1). Turning it off while Custom Home is on
  is the user's call.
- Nothing of Samsung's is disabled or uninstalled.
- The one lasting change is the appsvc row. Off and Undo reset it. Step 1
  checks what happens to it when the `org.tvauth.ui` package is updated or
  uninstalled, so that removing the suite cannot leave Home pointing at
  nothing.

## Steps

0. **Spike**: done, see *Spike results*.
1. **Skeleton**:
   - add the `org.tvauth.home` app to the manifest, branching on app id;
   - the dock window with show and hide;
   - tv-patchd's `custom_home`.

   Checks:
   - 100 Home presses, from apps, inputs and the dock itself, with Samsung's
     Home never started;
   - a cold boot with Custom Home on;
   - the Home target after updating and after uninstalling the package;
   - whether the app behind the dock keeps playing (else the dock goes
     full-screen).

   **Done 2026-11-11** on tv-living:
   - Cold boot with Custom Home on: the TV boots normally, the Home target is
     still `org.tvauth.home`, and the first Home press opens the dock.
     - With Autorun Smart Hub on, power-on still opens **Samsung's** Home:
       the boot-time autostart uses the factory default (csfs), not the
       current target.
     - Seven Samsung units were failed after this boot: aov-service,
       knox-browser, termsclient.message, hbbtv-service, rct-service,
       udev-trigger, vdca-update. udev-trigger ran at 3 s, before any suite
       unit; the rest ran after tv-blockd started (22 s). No stock boot has
       been recorded to compare against.
   - Home over YouTube shows the dock across the bottom with YouTube still on
     screen; Home again and BACK hide it.
   - 100 Home presses: no launch event at all. Samsung's Home never started,
     and the dock kept one process (AUL resumes it).
   - The **Samsung Home** tile opens csfs, home.sidebar and home.media.
   - The Home target survives reinstalling `org.tvauth.ui`.
   - Uninstalling the package that is the target makes the firmware reset it
     to csfs (tested with a throwaway `org.tvauth.probe` package).
   - A target that names an app that was never installed leaves Home dead
     (no fallback), so tv-patchd only sets it once `org.tvauth.home` is
     installed.
   - With the dock shown, YouTube's AUL state goes from focus (2) to visible
     (3), not background (4), and playback continues behind it (confirmed
     on the TV).
2. **Catalog and refresh**. Check: install and uninstall from the store, a
   WGT and a TPK, with the dock open and closed; each appears or disappears
   within 1 s.

   **Done 2026-11-11** on tv-living (TPK only so far):
   - 23 tiles: Samsung Home, the 21 apps in Home's order (Netflix, Prime
     Video, Disney+, Apple TV, Rakuten TV, YouTube, TV VLAANDEREN, VTM GO, RTL
     play, Browser, Xbox), then the rest by title, then TV suite. Samsung's
     icons; the row scrolls with focus.
   - A visible throwaway TPK (`org.tvauth.probe`) through `tvauth-cli` with
     the dock open:
     - install: tile in the dock 2.1 s after the command started, 2.0 s
       before `tvauth-cli` returned (it re-signs and installs through
       webappservice);
     - uninstall: tile gone 2.9 s after the start, 0.7 s before the command
       returned;
     - focus stayed on the focused app across the rebuild.
     The webappservice row is written as part of the install, so the 1 s
     target relative to pkgmgr's `end ok` holds in practice. It was not
     measured against the D-Bus signal itself.
   - A store install and a WGT were not tested.
   - Found while testing: webappservice gives a multi-app TPK's row to an app
     of its own choosing, and for `org.tvauth.ui` that is now the dock
     (`tizen_id=org.tvauth.home`, `is_hidden=1`). Since step 1, **TV suite
     is therefore missing from Samsung's own Home**. The pkgmgr main app is
     still `org.tvauth.ui`. Listing the dock first with `main="true"` on the
     manager does not help: this pkgmgr ignores `main`, so the dock became
     the main app and still got the row. Not fixed yet.
3. **Sources**. Check: XBOX SeriesX|S, PlayStation 5, Nintendo Switch with
   Samsung's art; switching to each; renaming a source in Samsung's source
   menu updates the tile.
4. **Samsung Home, Settings, TV suite tiles**. Check: Samsung Home opens with
   its sidebar 20 times out of 20; the next Home press shows the dock.
5. **Tweaks toggle and Undo**. Check: off returns stock Home behaviour at
   once; Undo resets the target.
6. **Soak and docs**: a day of normal use, standby and resume, a cold boot;
   README, persistence table, plan status.

## Out of scope

- Long-press Home (another route; Vetro notes it).
- Samsung's Ambient / Art mode / Game Hub entry points.
- The evdev and guard design that the spike made unnecessary.
