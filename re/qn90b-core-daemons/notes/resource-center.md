# resource-center — RE note

- **Path / unit:** `/usr/bin/resource-center` — `resource-center.service` (DefaultDependencies=no, Environment=LD_USE_LOAD_BIAS=1)
- **Size / stripped:** 371 KB, stripped? **n**  ·  **Upstream?** Samsung tv-resource-manager (policy/resource-center, uses libtv-resource-manager.so.1)
- **Runs as / SMACK label:** **User=owner Group=users (NON-root)**, Restart=always.

## What it does
Samsung resource policy engine: audio/video decoder/scaler arbitration, quality-zone switching, per-app resource strategies (PMPolicy*/PMRscStrategy*). D-Bus/GVariant message handler.

## 1. Persistence surface
**none.** No execve/system/popen/fork. dlopen = fixed root-owned SONAMEs (libtv-resource-manager.so.1, libaudio-control.so.0, libproc-stat.so.5, libsmart-deadlock.so, liblwipc.so). aul import is read-only `aul_app_get_pid`. "Launch"/"Relaunched"/SetLaunchCount symbols = internal relaunch-count bookkeeping, not process exec. No writable-path trust.

## 2. UEP / signature enforcement
None at runtime. N/A.

## 3. SMACK / privilege
Runs as unprivileged `owner`, so **lowest value of the set** even if a bug existed. No exec sink.

## 4. Debloat
**Load-bearing / risky.** Governs AV decoder/scaler allocation and quality zones; disabling likely breaks media resource arbitration (playback/zone switching). Keep.

## Leads worth a deeper look
- none.
