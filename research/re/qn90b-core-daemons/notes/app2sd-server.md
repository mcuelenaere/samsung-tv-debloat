# app2sd-server — RE note

- **Path / unit:** `/bin/app2sd-server` — `app2sd-server.service` (no `[Install]`; D-Bus-activated via `g_bus_own_name`)
- **Size / stripped:** 145 KB, stripped=yes  ·  **Upstream?** Tizen `app2usb-plugin-tv 1.0.9` (Samsung TV fork of app2sd)
- **Runs as / SMACK label:** **root** (no `User=`), `SmackProcessLabel=System`, `CapabilityBoundingSet=~CAP_MAC_OVERRIDE`; `ExecStartPre=insmod dm-mod.ko` + `dm-crypt.ko`

## What it does
Installs/moves Tizen apps onto encrypted external storage (SD/USB). Sets up a dm-crypt/LUKS loopback container per app, mounts it over the app install path, tracks state in an sqlite DB. Exposes a D-Bus API; methods carry `(pkgid, size, usb_path, sender_uid, target_uid)` (Install / Uninstall / Upgrade / Move / GetPath-style — see `pkgid(%s), usb_path(%s), sender_uid(%d)` log strings).

## 1. Persistence surface
Not a boot-autostart (D-Bus activated). Mounts caller-named `usb_path` and dm-crypt containers as root — relevant to storage-based persistence but not a boot hook itself. Writes under `%s/.mmc/`, `/dev/mapper/*`.

## 2. UEP / signature
None on its own inputs. Uses OpenSSL EVP + `libsecurestorage-client` to en/decrypt the container **passphrase** (`_app2sd_get_key_for_encryption`), and sqlite `pragma integrity_check` on its DB. No verification of the pkgid/usb_path it acts on.

## 3. SMACK / privilege — **ROOT CONFUSED DEPUTY (primary batch-1 finding)**
Root D-Bus service that builds shell commands from caller-supplied strings with **no charset validation anywhere** (no `isalnum`/`strpbrk`/`strspn`/regex in the binary). Three `system()` sinks run `/bin/sh -c` on `snprintf`'d templates:
- `0xb6a8`, `0xbac8`, `0xbf54` → the cryptsetup templates in `.rodata`:
  - `/bin/echo '%s' | /sbin/cryptsetup -q -i %d -c aes-cbc-lmk -s %d --align-payload=8 luksFormat %s`
  - `/bin/echo '%s' | /sbin/cryptsetup -q luksOpen %s %s`
  - `/bin/echo '%s' | /sbin/cryptsetup -M plain -c aes-cbc-plain -h plain open %s %s`
- The trailing `%s` mapper/device names are built from **pkgid** (`/dev/mapper/%s`, `/dev/mapper/%s_%d`, `loopback_device=%s, pkgid=%s`). The `echo '%s'` field is the decrypted passphrase.
- **Injection:** a `pkgid` (or `usb_path`, which also reaches mount paths) containing `'`, `;`, `` ` ``, `$()` breaks out of the `sh -c`/`echo '...'` → arbitrary command execution **as root**. No allow-listing.
- `execvp` helper `FUN_00024638` (argv-style, not shell) runs `/sbin/mkfs.ext4`, `/bin/cp`, `/bin/dd`, `/bin/mv` on pkgid/usb_path-derived paths — lower risk (no shell) but same tainted paths.

**Reachability caveat (verify on-device):** gated only by who may call its D-Bus bus name (dbus policy + SMACK `System`), and by `sender_uid`/`target_uid` handling — mirrors the UID-only confused-deputy pattern Agent 2 found in `pkgmgr-server`. Confirm the bus-name send policy and whether a normal app label can invoke an install/move with an attacker-chosen pkgid.

## 4. Debloat
Likely safe to mask **if** app2sd (apps-on-external-storage) is unused on this set — but confirm nothing invokes it during USB app install first. Not telemetry; low debloat priority. Keeping it enabled keeps the root attack surface above.

## Leads worth a deeper look
- Trace `pkgid`/`usb_path` from the D-Bus method-call handler → `FUN_00023de8` (path builder) → the `system()` snprintf at `0xb64c`–`0xb6a8` to prove taint with no sanitizer in between.
- On-device: dump `introspect` XML + the dbus send policy for its bus name; check which SMACK labels may call it.
