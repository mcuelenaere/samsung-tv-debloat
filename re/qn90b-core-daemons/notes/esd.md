# esd — RE note

- **Path / unit:** `/usr/bin/esd` — `esd.service`
- **Size / stripped:** 80 KB, stripped: **y**  ·  **Upstream?** Upstream Tizen (`esd-0.2.4`, "Event System Daemon"); GDBus + Tizen cion/eventsystem
- **Runs as / SMACK label:** **`User=app_fw` / `Group=app_fw`**, **`SmackProcessLabel=System`**, `Type=dbus BusName=tizen.system.event.app2esd`. `Requires=buxton2.service`. Least-privileged of the five.

## What it does (2–4 lines)
Standard Tizen app-framework daemon. Broadcasts system events (`tizen.system.event.*`: boot_completed, battery/charger, display_state, low_memory, language/region/font, location/roaming, earjack, etc.) to apps, and **launches apps on registered events** (`launch_on_event` appcontrol, cion on-demand). Core Tizen plumbing; **no telemetry, no outbound network, no URLs.**

## 1. Persistence surface — none at boot
No boot-time exec of writable content. It launches **apps** (by app-id, via AUL/cion) in response to events — not shell, not files. Writes marker `/tmp/esd_ready`. Launch targets come from the Tizen event-launch DB (sqlite via `sqlite3_exec`) registered by installed packages, not from writable config.

## 2. UEP / signature enforcement — present (platform-cert gate)
**Yes:** `__esd_check_platform_cert` / `__esd_check_certificate_match` via `cert-svc-vcore` + `pkgmgrinfo` compare an app's certificate before honoring **trusted / user-event** launches (`RequestTrustedEventLaunch`, `"%s has a platform certification"`). Caller identity checked with **cynara** (`cynara_creds_gdbus_*`) + D-Bus `GetConnectionUnixProcessID`/`User`. This is an app-launch authorization gate, not a code-signing gate on esd itself.

## 3. SMACK / privilege
Runs as **app_fw / label `System`** (not root) — good. Deputy is well-guarded: cynara + platform-cert checks before trusted launches; untrusted events limited.

## 4. Debloat — **DO NOT disable** (core app framework)
- **Collects/transmits:** nothing off-device. Internal system-event bus only.
- **What breaks if disabled:** any app relying on launch-on-event or on `tizen.system.event.*` broadcasts — boot_completed autostart, charger/battery/display/low-memory reactions, locale-change propagation. High risk of breaking app behavior and TV boot. **Not telemetry; not a debloat candidate.**
- **Marker trick:** none, and don't add one.
- **Recommendation:** **leave enabled.** Masking is likely to break app launch and system-event delivery. No privacy upside.

## Leads worth a deeper look
- None relevant to debloat/root. (If ever needed: `esd` sqlite event-launch DB lists which apps start on which events — an inventory source, not an attack surface.)
