# fconfigd — RE note

- **Path / unit:** `/usr/bin/fconfigd` — `fconfigd.service` (`ExecStart=/usr/bin/fconfigd -d`, `Type=forking`, PIDFile `/tmp/.fconfigd.pid`, `Restart=always`, no `ConditionPathExists`)
- **Size / stripped:** 47 KB, stripped=yes (ARM32 Thumb PIE). Build: `feature-config-daemon-1.0.0` → **Samsung-proprietary** (Feature/FMS config daemon).
- **Runs as / SMACK label:** `User=root Group=root`, `SmackProcessLabel=System`.

## What it does (2–4 lines)
Samsung "Feature Config / FMS" daemon. Owns a sqlite feature-flag DB and serves get/set/reset over a UNIX socket (`/run/fconfig-0`) to `libfconfig.so` clients (`fconfig_get_bool/int/str`, `fconfig_set_*`). At boot it decides whether the working DB is current; if not it rebuilds the DB from a factory "delta" xz blob. Values are consumed system-wide as feature flags.

## 1. Persistence surface
- **Trusts a writable, unsigned sqlite DB.** Working DB `/opt/data/fconfig/fconfig.db` (plus `fconfig_db_backup`, `hash_file`, `feature-config-version`, `need_fixed_update`) live on the **writable** `/opt/data` partition. On a normal boot `fcdb_init @0x16908` only (a) string-compares version files and (b) `fcdb_operation_guarantee @0x16664` runs sqlite `PRAGMA integrity_check` — then opens and trusts the DB. **An attacker who writes `fconfig.db` (valid sqlite + matching `feature-config-version` string + matching `hash_file`) has their flag values served to every client as root.** This is a feature-flag tampering primitive, not direct code-exec.
- **`system("/sbin/insmod …")` as root during DB rebuild.** `FUN_00015d78` (rebuild path) runs `system("/sbin/insmod /lib/modules/linux/kernel/soc/PreInit.ko")` (guarded by `access()` on the .ko) and `system("/sbin/insmod /lib/modules/linux/kernel/tztv-eeprom.ko")` for model/EEPROM detection. Both use **absolute** `/sbin/insmod` and module paths on the **RO rootfs** → no PATH/arg injection. Only a lead if `/lib/modules/...` is made writable. Rebuild only runs when `/opt/etc/need_fcdb_setup` exists, version differs, or hash differs — not every boot.
- Delta source `/etc/fconfig/delta/<model>…` (xz, RO) → `do_unxz @0x14480` runs `unxz -c %s > %s`.
- No exec of writable content on the hot boot path. Verdict: **lead (writable unsigned DB = flag-tamper); insmod is root-exec but RO-sourced.**

## 2. UEP / signature enforcement
- **No cryptographic verification anywhere.** Only "verify"-like logic is:
  - `PRAGMA integrity_check` (sqlite structural check) in `FUN_00013194` / `fcdb_operation_guarantee @0x16664` — on failure it `copy_file`s the backup / `fcdb_reset`s. Structural, not authenticity.
  - `feature-config-version` plaintext `major.minor.patch` compare (`get_field_in_line`, `fcdb_init @0x16908`).
  - `hash_file` is a **change-detector** ("DB hash file different, do FMS DB setup"), not an HMAC gate — and it sits in the same writable dir as the DB, so an attacker regenerates it too.
- No UEP / RSA / cert / sha / hmac imports or strings. Verdict: **config is NOT verified before it is trusted.**

## 3. SMACK / privilege
- Runs root / label `System`. Server socket `sock_get_server @0x170d8`: sets `security.SMACK64IPIN`/`IPOUT = "system"`, then **`chmod(path, 0666)`** → world-connectable at the FS layer; only kernel SMACK IP rules gate who may talk to label `system`.
- **Confused deputy: SET/RESET are unauthenticated.** Dispatch `FUN_00017c64 @0x17c64`: msg types 1–4 = GET, **5/6/7 = SET → `set_value_cache_and_db @0x17770`**, 8 = `reset_db`. `sock_get_client_cred @0x16db0` (SO_PEERCRED) and `sock_get_client_label @0x16c50` (SO_PEERSEC) are fetched **only for `__dlog_print` logging** — there is **no label/uid authorization check** before set/reset. Any process whose SMACK label is permitted to write label `system` (or if SMACK is permissive) can flip or reset every feature flag as root.
- `-s --set` CLI path is gated by `is_release_image()` (refuses on release), but the **socket SET path is not** gated.

## 4. Debloat
- No `ConditionPathExists`. Disabling is **risky**: `libfconfig.so` clients across the system call `fconfig_get_*` against `/run/fconfig-0`; with the daemon down those calls fail and consumers fall back to defaults or break (unknown blast radius incl. model/localset init). Verdict: **do NOT blindly disable.** Better debloat lever is editing flag *values* in the DB, not killing the daemon.

## Leads worth a deeper look
- `FUN_00017c64 @0x17c64` — SET(5/6/7)/RESET(8) with zero authz. Prime lever: connect to `/run/fconfig-0`, flip any feature flag. Find flag keys worth flipping (debug/engineering/SSH/adb) by dumping `/opt/data/fconfig/fconfig.db` keys at runtime.
- `/opt/data/fconfig/fconfig.db` (+ `hash_file`, `feature-config-version`) writable & unsigned — offline tamper to force values, persists across reboot.
- `FUN_00015d78 @0x15d78` — root `system("/sbin/insmod …")`; escalates to root code-exec only if `/lib/modules/linux/kernel/soc/PreInit.ko` or `tztv-eeprom.ko` can be made writable.
- `fcdb_operation_guarantee @0x16664` / `FUN_00013194` — the only integrity gate (sqlite `PRAGMA integrity_check`); trivially satisfied by a well-formed tampered DB.
