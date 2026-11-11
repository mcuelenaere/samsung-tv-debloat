# pkginfo-server — RE note

- **Path / unit:** `/usr/bin/pkginfo-server` — `pkgmgr-info.service` (Type=notify, `Requires=pkgmgr-info.socket`, `WantedBy=basic.target`)
- **Size / stripped:** ARM32 PIE, **stripped** · **Upstream?** upstream Tizen `pkgmgr-info` server (build path `pkgmgr-info-0.21.20/src/server/main.cc`), no Samsung signing deltas visible
- **Runs as / SMACK label:** `User=app_fw` / `Group=app_fw`, `SmackProcessLabel=System`, inheritable cap `cap_dac_override` only, RT sched (rr prio 1)

## What it does (2–4 lines)
Socket-activated read/cache service for the pkgmgrinfo DB (app/pkg metadata). Clients query it over `pkgmgr-info.socket`; it answers from the sqlite pkg DBs. It is an **info/query** daemon — it does not install, exec backends, or verify packages. `sd_notify` readiness, ready-flag `/run/.pkginfo_server_ready`.

## 1. Persistence surface
- No `exec*`/`g_spawn`/`fork` of external content (dynamic imports are just `sd_notify`, `signalfd`, libsystemd). Reads DBs + serves socket. Verdict: **none.**

## 2. UEP / signature enforcement
- **None.** No signature/cert/crypto strings or logic; it is a metadata query server. Not part of the verification path. Verdict: **n/a.**

## 3. SMACK / privilege
- SMACK `System`, `app_fw:app_fw`, only `cap_dac_override` (inheritable) so it can read DBs across DAC. Stripped, so no symbol-level audit here; attack surface = the socket protocol parser (not reversed — low priority vs backends). No exec of attacker input.

## 4. Debloat
- **Load-bearing — do NOT disable.** Socket/notify service that `pkgmgr-server`, the launcher and the app framework depend on for app/pkg lookups. `WantedBy=basic.target`. Removing it breaks app enumeration/launch. Keep it.

## Leads worth a deeper look
- Low value for the root/UEP goal. Only revisit if targeting the pkginfo **socket protocol** as an IPC attack surface; otherwise skip.
