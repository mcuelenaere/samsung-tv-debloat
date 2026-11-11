# usb-launcher-service — RE note

- **Path / unit:** `/usr/bin/usb-launcher-service` — `usb-launcher-service.service`, `After=csf-content-engine.service`, `WantedBy=delayed.target`, `Restart=always`
- **Size / stripped:** 88 KB, stripped=yes  ·  **Upstream?** Samsung `usb-launcher-service 24.09.26.0` (date-versioned, recent)
- **Runs as:** **`User=owner` `Group=users`** (non-root)

## What it does
Watches USB mass-storage connect/disconnect (`usb_set_connected_cb`, `usb_device_get_mountpath`, `usb_mass_storage_get_filesystem_type`) and launches a media/player app for the inserted device: `LaunchVideoPlayer`, `LaunchDemoPlayer`, `LaunchInternalDemoPlayer`, `LaunchUsbLauncherTV`, `LaunchPlayer`, `LaunchMyconent`. Classes `CMMDeviceMgr` / `CMMLaunchApp`; parses JSON (`json_value.cpp`).

## 1. Persistence surface
Launches **installed apps via AUL** on USB insert — no direct `system`/`execve`/`fork+exec` (triage imports are USB + `system_info_*` + AUL only). "Demo player" auto-launch is a known Samsung retail path.

## 2. UEP / signature
None in-process.

## 3. SMACK / privilege
Non-root (`owner`). Interesting mainly as an **attacker-reachable-by-physical-USB** input surface: the mount path and an on-device JSON drive which app launches. But targets are AUL-launched signed apps and the process is `owner`, so direct escalation value is low.

## 4. Debloat
Controls USB auto-launch of the media/demo player. **Safe-ish to disable if you don't want USB auto-play of video**, but it also backs the normal "USB stick → video player" UX — mask only if that UX is unwanted. Non-essential to boot. Not telemetry.

## Leads worth a deeper look
- Where the JSON (`json_value.cpp` / `MMLaunchApp.cpp`) is read from — if any field is taken off the **USB medium** and influences the launch target/args, that's a physical-access input path worth tracing (still AUL-mediated, `owner`-bounded).
