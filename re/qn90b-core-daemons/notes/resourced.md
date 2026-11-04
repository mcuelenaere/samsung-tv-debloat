# resourced — RE note

- **Path / unit:** `/usr/bin/resourced` — `resourced.service` (WantedBy=vdtizen-bootseq-predelayed.target)
- **Size / stripped:** 208 KB, stripped? **n** (full symbols)  ·  **Upstream?** upstream Tizen `resourced` (OOM/memory/cgroup resource daemon)
- **Runs as / SMACK label:** root (no User=), file mode `0711`. MemoryLimit=30M, Restart=on-failure.

## What it does
Upstream Tizen resource daemon: monitors app launches via D-Bus prelaunch handler, assigns OOM scores, manages cgroup/memory limits, loads internal "modules" (plugins compiled in). Not a Samsung-proprietary piece.

## 1. Persistence surface
**none.** No execve/system/popen/fork imports. "aul_launch"/"launch request" strings are log lines for launch *events it observes* via D-Bus, not process exec. "launching all modules" = in-process plugin init, no dlopen of external path. No writable-path trust.

## 2. UEP / signature enforcement
Carries a passive `##UEP63 0x...:UEP` signature trailer appended to the binary (Samsung firmware image integrity on the file itself). Daemon does NOT verify signatures at runtime — purely a signed artifact. Relevant only in that replacing it on a signed partition needs re-sign / unsigned mount.

## 3. SMACK / privilege
Runs root. Only privileged sinks: writes OOM scores / cgroup/memory nodes. No exec of attacker-influenced input. imports: eventsystem_send_system_event, system_info_get_custom_int only.

## 4. Debloat
**Load-bearing — do NOT disable.** Core OOM/memory governor on the predelayed boot target; apps rely on its OOM-score assignment. Disabling risks memory instability / unmanaged OOM kills.

## Leads worth a deeper look
- none (upstream, no exec/writable-path foothold).
