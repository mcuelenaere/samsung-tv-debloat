# QN90B core daemons — RE findings

Static reverse-engineering of the 22 collected daemons (ARM32 Tizen 6.5), done
read-only on macOS with Ghidra 12.2 headless + objdump/strings/nm. Per-daemon
detail in `notes/`. This file summarizes the security posture relevant to the
project's documented goals: understanding root/persistence constraints, how
UEP/signature enforcement is structured, and which services are safe to debloat.

Everything here is inference from the pulled binaries. Each "verify on-device"
marks a conclusion whose real-world reachability depends on live dbus/SMACK
policy and partition mount flags that are **not** part of this pull.

## Attack-surface ranking (for defensive understanding / debloat decisions)

| # | Daemon | Priv | Observation |
|---|--------|------|-------------|
| 1 | **app2sd-server** | root / `System` | D-Bus install API builds root `system()` cryptsetup commands from caller-supplied `pkgid`/`usb_path` with no charset validation in the binary — classic confused-deputy shape. Reachability gated by its bus-name send policy. |
| 2 | **pkgmgr-server** | root-capable / `System` | UID-only authorization (no cynara import at all); forwards unsanitized client args to root install backends. Actual package-signature logic is in backends **not in this pull**. |
| 3 | **fconfigd** | root / `System` | Serves feature flags from a writable, **unsigned** DB (`/opt/data/fconfig/fconfig.db`) trusted across boots; request socket `/run/fconfig-0` is mode 0666 and SET/RESET are not authorized (peer creds used only for logging). |
| 4 | **buxton2d** | root | Unit sets `BUXTON_CYNARA_PERMISSIVE_MODE=1`; the cynara/SMACK access check is short-circuited, so label enforcement on the vconf store is effectively off. DB layers under `/var/lib/buxton2` are writable/unsigned. |
| 5 | **stability-monitor** | root | inotify worker on an app-writable dir (`/tmp/tager/req/`, which the daemon itself relabels `User::App::Shared`) leads to a root `popen()` on file-derived content. On tmpfs, so state is per-boot. |
| 6 | **parseinfo.sh** (AT-HUB) | root | A stock boot unit executes a root-owned bash script from the writable `/opt` partition; integrity rests solely on kernel-side UEP. This is the persistence lead already noted in `docs/` (danielyc #17). |
| 7 | **issue_report_agent** | root | Invokes root helper scripts (`ira_encrypt.sh`, `Hitcomp_buffer.sh`) from `/opt/usr/share/HitComp/`; whether that path is writable on this set is unverified. |
| 8 | **storaged** | `System::Privileged` | `dlopen`s modules from a fixed dir (`/usr/lib/storaged`); only relevant to persistence if rootfs can be remounted writable. |
| — | deviced, amd, app-boot-manager, usb-launcher-service, app-control-service, resourced, reclaimd, rscmgr-service, resource-center, gen_rm_msgq, pkginfo-server, eventd, esd, emergencyLogTransmitter | — | No reachable write-to-root-exec found; mostly load-bearing. See individual notes. |

## Cross-cutting observations

- **No daemon re-verifies signatures of its own inputs.** UEP is a *file-level*
  signature (`##UEP…:UEP` trailers on `parseinfo.sh`, `resourced`) enforced
  kernel-side at exec time — none of these services check it. So the integrity
  of config DBs, IPC sockets, and `/opt` scripts rests on partition mount flags
  plus SMACK, both of which are comparatively weak (items 3/4/6).
- **Writable, unsigned surfaces confirmed from the binaries:** `/opt`,
  `/opt/data` (fconfig DB), `/var/lib/buxton2` (vconf DBs), `/mnt/systemrw`
  (deviced factory-reset flag). rootfs is read-only — that is the main structural
  barrier limiting items 1/2/8 to runtime rather than on-disk tampering.
- **Authorization, not cryptography, is the weak layer.** `fconfigd`,
  `buxton2d`, and `pkgmgr-server` each ship with weak/absent caller
  authorization on privileged operations. These matter more than any crypto
  question because they involve no memory-corruption — only whether SMACK policy
  lets a given label reach the endpoint.

## Debloat guidance (from the notes)

**Safe to mask** (telemetry / field diagnostics; nothing user-facing breaks):
- `stability-monitor` — also stops a hardcoded **plaintext-HTTP** phone-home to a
  Samsung-internal dev host (`168.219.244.42`, a `168.219/16` address left in prod
  firmware). `systemctl mask`.
- `issue_report_agent` — mask + remove its two `.wants` symlinks
  (`starter.target.wants`, `vdtizen-bootseq-PNP-starter.target.wants`).
- `emergencyLogTransmitter` — mask; it already self-gates ("ELT disabled on this
  SET") and supports a `ConditionPathExists` marker.

**Droppable with minor effect:** `reclaimd` (proactive memory reclaim only;
kernel + resourced still handle real OOM).

**Keep (load-bearing):** deviced, storaged, resourced, rscmgr-service,
resource-center, buxton2d, fconfigd (system-wide consumers), pkgmgr-server,
pkginfo-server, amd, app-boot-manager, esd, eventd. Disabling these risks a
blank boot or broken app launch / power management.

## Gaps in this pull (needed before any deeper conclusion)

The decisive logic for several items is in files that were **not collected**:
- `libamd.so.1`, `libaul-server.so.0` — amd's real launch/authorization code
  (the on-disk `amd` is a 15 KB stub).
- `/etc/package-manager/backend/*` and the WGT/TPK install backend +
  `signature-validator` / `cert-svc` — where package-signature enforcement
  actually lives (pkgmgr-server only routes to them).

On-device facts to confirm the posture items above (observation, not action):
whether UEP is enforcing and covers `/opt`; the dbus send-policy and SMACK rules
governing which labels can reach the app2sd / pkgmgr bus names and the
fconfig/buxton sockets; and the writability of `/opt/usr/share/HitComp` and
`/lib/modules`.

## Method / reproducibility

Tooling in `notes/`: `triage.sh` (categorized strings + dynamic imports + local
symbols), `ghidra-decompile.sh` + `DumpDecompile.java` (targeted/function-level
Ghidra headless decompile; needs brew `openjdk@25`, path baked into the wrapper).
`shasum -a 256 -c SHA256SUMS` verifies the inputs.
