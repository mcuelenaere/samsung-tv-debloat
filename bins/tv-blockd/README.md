# tv-blockd

Root daemon that keeps Samsung's ads, tracking, telemetry and firmware updates
switched off. It replaces Vetro's blocker; uninstall Vetro first, or the two
fight over `/etc/hosts`.

Every 60 s, and right after each configuration change, it reconciles:

- **hosts**: writes `/run/tv-suite/hosts` (the TV's own entries plus
  `0.0.0.0`/`::` lines for every blocked name) and bind-mounts it over
  `/etc/hosts` in every mount namespace. Mount propagation from PID 1 covers
  most namespaces; for the rest a forked child `setns`es in and binds the file
  by path (a `/proc/self/fd` source does not work across namespaces). New
  namespaces are picked up on the next pass. Essential hosts (connectivity
  check, clock, developer certificate) are never blocked.
- **OTN lock**: `vconf db/menu/support/softwareupdate/autoupdate` stays 0.
- **service options** (`SERVICES` in `src/daemon.rs`): while an option is
  on, its units are `systemctl mask --runtime`d and stopped, its processes
  killed and its apps closed (when it is switched on, and once after each
  start of tv-blockd). Runtime masks live in `/run`, so a reboot without
  tv-blockd brings every service back. The lists cover several models: a
  unit or app a TV does not have is skipped harmlessly.
  - **telemetry** (on by default): `canalysis`, `canalysis-service`,
    `adagent-service`, `dlog_logger`, `ep-agent`, `faultmgr`,
    `smart_deadlock`, `trace_manager`, `pisa-service` (and `.path`),
    `vision-data-recognition-service`, `elt-daemon`,
    `emergency-log-transmitter`, `contents-recognition-service`,
    `contents-ui-detector-service`, `mirroring-recognition-manager`,
    `kpi-logging`, `devicetracker`; kills `EdgeScraper` and
    `contents-ott-detector`; the `/tmp/eltlaunch.cond` brake.
  - **airplay** (off): `com.samsung.tv.airplay-service(-2)` `.service` and
    `.path`; closes `com.samsung.tv.airplay-app(-2)`. iPhones and Macs can no
    longer cast to the TV.
  - **multiview_preload** (off): `com.samsung.tv.multiscreen.service`;
    closes `com.samsung.tv.multiscreen`. Multi View then starts cold. The
    unit does not start while Samsung's own `/tmp/multiview_low_memory`
    exists, which the firmware creates when memory runs low; it is Samsung's
    and left alone.
- **permanent options** (`DISABLES` in `src/daemon.rs`, all off): service
  apps that amd restarts, disabled in pkgmgr's database with
  `pkgcmd -D -t app -n <app> --global` (run with a 20 s limit). This outlives
  a reboot and tv-blockd, so `changes.json` records each app tv-blockd
  disabled; switching the option off (or `undo`) runs `pkgcmd -A` and starts
  exactly those again. Apps disabled by something else are left alone. State
  is read from `/opt/dbspace/.pkgmgr_parser.db` with the TV's `sqlite3`.
  - **disable_hbbtv**: `org.tizen.hbbtv-service` (it relaunches HbbTV at boot
    and on source changes); closes `org.tizen.hbbtv`. HbbTV also runs
    Samsung TV Plus on this firmware.
  - **disable_acr**: `com.samsung.tv.acr`.
  - **disable_ads**: `com.samsung.tv.targeted-ad-service`.

  On tv-living the services the defaults stop took about 23 MB of RSS and
  27 MB of swap; AirPlay and the Multi View preload about 17 + 28 and
  25 + 27 MB, with swap full. Measured in a read-only survey, 2026-11-11.

Turning an option off undoes exactly what tv-blockd did for it, without a
reboot:
- **hosts**: unmounted everywhere;
- **OTN lock**: puts back the auto-update value it found;
- **service options**: deletes the brake file it created, unmasks the units,
  and starts the ones it stopped (recorded before its apps are closed).

`undo` (or **Undo all changes** in the manager) switches them all off. The
record lives in `/opt/tv-suite/var/changes.json`, because the auto-update
setting outlives a reboot. Killed processes have no unit to restart; they
come back at the next boot.

Older builds braked telemetry with files under `/opt/usr/share/systemd/system`,
which outlived a reboot without tv-blockd. Every pass removes any such file
or directory that `changes.json` records as tv-blockd's (on tv-living: four,
on 2026-11-11).

Not done: voice apps (Bixby and friends) are also restarting service apps,
but which can go without breaking the remote's microphone is untested.

## Configuration

`/opt/tv-suite/etc/blockd.json`, created with defaults on first start:

| Field | Default | |
|---|---|---|
| `hosts` | `true` | sinkhole the selected hosts |
| `groups` | all but `home-images` | built-in host groups (`policy.rs`, evidence-labelled) |
| `custom` | `[]` | extra host names (max 500) |
| `lists` | `[]` | HTTPS hosts-format or domain-per-line lists (max 8), fetched daily, cached in `/opt/tv-suite/var/lists` |
| `otn_lock` | `true` | keep automatic updates off |
| `telemetry` | `true` | stop telemetry services |
| `airplay` | `false` | turn off the AirPlay receiver |
| `multiview_preload` | `false` | don't preload Multi View |
| `disable_hbbtv`, `disable_acr`, `disable_ads` | `false` | permanent app disables |

## Control

`/run/tv-suite/blockd.sock` (`root:users 0660`), newline-delimited JSON:
`{"method":"status"}`, `{"method":"set","config":{...}}` (replaces the whole
config) and `{"method":"undo"}`; each answers with the new status, which
includes the `changes` record. `{"method":"system"}` answers the manager's
System tab: CPU, memory, swap, GPU and IO rates since the previous call (or
over half a second), the top processes by CPU and by memory, and per option
the processes it covers (its units' cgroups under
`/sys/fs/cgroup/systemd/system.slice`, its apps from `app_launcher -S`, its
process names) with their memory and CPU. The GPU figure averages Mali's
`dvfs_usage` (load over its last 20 ms, so one read is mostly 0), sampled
every 25 ms by a thread that runs only while the System tab asks; the NPU
has no load counter, so its activity is the `triv2` interrupt rate from
`/proc/interrupts`. SSH, SDB and SSH keys are tv-authd's (`access.*` in
`bins/tv-authd/PROTOCOL.md`), so every change to who can reach the TV is
in its audit log. Just before an option first stops
its processes, tv-blockd records what they used in `changes.json`
(`freed`). Callers: root, or uid 5001 with SMACK
label `User::Pkg::org.tvauth.ui` (the manager's **Tweaks** tab).

```sh
/run/opt/tv-suite/bin/tv-blockd status   # from root, inside the unit's namespace
tv-blockd set '{"hosts":true,"otn_lock":true}'
tv-blockd undo
```

Log: `/run/tv-suite/blockd.log` (one line per pass and per change).

## Build and install

```sh
bins/tv-blockd/build-armv7.sh            # gnueabi: links the TV's libvconf/libcurl
```

Binary at `/opt/tv-suite/bin/tv-blockd` (0755), unit
`systemd/tv-blockd.service` at `/opt/tv-units/tv-blockd.service` (0600); the
AT-HUB boot hook starts it. The unit runs the binary through a `/run` bind
mount because UEP rejects unsigned ELFs under `/opt`.

## Not included

A general bloatware disabler. `pkgcmd -D` (package level) hangs when run as
root, and `pkgcmd -D -t app … --uid 5001` reports success but changes
nothing visible; the app-level `--global` form the permanent options use
works (tv-living, 2026-11-11: about 140 ms each way).
