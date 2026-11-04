# Samsung UEP on the TV — Setup & Capabilities

*Target: `root@192.0.2.10` · Tizen 6.5.0 · build `T-PTMDEUC-REL-202602250226`
(QE55S95BATXXH / S95B, PontusM) · investigated 2026-11-04 (read-only).*

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
kernel-memory primitive — but on **this** exact build that path is deliberately
refused (see §8).

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
- For this PontusM family the toolkit's RE places that word at **virtual
  `0xc08c26c4` / physical `0x208c26c4`,** and describes the transition as a
  compare-before-write (verify surrounding state, write, verify read-back).
  See §8 for the caveat that applies to this exact build.

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

**Important caveat for this exact TV.** The fixed UEP address is gated behind the
toolkit's `require_tested()` check, which only passes on the *tested* QN90B build
`T-PTMAKUC-REL-202310071804`. Our build
`T-PTMDEUC-REL-202602250226` (S95B / PontusM) is only *compatible* — it is
accepted for the FDET root route, but `uep disable` **fails closed** with
*"…uses build-specific addresses and requires the tested builds …"*. In other
words, on this unit the toolkit will not flip the gate until the address is
re-verified for this build. (See `compatibility.py` next to
`QN90B_REPRODUCTION.md` §UEP in the toolkit checkout.)

This is consistent with the general rule: **UEP disable is per-firmware RE**, not
a portable primitive. Any future work here — including the `AT-HUB.service` →
`/opt/etc/parseinfo.sh` boot-persistence lead noted in
[`re/qn90b-core-daemons`](../re/qn90b-core-daemons/README.md) — must re-derive the
gate address for `T-PTMDEUC-1720.7` before relying on it.

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

What it **cannot** see without a kernel-memory primitive:

- The **current gate value** (no sysfs/proc/ioctl surface).
- The **module's symbols** (`/proc/kallsyms` absent; module built-in).
- **Physical memory** at will (`/dev/mem` is not present).

The gate value on this TV is therefore only readable via the toolkit's
`qn90b uep status`, which on this build is refused (§8); otherwise it would need
the same address re-derived.

---

## 10. Evidence & method

All facts above were gathered **read-only** over SSH (and by extracting
`/usr/lib/libkUEPUser.so` and `/opt/etc/parseinfo.sh` for offline `strings`/`nm`
analysis). No files were changed on the TV. Key sources:

- On-TV: `/sys/module/uep/version`, `/proc/cmdline`, `/proc/version`, `/proc/devices`,
  `/proc/misc`, `/proc/self/zone`, `dmesg`, `ls -l /run`, `/etc/prd_info.ini`,
  `/etc/device-sec-policy` (contains `kuepuser-release`), `/etc/LOG_TAG_LIST`
  (tag `kuepuser`), `systemctl`-style unit `sfpmd.service`.
- Binaries: `/usr/lib/libkUEPUser.so`, `/usr/bin/sfpmd`, `/usr/bin/bash`,
  `/usr/bin/node`, `/usr/bin/tzdaemon`, `/usr/bin/zone-template-launcher`,
  `/usr/bin/fw-update-service`, `/usr/bin/fw-usb-update-service`.
- Offline toolkit checkout: `~/Projects/personal/samsung-tv-root` —
  `docs/research-notes/EXPLOIT_CHAIN.md` §"UEP bypass",
  `docs/research-notes/QN90B_REPRODUCTION.md` §"UEP",
  `src/samsung_tv_root/{qn90b,compatibility}.py`.
- Local repo: [`docs/samsung-qn90b-architecture.md`](samsung-qn90b-architecture.md) §3,
  [`dropbear/README.md`](../dropbear/README.md), [`re/qn90b-core-daemons`](../re/qn90b-core-daemons/README.md).

### Open questions

1. Exact semantic of the kernel gate's "file type" (`#RPA`) and which paths/labels
   are exempt from the exec check.
2. Whether the two embedded RSA keys map 1:1 to (a) firmware/update files and
   (b) scripts, or split differently.
3. The precise `_UepSigMeta` blob layout (header/type byte + signature) for the
   `##UEP…:UEP` script marker.
4. The gate-word address for `T-PTMDEUC-1720.7` (needed if UEP disable is ever
   wanted on this build).
5. Whether the patched `bash`/`node` `uep_verify` can be told to skip via vconf or
   an environment flag ("Disable Script UEP" string in `libkUEPUser.so`).
