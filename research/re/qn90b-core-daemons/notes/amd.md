# amd — RE note

- **Path / unit:** `/usr/bin/amd` — `ac.service` (Application Management Daemon), `WantedBy=sysinit.target`, `Type=notify`, socket-activated (`ac.socket`)
- **Size / stripped:** 15 KB, stripped=no  ·  **Upstream?** Tizen AUL `amd` (app launcher) — Samsung build
- **Runs as / SMACK label:** `User=app_fw` `Group=app_fw`, **`SmackProcessLabel=System`**, `Capabilities=cap_setuid,cap_setgid,cap_mac_admin,cap_kill,cap_dac_override,cap_sys_admin=i` + `SecureBits=keep-caps`

## What it does
Central application launcher / lifecycle manager (AUL). The on-disk `amd` is a **thin stub** — its 15 KB has only `main`→`amd_main`; all logic is in **`libamd.so.1`** (NEEDED, **not collected**). Links the interesting libs: `libcynara-client` (authorization), **`libcert-svc-vcore.so.2`** (app signature/cert validation), `libaul-server`, `libbuxton2`, `libdbuspolicy1`, `libsecurity-manager`-adjacent.

## 1. Persistence surface
It is *the* thing that launches apps, but via AUL/launchpad of installed (signed) packages — not a writable-script exec. Boot-app decisions depend on buxton/vconf + app DB.

## 2. UEP / signature
Links `libcert-svc-vcore` → validates app author/distributor certs at launch (the enforcement that makes unsigned apps fail to run). Actual check is in `libamd.so.1` / cert-svc, not this stub.

## 3. SMACK / privilege — high architectural value
Runs `System`-labelled with **`cap_mac_admin`** (can set SMACK labels) and **`cap_sys_admin`**, `cap_setuid/setgid`, all *inheritable* with keep-caps — so amd can relabel and change uid of things it launches. A flaw in its launch/authz path (in libamd.so.1) would be a strong privilege lever. Authz is via cynara.

## 4. Debloat
**Do not disable** — load-bearing; without amd no apps launch.

## Leads worth a deeper look
- **Pull `libamd.so.1`** (and `libaul-server.so.0`) off the device — that's where launch authorization, the cynara checks, and any `aul_launch`/exec sinks live. The collected stub is not analyzable on its own.
- Map amd's socket/D-Bus launch API and whether a low-privilege caller can influence launch target, uid, or SMACK label (given cap_mac_admin).
