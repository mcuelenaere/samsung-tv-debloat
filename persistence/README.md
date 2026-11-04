# Boot-persistent dropbear via `AT-HUB.service`, managed by systemd

This directory holds a persistence design for the TV (Tizen 6.5, S95B/PontusM,
build `T-PTMDEUC-1720.7`). It was validated live on the target (see below) and is
fully reversible — keep the stock-hook backup before rolling it out.

| File | Installs to | Purpose |
|---|---|---|
| `parseinfo.sh` | `/opt/etc/parseinfo.sh` | `#!/usr/bin/gawk -f` boot hook run by `AT-HUB.service` (root). Preserves the stock behaviour, then injects the unit below into `/run/systemd/system` and starts it. |
| `dropbear.service` | `/opt/systemd/dropbear.service` | The actual dropbear unit. systemd (PID 1) owns spawning, respawn, cgroup and logging. |

Background and evidence: [`../docs/samsung-tv-secure-boot.md`](../docs/samsung-tv-secure-boot.md),
[`../re/at-hub-persistence-analysis.md`](../re/at-hub-persistence-analysis.md),
[`../re/dotnet-boot-persistence-analysis.md`](../re/dotnet-boot-persistence-analysis.md),
[`../docs/samsung-tv-uep.md`](../docs/samsung-tv-uep.md).

## Design

There is **no writable, non-volatile systemd unit path** on this box:
`/etc/systemd/system` is on the read-only VDFS rootfs (`touch` → `EROFS`),
`/run/systemd/system` is tmpfs, and `/opt` is not in systemd's `UnitPath`. So the
*unit itself* cannot persist. What persists is:

```
/opt/etc/parseinfo.sh          (gawk boot hook, persistent)   -- runs every boot
/opt/systemd/dropbear.service  (unit text, persistent)        -- copied into /run at boot
        │
        │  boot: cp unit -> /run/systemd/system
        │        systemctl restart dropbear.service   (loads the new unit on demand)
        ▼
     systemd (PID 1)  ── owns spawn / respawn / cgroup / logs
        │
        └─ dropbear.service  ExecStartPre: stage binary to /run, gen host key
                             ExecStart:    /run/tv-sshd/dropbear … -p 22
```

Why it works (three independent properties):

1. **The hook file is interpreted, not executed as an ELF.** A `#!/bin/bash`
   script on writable `/opt` trips *bash's* userspace `uep_verify()`;
   `/usr/bin/gawk` has no such check, and gawk's `system()` runs `sh -c "<string>"`
   (an inline string, **not a script file**) so no RW-script UEP check applies to
   our logic either.
2. **The injection path is permitted.** The TV already creates transient units at
   runtime (Samsung's `tv-restart-sdbd.*`), and the systemd D-Bus policy has
   `<policy user="root"><allow send_destination="org.freedesktop.systemd1"/>`
   with no polkit. `/run/systemd/system` is `System::Run transmute=TRUE`, so a
   unit written there is readable by PID 1.
3. **The payload ELF is executed from `/run`, not `/opt`.** The same binary
   exec'd from `/opt` (VDFS) returns `EPERM`; from `/run` (tmpfs) it runs. That is
   UEP, not SMACK (our `User` shell holds `rwxat` on both labels).
4. **A new unit file is loaded on demand — no full reload.** systemd resolves a
   not-yet-loaded unit by name (`Manager.LoadUnit`), which `StartUnit` /
   `systemctl restart` also do implicitly. So writing the file and calling
   `systemctl restart dropbear.service` is enough; `daemon-reload` is only needed
   to re-read units that are *already loaded* (and to re-run generators /
   re-scan enablement). Verified on-device: `systemctl restart <brand-new unit>`
   → `rc=0`, and `busctl call … LoadUnit s <unit>` returned its object path.

## Validation

### Controlled test (2026-11-04, later torn down)

A live test wrote a unit to `/run/systemd/system`, started it, and confirmed:

- unit **loaded from `/run/systemd/system`**;
- a freshly-written unit was loaded **without `daemon-reload`** (both
  `busctl … LoadUnit` and plain `systemctl start`/`restart` worked);
- running as **uid 0**, SMACK `System::Privileged`, in its **own cgroup**
  `/system.slice/…`;
- **logged in as root** on an alternate port via `ssh`;
- **respawned by systemd** after `kill` of the main PID (`NRestarts=1`), port
  re-bound;
- `ExecStartPre`'s inline `sh -c` copy from `/opt` → `/run` ran with **no UEP
  script denial**.

That test also de-risks the earlier open question: a process **freshly spawned by
PID 1** exec'd an unsigned ELF **from `/run`** with no UEP block — i.e. the
boot-time context behaves the same as the interactive one. Exec from `/opt`
(VDFS) remains the known-blocked case.

### Production run (installed on the target, 2026-11-04) — **working**

The design is **installed and in service** on the TV: `/opt/etc/parseinfo.sh` and
`/opt/systemd/dropbear.service`, with the pristine stock hook kept at
`/opt/etc/parseinfo.sh.orig` (see step 0 below for its checksum). Confirmed
across multiple reboots:

- `AT-HUB.service` ran the gawk hook at **~+17 s post-kernel**, exit status `0`;
- the hook copied the unit into `/run/systemd/system` and
  `systemctl restart dropbear.service` returned **`rc=0` with no
  `daemon-reload`** (on-demand `LoadUnit`/`StartUnit`);
- `dropbear.service` came up **`active (running)`** under systemd — own cgroup,
  `NRestarts=0` — and is **listening on `:22`** at **~+23 s post-kernel**
  (≈21 s *before* the TV finishes booting);
- **root SSH login** works (`ssh root@192.0.2.10`);
- `/run/at-hub-boot.log` shows `BEGIN` → `target_version=…` → `stage unit rc=0`
  → `start dropbear rc=0`, and **nothing** is written to `/opt` (no flash
  writes).

The full boot path was also exercised without a reboot via
`systemctl restart AT-HUB.service`, which re-ran the hook and re-started
dropbear the same way.

## Install

Already installed on the target (see *Production run* above); these are the steps
to reproduce it elsewhere. Back up the stock hook first.

```sh
# 0. Snapshot the pristine hook (persistent backup on /opt + off-box copy)
sha256sum /opt/etc/parseinfo.sh     # expect c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61
cp -p /opt/etc/parseinfo.sh /opt/etc/parseinfo.sh.orig
ssh root@192.0.2.10 'cat /opt/etc/parseinfo.sh' > parseinfo.sh.orig

# 1. Install the unit template and the hook (edit-in-place keeps the file's label)
mkdir -p /opt/systemd
cp dropbear.service  /opt/systemd/dropbear.service
cat parseinfo.sh   > /opt/etc/parseinfo.sh        # (push over scp/sdb)

# 2. Reboot, then observe
cat /proc/target_version                 # same stock value
cat /run/at-hub-boot.log                 # gawk ran; stage/start rcs (tmpfs, no flash writes)
systemctl status dropbear.service        # active (running), own cgroup
netstat -tln | grep ':22 '
ssh root@192.0.2.10                    # login as root

# 3. Rollback
cp -p /opt/etc/parseinfo.sh.orig /opt/etc/parseinfo.sh && sync
systemctl stop dropbear.service
rm -f /run/systemd/system/dropbear.service /opt/systemd/dropbear.service
systemctl daemon-reload
```

## Caveats

- **Port collision:** if the interactive launcher
  ([`../dropbear/tv-start-sshd.sh`](../dropbear/tv-start-sshd.sh)) is also running
  a dropbear on 22, this unit won't bind and will retry forever
  (`StartLimitIntervalSec=0`). Pick one: use the unit, or the manual launcher.
- **`authorized_keys`** must exist at `/opt/tv-sshd/authorized_keys` (mode 0600);
  the unit uses `-D /opt/tv-sshd`. The unit's `ExecStartPre` generates the host
  key if missing but does not create `authorized_keys`.
- **The binary source path** is hard-coded to the staging location
  `/home/owner/share/tmp/sdk_tools/dropbear`; adjust if you relocate it.
- A systemd-managed dropbear runs as SMACK `System::Privileged` (vs `User` for the
  interactive launcher) — more privileged label-wise, still uid 0 either way.
- Everything under `/run` (the unit, the staged binary) is recreated each boot by
  the hook; nothing in this design writes to the signed rootfs or to `/opt`
  systemd paths.
- **Logging (no flash writes):** the hook appends to `/run/at-hub-boot.log`
  (tmpfs — survives the boot, wiped at the next reboot; read it over SSH after a
  reboot). Do **not** log to `/opt` (MMC wear). stdout/stderr are *not* usable:
  `/etc/systemd/system.conf` sets `DefaultStandardOutput=null` /
  `DefaultStandardError=null` and `systemd-journald` is inactive, so systemd
  discards both streams and `systemctl status AT-HUB.service` shows no program
  output.
