# stability-monitor — RE note

- **Path / unit:** `/usr/bin/stability-monitor` — `stability-monitor.service`
- **Size / stripped:** 2441 KB, stripped: **y**  ·  **Upstream?** Samsung-proprietary (`stability-monitor-0.5`); statically links TensorFlow-Lite + Eigen + ruy (on-device ML)
- **Runs as / SMACK label:** **root** (unit has no `User=`, no `SmackProcessLabel=`). Starts at `sysinit.target` (very early), `Requires=dbus.service`, `Restart=always`.

## What it does (2–4 lines)
System performance/stability watchdog. Monitors CPU/hrtimer thresholds, foreground/background app timeouts (D-Bus `org.tizen.aul.AppStatus` / `org.tizen.resourced.process`), flash, lost video signal, hardware errors (`/proc/kpi_hw_error_info`). Runs a Keras/TFLite model `/usr/bin/New_MemoryPattern_keras.tflite` ("DSOT" memory-pattern predictor) to drive proactive memory cleanup. Implements the **"tager"** cache/OOM manager that TV web apps drive via files in `/tmp/tager/req/`. **Phones KPI/DSOT telemetry home via a shelled-out `curl`.**

## 1. Persistence surface — **LEAD (confused-deputy, volatile)**
- Spawns an **inotify thread on `/tmp/tager/req/`** and acts, as root, on request files written there: `runCmdNativeApps`, `killApp`, `clearAppCache`, `memBoost`, `AdaptiveClean.txt`, `CleanAll.txt`, `calcAppDataCacheSize_`. `runCmdNativeApps` → `_Z16runCmdNativeAppsv` → **`popen`** on content derived from the file (`popen` is imported; format strings `"%s %s"`, `AppID:%s`).
- The daemon itself **chsmacks `/tmp/tager/` + `/tmp/tager/req/` to `User::App::Shared`** (shells out `/bin/chsmack`), i.e. it *deliberately makes the request dir app-writable*. Any process with that label can drop a request file → root-side action. This is a real **root confused-deputy** worth a closer look for the rooting effort (though `/tmp` is tmpfs, re-created each boot, so not non-volatile on its own).
- Other root exec via `system`/`popen`: `/usr/bin/TagDebug &`, `/usr/bin/rm -rf /opt/data/VideoTest/videos/*`, `launch_app com.samsung.mtm-banner`, launches `/opt/usr/apps/tomato-agent/bin/tomato-agent`. Targets are fixed rootfs paths (not writable), except the tager channel above.
- Config is read from `com.samsung/featureconf/*` (buxton) and vconf — not from writable exec content.

## 2. UEP / signature enforcement
None for code. Only `flatbuffers`/`tflite` `Verify*` (ML-model flatbuffer sanity-check of `New_MemoryPattern_keras.tflite`), not a security gate. No cert/UEP.

## 3. SMACK / privilege
Runs **root**, no label pinning. Weakens isolation by labelling its IPC dir `User::App::Shared`. `runCmdNativeApps` = attacker-influenced `popen` as root (see §1). No cap management seen.

## 4. Debloat — **phones home; safe to mask**
- **Collects/transmits:** `curlKPI` class builds a curl command (`generate_curl_command`) and `send_data_to_server` / `send_to_local_kpi` POSTs KPI/DSOT data (DUID via `set_duid`, process list, abnormal-indication event `EV001`, HW error info). Log line: `"Data to KPI sent!"`.
- **WHERE (endpoint):** hardcoded in `.rodata` @0x2290c4 — plaintext **`http://168.219.244.42/SriD_Dev/sourabh1.j/dsotkpiserver/cgi-bin/server.py`** — a **Samsung-internal (168.219.0.0/16) developer box left in production firmware**. Almost certainly unreachable from a home LAN, but it is an unconditional outbound attempt.
- **What breaks if disabled:** proactive memory cleanup / OOM "AdaptiveClean" for apps, app-timeout watchdog, DSOT memory prediction, KPI upload. TV works; heavy multi-app memory pressure is handled less aggressively. The `/tmp/tager` cache-manager IPC for the device web UI stops (cache-clear buttons in the service menu may no-op).
- **Marker trick:** unit has **no `ConditionPathExists`**. No built-in off switch. `db/factory/send_kpi_log` vconf gates KPI only, not the daemon.
- **Recommendation:** `systemctl mask stability-monitor.service` (it's `WantedBy=sysinit.target`, so a persistent mask on the rootfs/overlay is the clean kill). If you want to keep the memory manager but stop the phone-home, instead null-route 168.219.244.42 or blank the KPI — but masking is the lazy correct debloat. Removes a root `popen`-from-tmpfs surface as a bonus.

## Leads worth a deeper look
- `0x5b4c0 _Z16runCmdNativeAppsv` — exact command format `popen`'d as root from `/tmp/tager/req/runCmdNativeApps` (app-writable). Rooting candidate.
- `0x510ec curlKPI::generate_curl_command` / `0x56228 send_data_to_server` — confirm exact curl argv + whether DUID/serial is exfiltrated.
- `/bin/chsmack ... User::App::Shared /tmp/tager/req/` — who legitimately writes here (the TV "tager"/device-manager web app).
