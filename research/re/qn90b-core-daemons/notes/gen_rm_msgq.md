# gen_rm_msgq — RE note

- **Path / unit:** `/usr/bin/gen_rm_msgq` — ExecStartPre helper of `rscmgr-service.service`
- **Size / stripped:** 9 KB  ·  **Upstream?** Samsung helper
- **Runs as / SMACK label:** root (runs in rscmgr-service unit context).

## Summary (dead end)
Tiny helper: creates the SysV message queue used by rscmgr-service and writes readiness file `/run/rsc_mng_ready`. No exec/fork/system/dlopen imports. No persistence or signature surface. Not independently disableable (tied to rscmgr-service start). **none.**
