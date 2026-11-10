# Boot-persistent extra services via AT-HUB and `/opt/tv-units`

*Target: S95B/PontusM, Tizen 6.5, build `T-PTMDEUC-REL-202602250226`.*

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

## Executables: bind mount, not copy

`/opt` is a writable VDFS mount, and the kernel **UEP** gate rejects execution of
unsigned ELFs whose path falls under `/opt`, `/tmp`, `/var`, or `/home`. The check
is **path-scoped** (built with `d_path()`), so the same file reached through a
bind mount under `/run` — outside those prefixes — is not verified. See
[`../docs/samsung-tv-uep.md`](../docs/samsung-tv-uep.md).

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
| `/opt/tv-units/dropbear.service` | `0600` | Dropbear unit |
| `/opt/tv-units/tv-authd.service` | `0600` | Root broker unit |
| `/opt/tv-units/tv-auth-popupd.service` | `0600` | Prompt/toast overlay (uid 5001, `User::Pkg::org.tvauth.ui`) |
| `/opt/tv-units/tv-blockd.service` | `0600` | Hosts/telemetry/OTN blocker (root) |
| `/opt/tv-suite/bin/tv-blockd` | `0755` | Blocker binary (run via `/run/opt/tv-suite/bin`) |
| `/opt/tv-suite/etc/blockd.json` | `0600` | Blocker configuration |
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
2. enumerates `/opt/tv-units/*.service`;
3. copies each unit to `/run/systemd/system/`;
4. starts each unit independently with `systemctl start`;
5. falls back to `daemon-reload` and one retry if on-demand loading fails;
6. continues after individual unit failures and logs failures to
   `/run/at-hub-boot.log`.

It intentionally does **not** restart an already-active unit on a repeated hook
run. New units are loaded on demand by systemd 244 without a full reload.

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

Note: `systemctl reboot` fails on this firmware with
`Unit reboot.target not found`; `/usr/sbin/reboot` works.

## Files in this directory

| File | Deploys to | Purpose |
|---|---|---|
| `parseinfo.sh` | `/opt/etc/parseinfo.sh` | Dynamic boot hook |
| `dropbear.service` | `/opt/tv-units/dropbear.service` | Dropbear from root-only source |

The broker unit and payload are maintained under `bins/tv-authd/`.

## Deployment and recovery

Before this deployment the previous `/opt/etc/parseinfo.sh`,
`/opt/systemd/dropbear.service`, and Dropbear binary were backed up off-box.
The rollback procedure is:

```sh
# Stop the extra services and restore the previous hook.
systemctl stop tv-authd.service
systemctl stop dropbear.service
cp -p /opt/etc/parseinfo.sh.pre-tv-units /opt/etc/parseinfo.sh
# Restore the old unit source/payload if rolling the whole deployment back.
# Then reboot, or run the restored hook and re-stage the old unit manually.
```

Do not remove the root-only Dropbear copy while its unit is active. Keep the
existing alternate recovery channels available until the cold boot is verified.

## Caveats

- The hook executes as root on every normal graphical boot; a malformed hook can
  damage writable state.
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
