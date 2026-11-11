# Boot-persistent extra services via AT-HUB and `/opt/tv-units`

*Target: S95B/PontusM, Tizen 6.5, build `T-PTMDEUC-REL-202602250226`.*

`tv setup` applies this hook on the TV through the thin setup driver and
`tv-apply.sh`; it does not use host SSH. `deploy.sh` remains the manual guarded
install/rollback tool for an operator with an authorized root SSH session and
off-device snapshots.

## Purpose

`AT-HUB.service` runs `/opt/etc/parseinfo.sh` as root on every normal
graphical boot. The hook preserves the stock `target_version` behavior and gives
this project a small, dynamic boot extension point:

```text
/opt/tv-units/*.service
        │  copy at boot
        ▼
/run/systemd/system/*.service
        │  systemctl start, independently
        ▼
PID 1-owned services (own cgroups, restart policy, logs)
```

`/opt/tv-units` is a clearer name than `/opt/systemd`, which resembles a real
systemd search path. It is a **privileged extension point**: a unit placed there
runs as root. It must remain root-owned, mode `0700`, and non-app-writable.

Only the unit *text* is copied into `/run`. The **executables are not copied**;
they stay on `/opt` and each unit exposes them under `/run/opt/<service>` with a
private read-only bind mount. See the next section.

## Why this is not the TU7092 DeviceTracker method

[PR #49](https://github.com/chris-ritsen/samsung-tv-root/pull/49) documents a
one-TV, exact-build TU7092 boot hook: the normal `init.wrapper` branch backgrounds
`/opt/data/DeviceTracker/Dlog_buffer.sh`, and that writable script is replaced by
an awk carrier that runs optional work and then preserves the vendor body.

The technique is sound, but **the boot trigger does not exist on this S95B**. A
read-only check of `tv-living` on 2026-11-11 found:

- firmware `T-PTMDEUC-REL-202602250226`, not the PR's
  `T-KTSU2DEUC-REL-202607022000`;
- `/opt/data/DeviceTracker` exists, but there is no `Dlog_buffer.sh`;
- neither `init.wrapper` path references DeviceTracker or `Dlog_buffer`;
- `devicetracker.service` directly executes `/bin/devicetracker`; it is not a
  shell-script hook.

Creating `Dlog_buffer.sh` therefore would not create a boot hook. No
DeviceTracker installer or compatibility abstraction is provided here. The
AT-HUB route above remains the deployed one because it also gives the native
broker, SSH, blocker, proxy, and popup processes PID 1 supervision, restart
policy, dependencies, and private `/run` bind mounts.

## Executables: bind mount, not copy

`/opt` is a writable VDFS mount, and the kernel **UEP** gate rejects execution of
unsigned ELFs whose path falls under `/opt`, `/tmp`, `/var`, or `/home`. The check
is **path-scoped** (built with `d_path()`), so the same file reached through a
bind mount under `/run` — outside those prefixes — is not verified. See
[`../../../docs/samsung-tv-uep.md`](../../../docs/samsung-tv-uep.md).

So instead of copying the payload into `/run` at boot, each unit declares:

```ini
BindReadOnlyPaths=/opt/<service>:/run/opt/<service>
ExecStart=/run/opt/<service>/<binary> …
```

systemd creates that bind mount inside the unit's **private mount namespace** and
tears it down when the unit stops. Consequences:

- Nothing is copied into `/run` for the executable, and nothing is ever written
  to the signed rootfs.
- The mount is invisible to the host and to other units (`findmnt` from a shell
  will not show it); the service still shares the `/run` tmpfs for sockets/logs.
- The source directory stays writable at its real `/opt` path — the broker still
  rewrites its sibling `grants.json` — only the `/run/opt/...` view is read-only.
- `BindReadOnlyPaths=` requires systemd ≥ 233 (this firmware is 244).
- A multi-call helper symlink must be **relative** (e.g.
  `/opt/tv-sshd/bin/dropbearkey -> dropbear`); an absolute symlink would resolve
  back to `/opt/...` and be rejected again.

## Current deployment

Deployed and live-tested on 2026-11-05:

| Path | Mode | Purpose |
|---|---|---|
| `/opt/etc/parseinfo.sh` | `0755`, root, SMACK `_` (`gawk` script) | Stock behavior + dynamic unit staging |
| `/opt/tv-units/` | `0700`, root, `System::Shared` | Root-owned unit sources |
| `/opt/tv-units/disabled` | optional `0600`, root, `System::Shared` | Suppress optional services on subsequent boots |
| `/opt/tv-units/dropbear.service` | `0600` | Dropbear unit |
| `/opt/tv-units/tv-authd.service` | `0600` | Root broker unit |
| `/opt/tv-units/tv-auth-popupd.service` | `0600` | Prompt/toast overlay (uid 5001, `User::Pkg::org.tvauth.ui`) |
| `/opt/tv-units/tv-blockd.service` | `0600` | Hosts/telemetry/OTN blocker (root) |
| `/opt/tv-suite/bin/tv-blockd` | `0755` | Blocker binary (run via `/run/opt/tv-suite/bin`) |
| `/opt/tv-suite/etc/blockd.json` | `0600` | Blocker configuration |
| `/opt/tv-units/tv-patchd.service` | `0600` | TizenTube proxy (root) |
| `/opt/tv-suite/bin/tv-patchd` | `0755` | Proxy binary (run via `/run/opt/tv-suite/bin`) |
| `/opt/tv-suite/etc/patchd.json` | `0600` | Proxy configuration |
| `/opt/tv-suite/var/patchd/` | `0700` | Proxy CA and certificate, TizenTube script |
| `/opt/tv-auth/` | `0711`, root, `System::Shared` | Broker binary, grant state, staging, trust |
| `/opt/tv-auth/tv-authd` | `0755` | Static ARMv7 broker binary |
| `/opt/tv-auth/grants.json` | `0600` | Exact package-label grants |
| `/opt/tv-sshd/` | `0700`, root | SSH keys and root-only Dropbear source |
| `/opt/tv-sshd/bin/dropbear` | `0755`, `System::Shared` | Root-only Dropbear source |
| `/opt/tv-sshd/bin/dropbearkey` | symlink → `dropbear` | Multi-call keygen alias (relative) |

The previous app-writable `/opt/systemd` directory was removed. The root-only
Dropbear source at `/opt/tv-sshd/bin/dropbear` replaces
`/home/owner/share/tmp/sdk_tools/dropbear`, so an unprivileged app can no longer
replace a binary that root executes at boot. Each unit reaches it through a
read-only bind mount under `/run/opt/<service>` rather than a copy.

M2 also installs the managed `org.tvauth.ui` package. It connects directly to the
broker as `User::Pkg::org.tvauth.ui`, is the only package allowed to use the UI
methods, and updates `/opt/tv-auth/grants.json` live for Always allow and revoke.
The unit source explicitly sets `XDG_RUNTIME_DIR=/run` and `HOME=/root` so the
broker's fixed `app_launcher` call works from systemd.

`/run/tv-auth/` is recreated each boot and is mode `0711`, so apps can traverse
to the known socket path but cannot list runtime entries. It contains
`broker.sock` (`0660 root:users`) and the bounded daemon log (`0600`); the
executable is bind-mounted at `/run/opt/tv-auth/tv-authd`, not copied here.

## Hook behavior

The hook:

1. writes the stock `__<BUILD>__<MODEL>` value to `/proc/target_version`;
2. checks the root-only `/opt/tv-units/disabled` stop file;
3. when enabled, enumerates `/opt/tv-units/*.service`;
4. copies each unit to `/run/systemd/system/`;
5. starts each unit independently with `systemctl start`;
6. falls back to `daemon-reload` and one retry if on-demand loading fails;
7. continues after individual unit failures and logs failures to
   `/run/at-hub-boot.log`.

It intentionally does **not** restart an already-active unit on a repeated hook
run. New units are loaded on demand by systemd 244 without a full reload.

### Stop file

`/opt/tv-units/disabled` is an opt-in, root-owned (`0600`, `System::Shared`)
regular file inside the root-only `0700` unit directory. When present, the hook
still performs the stock `target_version` write, logs
`optional services disabled`, and exits before staging or starting any service.
It never stops services that are already running; the effect begins on the next
boot. Use the guarded commands below rather than creating the file by hand.
`disable` refuses if the live hook does not contain the stop check, and `check`
reports `stop_control=not-in-live-hook` in that state.

## Live validation

On 2026-11-05 the deployed hook was invoked manually with gawk, without a
reboot:

- `hook_rc=0`;
- both units were discovered from `/opt/tv-units`;
- `dropbear.service` remained active without disrupting the current SSH session;
- `tv-authd.service` started as UID 0 in its own systemd cgroup;
- `/run/tv-auth/broker.sock` was `0660 root:users`;
- a granted managed .NET package executed `id` and received a valid broker
  response;
- an ungranted package received `status: denied`, `error: not_granted`;
- `systemctl restart tv-authd.service` recovered the service and the next client
  request succeeded;
- test packages, test grants, and temporary files were removed afterwards.

Cold boot passed on 2026-11-05. The boot ID changed, `/run/at-hub-boot.log`
recorded `start units rc=0`, Dropbear and `tv-authd` were automatically active
from `/opt/tv-units`, and the broker recreated its socket with empty grants.
The updated root-only Dropbear source path was used successfully.

The guarded deployment preflight was run read-only on 2026-11-11 against the
same TV. It accepted the exact build, known previous hook, all five unit files,
and reported the new stop file absent. The new hook/stop-file changes have not
yet been installed or cold-boot tested; the existing deployed hook remains the
last cold-boot-validated state until that run is done.

Note: `systemctl reboot` fails on this firmware with
`Unit reboot.target not found`; `/usr/sbin/reboot` works.

## Files in this directory

| File | Deploys to | Purpose |
|---|---|---|
| `parseinfo.sh` | `/opt/etc/parseinfo.sh` | Dynamic boot hook with the stop-file check |
| `dropbear.service` | `/opt/tv-units/dropbear.service` | Dropbear from root-only source |
| `deploy.sh` | host | Guarded check, install, rollback, disable, and enable |
| `tv-apply.sh` | `sh -s` on the TV | Exact-build, hash-guarded atomic file replacement |
| `~/.local/state/neon-lagoon/persistence-backups/<timestamp>-<hash>/` | host only | Private pre-deployment snapshot; override with `PERSISTENCE_BACKUP_ROOT` |

The broker, popup, blocker, and proxy units remain with their packages under
`bins/*/systemd/`; the deployment flow includes those source files directly.

## Guarded deployment and recovery

`deploy.sh` refuses to guess its target: set `TV_HOST` to the root SSH endpoint.
It also requires an operator-held, byte-exact pristine vendor hook for every
upgrade where the live hook is no longer pristine:

```sh
export TV_HOST=root@tv-living.home
export PRISTINE_HOOK=/secure/off-box/parseinfo.sh.original

./deploy.sh check "$PRISTINE_HOOK"
./deploy.sh install "$PRISTINE_HOOK"
```

The pristine file is not stored in this repository. It must be a regular
805-byte file with SHA-256
`c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61`.
If the live hook is already pristine, `install` can snapshot it and does not
need an external copy.

`install` performs these steps:

1. validates the exact S95B build, UID 0, all five unit sources, the live hook
   metadata (`root:root`, `0755`, SMACK `_`), the unit directory
   (`root:root`, `0700`, `System::Shared`), unit ownership/mode/label, and the
   stop file if present;
2. accepts only the known pristine hook, the known previous hardening hook, or
   the exact source target hash;
3. writes a private off-device snapshot containing the previous hook, pristine
   hook, all five previous unit files, and a hash manifest;
4. creates a root-only `/opt/tv-deploy/<id>` staging directory and uploads the
   hook and units there;
5. rechecks every live hash and metadata immediately before mutation, verifies
   the staged hashes, atomically replaces the units first and the hook last,
   then verifies the result;
6. restores the previous files from its private transaction copy if an ordinary
   failure occurs after mutation begins.

The snapshot is deliberately outside the TV and, by default, outside the Git
checkout because it contains Samsung's writable vendor script. A path supplied
through `PERSISTENCE_BACKUP_ROOT` inside the repository is ignored by Git, but
`git clean -xdf` would still remove it. Keep the snapshot private and backed up.

Stop and resume optional services for the next boot:

```sh
./deploy.sh disable
./deploy.sh enable
```

Rollback restores every file in a snapshot, not just the hook:

```sh
./deploy.sh rollback \
  "$HOME/.local/state/neon-lagoon/persistence-backups/<timestamp>-<hash>"
```

The rollback accepts only the exact hook and unit hashes recorded as installed
by that snapshot. It refuses an unknown newer hook. Running services are not
stopped over the current Dropbear session: stopping the unit would tear down
its cgroup and the SSH session. Reboot after the restore to leave the previous
set stopped. All five units are covered:
`dropbear.service`, `tv-authd.service`, `tv-auth-popupd.service`,
`tv-blockd.service`, and `tv-patchd.service`.

Do not remove the root-only Dropbear copy while its unit is active. Keep an
alternate recovery channel and the snapshot available until the cold boot is
verified.

## Caveats

- The hook executes as root on every normal graphical boot; a malformed hook can
  damage writable state.
- The stop file suppresses optional unit startup but does not stop services,
  remove `/run/systemd/system` unit copies, uninstall payloads, or alter the
  stock `target_version` write.
- Units in `/opt/tv-units` are trusted root configuration. Never relax its
  ownership or mode.
- `systemd-journald` is inactive; unit stdout/stderr are discarded. Use
  `/run/tv-auth/tv-authd.log` and `/run/at-hub-boot.log`.
- Each unit exposes its payload via a **private read-only bind mount** under
  `/run/opt/<service>` (`BindReadOnlyPaths=`), not by copying into `/run`; the
  mount is invisible from the host shell and disappears when the unit stops.
- M2's approval UI updates `/opt/tv-auth/grants.json` live through the daemon
  and does not require a restart. Manually edited grants are still only read at
  daemon startup, so restart `tv-authd.service` after external edits.
- OTA updates may replace or remove `AT-HUB.service`, `/opt/etc/parseinfo.sh`, or
  paths under `/opt`; verify after every firmware update.
