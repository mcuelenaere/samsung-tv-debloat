# Samsung TV — Secure Boot & the Boot Process

*Target: `root@192.0.2.10` · QE55S95BATXXH / S95B (PontusM, `sdp-soc`) ·
Tizen 6.5.0 · Linux 5.4.77 `armv7l` · build `T-PTMDEUC-1720.7`
(`T-PTMDEUC-REL-202602250226`) · investigated 2026-11-04, **read-only**.*

This note complements [Samsung QN90B — System Architecture](samsung-qn90b-architecture.md)
(§2 boot, §3 security model), [Samsung UEP on the TV](samsung-tv-uep.md), and
[Samsung QN90B SMACK Policy](samsung-tv-smack-policies.md). It exists to answer one
question the earlier docs left implicit: **what exactly happens between pressing the
power button and `systemd` running, what is authenticated along the way, and what does
that mean for getting something like `dropbear` to survive a reboot?**

---

## 0. TL;DR

The TV boots a **hardware root-of-trust chain** with four independent, differently
enforced layers:

1. **Secure boot (the bootloader chain).** The SoC BootROM authenticates ("dauth") and
   loads an **encrypted bootloader** from the eMMC hardware boot partition `boot0`.
   The next stages live in dedicated eMMC GPT partitions (`ddr.init`, `seret.bin`,
   `secos.bin`, …) and are encrypted/signed. There is no user-accessible U-Boot
   prompt, no `fw_printenv`, and `boot0`/`boot1` are presented read-only.
2. **Signed kernel image.** The Linux kernel (`uImage`, `mmcblk0p6`) and device tree
   (`dtb.bin`, `mmcblk0p8`) are accompanied by a detached signature blob
   (`sign.bin`, `mmcblk0p10`, an OpenSSL `Salted__` container). The bootloader
   verifies this before transferring control.
3. **Signed root filesystem.** The rootfs (`platform.img`, `mmcblk0p19`) is a
   read-only **VDFS4** volume that was created with a **signing key**
   (`/tmp/Tizen_VDFS_2022.product.key`). The kernel's VDFS driver enforces
   **per-file RSA signatures at decompression time**; editing a system file makes it
   fail verification.
4. **Runtime gates.** **UEP** blocks execution of unsigned ELF binaries and scripts
   *even as root*; **SMACK** governs which label may exec/read/write what; **Cynara**
   gates privileged APIs; the **TrustZone Secure OS** (`secos.bin`) holds the
   firmware/key material.

The writable, reboot-surviving storage is `mmcblk0p21` → `/opt` (+ `/var`→`/opt/var`),
`mmcblk0p20` → `/mnt/systemrw`, and `mmcblk0p17` → `/mnt/drm`. None of them is in the
systemd unit search path. **The only stock way to run code from a writable partition
at boot is a handful of services whose `ExecStart` points into `/opt`** — most notably
`AT-HUB.service` → `/opt/etc/parseinfo.sh` (root, no `User=`), gated only by UEP's
*script* signature check.

So the practical persistence problem is **not** "beat secure boot" — it is
**"get code into a path that an already-trusted, already-root boot service executes,
without tripping UEP."** §6 enumerates the candidates and what each requires.

---

## 1. The security layers, and what each one gates

| Layer | Where it lives | What it gates | Reboot-persistent? |
|---|---|---|---|
| **BootROM chain (secure boot)** | SoC mask ROM → eMMC `boot0` → boot partitions | Only Samsung-signed/encrypted bootloader + kernel + DTB boot | n/a (it *is* the chain) |
| **Kernel/DTB signature** | `sign.bin` (`p10`), verified by bootloader | Kernel + DTB integrity/authenticity | yes |
| **VDFS signed volume** | rootfs `platform.img` (`p19`), key `Tizen_VDFS_2022.product.key` | Per-file integrity of `/`, `/usr`, `/etc` | yes |
| **TrustZone Secure OS** | `secos.bin` (`p13`), `secos_drv.bin` (`p15`), `tzdev`/`tzdaemon` | TEE, key storage, firmware-decryption passphrase | yes |
| **UEP / kUEP** | kernel `uep` module + `libkUEPUser.so` + `sfpmd` | Execution of **unsigned ELF binaries and scripts**, even as UID 0 | volatile (RAM gate word) |
| **SMACK** | `/etc/smack/accesses.d` + per-app rules from `security-manager` | Which label may exec/read/write an object (e.g. exec from `/opt` userdata) | policy re-generated each boot |
| **Cynara / security-manager / zones** | `cynara-launcher`, `security-manager-*` | Per-privilege API access (internet, debugging, …) | DB on `/opt`, re-applied at boot |
| **IMA** | kernel `+IMA` (systemd builds with IMA) | Measurement/claims (observed infrastructure present; no policy dump in this session) | yes |

`SELP_ENABLE=<code>` on the kernel cmdline is the Samsung "secure boot / SE platform"
token the bootloader passes to the kernel (see §4.6). It has been present on Samsung
TV kernels for over a decade (e.g. `SELP_ENABLE=1198282` in a 2012 SNE report,
`SELP_ENABLE=20102011` on a 2020 KantS2 UN43T5300, `SELP_ENABLE=20227876` here).

---

## 2. Storage map (eMMC `mmcblk0`, GPT)

Read with `blkid` + `fdisk -l` (read-only). **A/B duplicate pairs are named in the
GPT**; on this unit the odd ("A") copies are **all-zero/unprogrammed**, and the even
("B") copies are the live ones. The live kernel is `p6`, the live rootfs is `p19`.

| # | GPT `PARTLABEL` | Size | Role / contents (observed) | Slot |
|---|---|---|---|---|
| 1–2 | `ddr.init` | 512 K ea | DDR init/training blob (encrypted) | A / **B** |
| 3–4 | `seret.bin` | 3.5 M ea | encrypted bootloader stage | A / **B** |
| 5–6 | `uImage` | 16 M ea | Linux kernel, U-Boot legacy image header, `Linux-5.4.77`, uncompressed, load `0x80008000` | A / **B** |
| 7–8 | `dtb.bin` | 1.5 M ea | Flattened device tree (text bootargs templates visible) | A / **B** |
| 9–10 | `sign.bin` | 64 K ea | detached kernel signature blob (`[len][Salted__…]`, OpenSSL container) | A / **B** |
| 11 | `VD-HEADER` | 64 K | board/image header: `$MMC_OTPE`, eMMC p/n `KLM8G1GETF-B(VD4.0)H`, base fw `T-PTMDEUC_1310.1` | — |
| 12–13 | `secos.bin` | 4 M ea | TrustZone Secure OS image (encrypted) | A / **B** |
| 14–15 | `secos_drv.bin` | 1 M ea | Secure OS driver (encrypted) | A / **B** |
| 16 | *(NONE)* | 2 M | reserved / scratch | — |
| 17 | *(NONE)* | 12 M | mounted `/mnt/drm` (rw **VDFS dncs**) — secure storage | — |
| 18–19 | `platform.img` | 1.7 G ea | **rootfs**, read-only **VDFS4**, zlib, **signed volume** → `/` | A(empty) / **B** |
| 20 | `systemrw.img` | 20 M | mounted `/mnt/systemrw` (rw **VDFS dncs**) — OTA staging | — |
| 21 | `data_peakSeason.img` | 3.8 G | mounted `/opt` (rw **VDFS dncs**) — userdata / `/var` / `/home` | — |
| 22–23 | `factory_peq.img` | 12 M ea | squashfs, factory audio EQ → `/factory_peq` | A(empty) / **B** |
| 24 | `reserved` | 4 M | `/common` (recovery images `data_backup.xz`, `systemrw_backup.xz`) | — |

Plus the eMMC **hardware** areas:

- `mmcblk0boot0` (4 MiB, `force_ro=1`) — **encrypted first-stage bootloader** (high
  entropy, no strings). `mmcblk0boot1` is empty.
- `mmcblk0rpmb` — Replay-Protected Memory Block (TEE/rpmb secure storage).

Live mounts (`/proc/mounts`):

```
/dev/root   /               vdfs   ro,relatime
/dev/mmcblk0p20 /mnt/systemrw vdfs rw,relatime,dncs
/dev/mmcblk0p21 /opt          vdfs rw,relatime,dncs
/dev/mmcblk0p17 /mnt/drm      vdfs rw,relatime,dncs
/dev/mmcblk0p23 /factory_peq  squashfs ro
tmpfs /run, /tmp, /dev/shm
smackfs /sys/fs/smackfs
```

The `ro` on `/` is the *mount flag + cmdline*; the block device itself reports
`ro=0`. The barrier to editing `/` is **VDFS signatures**, not the mount flag.

---

## 3. The boot process, stage by stage

### 3.0 BootROM / BL0 — immutable root of trust

The kernel debugfs node `/sys/kernel/debug/boot-trace/dump` still holds the **last
boot's on-chip trace** (written by BL0 before Linux). It reads:

```
boot-trace dump at e7000390(magic 0x31765442 ("BTv1"), entry size 360, flags 00000000)
        40ns( 1)         40ns E onboot start
   2465494ns( 60592)  2465454ns . pre init pll
  10114746ns(248580)  7649251ns . init pll
  20377319ns(500793) 10262573ns . clock reset
  21337972ns(524402)   960652ns . dauth complete        <-- device authentication
  39234252ns(964221) 17896280ns . init emmc
  39517008ns(971170)   282755ns . load board setting
  95920735ns(2357348) 56403727ns . do bl01
  95929850ns(2357572)     9114ns . do bl02
  95943155ns(2357899)    13305ns . do bl03
  96048990ns(2360500)   105784ns . setup vectors
  96065917ns(2360916)    16927ns . store board setting
 104734130ns(2573946)  8668212ns . load bl1
 403967895ns(9927915) 299233764ns . do bl1 lowlevel
 404797648ns(9948307)   829752ns E onboot end(warm)
```

Interpretation: BL0 initialises PLLs/clocks, performs a **`dauth`** (device
authentication — the root-of-trust check), initialises eMMC, reads the board setting,
runs sub-stages `bl01/bl02/bl03`, loads **BL1**, and jumps into it ("do bl1
lowlevel"). The BootROM is mask ROM: it cannot be modified, and it is the anchor the
whole chain ultimately rests on. There is no user-visible "developer/unlock" mode.

### 3.1 BL1 / secondary bootloader — eMMC `boot0`

`boot0` (4 MiB, `force_ro=1`) holds **encrypted bootloader code** (no readable
strings). The SoC is configured to boot from eMMC boot area, so BL1 is the first
attacker-relevant, Samsung-controlled, authenticated stage. It (and the stages it
loads) reads the GPT partitions below.

### 3.2 Secondary stages — `ddr.init`, `seret.bin`

- `ddr.init` (`p2`, 512 K) — DDR initialisation/training firmware.
- `seret.bin` (`p4`, 3.5 M) — an encrypted bootloader/recovery stage (the name is used
  across Samsung TV generations for a "recovery boot" image; treated here as an
  encrypted, signed next stage loaded from eMMC).

Both are high-entropy (encrypted). Their GPT labels and the `do bl01/02/03` trace
entries are the evidence; the exact split of who loads whom is not visible from the
running OS.

### 3.3 TrustZone Secure OS — `secos.bin` / `secos_drv.bin`

`p13` (`secos.bin`, 4 M) and `p15` (`secos_drv.bin`, 1 M) are the ARM **TrustZone
Secure OS** and its driver, loaded during the boot chain and surfaced to Linux as:

- `/dev/tzdev`, `/dev/tzdev_invoke`, `/dev/tzmem`, `/dev/tzrsrc`, `/dev/tzlog`
  (misc devices 48–53).
- kernel modules `tzdev`; daemons `tzdaemon` (`security-tzdaemon.service`),
  `secure-clock.service`, and `psa-init.service` (ARM PSA secure-boot init, runs as
  `owner`).
- reserved memory `secos_emmc` (128 KiB, `no-map`) and TZ prevent regions
  (`tzprevent0..7`, `prevent_reserved@0x100ff0000`) in the device tree.

The TEE is what owns the firmware AES passphrase and performs privileged crypto
(see §4.4 / §4.5).

### 3.4 Kernel + DTB + signature, and the Linux cmdline

The bootloader loads, verifies, and boots:

- `uImage` (`p6`) — a **U-Boot legacy image** (`magic 27 05 19 56`, name
  `Linux-5.4.77`, `os=05` Linux, `arch=02` ARM, `comp=00` none, load/entry
  `0x80008000`, size ≈ 9.4 MB).
- `dtb.bin` (`p8`) — the device tree. Notably it contains **bootargs templates** with
  empty placeholders that the bootloader fills in at runtime:

  ```
  dtsversion=0000000_pontusm-2.5G root=/dev/mmcblk0p18 init=/sbin/init rootfstype=vdfs ro \
  console=ttyS0,115200N8 rs232=0 SELP_ENABLE=       onboot_ver=       rootwait \
  no_console_suspend earlyprintk sdp-soc sdp_sparsemem vmalloc=400M main quiet \
  trace_event=early_event,user bdinfo=       model=       pba_none
  ```

  Variants exist for `main` / `oc` / `sm` / `-2G` SKUs. The **live** cmdline is the
  template with the bootloader's choices substituted:

  ```
  dtsversion=0000000_pontusm-2.5G root=/dev/mmcblk0p19 init=/sbin/init rootfstype=vdfs ro \
  console=ttyS3,115200N8 rs232=1 SELP_ENABLE=20227876 onboot_ver= rootwait \
  no_console_suspend earlyprintk sdp-soc sdp_sparsemem vmalloc=400M main quiet \
  trace_event=early_event,user bdinfo=10452002 model=4kbtin pba_none
  ```

  Note the bootloader switched `root=` from the template's `p18` (A slot) to **`p19`
  (B slot)**, set `SELP_ENABLE=20227876`, `rs232=1`, `console=ttyS3`, and
  `model=4kbtin`. So the **slot selection and SELP value are made by the bootloader,
  not the OS.**
- `sign.bin` (`p10`) — the detached signature: 4 zero bytes, 4-byte length
  (`0x134`), then an OpenSSL `Salted__` container, followed by a long hex blob (the
  RSA signature material). This is the authenticity proof the bootloader checks.

There is **no initramfs** (no `initrd=`; `init=` points straight at the rootfs). The
kernel cmdline also carries a **BOOT-mode structure hint** in the DTB
(`chosen/samsung,sdp-micom-gpr`): `0x9C12C0,bootprog@0x18,logbufaddr@0x38,logbufsize@0x3C,bootmode@0x04`
— a fixed-address boot-info block (bootmode/`bootprog`/log buffer) passed from the
bootloader/MICOM.

### 3.5 Root filesystem — VDFS4, signed, read-only

The kernel mounts `platform.img` as a **read-only VDFS4** volume. The volume's own
superblock records the exact `mkfs.vdfs` invocation that created it:

```
VDFS2007
mkfs.vdfs
-i -h sha256
-V -s /tmp/Tizen_VDFS_2022.product.key
-z 1740800K -c zlib --read-only
-r /var/tmp/mic-tmp-…          (2026.02.25-03:48)
VDFS4_VOLUME  1.56-211108
```

That is the smoking gun for rootfs integrity: the image was built **with a signing
key** (`sha256` hashes, `zlib` compression, read-only). The kernel's VDFS driver
enforces it at run time — strings in the kernel image include:

```
vdfs4-WARN: Driver supports only signed volumes
vdfs4-WARN: Image(%s) is RO image. But no hashtable.
vdfs4-WARN: %s: bad signature - %.8s, expected - %.8s
File based decompression RSA signature mismatch. Inode number - %lu, file name - %s
[%s] file %s has incorrect signature.(fsname:%s, mnt_flags:%x)
[%s] signature general check error on file %s.(fsname:%s, mnt_flags:%x)
rsa signature padding is incorrect
```

So the rootfs is **not** protected by dm-verity (there is none) but by **per-file
RSA signatures verified when a file is decompressed**. Modifying `/etc`, `/usr`, … on
flash produces a signature mismatch the next time the kernel reads the affected file,
not a silent boot. This is the concrete reason on-flash edits to `/` "brick/recover
rather than persist."

### 3.6 `init`, SMACK, and `systemd`

`init=/sbin/init` → `/sbin/init.wrapper` (`/usr/sbin/init.wrapper`), a shell script,
runs first and:

1. mounts `/proc`, `/tmp` (tmpfs), `/sys`, `/sys/kernel/debug`, then
   `/etc/rc.d/init.d/mount.sh` (mounts `factory_peq`, `systemrw`, `opt` via
   `mount.sh.common`),
2. selects a **boot mode** by probing markers:
   `/proc/pba_boot` → `pba`; `/mnt/systemrw/org.tizen.tv.swu/data/tv/STANDALONE` →
   `otn2.0`; `/mnt/systemrw/upgrade/.key` → `otm` (OTA); `/proc/lcm_boot` → `lcm`;
   otherwise **`normal`**,
3. mounts `/run` (tmpfs), `/dev/shm`,
4. runs `/etc/rc.d/init.d/smack_apply` (mounts `smackfs smackfsdef=*`, loads
   `default-ac-domains`, sets `mode=2`, `ptrace=1`, and backgrounds
   `smack_apply_app` which starts cynara + `security-manager-rules-loader`),
5. runs `fconfig_init.sh`, optionally `/etc/BM/bm_init.sh` (EarlyWarning/JBM boot mode),
6. `exec`s `/usr/lib/systemd/systemd`.

Then **systemd 244** (`+SMACK +IMA -SELINUX`) boots its own staged target graph:

```
default.target == bootmode.target
  └─ bootmode-graphical.service   (oneshot: `systemctl start starter.target`)
       └─ starter.target          (Alias=default.target)
            └─ (basic.target → sysinit.target → multi-user.target →
                vdtizen-bootseq-preload → SI-starter → predelayed → delayed → idle →
                graphical.target)
```

`starter.target` pulls ~60 units including **`AT-HUB.service`** (see §6). The
`vdtizen-bootseq-*` targets are Samsung's fast-start staging (only picture-path
services in the preload stage; everything else deferred until the panel is up).

### 3.7 App runtimes

After the platform targets, the app framework takes over: `amd` (App Manager Daemon),
AUL (launch protocol), `launchpad-process-pool` (pre-forked warm app processes), and
the three runtimes (native/EFL, .NET Core, WRT/Chromium). .NET preload apps are
launched via `/usr/bin/dotnet_init_and_launch_preload_apps.sh` → `app_launcher` as
uid 5001. None of this is in the early root boot path, but it *is* where several
community persistence routes live (§6).

---

## 4. How secure boot actually works (what is authenticated, and how)

### 4.1 Encryption vs. signature — two different things

Samsung's TV boot images are a mix:

- **Encrypted + authenticated**: the bootloader (`boot0`), `ddr.init`, `seret.bin`,
  `secos.bin`, `secos_drv.bin` are high-entropy (encrypted). The AES material is
  device/SoC-bound (handled by the ROM/TZ), not on the user partition.
- **Authenticated, not encrypted**: the Linux `uImage` + `dtb.bin` are plaintext but
  carry a detached signature in `sign.bin`; the rootfs `platform.img` is compressed
  (zlib) but not encrypted, and is protected by VDFS per-file signatures.

### 4.2 The root of trust

The `dauth complete` trace line is the BootROM's authentication step. Everything after
it chains from BL1 in `boot0`. There is no user-side key, unlock, or "developer boot"
path exposed.

### 4.3 Rollback / A-B

The GPT carries complete A/B image sets (`uImage`, `dtb.bin`, `sign.bin`, `secos.bin`,
`secos_drv.bin`, `platform.img`, `factory_peq.img`). On this unit slot **B is live**
and slot **A is zeroed**. This gives the bootloader/OTA a fallback slot, but it does
not help an attacker: a slot swap still requires **signed** images for that slot.
(Note the template `root=` points at `p18` and *this* boot used `p19` — i.e. the
slot was chosen by the bootloader, presumably because the image was flashed to B.)

### 4.4 Firmware / OTA crypto

Firmware upgrades (`SWUStandalone`, `fw-update-service`, `SWUVerifierTV`,
`swu-verifier-tv.service`) process an **AES-encrypted** update bundle whose key is
decrypted inside TrustZone. The public key material ships on the box:

- `/usr/share/org.tizen.tv.swu/itemsAESPassphraseEncrypted.txt` (TZ-wrapped AES
  passphrase),
- `itemsPublicRSAKey.txt`, `itemsPublicSubModelRSAKey.txt` (RSA verification keys),
- `OpenAPIAESPassphraseEncrypted.txt`, `OpenAPISubmodelAESPassphraseEncrypted.txt`.

Update staging lives on the writable `/mnt/systemrw` (`…/org.tizen.tv.swu/data/tv/STANDALONE`,
`…/upgrade/.key`), and `/usr/share/upgrade/{update.sh,update_mount.sh,verify.sh}`
perform the flash/reset. (The Synacktiv Q60T work bypassed the update verifier by
patching `SWUStandalone`; that is the general shape of a firmware-key attack, but it
needs on-device root and a firmware-verification weakness.)

### 4.5 UEP — the runtime execution gate

Covered in detail in [Samsung UEP on the TV](samsung-tv-uep.md). Summary relevant
here:

- kernel `uep` module gates **ELF execution** (`SfdUepCheckFileType`), holding a
  volatile 32-bit gate word,
- **patched `bash`/`sh`/`node`** gate **script** execution via `libkUEPUser.so`
  (`uep_verify`); signed scripts carry a trailing `##UEP…:UEP` line,
- `sfpmd` can clear the gate for one boot when it sees a Samsung-signed `VDUEPOFF.*`
  certificate on USB,
- the gate is **RAM-only and lost on reboot**; the community flips it by kernel
  memory write. On build `T-PTMDEUC-REL-202602250226` the word is physical
  `0x208c26c4`, verified from the `mmcblk0p6` image and flipped on-device
  ([UEP notes](samsung-tv-uep.md) §8).

**Key distinction for persistence:** UEP is a *file*-level check at `execve`/interp
startup. It is **not** applied to (a) ELF code mapped and executed from memory
(`memfd_create`/anonymous mapping — the documented bypass used by the UN43T5300 LPE),
or (b) managed code loaded by an interpreter/JIT that doesn't itself call `uep_verify`
(e.g. .NET IL, which is the basis of the levkropp route).

### 4.6 `SELP_ENABLE`

`SELP_ENABLE=20227876` is a bootloader-supplied token. It is set at boot (the DTB
template leaves it empty) and consumed by the kernel (the kernel image contains
`[SELP] while loop ... please attach T32...` and `SELP_ENABLE`). The exact semantics
are undocumented; it has appeared on Samsung TV cmdlines for ~15 years with different
values. On this box it is a **fingerprint of which secure-boot policy the bootloader
selected**, not something the OS can meaningfully change after the fact.

---

## 5. Writability map — what survives reboot and what is checked

| Path | Backing | Reboot-survives | Integrity check on content |
|---|---|---|---|
| `/` `/usr` `/etc` `/bin` `/sbin` `/lib` | `p19` VDFS ro | editable at block level but **not usable** | **VDFS per-file RSA signature** |
| `/opt`, `/var`, `/home` | `p21` VDFS rw (`dncs`) | **yes** | none (unsigned, writable) |
| `/mnt/systemrw` | `p20` VDFS rw (`dncs`) | yes | none |
| `/mnt/drm` | `p17` VDFS rw (`dncs`) | yes | none (secure-storage data) |
| `/common` | `p24` VDFS ro | no (recovery images) | — |
| `/run`, `/tmp`, `/dev/shm` | tmpfs | **no** | — |
| `boot0`/`boot1` | eMMC HW boot area | yes | `force_ro=1`; encrypted+signed |
| kernel/DTB/sign | `p5–p10` | yes | **bootloader signature check** |

**There is nothing writable in the systemd unit path.** `systemctl get-default` is
`bootmode.target`; `UnitPath=/etc/systemd/system /run/systemd/system
/run/systemd/generator /usr/lib/systemd/system /run/systemd/generator.late` — the
first is on the read-only VDFS rootfs, the rest are tmpfs. You cannot drop a `.service`
that persists.

---

## 6. Persistence analysis (given secure boot)

### 6.1 Why the obvious ideas fail

- **Edit a system service / add a unit** → rootfs is a signed VDFS volume; the file
  fails RSA verification. (Also no writable unit path.)
- **Replace the kernel / bootloader** → encrypted + signed; `boot0` read-only.
- **Put a binary on `/opt` and run it at boot** → SMACK blocks exec from userdata
  (that is why dropbear is copied to `/run` → `System::Run`), *and* UEP rejects the
  unsigned ELF if SMACK is satisfied.
- **Make `/run` persist** → it is tmpfs and recreated each boot.

### 6.2 The actual attack surface: root-owned boot hooks that read from writable storage

The boot path is not fully closed — it executes a number of things from `/opt`. The
canonical one:

**`AT-HUB.service` → `/opt/etc/parseinfo.sh`**

```ini
# /usr/lib/systemd/system/AT-HUB.service
[Unit]
After=buxton2.service systemd-udevd.service connman.service net-config.service deviced.service
Description=Launch AT-HUB
[Service]
ExecStart=/opt/etc/parseinfo.sh &      # no User= → runs as root
[Install]
WantedBy=starter.target
```

Observed attributes:

- `/opt/etc` is SMACK `System::Shared` with `transmute=TRUE`; the script itself is
  labelled **`_` (floor)** → read/execute allowed to everyone.
- The process runs as **root** under AT-HUB's label (systemd default, effectively
  `System::Privileged`).
- The script is a `#!/bin/bash` script carrying a **UEP script signature**
  (`##UEP…:UEP`).
- Its functional body is trivial: it writes `__<BUILD>__<MODEL>` to
  `/proc/target_version`.

**Verified mechanics (full evidence in
[re/at-hub-persistence-analysis.md](../re/at-hub-persistence-analysis.md)):**

- systemd does **not** use a shell: it `execve()`s the file and the kernel runs the
  `#!` handler. The trailing `&` is passed as **`argv[1]`** (harmless), not as
  backgrounding. `systemctl show` records `ExecStartEx={ path=/opt/etc/parseinfo.sh ;
  argv[]=/opt/etc/parseinfo.sh & ; … }`; the stock run exits `status=0` after ~4 s.
- The process is **uid 0**, SMACK **`System::Privileged`** (PID 1's label; the unit
  has no `SmackProcessLabel=` and the script has no `SMACK64EXEC`). `System::Privileged`
  has `x` on both `_` and `System::Shared`, so SMACK is not a blocker.
- The script UEP check is **userspace, inside the interpreter**: `bash`/`sh`/`node`
  `dlopen()` `libkUEPUser.so` and call `verifyFileWihtoutKey()`, which first calls
  `BSContext::checkFileIsInRWUser()` — i.e. **only scripts living on a writable mount
  are verified** (RO-rootfs scripts are allowed, which is why the unsigned
  `init.wrapper` boots). On this chassis the check is armed because `/proc/modules`
  contains the platform token `micom` (`tztv_micom`). Failure prints
  `[uep][bash] the file is NOT signed!! : <path>` and `exit(1)`. Sourcing (`.`) a
  file skips the check entirely — the same rule already recorded in
  [dropbear/README.md](../dropbear/README.md).
- The trailer is `[signature][base64(len)][":UEP"]`, i.e. the last line
  `##UEP…:UEP`; editing the body invalidates it.
- **The kernel `uep` module has no script path** — its symbols are ELF-only
  (`SfdUepVerifyExecutableBinary`, `SfdUepVerifyDynamicLibrary`). So the gate is
  whatever the interpreter does in userspace.

This is the danielyc #17 route (already in
[the ecosystem notes](samsung-tv-root-ecosystem.md) §5). The question is only whether
UEP blocks an edited copy:

- **If UEP script checking covers `/opt` scripts**, editing the body will trip
  `[uep][bash] the file is NOT signed!!`.
- **Bypass: change the interpreter.** The script UEP check lives in the *interpreter*
  (`bash`/`sh`/`node` import `libkUEPUser.so`). `/usr/bin/awk → gawk` **does not link
  `libkUEPUser.so`** (verified: 0 occurrences of `kUEP`/`uep_verify` in the binary).
  Replacing the first line with `#!/usr/bin/gawk -f` (and writing a gawk-compatible
  body) sidesteps the check entirely — exactly the danielyc #17 finding, and it
  applies to *any* writable script that a root service executes.

This is the single most promising persistence primitive on this box, but it is
**untested here** and must be validated carefully (see the test plan referenced in §6.7).

### 6.3 Other root `ExecStart` targets under `/opt`

Many stock units run binaries from `/opt/usr/apps/<pkg>/bin/…` (e.g.
`pisa-service`, `canalysis-service`, `context-aware-agent/-service`,
`com.samsung.tv.ghdaemon`, `slive-provider-daemon`, …). Those binaries are
**Samsung-signed app payloads**; swapping the binary trips UEP (unsigned ELF) *and*
would fail nothing else (the files are writable). So:
- replacing a native app binary → blocked by UEP (unless a memory-exec primitive is
  available),
- replacing **managed (.NET) IL** → **not** caught by UEP. The generic levkropp route
  ("replace the IL of a `dotnet-inhouse` app") maps onto this build as follows (full
  evidence in [`re/dotnet-boot-persistence-analysis.md`](../re/dotnet-boot-persistence-analysis.md)):
  - the apphosts (`dotnet-launcher`, `dotnet-launcher-inhouse` — the same binary,
    `dotnet-loader`, `dotnet-hydra-loader`, `launchpad-starter`) are **UEP-signed
    ELFs — do not touch them**;
  - the managed payloads are **PE/COFF (`MZ`), carry no `:UEP`, and are `mmap`ed by
    the CLR, not `execve`d**, so replacing them is UEP-exempt;
  - the artifact that actually executes is the **ReadyToRun `.ni.dll`** under
    `bin/.native_image/`, not the plain `.dll` (the plain `.dll` is not mapped —
    confirmed from `/proc/<pid>/maps`, so a `.dll`-only swap does nothing);
  - the one **boot-launched** `.NET` app whose managed files sit on writable `/opt`
    is `org.tizen.art-app` (`type="dotnet-inhouse"`), started every boot by
    `/usr/bin/art_start.sh` ← `/usr/apps/fastboot/bin/fb.sh`;
  - secondary stock hooks: `dotnet-init.service` runs `dotnet-init.sh` **as root** and
    re-links `Tizen.Bixby.{dll,ni.dll}` for four apps from writable
    `/opt/usr/dotnet/…`, and reads the uid-5001-writable
    `/opt/usr/home/owner/dotnet-apps.txt`;
  - **caveat:** the app runs as **uid 5001 (`owner`)**, so this gives boot-time,
    UEP-exempt code execution but **not root by itself** — a further LPE step is
    needed (the same root toolkit primitive used elsewhere).

### 6.4 `ConditionPathExists` markers — influence without execution

Many units are gated by writable markers under `/opt/usr/share/systemd/system/`
(e.g. `ConditionPathExists=!/opt/usr/share/systemd/system/canalysis-daemon`). This
directory exists on this unit and is empty. Creating markers can **disable** services
(debloat), but cannot add code. Useful context, not a persistence primitive by itself.

### 6.5 cgroup / lifecycle caveat (matters for sshd specifically)

The running `dropbear` lives inside **`sdbd.service`'s cgroup** (root access here is
delivered through the sdb/dotnet chain). Restarting or killing `sdbd.service` tears
down dropbear too. Any "keep sshd up" scheme must either:

- move it out of that cgroup (start it from a different, stable unit / `systemd-run
  --scope` at boot), or
- accept that it is re-launched by whatever boot hook is used.

### 6.6 Candidate routes ranked (for this Tizen 6.5 / PontusM build)

| # | Route | Needs | Blocks | Confidence |
|---|---|---|---|---|
| 1 | **`AT-HUB.service` + `parseinfo.sh` gawk-shebang rewrite** | write to `/opt/etc/parseinfo.sh`; gawk available | UEP *script* check is userspace + **RW-mount-only**; gawk has no `libkUEPUser` | **High** (mechanism verified; only the live `#!`-exec test remains) |
| 2 | **`AT-HUB.service` + a UEP-valid replacement** | a script that still passes UEP (e.g. keep the signature? not possible if body changes) | — | Low unless a signing bypass exists |
| 3 | **`.NET IL/NI replacement of a boot-time app** (levkropp) | boot-launched `.NET` app with writable managed files → **`org.tizen.art-app`** (replace/remove `bin/.native_image/ArtApp.ni.dll`) | UEP does not cover managed IL (`mmap`, not `execve`) | **High** for boot code-exec as uid 5001; **not root** without a second step |
| 4 | **Startup-slot stub TPK / GamingHub slot** (supersharp / kninetimmy) | package install + startup slot + signature behaviour | UEP/SMACK only at the native boundary | Medium |
| 5 | **Kernel-memory UEP-off + run unsigned from memory each boot** | an initial code-exec each boot (chicken-and-egg) | defeats UEP but not the boot problem | Only with another hook |
| 6 | **Host/TV-local watchdog reacquiring root each boot** | a host on the network, or a TV-local trigger that runs (see #1) | none (this is the current fallback) | High but not self-contained |
| 7 | **Modify rootfs / bootloader** | — | VDFS signatures + secure boot | Not viable |

### 6.7 Safe, read-only checks — completed

The following were carried out read-only (details and evidence in
[re/at-hub-persistence-analysis.md](../re/at-hub-persistence-analysis.md) and
[re/dotnet-boot-persistence-analysis.md](../re/dotnet-boot-persistence-analysis.md)):

- **Full inventory** of units whose `ExecStart` references a writable path, with their
  `User=`. Result: `AT-HUB.service` is the **only** root unit that executes a *script*
  from writable storage; the rest are either `User=owner` app payloads or
  `setcap`/`touch`/log-path-only usages.
- **Interpreter UEP mapping**: `/usr/bin/gawk` (no `kUEP`), `/usr/bin/awk`→gawk (no),
  `/usr/bin/bash`/`sh`/`node` (yes). The bash check is userspace and RW-mount-only.
- **Boot-time `.NET` enumeration**: the boot-launched app with writable managed files is
  `org.tizen.art-app` (`.ni.dll` executes, uid 5001).
- Boot evidence re-read on-device: `/sys/kernel/debug/boot-trace/dump`, `/proc/cmdline`,
  the DTB, and the VDFS superblock.

Then, only when the user approves a **controlled test** (outside the read-only
constraint):

1. Back up `/opt/etc/parseinfo.sh` (it is on `/opt`, persistent; keep a copy in
   `/opt/…`).
2. Replace the shebang with `#!/usr/bin/gawk -f`, port the one-line body to gawk, add
   the dropbear launch, and reboot.
3. Observe whether UEP blocks it; if it does, restore from the backup.
   *(This is a mutation and must be an explicit, separate decision.)*

---

## 7. Open questions

1. **`SELP_ENABLE` semantics** — what does the value encode, and is it enforced by
   the kernel, by TrustZone, or purely informational?
2. **Which component enforces the VDFS signatures** — `sdp-unzip-vdfsapi` debugfs
   exists (`hw_capaility: comp_type 0x0c, hash_type 0x08`), suggesting a hardware
   decompression/signature engine; the exact policy (all files vs. RO files only) is
   unconfirmed.
3. **Does the kernel also gate a `#!` exec of an RW-mount script at `execve`?** The
   userspace library re-implements the RW-mount rule (`checkFileIsInRWUser`), which
   implies there is no kernel script path — but this is the one decisive test for
   route #1 (test plan in
   [re/at-hub-persistence-analysis.md](../re/at-hub-persistence-analysis.md) §5).
4. **Does the app framework verify `author-signature.xml` at launch?** (`amd`'s real
   logic is in the unpulled `libamd.so.1`/`libaul-server.so.0`.) Decides whether the
   `.NET`-IL route (#3) survives a manifest/signature check.
5. **`bdinfo=10452002` / `model=4kbtin` / `onboot_ver=`** — boot-info fields; not yet
   decoded.
6. **Exact role split** of `ddr.init` / `seret.bin` / `bl01..bl03` in the ROM→BL1
   handoff.

*Resolved since the first draft: UEP's scope for `/opt` scripts, the root boot-hook
inventory, and the `.NET` boot path — see the two analysis notes under
[`re/`](../re/).*

---

## 8. Evidence & method

All gathered **read-only** over SSH; no files were changed on the TV. Key sources:

- **Partitions / crypto:** `blkid`, `fdisk -l /dev/mmcblk0`, `cat /proc/partitions`,
  `dd if=/dev/mmcblk0pX … | od` for `p3..p24`, `boot0`/`boot1` and the VDFS superblock
  of `p19`.
- **Boot chain:** `/sys/kernel/debug/boot-trace/dump`, `/proc/cmdline`,
  `/proc/device-tree/chosen/bootargs`, `/proc/device-tree/**` (incl.
  `chosen/samsung,sdp-micom-gpr`), `dmesg`.
- **Rootfs integrity:** kernel image strings from `p6`
  (`dd if=/dev/mmcblk0p6 bs=64 skip=1 | tr -c '[:print:]' '\n' | grep …`), VDFS
  superblock `mkfs.vdfs` args.
- **Init / systemd:** `/usr/sbin/init.wrapper`, `/etc/rc.d/init.d/{mount.sh,
  mount.sh.common,smack_apply,smack_apply_app,psa_mount.sh,smack_apply_app}`,
  `/etc/BM/bm_init.sh`, `systemctl get-default`, `systemctl list-units --type=target`,
  `systemctl show -p UnitPath`, `/usr/lib/systemd/system/**`, unit `.wants` dirs.
- **Persistence surface:** `AT-HUB.service`, `/opt/etc/parseinfo.sh`, `/usr/bin/gawk`
  vs `bash`/`sh`/`node` (`grep -c kUEP`), `/opt/usr/share/systemd/system/`,
  `ps -Z`, `chsmack`, `systemctl status sdbd.service`.

### Cross-references

- [Samsung QN90B — System Architecture](samsung-qn90b-architecture.md) — §2 boot, §3 security model.
- [Samsung UEP on the TV](samsung-tv-uep.md) — the exec gate, `##UEP…:UEP`, `sfpmd`.
- [Samsung QN90B SMACK Policy](samsung-tv-smack-policies.md) — labels, live policy.
- [re/qn90b-core-daemons](../re/qn90b-core-daemons/README.md) — `parseinfo.sh` /
  `AT-HUB` persistence lead; `FINDINGS.md` for other writable surfaces.
- [re/at-hub-persistence-analysis.md](../re/at-hub-persistence-analysis.md) — deep-dive
  of the `AT-HUB` / `parseinfo.sh` / gawk route (systemd exec, UEP internals, test plan).
- [re/dotnet-boot-persistence-analysis.md](../re/dotnet-boot-persistence-analysis.md) —
  `.NET` IL/NI boot-persistence analysis (`org.tizen.art-app`, loaders, UEP coverage).
- [samsung-tv-root ecosystem](samsung-tv-root-ecosystem.md) — §5 persistence routes
  (levkropp .NET, danielyc #17 gawk, kninetimmy #42, supersharp #29).
- External: Synacktiv *Rooting Samsung Q60T* (same partition naming `seret.bin`,
  `secos.bin`, `sign.bin`, `uImage`; UEP + encrypted firmware via TrustZone);
  F-Secure *Opening up the Samsung Q60 series*; califio *UN43T5300* (memfd UEP
  bypass).
