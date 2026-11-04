# reclaimd — RE note

- **Path / unit:** `/usr/bin/reclaimd` — `reclaimd.service` (unit prefixed with Samsung `#RPAC=...` obfuscation/anti-tamper comment tag; inert)
- **Size / stripped:** 30 KB, stripped? **n** (symbols; build path reclaimd-1.1)  ·  **Upstream?** Samsung-authored (small, src/{config,main,reclaimer,system_monitor}.cpp)
- **Runs as / SMACK label:** root, OOMScoreAdjust=-1000 (never OOM-killed). StandardOutput=tty /dev/console.

## What it does
Proactive memory reclaimer. Polls /proc/meminfo + /proc/stat; when free mem < watermark and CPU idle, walks /proc/*/comm and writes `/proc/<pid>/reclaim` (ioctl/write) to push pages out, skipping names in a blacklist. All tunables come from `RECLAIMD_*` env in the unit.

## 1. Persistence surface
**none.** No exec/fork/system. `RECLAIMD_BLACK_LIST=/etc/reclaimd/black_list` is read-only parsed (parse_black_list) into process-name skip list — if writable, max impact is choosing which procs get reclaimed (DoS / anti-reclaim), NOT code exec. No launch of writable content.

## 2. UEP / signature enforcement
None (no signature trailer matched, no verify logic). N/A.

## 3. SMACK / privilege
Root. Privileged action = writing /proc/<pid>/reclaim. Also subscribes to AMD D-Bus signal (g_dbus_connection_signal_subscribe) — read-only. No confused-deputy exec.

## 4. Debloat
**Droppable** with caveat: it only does proactive reclaim; kernel + resourced still handle real OOM. Disabling increases memory pressure / may raise OOM-kill frequency but breaks nothing functionally. Safe to disable on a debloat if memory headroom is OK.

## Leads worth a deeper look
- none.
