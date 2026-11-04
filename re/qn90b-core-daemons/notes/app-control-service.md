# app-control-service — RE note

- **Path / unit:** `/usr/bin/app-control-service` — `app-control-service.service`, `WantedBy=starter.target`, `ConditionPathExists=!/proc/tztv-bootmode`, `Restart=always`, `EnvironmentFile=/etc/sysconfig/tizen-mobile-ui`
- **Size / stripped:** 145 KB, stripped=no (C++ symbols)  ·  **Upstream?** Samsung `app-control-service 0.1.2`
- **Runs as:** **`User=owner` `Group=users`** (non-root)

## What it does
TV "interaction scenario" manager built on the Samsung **Lotus TCServer** D-Bus framework (`AppControlServer`, `CServiceMgr`). Handles no-signal/external-no-signal power-off timers, sleep timer, auto-power-off, shop-mode timer, low-resolution timer, fan-error power-off, self-diagnosis, absent-power. Loads config via `CConfigLoader`; reads many vconf keys; `sd_notifyf` watchdog.

## 1. Persistence surface
None. Launches syspopups / `com.samsung.tv.wallscreen` via AUL only. No `system`/`execve`/`fork+exec`.

## 2. UEP / signature
None (the two `UEP`-substring hits are C++ mangled symbol noise, not signature logic).

## 3. SMACK / privilege
Runs as `owner` (non-root). D-Bus server via Lotus; methods drive timers/power. No privileged exec sink → low escalation value. Any confused-deputy would be bounded by the `owner` user.

## 4. Debloat
Governs auto-power-off / no-signal-power-off / sleep behaviour. **Disabling changes power management** (TV may stop auto-powering-off, or lose shop-mode/self-diagnosis). Not telemetry — keep unless you specifically want to kill no-signal auto-power-off.

## Leads worth a deeper look
- None high-value. If chasing a non-root IPC bug, the `AppControlServer` Lotus method handlers (`_ZN16AppControlServer*`) are the surface, but payoff is capped at the `owner` user.
