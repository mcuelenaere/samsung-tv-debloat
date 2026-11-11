# Samsung UEP on the TV — Setup & Capabilities

*Target: `root@192.0.2.10` · Tizen 6.5.0 · build `T-PTMDEUC-REL-202602250226`
(QE55S95BATXXH / S95B, PontusM) · investigated 2026-11-04 (read-only); UEP gate
re-derived and tested, path scope confirmed 2026-11-05.*

This note documents **UEP** ("Unsigned Execution Prevention" / Samsung *kUEP*) as
it is actually deployed on this TV: the kernel module, the userspace verification
library, the daemon that can switch it off, the signed-script marker format, and
which components on the box consume it. It complements
[Samsung QN90B — System Architecture](samsung-qn90b-architecture.md) (§3 Security
model) and the dropbear write-up, both of which touch UEP from the side.

---

## 0. TL;DR

UEP is Samsung's **unsigned-execution gate**, distinct from both SMACK and
secure boot, and it stays in force **even as UID 0**. On this TV it is three
cooperating pieces:

1. A **built-in kernel module** `uep` (`v.0.0.1`) that checks files at execution
   time (`SfdUepCheckFileType`) and holds a single 32-bit **gate word** that its
   own packet handler (`SfdUepPacketHandler`) clears when Samsung hands it an
   authorized, device-bound *UEP-disable certificate*.
2. A **userspace library** `libkUEPUser.so` (package `kuepuser-release-2.2.4`)
   that performs the actual signature/device-ID verification (RSA via OpenSSL,
   two embedded public keys) and reports verdicts to `sfpmd`.
3. A **daemon** `sfpmd` ("Smart Security Product daemon", package
   `smartsecurity2-5.0.5`) whose `SfKUEPSwitcher` looks for a `VDUEPOFF.*`
   certificate on USB storage and, if it verifies, tells the kernel to disable
   UEP **for the current boot**.

The gate is therefore **volatile by design** (a reboot restores it), and the only
*authorized* way to clear it is a Samsung-signed USB certificate. The community
root toolkit works around this by flipping the same gate word in RAM through a
kernel-memory primitive. On **this** exact build that word was re-derived from
the live kernel and flipped successfully (see §8).

Enforcement is also **path-scoped**: the kernel only verifies files whose
`d_path()` falls under `/opt`, `/tmp`, `/var`, or `/home`. A bind mount that
exposes such a file under a path outside that list (e.g. `/run/...`) skips the
UEP check entirely — confirmed on-device on 2026-11-05 (§8).

---

## 1. What "UEP" is (and what it is not)

| Layer | Mechanism | What it gates |
|---|---|---|
| Secure boot | `SELP_ENABLE=20227876` on the kernel cmdline + RSA-signed system image | Boots only a signed image |
| **UEP / kUEP** | kernel `uep` module + `libkUEPUser.so` + `sfpmd` | Execution of **unsigned ELF binaries and unsigned scripts**, at runtime, even for root |
| SMACK | `+SMACK` LSM, labels from `/etc/smack/accesses.d` | Which label may exec/read/write what (e.g. exec from the userdata mount) |
| Cynara / zones / `security-manager` | per-privilege + container policy | App privileges, Knox-style zones |

These are complementary. A concrete example from this box: our own dropbear is an
unsigned native ELF; SMACK is what rejects it *from the userdata mount*, so the
launcher copies it to `/run` (`System::Run`) to run. UEP is the separate boundary
that a root toolkit has to defeat if it wants to run *uploaded* native payloads.

The naming that appears on the box:

- dlog tag **`kuepuser`** and the package name **`kuepuser-release`**.
- `sfpmd` symbols use **`KUEP`** (`SfKUEPSwitcher`, `DisableKUEP`, `SendDUIDToKernel`).
- `tzdaemon` carries a **`ZUEP`** marker and reports through `SendSecurityReport`.
- Kernel printk prefix **`SfdUep…`** ("SFD" = the Samsung security framework the
  module belongs to).

---

## 2. Component inventory (as found on the TV)

| Artifact | Version / detail |
|---|---|
| Kernel module `uep` | built-in (`/sys/module/uep` present, absent from `/proc/modules`); `version = v.0.0.1` |
| `libkUEPUser.so` | ELF 32-bit ARM, stripped; `kuepuser-release-2.2.4` (build path `…/BUILD/kuepuser-release-2.2.4/source/libbs/src/…`); ~42 KB |
| `sfpmd` | `/usr/bin/sfpmd`; `smartsecurity2-5.0.5`; systemd unit `sfpmd.service` (`Restart=always`) |
| Signature config | two embedded RSA-2048 `SubjectPublicKeyInfo` PEM keys inside `libkUEPUser.so` |
| USB certificate names | `VDUEPOFF.cer`, `VDUEPOFF.get`, `VDUEPOFF.uid` |
| Runtime sockets / markers | `/run/security-manager.socket`, `/run/securezone_ready` |
| Kernel cmdline | `SELP_ENABLE=20227876 … sdp_sparsemem … model=4kbtin quiet` |

`libkUEPUser.so` links: `libcrypto.so.1.1` (OpenSSL 1.1.1w), `libvconf.so.0`,
`libjsoncpp.so.22`, `libcapi-system-usbdevice.so` (USB presence), `libdlog.so.0`.

---

## 3. Kernel layer — the exec gate and the state word

- The module is **built into the kernel** and exposes essentially nothing to
  userspace: only `/sys/module/uep/version` (`v.0.0.1`) and `uevent`. There is
  **no** status file, `ioctl` node, or `/proc` entry, and `/proc/kallsyms` does
  not exist on this build.
- It runs a **file-type/signature check at execution time**. Kernel log evidence:

  ```text
  [SfdUepCheckFileType] unknown file type(#RPA)
  ```

  (observed repeatedly in `dmesg`; unrelated to SMACK denials).
- It owns a single 32-bit **gate word**: `1` = UEP closed/on, `0` = open/off.
  Samsung's own handler `SfdUepPacketHandler` clears it (`1 → 0`) after accepting
  a valid device-specific UEP-disable certificate.
- For this PontusM family the word is at **virtual `0xc08c26c4` / physical
  `0x208c26c4`,** and the transition is a compare-before-write (verify
  surrounding state, write, verify read-back). This was confirmed on this exact
  build; see §8.
- Enforcement is **path-scoped**, not inode-scoped. The verifier builds the
  file's absolute path with `d_path()` (`0xc01ac8f4`) and compares it against
  `/opt:/tmp:/var:/home` (`0xc09f46b0`). Paths outside that list take the
  "UEP disabled" branch without a signature check; a bind mount therefore
  changes whether a file is checked. See §8.

Because the state lives only in RAM, any disable is **lost on reboot**, which is
why every project in this space pairs "root" with "re-acquire after boot".

---

## 4. Userspace layer — `libkUEPUser.so`

This library is the verifier. Its exported surface (unmangled from the stripped
ELF's dynsym) is the clearest description of the UEP capability set:

| Class / symbol | Capability |
|---|---|
| `BSContext::verifyFile(path, …)` | Verify one file against Samsung's signatures |
| `BSContext::verifyFileWithDeviceId(path, duid, …)` | Verify a file **bound to the device's unique ID** |
| `BSContext::SelectPubKeyEcc(…)` | Select the public key (ECC-capable path) |
| `VerifyContext` | RSA verify primitives over OpenSSL (`EVP_sha256`, `EVP_VerifyFinal`, `PEM_read_bio_PUBKEY`) |
| `Signature::loadFromFile(…, _UepSigMeta)` | Parse the on-disk signature blob + metadata (typed by one byte, `[%c]`) |
| `SecureZoneContext::{isIsolatedContainer, getContainerInformation, isRootUID}` | Read `/proc/self/zone`; UEP level/type per process/zone |
| `UnixSocket::*` + `SfDaemon/UserMsg` | Report results to `sfpmd` over a Unix socket |
| `SendSecurityReport(FileReport / DataReport / LogReport)` | Emit structured JSON security reports |
| `GetDUID` (`vconf` key `db/comss/hwduid`) | Device Unique ID used for device-bound verification |
| `FindUEPDiableFile()` *(sic)* | Locate the USB UEP-disable certificate |

Verdict codes seen in strings:

```text
BS_NOT_SIGNED / BS_SIGNATURE_INCORRECT
"[uep][%s] : error during signature check [ file_path : %s ]"
"Correct/Incorrect/Not Signed [ file_path : %s ]"
"DUID Correct/Incorrect Signed [ file_path : %s ]"
```

**Embedded trust anchors** (two distinct RSA-2048 public keys, Base64 DER bodies
extracted from the binary):

| # | SHA-256 of DER `SubjectPublicKeyInfo` |
|---|---|
| 0 | `e70652ef420f91863bde548d6bff5a351a6a628c4b921f9474138a26bf46ee89` |
| 1 | `219eaa6a33ef654cae83a6d76f1fcc8f212687f1f312d497bf62dda4eedb0eb5` |

These are the keys against which UEP signatures (firmware/update files and
scripts) are checked on this unit.

---

## 5. `sfpmd` — the authorized "turn UEP off" path

`sfpmd` is the only component that can *legitimately* open the gate. Its
`SfKUEPSwitcher` class exposes (from symbol names):

```text
SfKUEPSwitcher::FindUEPDiableFile()      // look for VDUEPOFF.* on storage
SfKUEPSwitcher::ProcessFolder()          // scan a folder (USB mount, _UEP_MNT)
SfKUEPSwitcher::InitializeDUID()         // read device unique ID
SfKUEPSwitcher::SendDUIDToKernel()       // push DUID to the kernel
SfKUEPSwitcher::PlaceDUIDToUSBStorage()  // write the .uid sidecar back
SfKUEPSwitcher::DisableKUEP(path)        // perform the disable
```

Observed log strings:

```text
sfpmd: removable storage attached./detached.
sfpmd: Get storage path failed;  USB is not pluged
sfpmd: Find USBUEPPath : %s
sfpmd: Found file to disable kUEP at run-time
sfpmd: Try to disable kUEP;
```

So the **documented, sanctioned flow** is:

1. A Samsung-signed `VDUEPOFF.cer` (+ `.get` / `.uid`, device-bound) is placed on
   a USB mass-storage device.
2. Plugging it in raises a removable-storage event (`libcapi-system-usbdevice`).
3. `sfpmd` finds the certificate at the USB mount (`_UEP_MNT`), verifies it with
   `libkUEPUser` **against this device's DUID** (one of the two embedded keys),
   and — only if it verifies — calls `DisableKUEP`.
4. The kernel clears the gate word for the current boot: **unsigned natives now
   run until the next power cycle.**

This is the factory/service mechanism; it is not an attack. No such certificate
was present or mounted during this investigation.

---

## 6. Script UEP — the `##UEP…:UEP` marker

UEP also covers **shell and JS scripts**, via patched interpreters:

- `/usr/bin/bash` and `/usr/bin/sh` import `libkUEPUser.so` and carry an
  internal `uep_verify`; an unsigned script produces
  `[uep][bash] the file is NOT signed!! : <path>`.
- `/usr/bin/node` (Node.js 12.5.0) does the same for JS:
  `[uep][nodejs] the file is NOT signed!!`.

Samsung-signed scripts carry an appended marker line of the form:

```text
##UEP<base64>:UEP
```

`/opt/etc/parseinfo.sh` (a stock script executed from the `AT-HUB.service` boot
hook) contains exactly one such line, 533 bytes long, `##UEP` + 524 base64 chars
+ `:UEP`; the payload decodes to ~392 bytes of binary (signature/metadata, not a
plain OpenPGP packet). This is a useful corroboration that the same trust anchors
in `libkUEPUser.so` sign *both* update files and boot-relevant scripts.

Practical consequence on this TV (recorded in the dropbear write-up): a custom
launcher script must be `source`d rather than passed as an argument to `bash`,
because the latter trips this check.

---

## 7. Who uses UEP on this box

`libkUEPUser.so` is mapped or referenced by:

| Consumer | Role |
|---|---|
| `/usr/bin/bash`, `/usr/bin/sh` | Script signature check (`uep_verify`) |
| `/usr/bin/node` | Script signature check (`[uep][nodejs]`) |
| `/usr/bin/smack-logger` | Security logging |
| `/usr/bin/tzdaemon` | TrustZone daemon; `ZUEP` marker, `SendSecurityReport` |
| `/usr/bin/zone-template-launcher` | `zone_verify_uep_level_from_file`, `zone_verify_uep_sign` — UEP **level** and **signature** checks for lxc/zone templates |
| `/usr/bin/fw-update-service`, `/usr/bin/fw-usb-update-service` | Verify firmware/update file signatures (`"[uep][%s] the file is signed successfully"`) |
| `/usr/bin/sfpmd` | The switch itself (`SfKUEPSwitcher`) |

Related but separate consumers of the same trust model: `security-manager`,
`zone-manager`, `zone-mount-manager` (all present in `/usr/bin`).

---

## 8. How this interacts with root (and why this build matters)

Root and unsigned execution are **separate** boundaries; UID 0 alone does not run
an unsigned ELF. The community toolkit
([`chris-ritsen/samsung-tv-root`](https://github.com/chris-ritsen/samsung-tv-root))
therefore ships explicit UEP control:

```console
samsung-tv-root qn90b uep <tv> status    # read the gate word
samsung-tv-root qn90b uep <tv> disable   # 1 -> 0, volatile
```

It reads/writes the fixed physical address `0x208c26c4` through the FDET
physical-memory primitive, and the transition is lost on reboot.

**Resolved on this build (2026-11-05).** The earlier caveat was that the fixed
address was gated behind the toolkit's `require_tested()` check, which only
accepted `T-PTMAKUC-REL-202310071804`; our build was root-compatible but
`uep disable` failed closed. The address is in fact unchanged:

- The active image is an uncompressed ARM `Image` in `mmcblk0p6` (u-boot uImage,
  `Linux-5.4.77`). Its `linux_banner` string sits exactly at the start of
  `Kernel rodata` (`0x206da000` per `/proc/iomem`), which fixes the mapping
  virtual `0xc0008000` ↔ physical `0x20008000`.
- `SfdUepPacketHandler` at `0xc0283048` clears the word with
  `movw r3,#0x26c0; movt r3,#0xc08c; str r2,[r3,#4]` (`r2=0`), i.e. virtual
  `0xc08c26c4` / physical `0x208c26c4`.
- `SfdUepVerifyFileSignature` at `0xc0282088` reads the same word (`[base+4]`);
  when it is `0` the function takes the skip-verification branch.
- A read-only physical read through the already-staged `FdetProbe.dll`
  (`rw 0x208c26c4 1`) observed `1`; `wwi 0x208c26c4 0 0x1 0x0` performed the
  compare-before-write `1 -> 0`, and a readback observed `0` with the rest of
  the 24-word neighbourhood unchanged.

The toolkit now lists `T-PTMDEUC-REL-202602250226` in the QN90B
`additional_tested_builds`, so `uep status` and `uep disable` work on this unit:

```console
samsung-tv-root qn90b uep status
samsung-tv-root qn90b uep disable
```

The transition is still volatile: a reboot restores `1`, so it must be repeated
each boot (the controller does this when `disable_native_execution_policy =
true`). It opens unsigned **native ELF** execution only; the userspace script
checks in `bash`/`node` are a separate path (§6). Other builds still need the
same per-image check: **UEP disable is per-firmware RE**, not a portable
primitive.

### Path-scoped enforcement and the bind-mount bypass (2026-11-05)

The gate is not a property of the inode or superblock; it is a check on the
**absolute path**. The verifier builds that path with `d_path()`:

- `SfdConstructAbsoluteFileNameByFile` (`0xc027e620`) →
  `SfdConstructAbsoluteFileNameByPath` (`0xc027e560`) → `d_path()`
  (`0xc01ac8f4`).
- `SfdCheckFileIsInRWUsingPath` (`0xc027e66c`) compares the result against the
  prefix table at `0xc09f46b0`, whose four entries are `/opt`, `/tmp`, `/var`,
  and `/home` (`0xc0782d24`…`0xc0782d33`).
- A path outside that list produces the "UEP disabled" result (2) and no
  signature verification.

Because `d_path()` is mount-aware, a **bind mount** changes the path the kernel
sees. A bind mount is not a symlink: `d_path()` resolves symlinks back to
`/opt/...`, but a bind mount has its own `vfsmount` and mount point.

Confirmed on the TV with the gate at `1` (UEP enabled):

```console
# unsigned static ARMv7 binary; real path under /opt
mount --bind /opt/uep-alias /run/uep-mnt
/run/uep-mnt/hello          # runs: exit 0, no UEP rejection
/opt/uep-control/hello      # rejected: exit 126, "Operation not permitted"
```

`dmesg` recorded only the control failure:

```text
[Verification fail] file /opt/uep-control/hello is not signed. Sign it.(fsname:vdfs, mnt_flags:1020)
```

The bind mount showed as `/dev/mmcblk0p21 on /run/uep-mnt type vdfs (rw,...)`.
The alias executed successfully while the identical binary at its real `/opt`
path was rejected. Cleanup removes the mount and temporary copies; the bind
mount is volatile and disappears on unmount or reboot.

Caveats:

- The alias must be visible in the **exec'ing process's mount namespace**.
  The root shell is in the init namespace (`mnt:[4026531840]`), so a bind mount
  there is visible to it; a zone/container with its own namespace would not see
  it.
- The executable's SMACK label still has to permit execution (the copies under
  `/opt` were labeled `System::Privileged`; `/run` is `System::Run`).
- This bypasses the **path scope**, not the key or the gate; UEP remains enabled
  for files actually under the four prefixes.
- The bind mount is a runtime VFS object. It does not modify the rootfs or its
  VDFS signatures; `umount` or reboot restores the original view.

---

## 9. Observability & a note on limits

What a plain root shell **can** see:

```console
cat /sys/module/uep/version         # v.0.0.1
cat /proc/cmdline                   # SELP_ENABLE=... sdp_sparsemem ...
dmesg | grep SfdUep                 # [SfdUepCheckFileType] ...
ls -l /run/security-manager.socket /run/securezone_ready
cat /proc/self/zone                 # "50 0"  (level 50, type 0)
```

A bind mount outside the four checked prefixes (e.g. `/run/...`) changes the
check result without touching the gate or the key table; see §8.

What it **cannot** see without a kernel-memory primitive:

- The **current gate value** (no sysfs/proc/ioctl surface).
- The **module's symbols** (`/proc/kallsyms` absent; module built-in).
- **Physical memory** at will (`/dev/mem` is not present).

The gate value on this TV is therefore only readable through the FDET
physical-memory primitive: via the toolkit's `qn90b uep status`, or directly as
`FdetProbe.dll rw 0x208c26c4 1` (§8).

---

## 10. Evidence & method

The 2026-11-04 investigation was **read-only** over SSH (and by extracting
`/usr/lib/libkUEPUser.so` and `/opt/etc/parseinfo.sh` for offline `strings`/`nm`
analysis). The 2026-11-05 follow-up wrote the volatile gate once, read the key
table only, and ran the bind-mount test: temporary copies under `/opt`, a bind
mount under `/run`, execution, then unmount and removal of the copies; the gate
was left at its pre-test value. Key sources:

- On-TV: `/sys/module/uep/version`, `/proc/cmdline`, `/proc/version`, `/proc/devices`,
  `/proc/misc`, `/proc/self/zone`, `dmesg`, `ls -l /run`, `/etc/prd_info.ini`,
  `/etc/device-sec-policy` (contains `kuepuser-release`), `/etc/LOG_TAG_LIST`
  (tag `kuepuser`), `systemctl`-style unit `sfpmd.service`.
- Binaries: `/usr/lib/libkUEPUser.so`, `/usr/bin/sfpmd`, `/usr/bin/bash`,
  `/usr/bin/node`, `/usr/bin/tzdaemon`, `/usr/bin/zone-template-launcher`,
  `/usr/bin/fw-update-service`, `/usr/bin/fw-usb-update-service`.
- Kernel image: raw dump of `mmcblk0p6` (uncompressed ARM `Image`), disassembled
  with Capstone; `SfdUepPacketHandler` and `SfdUepVerifyFileSignature` at the
  addresses in §8.
- Offline toolkit checkout: `~/Projects/personal/samsung-tv-root` —
  `docs/research-notes/EXPLOIT_CHAIN.md` §"UEP bypass",
  `docs/research-notes/QN90B_REPRODUCTION.md` §"UEP",
  `src/samsung_tv_root/{qn90b,compatibility}.py`.
- Kernel disassembly: `SfdConstructAbsoluteFileNameByFile` (`0xc027e620`),
  `d_path` (`0xc01ac8f4`), prefix matcher (`0xc027e3cc`), prefix table
  (`0xc09f46b0`), and the on-device bind-mount test (§8).
- Local repo: [`docs/samsung-qn90b-architecture.md`](samsung-qn90b-architecture.md) §3,
  [`dropbear/README.md`](../third_party/dropbear/README.md), [`re/qn90b-core-daemons`](../research/re/qn90b-core-daemons/README.md).

### Open questions

> *Update 2026-11-04:* items 1, 3 and parts of 5 are resolved in
> [re/at-hub-persistence-analysis.md](../research/re/at-hub-persistence-analysis.md)
> (§2.5–2.6): `#RPA` is the `#RPAC=…` anti-tamper first-line tag on ~93 image unit
> files (inert to systemd); the trailer layout is
> `[signature][base64(len)][":UEP"]`; and the bash check is a **userspace** call into
> `libkUEPUser.so` (`BSContext::verifyFileWihtoutKey`) that only verifies scripts on a
> **writable mount**, armed by the platform token `micom` in `/proc/modules`.

1. ~~Exact semantic of the kernel gate's "file type" (`#RPA`) and which
   paths/labels are exempt from the exec check.~~ **Partially resolved
   2026-11-05:** the kernel checks `d_path()` against `/opt:/tmp:/var:/home`;
   paths outside the list are exempt, and a bind mount under `/run` bypasses the
   check (§8). The `#RPA` file-type details remain open.
2. Whether the two embedded RSA keys map 1:1 to (a) firmware/update files and
   (b) scripts, or split differently.
3. The precise `_UepSigMeta` blob layout (header/type byte + signature) for the
   `##UEP…:UEP` script marker.
4. ~~The gate-word address for `T-PTMDEUC-1720.7` (needed if UEP disable is ever
   wanted on this build).~~ **Resolved 2026-11-05:** the word is at physical
   `0x208c26c4`, the same as the tested QN90B; see §8.
5. Whether the patched `bash`/`node` `uep_verify` can be told to skip via vconf or
   an environment flag ("Disable Script UEP" string in `libkUEPUser.so`).
