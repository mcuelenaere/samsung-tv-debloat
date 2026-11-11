# pkgmgr-server — RE note

- **Path / unit:** `/usr/bin/pkgmgr-server` — `package-manager.service` (Type=dbus, BusName `org.tizen.pkgmgr`)
- **Size / stripped:** ARM32 PIE, **not stripped** · **Upstream?** upstream Tizen `pkgmgr-server` (src: `pkgmgr-server/.../pkgmgr-server.c`), Samsung-built, no obvious Samsung signing deltas *in this binary*
- **Runs as / SMACK label:** `User=app_fw` / `Group=app_fw`, `SmackProcessLabel=System`, inheritable caps `cap_chown,cap_dac_override,cap_dac_read_search,cap_fsetid,cap_kill,cap_setgid,cap_setuid,cap_fowner` (keep-caps) — i.e. can become **any uid/gid** and override DAC.

## What it does (2–4 lines)
D-Bus request router / job queue for package install/uninstall/enable/etc. It does NOT install anything itself: it detects the per-type backend under `/etc/package-manager/backend/<pkgtype>`, builds an argv string, and `g_spawn_async`'s the backend (`__fork_and_exec_with_args` @ `0x13960`). All real UEP/signature logic lives in those backends (wgt/rpm/tpk installers), not here.

## 1. Persistence surface
- **Exec path:** `__fork_and_exec_with_args` @ **0x13960** → `g_spawn_async(argv, child_setup=__set_environment, ...)`. argv built from template `"%s %s %s %d -k %s -u %d --sync"` / `--preload` (strings @ ~0x... `-p %s`, `--preload`).
- Backend binaries come from `/etc/package-manager/backend/` (dir not in this rootfs pull). **If any backend path there is writable, that is root-capable code exec at install time.** → **lead: audit `/etc/package-manager/backend/*` ownership/writability on-device.**
- `__set_environment` @ **0x192d8**: in child, `setuid/setgid` to the job's target uid, then `setgroups` adds hardcoded gids 10001/10002/10502, then `putenv()` of the job env array. No writable-path trust in the daemon itself.
- No boot-time exec of writable content by the daemon proper. Verdict: **indirect lead only (backend dir).**

## 2. UEP / signature enforcement
- **There is NO signature / cert / hash / UEP verification in this binary.** `nm` shows zero `sign/verify/cert/crypto/author/distributor` symbols; no `signature-validator`/`cert-svc`/`ckm` imports. Verification is entirely delegated to the forked backend.
- **Confused-deputy / flag-injection surface (the real finding):** in `__queue_install` @ **0x162f0** the client-supplied `args` array (D-Bus `install(... as args ...)`) is `g_variant_get_strv`'d and space-joined (loop @ pkg lines ~138-163) into one string, passed through `_push_queue` and ultimately onto the **backend command line** verbatim. Any D-Bus caller who can reach `org.tizen.pkgmgr` can therefore inject arbitrary backend CLI flags (e.g. `--preload`, `--force-*`, type overrides). **Whether that disables author-signature/distributor-cert checking is decided in the backend, not here** — this daemon imposes no allow-list on forwarded args.
- Matches the community `pkgcmd` WGT-signature bypass / unsigned-install reports: the bypass is reachable *through* this router but *implemented* (or not) in the backend. → **next target: the wgt/tpk backend binary + `signature-validator` lib + `/etc/package-manager/*.conf` / `signature` flags.**
- Verdict: **no check to forge here; this is the unauthenticated entry point that feeds the backend.**

## 3. SMACK / privilege
- Runs at SMACK `System` with setuid/setgid/DAC caps → a genuine **confused deputy**: it performs privileged, uid-switching installs on behalf of bus callers.
- **No cynara / security-manager / privilege-framework check anywhere** (confirmed: no such imports). Authorization is only:
  - `__handle_method_call` @ **0x1675c**: resolves caller uid/pid via `GetConnectionUnixUser`/`GetConnectionUnixProcessID` (the two `g_dbus_connection_call_sync` calls @ top) — used for an admin gate and logging only.
  - Cross-uid guard: if requested `uid != caller_uid`, require `__is_admin_user(caller_uid)` (@ **0x13ac8**, via `gum_user_get_sync` usertype==2). Installing/operating as *your own* uid has **no further gate**.
  - Global-app ops additionally gated by `uid<5000 || !global || __is_admin_user`.
- Only path-sanitizer is `__check_valid_path` @ **0x146e4** (realpath + prefix-compare), used solely by `cleardata_with_path`.

## 4. Debloat
- **Load-bearing — do NOT disable.** This is THE package manager; dbus-activated on `org.tizen.pkgmgr`. Disabling breaks all app install/uninstall/enable/disable, size queries, cleardata, splash/res management. Keep it.
- Useful instead for rooting: it is the privileged install primitive — reachable over D-Bus — not something to remove.

## Leads worth a deeper look
- `/etc/package-manager/backend/<pkgtype>` — on-device ownership/writability; the forked target = root-cap exec. (not in this pull)
- Backend binary (wgt/tpk/rpm installer) + `signature-validator`/`cert-svc` — where the actual author-sig/distributor-cert check and any `--preload`/skip flag lives. **This is where the forgeable/skippable check is.**
- `__queue_install` @ 0x162f0 / `_push_queue` — confirm exact slot the joined `args` string lands in on the backend argv (flag-injection reach).
- `__handle_method_call` @ 0x1675c — confirm no cynara: only uid/admin gating. Any app with bus access to `org.tizen.pkgmgr` can drive installs as its own uid.
