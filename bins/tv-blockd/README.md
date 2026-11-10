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
- **telemetry**: creates the units' `ConditionPathExists=!` brake files, stops
  and `systemctl mask --runtime`s the telemetry units (canalysis, dlog_logger,
  ep-agent, faultmgr, smart_deadlock, trace_manager, pisa-service,
  vision-data-recognition-service, elt-daemon), and kills `EdgeScraper`.

Turning an option off undoes exactly what tv-blockd did for it, without a
reboot:
- **hosts**: unmounted everywhere;
- **OTN lock**: puts back the auto-update value it found;
- **telemetry**: deletes only the brake files and directories it created,
  unmasks the units, and restarts the ones it stopped.

`undo` (or **Undo all changes** in the manager) switches all three off. The
record lives in `/opt/tv-suite/var/changes.json`, because brake files and the
auto-update setting outlive a reboot. `EdgeScraper` has no unit to restart;
it comes back at the next boot.

Known gap: most telemetry unit names are Vetro's and do not exist on this
firmware (see the backlog in `docs/rust-tv-suite-plan.md`).

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

## Control

`/run/tv-suite/blockd.sock` (`root:users 0660`), newline-delimited JSON:
`{"method":"status"}`, `{"method":"set","config":{...}}` (replaces the whole
config) and `{"method":"undo"}`; each answers with the new status, which
includes the `changes` record. Callers: root, or uid 5001 with SMACK
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

A bloatware app disabler. `pkgcmd -D` (package level) hangs when run as root,
and `pkgcmd -D -t app … --uid 5001` reports success but changes nothing
visible, after which `-A` fails with -999. Without a working disable path the
feature was dropped.
