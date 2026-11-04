# Samsung QN90B — System Architecture Write-up

*Target: `root@192.0.2.10` · Tizen 6.5.0 · investigated 2026-11-04 (read-only)*

---

## 0. TL;DR

It's a **Tizen 6.5** TV: a systemd-based embedded Linux with a **SMACK**-sandboxed,
**secure-boot-signed, read-only root filesystem**, a ~140-service Samsung "VD"
(Visual Display) middleware stack, and three parallel application runtimes —
**native (EFL)**, **.NET Core (Tizen .NET)**, and **web (WRT/Chromium)**. The home
screen and much of the UI are actually **.NET applications**. Root on this unit is
held by a **.NET "root agent" launched from your host** over SDB — not a persistent
install.

---

## 1. Hardware & kernel

| | |
|---|---|
| **SoC** | Samsung "pontusm" (`sdp-soc`, `dtsversion=..._pontusm-2.5G`) |
| **CPU** | 2× ARM Cortex-A73 (`armv7l`, ARMv7, NEON, AES/SHA/CRC32, LPAE) |
| **RAM** | ~1.78 GB + 0.9 GB zram/swap (swap nearly full — memory-tight) |
| **Kernel** | Linux **5.4.77** `#1 SMP PREEMPT`, 32-bit ARM |
| **Boot cmdline** | `root=/dev/mmcblk0p19 init=/sbin/init rootfstype=vdfs ro console=ttyS3,115200 ... SELP_ENABLE=... quiet` |
| **Console** | UART `ttyS3` @115200 (`rs232=1`) — serial debug header exists on the board |

`SELP_ENABLE` = Samsung's secure-boot platform flag; `console=ttyS3` confirms a
physical serial console.

### Storage layout (eMMC `mmcblk0`, all VDFS)

| Partition | Mount | Mode | Notes |
|-----------|-------|------|-------|
| `p19` | `/` | **ro** | System image, VDFS4 read-only/compressed, **100 % full (0 free)** |
| `p20` | `/mnt/systemrw` | rw | small system-writable area (`dncs`) |
| `p21` | `/opt` | rw | **userdata** — apps, home dir, our SSH state (`dncs`, 922 MB free) |
| `p17` | `/mnt/drm` | rw | DRM |
| `p23` | `/factory_peq` | ro | squashfs, factory audio EQ |

`VDFS` is Samsung's in-house flash filesystem (here **vdfs4**). It has a
read-only compressed mode for the system image and a journaling rw mode (`dncs`)
for userdata — same driver, two layouts. No dm-verity at runtime; integrity is
enforced at **boot time** via the signed image + SELP secure boot. The eMMC
partition is *not* hardware write-protected (`.../mmcblk0p19/ro = 0`); the `ro`
is purely the mount flag + kernel cmdline.

---

## 2. Init & boot sequence (systemd)

- **systemd 244**, built `+SMACK +IMA +ACL -SELINUX -PAM -SYSVINIT -SECCOMP`,
  **unified cgroup v2** hierarchy.
- **default target = `bootmode.target`** (not the usual `graphical.target`) — a
  Samsung boot-mode selector that chains into a custom staged boot:

```
bootmode.target
  └─ (Samsung VD boot sequence, staged for fast TV startup)
       vdtizen-bootseq-preload.target
       vdtizen-bootseq-SI-starter.target
       vdtizen-bootseq-predelayed.target
       vdtizen-bootseq-delayed.target
       vdtizen-bootseq-idle.target
  starter.target / app-starter.target   → launches the TV UI apps
  multi-user.target → graphical.target
```

The `preload / SI-starter / predelayed / delayed / idle` staging is how Samsung
hits its "TV picture in ~1 s" boot budget: only picture-path services start in
the preload stage; everything else is deferred to `delayed`/`idle` after the
panel is already showing content.

**Boot hooks relevant to persistence:** all of `/`, `/usr`, `/etc` are read-only,
so there is nowhere in the systemd unit search path to drop a unit. No
`rc.local`, no cron, no writable `*.target.wants`. The only writable real estate
is `/opt` (persistent) and tmpfs (`/run`, `/tmp`, wiped each boot).

---

## 3. Security model

Layered, and the reason the root filesystem can't simply be edited:

- **SMACK** (Simplified Mandatory Access Control) — the active LSM (not SELinux).
  Every process carries a label; system daemons run as `root`/`System`, apps as
  **uid 5001 (`owner`)** with per-app labels. Policy loaded from
  `/etc/smack/accesses.d` (`default-ac-domains`, `home-smack`, …). This is what
  blocks executing a binary from the userdata mount — hence our dropbear has to
  be copied into `/run` (label `System::Run`) to run.
- **IMA** — kernel integrity measurement (`+IMA` in systemd, enabled in kernel).
- **Cynara** — Tizen's per-privilege access-control daemon
  (`license-manager-agent` = "cynara agent").
- **Central Key Manager** (`central-key-manager.service` + 4 API sockets) and
  **secure storage** (`securestorage.service`) — key/secret store.
- **TrustZone** — `security-tzdaemon.service` (ARM TrustZone secure world),
  `secure-clock`, `psa-init` (ARM PSA secure boot init).
- **Secure boot** — `SELP_ENABLE` on the kernel cmdline; the system partition is
  RSA-signed and verified by the bootloader. This is why on-flash edits to `/`
  would brick/recover rather than persist.
- Device/policy: `device-policy-manager`, `zone-manager` + `zone-security-manager`
  (Knox-style zones), `sfpmd` ("Smart Security Product daemon").

**Debug / root path:** `sdbd.service` = **Samsung Debug Bridge Daemon** (SDB,
Samsung's ADB equivalent), listening via `sdbd_tcp.socket`. This is the channel
developer tooling — and the root exploit — connect through. `socite-remote`
("Remote Debugger Service") is the on-device debug agent.

---

## 4. Application model

Tizen runs **three app runtimes** side by side, all orchestrated by the same
app framework:

### Framework
- **amd** (`/usr/bin/amd`, pid 889) — **App Manager Daemon**, the central app
  lifecycle broker (launch/resume/kill/visibility).
- **AUL** (App Util Library) — the launch protocol. Every app's cmdline is full
  of base64 `__AUL_*` keys (`__AUL_HWACC__`, `__AUL_ROOT_PATH__`, loader id, …).
- **launchpad-process-pool** (pid 347, under `launchpad-process-pool.sh`) —
  pre-forks "candidate" processes with the runtime already warmed, then
  specializes one into the requested app on launch (`__AUL_FAST_LAUNCH__ true`).
  This is why apps appear instantly.
- **pkgmgr-info** — package/manifest database.

### The three runtimes
1. **Native (EFL)** — C/C++ apps on **EFL** (Enlightenment Foundation Libraries).
   `org.tizen.*` apps: `tv-viewer`, `volume-app`, `menu`, etc. The compositor is
   **`enlightenment`** (pid 272) — a Wayland compositor/WM.
2. **.NET (Tizen .NET)** — see §5. Processes show up with a **`DN_` prefix**
   (dotnet). **The home screen is .NET**: `HomeMedia.dll`, `HomeSidebar.dll`,
   `csfs.dll` (the content/smart-feed service, top RAM user at ~85 MB), plus
   store, search, ambient, weather, etc.
3. **Web (WRT)** — the **Web Runtime**: `wrt-loader`, `webappservice`,
   `litewebappservice`, rendering via **chromium-efl** (mounted as a squashfs
   image at `/usr/share/chromium-efl`). `efl_webprocess` = the renderer.

Apps live in `/usr/apps/<pkgid>/` (system) and `/opt/usr/apps/` (user-installed,
e.g. Prime Video, the Spotify lightweight node service).

---

## 5. The .NET stack (why there's "lots of .NET")

Samsung moved large parts of the Tizen TV UI to **.NET Core / Tizen .NET**, and
it's a full runtime, not a thin shim:

- **Runtime:** CoreCLR under `/usr/share/dotnet.tizen/netcoreapp/` —
  `libcoreclr.so`, `libclrjit.so`, `libmscordaccore.so`, `crossgen`, `createdump`,
  and the whole BCL (`System.*.dll`, `mscorlib.dll`, `netstandard.dll`).
- **AOT via Native Images:** every managed assembly ships with a precompiled
  **`.ni.dll`** (crossgen Native Image) next to it — startup avoids JIT by loading
  the native image. Critical on a memory-/CPU-tight TV.
- **TizenFX** (`/usr/share/dotnet.tizen/framework/`) — the C# platform bindings:
  `Tizen.Applications.*`, `Tizen.Account.*`, `ElmSharp.dll` (EFL UI from C#),
  `Samsung.Tizen.SmartThings.Client.dll`, OpenTK, i18n, etc.
- **Launchers:**
  - `dotnet-launcher` / `dotnet-loader` — standard app entry.
  - `dotnet-hydra-loader` — **"hydra" shared-CLR loader**: keeps a warmed,
    shared CoreCLR so multiple .NET apps share pages and start fast (memory
    dedup across the many .NET UI apps).
  - `dotnet-gc-collector`, `dotnet-memory-reclaimer` — TV-specific memory
    pressure helpers hooked to `resourced`/`reclaimd`.
  - Preloaded .NET apps enumerated in `/usr/bin/dotnet-apps.txt` (`corerun`,
    home, store, search, ambient, weather, notification, …).

**And the root exploit is .NET too:** `SamsungTvRootAgent.dll` runs **as root**
via `/usr/bin/dotnet`, launched from your host (`172.17.2.206`) with a one-time
token, alongside helper agents (`SamsungTvEventAgent`, `SamsungTvRemoteInputAgent`,
`Qn90bDisplayControl`, `Qn90bSourceControl`, `FdetProbe`). This is the
"samsung-tv-root" / FdetProbe chain — a .NET agent that obtains root and then
executes commands (including launching our dropbear). It is **not** installed;
it's a live process rooted from the host.

---

## 6. Daemon landscape (~140 services, ~406 procs)

Grouped by role (selected highlights):

**Core system**
`systemd-udevd`, `deviced` (device daemon), `storaged`, `resourced` +
`reclaimd` + `rscmgr`/`resource-center` (aggressive memory/resource management —
this is a RAM-constrained box), `buxton2` (config/settings key-value store),
`fconfigd` (feature config), `pkgmgr-info`, `eventd`/`esd` (event system),
`stability-monitor`, `issue_report_agent` + `emergency-log-transmitter`.

**Networking & connectivity**
`connman` (connection manager) + `net-config` + `vd-net-config`,
`wpa_supplicant`, `wifi-monitor`/`wifi-presence`, `wfd-manager` (Wi-Fi Direct),
`mdnsd` (Bonjour/DNS-SD), `pushd` (push), `NW-Daemon`, `bt-service` + the
Bluetooth framework stack (`bluetooth-frwk`, `ble`, `bt.connectionmanager`, …),
`nfc-daemon`.

**Casting / media sharing (the open ports)**

| Port | Service | Purpose |
|------|---------|---------|
| 7000 | `airplay-service` | AirPlay |
| 8001/8002 | `msf-server` | Samsung Multiscreen / Smart View (remote control API) |
| 8080/8443 | `webserver` | local web / DIAL |
| 8187 | `allshare-svc` | DLNA / AllShare |
| 9197 | `dmr` | DLNA Digital Media Renderer |
| 9119 | `screen_sharing` | screen share |
| 9999 | `ScreenMirroring` | Miracast / mirroring |
| 7678 | `remote-server` | remote control |
| 7100 | `muse-server` | VD media/UI server |
| 36669 | `spotifyd` | Spotify Connect (lightweight node service) |
| 15500 | `acm-service` | — |
| **22** | **dropbear** | **← our SSH; see [Dropbear setup](../dropbear/README.md)** |

Media pipeline: `media-server`, `mediacontroller`, `pulseaudio`,
`speaker-manager`, `avocd-launcher` (AV output control), `sealayer` (parses
RCT/stream tables), `ci-daemon` (CI+ CAM), `pvr-service` (recording),
`source-service` (input switching).

**AI / NPU / "smart"**
`NPUManager` (on-device NPU), `AISControlManager`, `aifw-*` (AI framework data
manager + MSF service), `contents-recognition-service`,
`contents-ui-detector`, `screen-analysis-service-manager`,
`context-aware-agent/-service`, `activity-inference-service`,
`mirroring-recognition` — content analysis & AI upscaling / ambient awareness.

**TV core**
`tvs_deamon`/`tvs_lite`/`tvs-ht` (TV service), `tvctx-manager`, `source-service`,
`hdmicec-service` (HDMI-CEC), `tvtimer-manager`, `p2p-asm` (TV viewer),
`sealayer`, `register-tv/-av/-device`.

**Samsung services / telemetry**
`sesd` (Samsung Experience Service), `adagent-service` (ads), `kpi-logging` +
`sp-kpi` + `canalysis` (analytics/telemetry), `abnormal_user_behavior`,
`bixbycapsuleviewer`/`mybixby` (Bixby voice), `swu-auto-upgrade` +
`was-background-update` + `fw-update-service` (OTA firmware), `register-device`.

**Userland basics**
glibc **2.30** (`ld-2.30.so`), gcc runtime (`libstdc++.so.6.0.27`), `bash`,
`/bin` + `/usr/bin` + `/sbin`. No `hostname` binary; hostname is "Samsung".

---

## 7. How this maps back to the SSH/dropbear goal

Everything above explains the persistence problem in one line: **the only
writable, reboot-surviving storage is `/opt`, nothing in the boot path is
writable, SMACK blocks exec from userdata, and root itself is a host-launched
.NET agent.** So a reboot-persistent dropbear can't be a normal "install" on the
TV — it has to be either (a) re-triggered from the host after each boot via the
same .NET root agent, or (b) hooked into some stock service that already execs
from `/opt` (needs a targeted hunt). The concrete binary build, launcher,
configuration, runtime paths, and workarounds are documented in
[Dropbear SSH on the Samsung TV](../dropbear/README.md).

---

*All data gathered via read-only commands over SSH; no changes were made to the TV.*
