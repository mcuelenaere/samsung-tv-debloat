# emergencyLogTransmitter (ELT) — RE note

- **Path / unit:** `/usr/bin/emergencyLogTransmitter` — `emergency-log-transmitter.service`
- **Size / stripped:** 199 KB, stripped: **y**  ·  **Upstream?** Samsung-proprietary (`emergency-log-transmitter-0.2`); boost (filesystem/property_tree/iostreams)
- **Runs as / SMACK label:** **root** (no `User=`/label in unit). `WantedBy=multi-user.target`, `Restart=always`. Already has **`ConditionPathExists=!/tmp/eltlaunch.cond`**.

## What it does (2–4 lines)
Orchestrator for "emergency" log collection/transmission. It does **not** upload itself — on a vconf trigger it `fork`/`execl`/`system`-launches a separate **TLC** app (`/opt/usr/apps/emergency-log-transmitter/tlc`) with a working URL, plus HSMC and a "robustness test". Also renders a QR code (`eltQRCode.png`) for on-screen log retrieval. Feature-gated: logs `"ELT Feature is disabled on this SET"` when off.

## 1. Persistence surface — minor
- Launches apps via `app_control_send_launch_request` (`Utils::appLaunch`/`appLaunchWithArgs`) and via `system`/`execl`/`fork` (`launch_TLC`, `launch_HSMC`, `launch_robustness_test`, `launch_log_transmitter`). Target = fixed app id / `/opt/usr/apps/emergency-log-transmitter/tlc`. The **URL handed to TLC comes from `/opt/usr/apps/emergency-log-transmitter/tlc/workingURL.txt` and vconf `rtc/db/comss/gpmurl/tls`** — if `workingURL.txt` (under `/opt/usr/apps`) is writable, you redirect where logs go (data-exfil redirect, not code-exec).
- Guarded against re-entry by `/tmp/eltlaunch.cond` and `STOP_TLC_LAUNCH` / robustness-test-running checks.
- No exec of writable *code* at boot.

## 2. UEP / signature enforcement
None. Ships a TLS CA bundle `/opt/data/cert/vdca.pem` (for the TLC HTTPS upload, `gpmurl/tls`). No code/UEP gate.

## 3. SMACK / privilege
Runs **root**, no label pinning. Confused-deputy limited to launching a fixed Samsung app with a (vconf/file-sourced) URL argument.

## 4. Debloat — **telemetry orchestrator; safe to mask (cleanest here)**
- **Collects/transmits:** triggers upload of HSM logs, KPI logs, robustness-test results. Reads identity/endpoint from vconf: `db/comss/atoken`, `duid`, `hwduid`, `infolinkversion`, `db/adagent/psid`, `rtc/db/comss/gpmurl/tls(state)` (the GPM/InfoLink upload base URL — **dynamic, not hardcoded**), `memory/sp_kpi/send_hsm_log`, `db/factory/send_kpi_log`. Feature flags: `com.samsung/featureconf/remote_service.enable`, `hitron_policy`, and an FMS feature check.
- **WHERE:** actual transmit is done by the **TLC** child over HTTPS to the runtime **GPM URL** (`gpmurl/tls`); ELT itself only launches it. QR fallback writes `eltQRCode.png`.
- **What breaks if disabled:** emergency/robustness log upload to Samsung and the on-screen QR log-retrieval. Nothing user-facing. **Prime debloat target**, and it's already feature-gated off on many sets.
- **Marker trick:** the unit **already uses `ConditionPathExists=!/tmp/eltlaunch.cond`**, but `/tmp` is tmpfs (reset each boot) so that is only a runtime re-launch guard, **not** a persistent disable. For a clean persistent off either:
  - **mask it** (`systemctl mask emergency-log-transmitter.service`), or
  - change the condition to a **persistent** marker, e.g. add `ConditionPathExists=!/opt/usr/share/elt-disable` and `touch` that file (survives reboot, mirrors the PISA `pisa-service` pattern). Masking is simpler.
- **Recommendation:** `systemctl mask emergency-log-transmitter.service`. Belt-and-suspenders: it also honors the `remote_service.enable` featureconf and vconf `send_*_log` flags.

## Leads worth a deeper look
- Is `/opt/usr/apps/emergency-log-transmitter/tlc/workingURL.txt` writable? (log-destination redirect).
- Decompile the TLC child (`/opt/usr/apps/.../tlc`, separate binary) to see the real GPM upload endpoint + payload — out of scope for this daemon set.
