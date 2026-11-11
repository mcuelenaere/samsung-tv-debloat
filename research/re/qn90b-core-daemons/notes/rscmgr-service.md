# rscmgr-service — RE note

- **Path / unit:** `/usr/bin/rscmgr-service` — `rscmgr-service.service` (DefaultDependencies=no, ExecStartPre=/usr/bin/gen_rm_msgq, Environment=LD_USE_LOAD_BIAS=1)
- **Size / stripped:** 242 KB, stripped? **n**  ·  **Upstream?** Samsung tv-resource-manager (rscmgr-service ~0.3)
- **Runs as / SMACK label:** root (no User=), Restart=always.

## What it does
Samsung TV resource-manager service (bandwidth/scaler/resource arbitration). Creates SysV msgq (via gen_rm_msgq pre-step), serves CMessageHandler requests, drops readiness flags, can show syspopups.

## 1. Persistence surface
**none.** No execve/system/popen/fork. dlopen targets are fixed SONAMEs resolved from root-owned /usr/lib (libbundle.so.0, libsyspopup_caller.so.0.1.0, libproc-stat.so.5, libsmart-deadlock.so, liblwipc.so) — not attacker-writable. aul imports are read-only (aul_app_get_appid_bypid, aul_app_get_status, aul_app_lifecycle_get_state) — **no aul_launch**. `memory/boot/cold_poweroff_request_pkg` is a vconf string it SETS, not a launch. Writable touches are predictable status flags only: `/tmp/.rsc_mgr_ready`, `/tmp/.scaler_swap_done`, `/tmp/.wm_ready`, `/run/rsc_mng/`. Pre-creating those flags = at most readiness confusion, not exec.

## 2. UEP / signature enforcement
No runtime signature/verify logic. N/A.

## 3. SMACK / privilege
Root. dlopen of fixed root-owned libs = not a confused deputy (no writable lib path, LD_LIBRARY_PATH not set by unit; LD_USE_LOAD_BIAS is unrelated to search path). No exec of external input.

## 4. Debloat
**Load-bearing / risky.** Other components likely block on `/tmp/.rsc_mgr_ready` / `.scaler_swap_done` / `.wm_ready` and on the SysV msgq. Disabling can hang dependents (scaler/window-manager resource handshakes). Keep unless a dependency map proves nothing waits on it.

## Leads worth a deeper look
- Minor: vconf `memory/boot/cold_poweroff_request_pkg` written as "tv-resource-manager" — SET only, no aul_launch; not a foothold.
