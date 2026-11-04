# buxton2d — RE note

- **Path / unit:** `/usr/sbin/buxton2d` — `buxton2.service` (`ExecStart=/usr/sbin/buxton2d -f`, `Type=forking`, `ExecStartPre=/bin/sh -c "/usr/bin/awaiter 0.1 /tmp/vconf_ready"`, `Restart=always`, `WantedBy=basic.target`). **`Environment=BUXTON_CYNARA_PERMISSIVE_MODE=1`.**
- **Size / stripped:** 102 KB, stripped=yes (ARM32 Thumb PIE). Build: `vconf-compat-1.1` → **upstream Tizen** (buxton2 key-value store, vconf successor + vconf-compat shim). Samsung delta = packaging/config only (notably the permissive-mode env).
- **Runs as / SMACK label:** unit sets no `User=`/`SmackProcessLabel=` → runs as root; socket IPIN/IPOUT labeled `system` (`sock_set_client`/socket setup). Uses `prctl`, `chown`, `chmod`, `dlopen`.

## What it does (2–4 lines)
Upstream Tizen buxton2 config daemon providing the vconf-compatible system key-value store. Backends (`direct`, DB layers) loaded via `dlopen` from `/usr/lib/buxton2` (RO). Clients reach it over `/run/buxton2` + `/run/buxton2-0`; DBus login1 integration for user layers. This is the system-wide `vconf`/`db/...` store.

## 1. Persistence surface
- No exec of writable content at boot. Backend `.so`s `dlopen`'d from `/usr/lib/buxton2` (RO rootfs). `start_launcher` string present (upstream launcher helper), no writable exec target found.
- DB layers stored writable: `/var/lib/buxton2`, `/var/lib/buxton2_backup` (also `system`, `system_rtc`, `system_factory` layers). Writable key store, but served as data, not code. Verdict: **none (writable KV data only).**

## 2. UEP / signature enforcement
- No crypto verification. `direct_verify @0x1b850` / `bxt_verify_db` / "verify system db - normal layer" are **layer backup/recover integrity** (restores `*_backup` on open failure), not signature/HMAC. Verdict: **no signing; backup-recover only.**

## 3. SMACK / privilege (label enforcement — the headline)
- **Cynara (SMACK-backed) access control is in PERMISSIVE mode.** `buxton_cynara_init @0x1ed48` does `getenv("BUXTON_CYNARA_PERMISSIVE_MODE")`; value `"1"` (set by the unit) sets a global permissive flag. `buxton_cynara_check @0x1eebc` short-circuits when that flag is set — it **skips `cynara_async_check_cache`/`create_request` and returns allow** (logs the client label but does not deny). So per-key client-label write policy is **logged, not enforced**.
- Net: any client that can reach `/run/buxton2-0` can read/write security-relevant `db/...` keys regardless of its SMACK label, as far as *buxton's own* check goes. The only remaining gate is the kernel SMACK rule on sending to socket label `system` (and any `check_key_name @0x197c0` name validation, which is syntactic, not privilege). `direct_set_priv @0x1e4ec` sets per-key SMACK label metadata but the permissive check makes it non-blocking for access decisions.
- `direct_check/direct_get @0x1e924/0x1e9b8` are the direct-backend path and do **not** call cynara at all.

## 4. Debloat
- **Do NOT disable.** This is the system `vconf` store; `awaiter … /tmp/vconf_ready` gates boot and countless clients read `vconf`/`db/...`. Removing it breaks large parts of userspace. Verdict: keep; if hardening, the lever is flipping `BUXTON_CYNARA_PERMISSIVE_MODE=0` (will then *enforce* cynara/SMACK and may break mislabeled Samsung clients) — opposite of a foothold.

## Leads worth a deeper look
- `BUXTON_CYNARA_PERMISSIVE_MODE=1` (unit) + `buxton_cynara_check @0x1eebc` — write-access to `db/...` keys is unenforced; an our-label client can set security-relevant vconf keys. Enumerate which keys gate debug/engineering/SSH-ish behavior.
- `/var/lib/buxton2` writable — offline edit of persisted layer values survives reboot (no signing).
