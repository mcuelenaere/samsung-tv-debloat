# deviced — RE note

- **Path / unit:** `/usr/bin/deviced` — `deviced.service`
- **Size / stripped:** 340 KB, stripped? **no** (1743 syms, `libdeviced-1.0.3` build tree)  ·  **Upstream?** upstream Tizen `deviced` + Samsung TV deltas (OLED EL/Off-sensing, `VideoSystemDiagnosis`, power-tv, featureconf)
- **Runs as / SMACK label:** **root** — unit has no `User=` / no `SmackProcessLabel=`, `WantedBy=sysinit.target`, `Restart=always`. Runs at SMACK `System` (default for a sysinit root daemon).

## What it does (2–4 lines)
Upstream Tizen system device daemon: udev monitor → device/display/power/extcon/LED/USB state, D-Bus service `org.tizen.system.deviced`. Samsung deltas are TV-specific: OLED EL-sensing / Off-sensing / pixel-refresh, cumulative-runtime standby reboot, `featureconf` reads, panel diagnosis. Load-bearing (power + display + poweroff).

## 1. Persistence surface
**No root-exec of writable or attacker-influenced content.** Full exec inventory:
- Imports **only `system()`** — no `execve`/`execl`/`popen`/`posix_spawn`/`fork+exec`.
- **2 `system()` call sites, both fixed strings:**
  - `powerdown` @0x1d5ac → `system("/etc/rc.d/rc.shutdown &")` — hardcoded, root-owned rootfs script (read-only).
  - `debug_thread_run_loop` @0x1bfe0, queue-cmd `1` → `system(snprintf(fmt, tzplatform_mkpath(8,…)))` — runs a **fixed TZ system path** (crash/defect popup helper). Fed by an internal `g_async_queue`, **not** by any D-Bus method; the only variable it interpolates is a `tzplatform` path, not a caller string.
- `dlopen` targets are all **fixed `/usr/lib` / `/lib` names** (libsyspopup_caller, libdisplay-panel, libpower-control, libvideo-*, libsmart-deadlock, VideoSystemDiagnosis glue, etc.). No `.so` from `/opt`,`/tmp`,`/var`,`/mnt`. `syspopup_launch` / `enable_usb.sh` are a dlsym name and a string in the debug-popup thread, not exec'd.
- `write_flash_reset_flag` @0x28734 writes a fixed 6-byte flag to **`/mnt/systemrw/91913196`** (writable partition) then `reboot()` — guarded by `is_linked_file()` (symlink check). A write, not an exec; path is hardcoded.
- **Verdict: none.** No writable program/script/.so run at boot or via IPC. (Only theoretical redirect: `tzplatform_mkpath` resolves via `/etc/tizen-platform.conf` / `TZ_SYS_*` env — both already root/config-controlled, no remote reach.)

## 2. UEP / signature enforcement
- `##UEP67…:UEP` blob + `signature (%s) is not container type` present, but this is upstream Tizen package-signature **string tooling / dlog**, not an in-process gate deviced enforces on anything it runs. No RSA/SHA/verify imports. deviced does **not** signature-check what it execs/loads. Not a bypass surface (nothing to bypass).

## 3. SMACK / privilege
- Root, label `System`, broad `/dev` + udev + sysfs access. **No in-process access control** — no `cynara_*`, no `smack_have_access` imports. D-Bus method authorization is external (dbus-1 policy + SMACK label), not in the binary.
- **D-Bus:** bus name `org.tizen.system.deviced` (+ sub-names `.core .display .power .PowerOff .PowerOffEvent .Process .SysNoti .Usbdev .Usbhost .ExtCon .Key .Led .displayState`), objects `/Org/Tizen/System/DeviceD/{Core,Display,Power,PowerOff,Process,SysNoti,Usbdev,Usbhost,ExtCon,Key,Led,…}`. Dispatch: `_method_call_handler` → `_dbus_handle_lookup_method`. Caller identified via `GetConnectionUnixProcessID` + `get_name_from_pid` (logging only).
- **Confused-deputy check: no method reaches `system()`/exec with a caller-supplied path/command** (both sinks are fixed, neither is wired to a D-Bus handler). Process/SysNoti methods act on pids/signals/sysfs (upstream), not on attacker paths — no root-exec deputy.

## 4. Debloat
- **Do not disable.** Load-bearing: owns power/display/poweroff/standby + OLED panel protection (EL/off-sensing, pixel-refresh) + USB/extcon. `WantedBy=sysinit.target`, many clients call `org.tizen.system.deviced`. Disabling → no clean shutdown/standby, display-state breakage, panel-protection skipped. No `ConditionPathExists` marker to gate it.

## Leads worth a deeper look
- `powerdown` @0x1d5ac — only fixed-path `system()`; `/etc/rc.d/rc.shutdown` is on read-only rootfs (if a rootkit remounts rw, editing that script = a shutdown-triggered root exec, but that already presupposes rw rootfs).
- `write_flash_reset_flag` @0x28734 — root writes `/mnt/systemrw/91913196`; confirms `/mnt/systemrw` is the device's writable partition (persistence target for other vectors, not via deviced).
- `debug_thread_run_loop` @0x1bfe0 — `system()` cmd embeds `get_name_from_pid()` output into a g_string report (not into the exec'd cmd); exec arg itself is a fixed tzplatform path. Not reachable from D-Bus.
