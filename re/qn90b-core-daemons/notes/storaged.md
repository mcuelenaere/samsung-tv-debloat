# storaged — RE note

- **Path / unit:** `/usr/bin/storaged` — `storaged.service`
- **Size / stripped:** 18 KB, stripped? **yes**  ·  **Upstream?** upstream Tizen `storaged-1.0.0`, stock (no visible Samsung delta)
- **Runs as / SMACK label:** `SmackProcessLabel=System::Privileged`, `Type=notify`, `After=deviced.service dbus.service`, `WantedBy=starter.target`. (storaged.service carries a `#RPAC=…` comment blob — inert.)

## What it does (2–4 lines)
Thin storage daemon: a module/plugin host. Takes D-Bus name `org.tizen.system.storage`, runs a GLib main loop, and `dlopen`s every plugin in `/usr/lib/storaged`, resolving `storaged_get_module_interface` per module. Signals readiness via `sd_notify` + `/tmp/storaged_ready`.

## 1. Persistence surface
- **No exec** — no `system`/`execve`/`popen`/`fork`. Only `dlopen`.
- `dlopen` loads modules from the **fixed `/usr/lib/storaged` directory** (`modules.c`, `storaged_open_module`), `%s/%s` over that path. On read-only rootfs → not attacker-writable. Nothing under `/opt`,`/tmp`,`/var`.
- **Verdict: none**, *conditional on `/usr/lib/storaged` staying read-only*. If that dir (or rootfs) is remounted rw, a dropped `*.so` exporting `storaged_get_module_interface` runs at `System::Privileged` on next start — the one thing to watch, but it presupposes rw rootfs.

## 2. UEP / signature enforcement
- None. No signature/verify of loaded modules (no UEP/RSA/SHA strings or imports). Plain `dlopen`+`dlsym`.

## 3. SMACK / privilege
- `System::Privileged` (not full root label, but privileged). D-Bus name `org.tizen.system.storage`; libdbus watch/timeout plumbing only. No `cynara`/smack check imports → method authorization is external (dbus policy + SMACK).
- No exec sink, so no confused-deputy-to-exec. Uses `flock` + `unlink` on a lock/ready file; `open64`.

## 4. Debloat
- **Load-bearing-ish — keep.** `deviced` depends-ordered before it; `WantedBy=starter.target`; provides `org.tizen.system.storage` (storage/mount/low-storage notifications consumed by the platform). Disabling risks storage-event / low-space handling and anything binding that bus name. Behaviour depends on which plugins ship in `/usr/lib/storaged`. No safe-disable marker.
