# app-boot-manager — RE note

- **Path / unit:** `/usr/bin/app-boot-manager` — `app-boot-manager.service`, `Type=oneshot`, `RemainAfterExit=yes`, `DefaultDependencies=no`. Unit file opens with an inert `#RPAC=<garbage>` anti-tamper tag line.
- **Size / stripped:** 101 KB, stripped=yes  ·  **Upstream?** Samsung `app-boot-manager 1.3.9` (build paths: `boot_mode.c`, `job_mgr.c`, `modes_*_boot_mode.c`)
- **Runs as:** **root** (no `User=`)

## What it does
Decides and triggers the first-screen app at boot (tv-viewer vs ambient/art vs custom launcher vs bizapp vs configuration mode). Reads device state via `system_info_*` / `mgr_util_get_system_info_*` and launches the chosen app through **AUL** (`aul_listen_app_dead_signal`, launch via socket). Several boot "modes" select which appid to launch.

## 1. Persistence surface
Launches **installed, signed** apps via AUL — no direct `system`/`execve`/`fork+exec` in the binary (triage imports: `connect`,`socket`,`prctl`,`open64`,`fopen64`,`aul_*` only). Boot-app choice is data-driven from `system_info`/feature values; "ambient / art app is launched by script" is a **log line about another component**, not a script exec here. Note `stolen tv. don't launch tv-viewer` → an anti-theft gate also keys off system info.

## 2. UEP / signature
No in-process signature logic (`cert_option(%d)` is a log line). Relies on AUL/amd to only launch signed apps.

## 3. SMACK / privilege
Root, but a confused-deputy only to the extent its boot-app selection is attacker-influencable via `system_info`/feature DB — and the launch itself is AUL-mediated (signed apps). Worth noting as an *influence* path if feature/vconf values are writable (see `fconfigd`/`buxton2d` notes), but not a direct exec.

## 4. Debloat
**Do not disable** — oneshot that brings up the TV's first app; masking it likely yields a blank boot.

## Leads worth a deeper look
- Which `system_info`/feature key selects the boot mode/appid (cross-ref `fconfigd` keys) — flipping it changes what launches at boot, AUL-mediated.
