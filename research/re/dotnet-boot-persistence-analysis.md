# .NET-IL boot persistence on the S95B (PontusM) — viability analysis

*Target: `root@192.0.2.10` · QE55S95BATXXH / S95B (PontusM, `sdp-soc`) ·
Tizen 6.5.0 · Linux 5.4.77 `armv7l` · build `T-PTMDEUC-1720.7`
(`T-PTMDEUC-REL-202602250226`), booted `Sun Oct 25 21:32:14 CET 2026`
(`/proc/uptime` = 861266 s). All work performed **read-only** over SSH; no file
on the TV was created, modified, or executed.*

Complements [`docs/samsung-tv-secure-boot.md`](../../docs/samsung-tv-secure-boot.md) §6.3
and [`docs/samsung-tv-root-ecosystem.md`](../../docs/samsung-tv-root-ecosystem.md) §5
(the levkropp "untethered" route).

---

## 0. TL;DR — ranked conclusion

**The .NET-IL replacement route is viable on this build, with two important
corrections to the generic recipe:**

1. **The artifact that actually executes is the `.ni.dll` (ReadyToRun native
   image), not the `.dll`.** On this unit the CLR loads
   `<app>/bin/.native_image/<App>.ni.dll` as the assembly (mapped `r-xp`); the
   plain `<App>.dll` is **not** mapped at all for the two writable-IL apps
   (`org.tizen.art-app`, `com.samsung.tv.store`). A `.dll`-only swap will not
   change behaviour. You must replace the `.ni.dll` (a real R2R image) **or**
   delete it so the runtime falls back to JIT-compiling the `.dll`.
2. **The boot-time .NET app with writable managed IL on this box is
   `org.tizen.art-app`** (manifest `type="dotnet-inhouse"`), which is launched at
   every boot by `/usr/bin/art_start.sh` (called from the fastboot app
   `/usr/apps/fastboot/bin/fb.sh`), and whose `bin/` + `bin/.native_image/`
   live on writable `/opt` (owner `tizenglobalapp`, uid 201).

UEP is **confirmed not to cover managed IL**: the apphost ELFs carry an appended
`##UEP…:UEP` trailer (so *they* are gated), but the managed `.dll`/`.ni.dll`
files are PE/COFF (`4d 5a`), carry **zero** `:UEP` markers, and are `mmap`-loaded
rather than `execve`d. SMACK is satisfied because each app reads its own
package directory.

The one caveat that keeps this from being a turnkey **root** persistence is that
the .NET app runs as **uid 5001 (`owner`)**, not root — the replaced IL gives you
reliable, UEP-exempt code execution *at boot*, but a further (already-known)
privilege-escalation step is required to hold root.

| # | Vector | Boot trigger | File to replace | Runs as | Confidence |
|---|--------|--------------|-----------------|---------|-----------|
| **1** | `org.tizen.art-app` IL/NI | `art_start.sh` ← `fb.sh` (fastboot), every boot | `/opt/usr/apps/org.tizen.art-app/bin/{.native_image/ArtApp.ni.dll, ArtApp.dll}` | uid 5001, `User::Pkg::org.tizen.art-app` | **High** code-exec at boot; needs LPE for root |
| **2** | stock `dotnet-init.sh` Tizen.Bixby relink | `dotnet-init.service` (root, oneshot) | `/opt/usr/dotnet/Tizen.Bixby/1.0.0-beta.1.4/Tizen.Bixby.{ni.dll,dll}` (+ make app copies symlinks, drop `_replaced` markers) | inside store/gallery/art-app/iot-notif | **Medium** (relink logic is stock; app must load Bixby) |
| **3** | `com.samsung.tv.store` IL/NI | none by default (on-demand) — needs a boot trigger (`startup_appid` vconf) | `/opt/usr/apps/com.samsung.tv.store/bin/.native_image/com.samsung.tv.store.ni.dll` | uid 5001 | **Medium** |
| **4** | `dotnet-apps.txt` → `/proc/vd_signal_policy_list` | `dotnet-init.sh` (root, oneshot) | `/opt/usr/home/owner/dotnet-apps.txt` | content echoed by root; **no code exec** | **Low** (channel, not exec) |
| **5** | `app-boot-manager` "custom launcher" (AUL-mediated) | `app-boot-manager.service` (root) via writable vconf/feature | (influences *which signed app* launches) | AUL/amd | **Low/Medium** (needs writable vconf) |

---

## 1. Method & constraints

Every command below was read-only (`cat`, `ls -laZ`, `grep -a`, `od`, `readlink`,
`getcap`, `md5sum`, `/proc/*/maps`, `systemctl`-style unit reads). **No writes,
no service changes, no execution of uploaded code.** `scp` is unavailable on the
device (`/usr/libexec/sftp-server` missing), so a few binaries were streamed to
the host with `ssh … 'cat /usr/bin/X' > X` for offline `strings`/`md5sum` — a
read of the TV, a write only to the host.

---

## 2. The .NET launch machinery

### 2.1 `/usr/bin/dotnet-apps.txt` — a **signal-policy list**, not a preload list

```
$ ls -la /usr/bin/dotnet-apps.txt
-rw-r--r-- 1 root root 407 2026-02-09 19:33 /usr/bin/dotnet-apps.txt

$ od -c /usr/bin/dotnet-apps.txt | head -3
0000000   c   o   r   e   r   u   n  \n   D   N   _   -   i   n   f   o
0000020   -   v   i   e   w   e   r  \n   D   N   _   a   p   p   -   u
0000040   p   g   r   a   d   e   r  \n   D   N   _   s   u   n   g   .
```

26 lines; every entry is a **15-character process `comm`** (`corerun`,
`DN_-info-viewer`, … `DN_ather-viewer`). This matches `ps` process names such as
`Name: DN_izen.art-app` and `Name: DN_ung.tv.store` (15-char truncation). The
only consumer is `dotnet-init.sh`, which **writes each line into
`/proc/vd_signal_policy_list`** — a Samsung resource-management signal policy,
**not a preload/launch list**.

`dotnet-apps.txt` also has a per-boot, writable twin
(see §4.1): `/opt/usr/home/owner/dotnet-apps.txt`.

### 2.2 `/usr/bin/dotnet-init.sh` — the root boot oneshot (the interesting one)

Unit (read-only):

```
$ cat /usr/lib/systemd/system/dotnet-init.service
#RPAC=p_gAdkAxPEhxY_...                        (inert anti-tamper tag line)
[Unit]
Description=Start dotnet initailize service
After=network.target e17.service
[Service]
Type=oneshot
RemainAfterExit=yes
User=root
ExecStart=/usr/bin/dotnet-init.sh
[Install]
WantedBy=vdtizen-bootseq-delayed.target

$ ls -la /usr/lib/systemd/system/vdtizen-bootseq-delayed.target.wants/
dotnet-init.service -> ../dotnet-init.service
```

The script (selected excerpts) does three notable things **as root, at boot**:

1. **Tizen.Bixby relink** — for `com.samsung.tv.gallery`,
   `com.samsung.tv.store`, `org.tizen.art-app`,
   `com.samsung.tv.iot-service-notification-v2`, if the app's
   `bin/Tizen.Bixby.dll` is a symlink pointing at msview's shared copy, it is
   re-pointed to a **writable `/opt` location**:

   ```sh
   if [ -L /opt/usr/apps/com.samsung.tv.store/bin/Tizen.Bixby.dll ] \
     && [ "$(readlink -f ...)" = "/usr/apps/com.samsung.tv.coba.msview/shared/res/Tizen.Bixby.dll" ]; then
       ln -fns /opt/usr/dotnet/Tizen.Bixby/1.0.0-beta.1.4/Tizen.Bixby.dll /opt/usr/apps/com.samsung.tv.store/bin/Tizen.Bixby.dll
   fi
   # … same for .native_image/Tizen.Bixby.ni.dll
   touch /opt/usr/apps/com.samsung.tv.store/bin/Tizen.Bixby.dll_replaced
   ```

   (On this unit the copies are *regular files*, and the `_replaced` markers
   already exist, so today this is a **no-op** — but it is a stock, root,
   boot-time mechanism that overwrites managed assemblies from writable `/opt`.)

2. `cp /etc/dotnet_flag.ini /opt/etc/dotnet_flag.ini` (seed for re-AOT).

3. **The writable `dotnet-apps.txt` hook**:

   ```sh
   if [ -e "/opt/usr/home/owner/dotnet-apps.txt" ]
   then
       while read FILE; do echo "${FILE}" > /proc/vd_signal_policy_list; done < /opt/usr/home/owner/dotnet-apps.txt
   else
       cp -rf /usr/bin/dotnet-apps.txt /opt/usr/home/owner/dotnet-apps.txt
       chown owner:users /opt/usr/home/owner/dotnet-apps.txt
       while read FILE; do echo "${FILE}" > /proc/vd_signal_policy_list; done < /opt/usr/home/owner/dotnet-apps.txt
   fi
   ```

### 2.3 `/usr/bin/dotnet_init_and_launch_preload_apps.sh`

Called from the fastboot app (`fb.sh`, see §3.1). It forks
`/usr/bin/app_launcher -f com.samsung.tv.pillarbox …` (if
`appfw.aul_extension.pillarbox_resolution > 0`) and runs
`/usr/bin/dotnet-init.sh &`.

### 2.4 The loaders — and what "inhouse" means

```
$ md5sum /usr/bin/dotnet-launcher /usr/bin/dotnet-launcher-inhouse /usr/share/dotnet_dummy/dotnet-launcher
27ebf19874555f4c63f8112f6f90cd24  /usr/bin/dotnet-launcher
27ebf19874555f4c63f8112f6f90cd24  /usr/bin/dotnet-launcher-inhouse
27ebf19874555f4c63f8112f6f90cd24  /usr/share/dotnet_dummy/dotnet-launcher
```

`dotnet-launcher`, `dotnet-launcher-inhouse`, and the `/usr/share/dotnet_dummy/`
copy are **the same binary** (identical md5; same BuildID
`8882f87c716fb13514908c168bdb792d9836a099`). Strings:

```
--standalone
/home/abuild/rpmbuild/BUILD/dotnet-launcher-6.0.0/NativeLauncher/launcher/exec/launcher.cc
_ZN5tizen7runtime10dotnetcore11CoreRuntime10initializeEPKc10LaunchMode
_ZN5tizen7runtime10dotnetcore11CoreRuntime6launchEPKcS4_S4_iPPcb
DOTNET_LAUNCHER   DOTNET_SYSTEM_GLOBALIZATION_INVARIANT
```

So `dotnet-launcher-inhouse` is **not a distinct inhouse code path** — it is the
same native apphost (`dotnet-launcher 6.0.0`) under a second name. "Inhouse" is
a property of the **app type**, not the loader:

* `/opt/usr/apps/org.tizen.art-app/tizen-manifest.xml` →
  `<ui-application appid="org.tizen.art-app" exec="ArtApp.dll" … type="dotnet-inhouse">`
* `/opt/usr/apps/com.samsung.tv.store/tizen-manifest.xml` →
  `… exec="com.samsung.tv.store.dll" … type="dotnet-inhouse"`
* Running cmdlines carry `_CMDLINE --appType inhouse --standalone <App>.dll`
  (see §3.2).

The one unit that invokes the inhouse loader explicitly is
`com.samsung.tv.hybrid-aot-manager.service` (root):

```
ExecStart=/usr/bin/dotnet-launcher-inhouse --standalone /usr/apps/com.samsung.tv.hybrid-aot-manager/bin/HybridAOTManager.dll
```

Other loaders:

| Binary | Role (observed) |
|---|---|
| `/usr/bin/dotnet` | Samsung apphost, host-launched root agent; caps `cap_setgid,cap_sys_admin=ei` |
| `dotnet-launcher` / `-inhouse` | standalone apphost; caps `cap_setgid,cap_setuid,cap_setpcap,cap_sys_chroot,cap_sys_ptrace,cap_sys_admin,cap_sys_nice,cap_mac_override,cap_mac_admin+eip` |
| `dotnet-loader` | per-app loader; caps incl. `cap_mac_admin,cap_mac_override` |
| `dotnet-hydra-loader` | shared-CLR ("hydra") warm loader, hosts multiple apps |
| `dotnet-run-ni.sh` | AOT/NI regeneration (`dotnettool --r2r`), re-sets caps |
| `dotnettool` | `--ni-system`, `--ni-dir`, `--r2r` (native-image generator) |

> **UEP note for all loaders:** each of `dotnet`, `dotnet-launcher`,
> `dotnet-loader`, `dotnet-hydra-loader`, `launchpad-starter` contains an
> appended `##UEP63…==:UEP` signature. They are **UEP-gated ELFs** — replacing a
> loader trips UEP. The IL-replacement route deliberately does *not* touch them.

---

## 3. Which .NET apps launch at boot, from where, as which uid

### 3.1 The fastboot script is the early launcher

`/usr/apps/fastboot/bin/fb.sh` (root, run early) starts the app machinery:

```sh
[ "$SKIP_PREOCESS_POOL" = "" ] && /usr/bin/launchpad-process-pool.sh init &
…
/usr/bin/chrt --fifo 22 /usr/bin/launch_root_app.sh
/usr/bin/dotnet_init_and_launch_preload_apps.sh&
…
if [ -e /usr/bin/art_start.sh ];then
	/usr/bin/art_start.sh&
fi
```

* `launchpad-process-pool.sh` → `setsid /usr/bin/launchpad-starter` →
  `launchpad-process-pool` (pid 347, `owner`) pre-forks the per-app CLR.
* `launch_root_app.sh` → `get_root_app` → launches the **startup app** from
  vconf.
* `art_start.sh` → launches `org.tizen.art-app` (see §3.3).
* `dotnet_init_and_launch_preload_apps.sh` → pillarbox + `dotnet-init.sh`.

The startup app is data-driven (writable vconf):

```
$ vconftool get db/appfw/home_appid
db/appfw/home_appid, value = com.samsung.tv.csfs (String)
$ vconftool get db/appfw/startup_appid
db/appfw/startup_appid, value = com.samsung.tv.csfs (String)
$ vconftool get db/appfw/is_root_cold_boot
db/appfw/is_root_cold_boot, value = 1 (Int32)
```

`get_root_app` strings: `db/appfw/is_root_cold_boot`, `db/appfw/home_appid`,
`NULL`. vconf's writable layer is `/opt/var/lib/buxton2/system.db`.

### 3.2 Running .NET processes (boot snapshot)

```
$ ps -eo pid,ppid,user,label,args | grep -iE "dotnet|launchpad|\.dll|app_launcher|amd"
  219   211 root  _                 /bin/sh /usr/bin/launchpad-process-pool.sh init
  347   219 owner System::Privileged /usr/bin/launchpad-process-pool
  367   347 owner User::Pkg::com.samsung.tv.spotlight-ss /usr/apps/…/ScreenSaverContent.dll … --appType inhouse --standalone …
  418  1206 owner System::Privileged /usr/bin/dotnet-loader 1
  535   347 owner User::Pkg::com.samsung.tv.csfs         /usr/apps/com.samsung.tv.csfs/bin/csfs.dll … --appType inhouse --standalone …
 1097   347 owner User::Pkg::com.samsung.tv.home.sidebar /usr/apps/…/HomeSidebar.dll … --appType inhouse …
 1135   347 owner User::Pkg::com.samsung.tv.home.media   /usr/apps/…/HomeMedia.dll   … --appType inhouse …
 1206   347 owner System::Privileged /usr/bin/dotnet-hydra-loader 2 0 1 <base64>
 1668     1 owner System::Privileged /usr/bin/app-control-service
 1976   347 owner User::Pkg::org.tizen.art-app           /opt/usr/apps/org.tizen.art-app/bin/ArtApp.dll _CMDLINE --appType inhouse --standalone …
 889      1 app_fw System                /usr/bin/amd
 10169 1206 owner User::Pkg::com.samsung.tv.store       /opt/usr/apps/com.samsung.tv.store/bin/com.samsung.tv.store.dll
22946 1206 owner User::Pkg::com.samsung.tv.glscreensaver-launcher /usr/apps/…/ScreenSaver.dll
32297  347 owner User                     /usr/bin/launchpad-loader 0
```

Two CLR hosting models: **per-app** (children of the pool, pid 347) and **shared
hydra** (children of pid 1206). All app processes run as **`owner` (uid 5001)**.

`systemd` boot units that launch apps (all via `app_launcher`; a non-exhaustive
read of `/usr/lib/systemd/system`):

```
com.samsung.tv.notification-app.service      app_launcher -f com.samsung.tv.notification-app firstboot launch
com.samsung.tv.notification-serviceapp.service app_launcher -f … firstboot launch
org.tizen.easysetup-daemon.service           app_launcher -s org.tizen.easysetup-daemon
com.samsung.tv.ghdaemon.service              app_launcher -s com.samsung.tv.ghdaemon
com.samsung.tv.passthru-service.service      app_launcher -s com.samsung.tv.passthru-service
com.samsung.tv.iot-hub-control.service       app_launcher -s com.samsung.tv.iot-hub-control
org.tizen.ssoservice.service                 app_launcher -s org.tizen.ssoservice
… (~40 more) …
com.samsung.tv.hybrid-aot-manager.service    dotnet-launcher-inhouse --standalone /usr/apps/…/HybridAOTManager.dll   (root)
```

But note: **almost all of these live under `/usr/apps` (read-only VDFS), and the
ones under `/opt/usr/apps` are mostly native ELF service apps** — the only two
`.NET` apps with a `.dll` under `/opt` are `org.tizen.art-app` and
`com.samsung.tv.store`.

### 3.3 The decisive boot-time .NET app: `org.tizen.art-app`

* Boot trigger (root script, from fastboot): `/usr/bin/art_start.sh` launches
  `app_launcher -f org.tizen.art-app -u 5001 mode artboot …` (cold) or
  `mode init duration serversync …` (background, after 15/85 s).
* Process: pid 1976, `Uid: 5001`, label `User::Pkg::org.tizen.art-app`,
  `Name: DN_izen.art-app`, cmdline `… --appType inhouse --standalone
  /opt/usr/apps/org.tizen.art-app/bin/ArtApp.dll …`.
* Location: **writable `/opt`**:

```
$ ls -la /opt/usr/apps/org.tizen.art-app/bin/
-rwxr-xr-x 1 tizenglobalapp root 945152 ArtApp.dll
-rwxr-xr-x 1 tizenglobalapp root 251904 ArtAppTextResources.dll
-rwxr-xr-x 1 tizenglobalapp root 1508352 ArtDataManagement.dll
drwxr-xr-x 1 tizenglobalapp root   100 bin
$ ls -la /opt/usr/apps/org.tizen.art-app/bin/.native_image/
-rw-r--r-- 1 tizenglobalapp root 2818048 ArtApp.ni.dll
-rw-r--r-- 1 tizenglobalapp root 364544 ArtAppTextResources.ni.dll
-rw-r--r-- 1 tizenglobalapp root 3919872 ArtDataManagement.ni.dll
```

* **What actually executes** (from `/proc/1976/maps`) — the **`.ni.dll`**, not
  the `.dll`:

```
$ grep -aE "org.tizen.art-app" /proc/1976/maps
ace47000-acf… r-xp … /opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtApp.ni.dll
1018e000-1053a000 r-xp … /opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtDataManagement.ni.dll
1005… r-xp … /opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtAppTextResources.ni.dll
$ grep -ac "ArtApp.dll" /proc/1976/maps      # plain IL
0
```

The same holds for store (`/proc/10169/maps` maps only
`.native_image/com.samsung.tv.store.ni.dll`). So the **ReadyToRun `.ni.dll` is
the loaded assembly**; the plain `.dll` is not mapped. (Contrast: the read-only
home app `HomeMedia` maps `/usr/apps/…/HomeMedia.dll` — different packaging.)

### 3.4 `com.samsung.tv.store`

Same shape (`dotnet-inhouse`, `/opt`, owner `tizenglobalapp`, uid 5001, maps only
`.native_image/com.samsung.tv.store.ni.dll`), **but it has no boot unit** — no
systemd unit references it (the only match under `/usr/lib/systemd` is
`sdbd_tcp.socket.d/store-develop-mode-release.conf`). It is launched on demand
via the hydra loader. Useful only if paired with a boot trigger (§5.3/§5.5).

---

## 4. Writable managed-IL surfaces on `/opt`

### 4.1 `/opt/usr/home/owner/dotnet-apps.txt`

```
$ ls -la /opt/usr/home/owner/dotnet-apps.txt
-rw-r--r-- 1 owner users 407 1970-01-01 01:00 /opt/usr/home/owner/dotnet-apps.txt
$ ls -laZ /opt/usr/home/owner/dotnet-apps.txt         # SMACK label is '*'
-rw-r--r-- 1 owner users * 407 1970-01-01 01:00 /opt/usr/home/owner/dotnet-apps.txt
```

* Writable by **uid 5001 (`owner`)** — which is the uid every .NET app runs as.
  So *any* running .NET app can rewrite it.
* Read + echoed into `/proc/vd_signal_policy_list` by **root** `dotnet-init.sh`
  each boot.
* Impact: it controls only the contents of the signal-policy list (a set of
  15-char process names). **It is a root-read channel from a writable file, not a
  code-execution hook** — unless the kernel-side handler of
  `/proc/vd_signal_policy_list` is found to act on attacker-supplied names beyond
  policy bookkeeping (unverified; needs kernel RE).

### 4.2 `Tizen.Bixby` drop site (stock relink target)

```
$ ls -laR /opt/usr/dotnet/Tizen.Bixby/1.0.0-beta.1.4/ ; ls -laZ …
-rw-r--r-- 1 root root System::Shared 16896 Tizen.Bixby.dll
-rw-r--r-- 1 root root System::Shared 45056 Tizen.Bixby.ni.dll
```

Root-owned (so needs root to overwrite), on writable `/opt`, and **already loaded
by the running store app** (`/proc/10169/maps` maps
`…/bin/.native_image/Tizen.Bixby.ni.dll`). `dotnet-init.sh` re-points four apps'
`Tizen.Bixby.{dll,ni.dll}` here at boot when they are msview symlinks (currently
not the case).

### 4.3 Other writable managed/dotnet paths (context)

```
$ ls -la /opt/usr/home/owner/share/dotnet/        # DOTNET_CACHE_PATH, mode 777
drwxrwxrwx 1 root root 2 .
-rw-r--r-- 1 owner users 1877 2026-09-25 01:25 CoBA.txt
-rw-r--r-- 1 owner users 40810 2026-09-25 01:25 TPA.txt
$ ls -la /opt/usr/dotnet.system.base.addr          # written by dotnet-run-ni.sh
-rw-r--r-- 1 root root 8471 2026-07-23 23:23 /opt/usr/dotnet.system.base.addr
```

---

## 5. UEP / SMACK coverage of managed IL

### 5.1 UEP does not see managed assemblies

* The **apphost ELFs are UEP-signed**; the managed payloads are **not ELF** and
  carry **no UEP trailer**:

```
$ for f in /usr/bin/dotnet-launcher /usr/bin/dotnet-loader /usr/bin/dotnet-hydra-loader /usr/bin/launchpad-starter; do strings -a $f | grep -o '##UEP63[0-9a-f]*:UEP' | head -1; done
##UEP630x66a39aab…:UEP        (dotnet-launcher)
##UEP630x8897491e…:UEP        (dotnet-loader)
##UEP630x68e3df06…:UEP        (dotnet-hydra-loader)
##UEP630xb8fe27c7…:UEP        (launchpad-starter)

$ for f in /opt/usr/apps/org.tizen.art-app/bin/ArtApp.dll \
           /opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtApp.ni.dll \
           /opt/usr/apps/com.samsung.tv.store/bin/com.samsung.tv.store.dll; do
    printf "%-70s :UEP=%s  magic=" "$f" "$(grep -c -a ':UEP' "$f")"; od -An -tx1 -N4 "$f"; done
/opt/usr/apps/org.tizen.art-app/bin/ArtApp.dll                        :UEP=0  magic= 4d 5a 90 00
/opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtApp.ni.dll       :UEP=0  magic= 4d 5a 00 00
/opt/usr/apps/com.samsung.tv.store/bin/com.samsung.tv.store.dll       :UEP=0  magic= 4d 5a 00 00
```

`4d 5a` = `MZ` (PE/COFF). UEP's kernel hook is an **exec-time** file-type check
(`[SfdUepCheckFileType] unknown file type(#RPA)`); it gates ELF/scripts, and the
community-blessed bypass is precisely "code mapped and executed from memory, not
via `execve`". A `.ni.dll` is `mmap`'d `r-xp` by the CLR (`/proc/1976/maps`),
**not** `execve`d. So **replacing/removing managed IL does not trip UEP.**

* The loaders do **not** link the userspace verifier:

```
$ for b in dotnet dotnet-launcher dotnet-launcher-inhouse dotnet-loader dotnet-hydra-loader launchpad-process-pool app_launcher amd; do echo "$b: kUEP=$(grep -c -a kUEP /usr/bin/$b)"; done
dotnet…: kUEP=0   (all zero)
```

(The ELF UEP check lives in the kernel; `libkUEPUser.so` is used for *scripts* by
bash/sh/node, not by these ELFs.)

### 5.2 SMACK is satisfied for the app's own directory

The app process carries `User::Pkg::<pkgid>`, and its `bin/` tree is labelled with
the same prefix:

```
$ cat /proc/1976/attr/current
User::Pkg::org.tizen.art-app
$ ls -laZ /opt/usr/apps/com.samsung.tv.store/bin/
drwxr-xr-x tizenglobalapp root User::Pkg::com.samsung.tv.store::RO .
-rwxr-xr-x tizenglobalapp root User::Pkg::com.samsung.tv.store::RO com.samsung.tv.store.dll
```

So an app reading its own package directory is allowed. (Reading a *different*
package dir would need care; the Tizen.Bixby drop site is `System::Shared`,
which is world-readable.)

### 5.3 Answer to the task's question

> *Does replacing a `.dll`/`.ni.dll` trip UEP?*

**No.** Both are PE files with no UEP signature, loaded by `mmap`, outside the
`execve` gate.

> *What about the native apphost / `.ni.dll`?*

* **Native apphost** (`dotnet-launcher`/`-inhouse`/`loader`/`hydra-loader`):
  UEP-**signed** ELF → **do not touch it**; a modified apphost would be blocked
  by UEP.
* **`.ni.dll`**: not UEP-gated (PE, `mmap`), and it is the artifact that actually
  executes.

---

## 6. Ranked persistence assessment (this build)

### #1 — `org.tizen.art-app` IL/NI replacement (boot-launched, writable, UEP-exempt)

* **App:** `org.tizen.art-app`, `type="dotnet-inhouse"`.
* **Files:** `/opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtApp.ni.dll`
  (the executable artifact) and/or `ArtApp.dll` (IL; delete the NI to force JIT).
  Owner `tizenglobalapp` (uid 201), mode `755` → writable by root or
  `tizenglobalapp`.
* **Boot trigger:** `/usr/bin/art_start.sh` (root) ← `fb.sh` (fastboot), **every
  boot**.
* **Context at exec:** uid 5001, label `User::Pkg::org.tizen.art-app`.
* **UEP/SMACK:** not gated (§5). 
* **Uncertainty:** (a) the R2R image must be replaced or removed — a `.dll`-only
  edit will not change behaviour; (b) whether the app-framework re-verifies
  `author-signature.xml` at launch (the manifest/signature files also live on
  writable `/opt`) is not proven either way — the levkropp premise and the
  absence of signature strings in `app_launcher`/loaders suggest **not**, but
  this needs the controlled test; (c) code runs as uid 5001, so a further LPE
  step is needed for root. 

### #2 — stock `dotnet-init.sh` Tizen.Bixby relink

* **Mechanism:** a **stock, root, boot** script overwrites managed assemblies in
  four apps from writable `/opt/usr/dotnet/Tizen.Bixby/1.0.0-beta.1.4/`.
* **To arm it:** make the target app's `Tizen.Bixby.{dll,ni.dll}` a symlink to
  msview's and remove the `*_replaced` marker; replace the drop-site files (root
  needed). `org.tizen.art-app` is boot-launched, so the payload loads at boot.
* **Uncertainty:** requires the app to bind `Tizen.Bixby` at startup; today the
  relink is a no-op, so this is untested.

### #3 — `com.samsung.tv.store` + a boot trigger

* Same writable-IL properties as #1, but **no boot unit**. Pair with a boot
  trigger such as setting `db/appfw/startup_appid`/`home_appid` (writable vconf
  `/opt/var/lib/buxton2/system.db`) or `app-boot-manager`'s "custom launcher"
  mode (§5 below). More moving parts → lower confidence.

### #4 — `dotnet-apps.txt` → `/proc/vd_signal_policy_list`

* A **real** writable-file → root-read boot channel (uid-5001-writable, root
  reads at boot), but its effect today is limited to a process-name policy list.
* Not a code-exec primitive unless the kernel handler is shown to act on the
  names. **Low.**

### #5 — `app-boot-manager` boot-app selection

* `app-boot-manager.service` (root) launches the first-screen app via **AUL**
  from `db/appfw/startup_appid` / feature values, and has log strings
  `custom launcher app booting`, `/opt/usr/home/owner/share/ep-customapp/candidate_appinfo.xml`,
  `/opt/usr/home/owner/share/ifw-thing/enable_tv_on`. The `ep-customapp`/
  `ifw-thing` dirs **do not exist** on this unit today. If a writable vconf/
  feature value selects an app, it is AUL-mediated — an *influence* path, not a
  direct exec. **Low/Medium; needs a test.**

---

## 7. Minimal reversible validation plan (**proposed — not run**)

Pick **#1 (`org.tizen.art-app`)** as the safest, most isolated test.

**0. Pre-conditions / read-only snapshot**
```sh
sha256sum /opt/usr/apps/org.tizen.art-app/bin/ArtApp.dll \
          /opt/usr/apps/org.tizen.art-app/bin/.native_image/ArtApp.ni.dll
cp -a /opt/usr/apps/org.tizen.art-app/bin/.native_image /opt/backup-ni-artapp
# record process map baseline
grep -aE "art-app" /proc/$(pidof ArtApp.dll)/maps
```

1. **Backup** (to persistent `/opt`, not `/tmp`):
   `cp -a ArtApp.dll ArtApp.dll.bak ; cp -a .native_image/ArtApp.ni.dll ArtApp.ni.dll.bak`
   (and keep a copy of the whole `bin/` for a fast restore).
2. **Change** (one variable at a time):
   * *Test A (cheapest, isolates the NI question):* `rm .native_image/ArtApp.ni.dll`
     and leave the `.dll` untouched, then reboot. If the app still launches and
     runs, the runtime JITs the IL (confirms a `.dll` swap is meaningful and the
     NI is optional). Restore the NI afterwards.
   * *Test B:* then replace `ArtApp.dll` with a **benign instrumented IL**
     (e.g. a build that only appends a timestamped line to a file under
     `/opt/usr/home/owner/share/dotnet/`), keep the NI removed, reboot.
3. **Observe:**
   * `ps`/`/proc/<pid>/maps` — did the app launch at boot and map the new files?
   * the benign marker file under `/opt` — did our code execute?
   * `dmesg | grep -i -E "SfdUep|uep"` — any UEP denial?
   * `getcap`/labels unchanged; `systemctl status dotnet-init.service art*` etc.
4. **Rollback:** restore `ArtApp.dll` and `ArtApp.ni.dll` from the backups,
   delete the marker file, reboot, re-verify sha256 equals §0.

**Do not** attempt to modify the apphost ELFs (`dotnet-launcher*`) — they are
UEP-signed. **Do not** change `dotnet-init.service`/the rootfs (VDFS-signed).

Optional secondary test (only after #1): arm the Tizen.Bixby relink for
`org.tizen.art-app` and verify the drop-site payload loads — reversible by
restoring the symlinks/markers and `/opt/usr/dotnet/Tizen.Bixby/*`.

---

## 8. Open questions / uncertainties

1. **Does the app framework verify `author-signature.xml` at launch?** Both
   `tizen-manifest.xml` and the 57 KB `author-signature.xml` are on writable
   `/opt` for art-app/store. No signature logic was found in `app_launcher`/the
   loaders, but `amd`'s real logic is in `libamd.so.1`/`libaul-server.so.0`
   (not pulled) — this is the key unknown for #1.
2. **NI/IL precedence:** confirmed the `.ni.dll` is mapped; not yet confirmed
   whether deleting it cleanly forces JIT for these Samsung apphosts (Test A).
3. **`/proc/vd_signal_policy_list` semantics** — what the kernel/consumer does
   with the names; would decide whether #4 is more than bookkeeping.
4. **vconf boot-app key** (`db/appfw/startup_appid`) is confirmed writable in
   principle (`/opt/var/lib/buxton2/system.db`), but whether buxton's write path
   is reachable from an app label (vs. Cynara/SMACK) needs a live test.
5. **Art-app launch reliability:** `art_start.sh` exits early if
   `fconfigd -g com.samsung/featureconf/homeui.artgallery_support` is `true`
   (observed `false` here), so art-app is launched at boot on this unit.

---

## 9. Evidence index (commands used)

* **Units / boot:** `cat /usr/lib/systemd/system/{dotnet-init,com.samsung.tv.hybrid-aot-manager,launchpad-process-pool,app-boot-manager}.service`;
  `ls -la /usr/lib/systemd/system/vdtizen-bootseq-delayed.target.wants/`;
  `grep -rl dotnet /usr/lib/systemd/system/`.
* **Scripts:** `cat /usr/bin/dotnet-init.sh`,
  `cat /usr/bin/dotnet_init_and_launch_preload_apps.sh`,
  `cat /usr/apps/fastboot/bin/fb.sh`, `cat /usr/bin/launch_root_app.sh`,
  `cat /usr/bin/art_start.sh`, `cat /usr/bin/launchpad-process-pool.sh`.
* **Apps/processes:** `ps -eo pid,ppid,user,label,args`,
  `ps -eZ`, `/proc/<pid>/{cmdline,status,attr/current,maps}`,
  `ls -laZ /opt/usr/apps/...`, `grep -ai type= /opt/usr/apps/*/tizen-manifest.xml`.
* **Loaders:** `md5sum`, `getcap`, `file`, `strings` (host-side),
  `grep -c -a kUEP`, `grep -aoE '/usr/bin/[a-z-]+' /usr/bin/launchpad-starter`.
* **State:** `vconftool get db/appfw/{home_appid,startup_appid,is_root_cold_boot}`,
  `od -c /usr/bin/dotnet-apps.txt`, `cat /proc/vd_signal_policy_list`.
* **Host-side helpers:** binaries streamed with
  `ssh root@192.0.2.10 'cat /usr/bin/X' > X`.

### Cross-references
* [`docs/samsung-tv-secure-boot.md`](../../docs/samsung-tv-secure-boot.md) — boot chain, §6.3.
* [`docs/samsung-tv-uep.md`](../../docs/samsung-tv-uep.md) — the exec gate.
* [`docs/samsung-qn90b-architecture.md`](../../docs/samsung-qn90b-architecture.md) §4–5.
* [`docs/samsung-tv-root-ecosystem.md`](../../docs/samsung-tv-root-ecosystem.md) §5.
* [`re/qn90b-core-daemons/`](qn90b-core-daemons/) — `app-boot-manager`, `parseinfo.sh`.

*All data gathered read-only over SSH on 2026-11-04; no changes were made to the
TV.*
