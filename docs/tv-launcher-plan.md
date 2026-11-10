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
- **Home key**: not grabbed via e20 (no `XF86Home` in
  `enlightenment_info -keygrab_status`; `tvctx-manager`
  `/etc/tvctx-manager/appctx-config.json` routes it). The remote's evdev
  nodes are `event0` (sdp-mcirb), `event1` (rc device) and `event3`/`event4`
  (Smart Control).
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

## Design

### Process

`bins/tv-launcher`, an EFL daemon like `tv-auth-popupd`:
- systemd unit in `/opt/tv-units`, binary through the `/run` bind mount (UEP);
- runs as **uid 5001 with `SmackProcessLabel=User::Pkg::org.tvauth.ui`**, so
  app launches come from the owner session with the manager's privileges
  (`appmanager.launch`);
- resident; hiding only unmaps the window.

Things that need root (stopping Samsung's Home if uid 5001 cannot; Autorun
Smart Hub) are requests to an existing root daemon, decided by spike 0.

### Home key

1. Read-only evdev on the nodes above (code 63; rescan
   `/proc/bus/input/devices` for hot-plugged remotes; never `EVIOCGRAB`, so
   nothing is lost if tv-launcher dies).
2. Press → show the dock.
3. **Event-driven suppression instead of timed kills.** Subscribe to app
   launch status (`aul_listen_app_status` / `app_manager` context events,
   spike 0). When csfs, home.sidebar or home.media start while suppression is
   armed, terminate them at once. Suppression arms on our Home press and
   disarms after about 5 s or when the user picks Samsung Home. If events
   are not available, fall back to Vetro's timed kills, but driven by one
   state machine, not detached shells.
4. Samsung's Home is never disabled (the Q80A boot script waits for csfs).

### Samsung Home that works

- The **Samsung Home** tile disarms suppression and records a pass-through
  until the next Home press.
- It then opens Home **the way the Home key does**: an app_control launch
  with operation `http://tizen.org/appcontrol/operation/main` and no app id,
  resolved by the system's own Home routing, from the owner session.
- Spike 0 tests the alternatives in order, on this firmware:
  1. that launch;
  2. app_control for `com.samsung.tv.csfs` (with home.sidebar and
     home.media);
  3. injecting `XF86Home` (uinput or `input_keyevent`) while passed through.
- Acceptance: Home is fully visible (sidebar included) in under 3 s, every
  time, and the next Home press brings our dock back.

### Apps, refreshed at once

- **Catalog**:
  - every launchable UI app from `.pkgmgr_parser.db` (`package_app_info`,
    `app_nodisplay = false`, enabled), which includes sideloaded store
    installs;
  - titles from `package_app_localized_info`, and from `widgetinfo` for WGTs
    (the wrapper ids Samsung's Home uses);
  - icons from `/opt/share/webappservice/apps_icon/all/<id>/`, else the
    package icon.
- **Order**: Home's row (`.appLayerCache.1` of home.media, else csfs) first,
  then the rest by title; a small exclude list for system "apps" that are
  marked displayable.
- **Refresh**: inotify on `/opt/dbspace` for `.pkgmgr_parser.db` and
  `.webappservice.db`, plus Home's `data/` dirs for `.appLayerCache.*` and
  `.sourceCache`. Re-query 300 ms after the last change; tiles update in
  place and keep focus.
  - inotify catches pkgmgr and the WAS row write that follows a WGT install,
    with no D-Bus or glib.
  - The pkgmgr signals stay a fallback if inotify proves too coarse.
- Target: an install or uninstall shows in the dock **within 1 s of pkgmgr's
  `end ok`**, including with the dock open.

### Sources with Samsung's names and icons

- **List**: the sources in Home's row (`s/source.layer.hdmi@…`), in that
  order, then any other connected HDMI source from `.sourceCache`
  (`ConnectionState`), then TV.
- **Name**: `EditName`, else `DetectedName`, else `HDMI n`, always drawn.
- **Icon**: `LauncherUnfocusedIconPath`/`LauncherFocusedIconPath` (Samsung's
  2K branded launcher art), else `IconPath`, else a generic HDMI icon. SVG
  through evas's SVG loader if the TV has it (spike 0), else the PNG
  `source_icon_*`.
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

- **Tweaks tab**: a **Custom Home** on/off toggle.
- **Off**:
  - Home suppression stops, so Samsung's Home answers Home at once;
  - Autorun Smart Hub goes back to its recorded value (set to 0 while on, and
    recorded, as tv-blockd records its changes);
  - the dock stays resident but idle.
- **Undo all changes** includes it.
- Nothing is disabled or uninstalled, so a reboot without the suite gives
  stock Home.
- Control socket and config writable by uid 5001 (`RuntimeDirectory=` in the
  unit; config under `/opt/usr/home/owner/share/tv-suite/`), with the same
  peer check as the other daemons.

## Steps

0. **Spike on tv-living** (throwaway probes in `bins/tv-launcher/examples`).
   Each answer is recorded here before the design is final.
   - Can uid 5001 with the manager's label terminate csfs, home.sidebar and
     home.media (`aul_terminate_app`, `app_manager_request_terminate_bg_app`),
     or does this need root?
   - Do app-status events reach an EFL loop (`aul_listen_app_status`, or
     `app_manager_set_app_context_event_cb` with ecore's glib integration)?
     How quickly after a Home press?
   - Which launch opens Samsung's Home fully (see above)?
   - Does tv-viewer switch sources from uid 5001 with the extras above?
   - Does evas load the theme's SVG icons?
   - Does `org.tizen.menu` open Settings by app_control from uid 5001?
1. **Skeleton**: unit, window, evdev Home, show/hide, suppression. Check:
   100 Home presses with Samsung Home never visible.
2. **Catalog and refresh**. Check: install and uninstall from the store, a
   WGT and a TPK, with the dock open and closed; each appears or disappears
   within 1 s.
3. **Sources**. Check: XBOX SeriesX|S, PlayStation 5, Nintendo Switch with
   Samsung's art; switching to each; renaming a source in Samsung's source
   menu updates the tile.
4. **Samsung Home, Settings, TV suite tiles**. Check: Samsung Home opens with
   its sidebar 20 times out of 20; the next Home press shows the dock.
5. **Tweaks toggle, Autorun Smart Hub, Undo**. Check: off returns stock Home
   behaviour at once; Undo restores `auto_start`.
6. **Soak and docs**: a day of normal use, standby and resume, a cold boot;
   README, persistence table, plan status.

## Out of scope

- Long-press Home (another route; Vetro notes it).
- Samsung's Ambient / Art mode / Game Hub entry points.
- Moving our units earlier in boot (backlog in `rust-tv-suite-plan.md`):
  until then Samsung's Home answers Home for the first seconds after boot.
