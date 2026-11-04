# Boot persistence via `AT-HUB.service` → `/opt/etc/parseinfo.sh`

This directory holds an **example** replacement for the stock
`/opt/etc/parseinfo.sh`, the one writable-storage script that a root,
boot-enabled systemd service executes on this TV (Tizen 6.5, S95B/PontusM,
build `T-PTMDEUC-1720.7`).

**Nothing here is installed on the TV.** The file is provided for review and a
later, deliberate, reversible test.

Background and evidence:

- [`../docs/samsung-tv-secure-boot.md`](../docs/samsung-tv-secure-boot.md) — boot
  chain, secure boot, why `/opt` is the only useful writable surface (§6).
- [`../re/at-hub-persistence-analysis.md`](../re/at-hub-persistence-analysis.md) —
  full analysis: how systemd runs it, the UEP script-check internals, the ranked
  candidate list, and the test plan.
- [`../docs/samsung-tv-uep.md`](../docs/samsung-tv-uep.md) — UEP components.

## What the stock script does (and why the example keeps doing it)

`/opt/etc/parseinfo.sh` is a 7-line `#!/bin/bash` script whose *entire* effect is:

```sh
echo "__${REPO}__${MODEL}" > /proc/target_version
```

On this unit that yields `__TIZEN-TRUNK2022-PontusM-RELEASE_20260225.1__T-PTMDEUC`.
A bounded grep for consumers of `target_version` under `/usr/bin`, `/usr/sbin`
and `/usr/apps` found none, so it is probably consumed off-device (OTA /
registration / MICOM). Its criticality looks low — nothing `Requires=` AT-HUB and
a failure does not stall boot — but it is an identity/version surface, so the
example reproduces it **exactly** before doing anything else.

## What the example does

1. Writes the same `__<BUILD>__<MODEL>` to `/proc/target_version` (stock parity).
2. Copies the unsigned dropbear from persistent `/opt` staging to `/run`, then
   starts it from there (`/run` is the placement that is known to work — see
   below), logging to `/run/tv-sshd/dropbear.log`.
3. Stays resident forever, because `AT-HUB.service` uses the default
   `KillMode=control-group`: if the `ExecStart` process exits, systemd kills the
   cgroup and takes dropbear with it.

It works because of two separate UEP properties:

- **The hook file is interpreted, not executed as an ELF.** A `#!/bin/bash`
  script on writable `/opt` trips *bash's* userspace `uep_verify()`. `/usr/bin/gawk`
  has no such check, so `#!/usr/bin/gawk -f` runs. gawk's `system()` then runs
  `sh -c "<string>"` — an inline string, **not a script file** — so no RW-script
  UEP check applies to our logic either.
- **The payload ELF is executed from `/run`, not `/opt`.** The same binary
  executed from `/opt` (VDFS) returns `EPERM`; from `/run` (tmpfs, `System::Run`)
  it runs. That difference is **UEP, not SMACK** — our shell (`User`) holds
  `rwxat` on both the `User::App::Shared` (`/opt`) and `System::Run` (`/run`)
  labels.

## The open question

`docs/`/`re/` established that `samsung-tv-root` does **not** disable UEP (its
UEP toggle is the separate, build-gated `qn90b uep disable` command, refused on
this build; the root flows never call it). So the running dropbear is unsigned
*yet* executes — because it is placed where UEP permits execution, not because
UEP is off.

What is **not** yet proven: whether that placement rule still holds for a process
started fresh from PID 1 by systemd (as this hook would be), versus a process in
the root toolkit's own tree. The evidence (UEP appears to key on the path/label,
`SecureZoneContext` / `zone_verify_uep_level`) suggests it should, but the one
decisive experiment below settles it.

## If/when you want to test (reversible, one reboot)

Not run — provided for a later, explicit decision.

```sh
# 0. Snapshot the pristine file (persistent backup on /opt + keep it out-of-band)
sha256sum /opt/etc/parseinfo.sh          # expect c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61
cp -p /opt/etc/parseinfo.sh /opt/etc/parseinfo.sh.orig
ssh root@192.0.2.10 'cat /opt/etc/parseinfo.sh' > parseinfo.sh.orig   # off-box copy

# 1. Install the example (edit-in-place keeps the file's SMACK label)
cat persistence/parseinfo.sh > /opt/etc/parseinfo.sh    # (pushed over scp/sdb)

# 2. Reboot, then observe
cat /proc/target_version                 # expect the same stock value
cat /opt/at-hub-boot.log                 # did gawk run? did dropbear start?
cat /run/tv-sshd/dropbear.log
netstat -tln | grep ':22 '
ps -Z | grep -E 'gawk|dropbear'
dmesg | grep -iE 'SfdUep|uep|Blocked'    # any UEP denial?

# 3. Rollback if needed (no reboot required if a shell is still reachable)
cp -p /opt/etc/parseinfo.sh.orig /opt/etc/parseinfo.sh && sync
systemctl restart AT-HUB.service         # optional, to observe faster
```

If dropbear fails to start at boot but works from an interactive root shell, the
difference is the boot-process UEP state, and the fallback is a `.NET`-IL
boot hook (`../re/dotnet-boot-persistence-analysis.md`) or a startup-slot TPK.

## Caveats

- Keep `authorized_keys` present at `/opt/tv-sshd/authorized_keys` (mode 0600) —
  dropbear uses `-D /opt/tv-sshd`.
- The dropbear binary must exist at the staging path
  `/home/owner/share/tmp/sdk_tools/dropbear` (see [`../dropbear/`](../dropbear/)).
- `KillMode=control-group` means any restart of `AT-HUB.service` also restarts
  dropbear; the keep-alive loop is intentional.
- This hook runs **before** most of the stack (it is `WantedBy=starter.target`);
  keep it fast and dependency-free.
