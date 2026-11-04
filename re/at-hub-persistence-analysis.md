# AT-HUB.service → /opt/etc/parseinfo.sh — persistence feasibility analysis

*Target: `root@192.0.2.10` · QE55S95BATXXH / S95B (PontusM, `sdp-soc`) ·
Tizen 6.5.0 · Linux 5.4.77 `armv7l` · build `T-PTMDEUC-1720.7`
(`T-PTMDEUC-REL-202602250226`) · investigated 2026-11-04, **read-only**.*

This note answers, with on-device evidence and offline RE of the pulled
binaries, whether the community route *"edit the root boot script
`/opt/etc/parseinfo.sh` that `AT-HUB.service` runs, and dodge UEP by changing
its interpreter to `gawk`"* (danielyc #17) is viable on **this exact build**.

> **Read-only constraint honoured.** No file, service, label, kernel-memory or
> `/tmp` state was changed on the TV. One attempt to confirm
> `/etc/systemd/system` is read-only used `touch`; the kernel returned
> `EROFS` and nothing was written. Everything else was `cat/ls/grep/od/dd`,
> `systemctl show/status`, `ps`, `chsmack`, `smackaccess` and reading files
> off the box for offline Ghidra analysis.

---

## 0. TL;DR / ranked conclusion

1. **The route is mechanically viable on this build.** `AT-HUB.service` is a
   stock, image-enabled root service (no `User=`) whose `ExecStart` is the
   writable, persistent script `/opt/etc/parseinfo.sh` on `/opt`
   (`mmcblk0p21`, VDFS `rw,dncs`). It is pulled in at every boot via
   `default.target → bootmode.target → bootmode-graphical.service →
   starter.target → AT-HUB.service`. **Confidence: high** that the hook runs.
2. **The script-level UEP check is a *userspace* check inside the interpreter,
   not a kernel check.** `/usr/bin/bash` (`sh` is a symlink to it) dlopens
   `/usr/lib/libkUEPUser.so` and calls its `verifyFileWihtoutKey()` for any
   *script file given as an argument*; on failure it prints
   `[uep][bash] the file is NOT signed!!` and `exit(1)`. `/usr/bin/node`
   does the same for JS. The kernel `uep` module only carries ELF checks
   (`SfdUepVerifyExecutableBinary`, `SfdUepVerifyDynamicLibrary`); there is no
   kernel script path. **Confidence: high.**
3. **`/usr/bin/gawk` performs no UEP check at all** — 0 occurrences of
   `kUEP`/`UEP`/`uep_verify`, and it does not link `libkUEPUser.so`.
   Replacing the shebang with `#!/usr/bin/gawk -f` (plus a gawk-syntax body)
   sidesteps the check. **Confidence: high** for the bypass; the only
   untested piece is the live exec (see §5/§6).
4. **SMACK is not a blocker.** The script is `_` (floor); if replaced in
   `/opt/etc` it gets `System::Shared` (the directory is `transmute=TRUE`,
   confirmed live), and the process — `System::Privileged`, the systemd
   default — has `x` on both (`smackaccess System::Privileged System::Shared x → 1`).
5. **Remaining uncertainty (must be validated live):** whether the *kernel*
   `uep` module also gates a `#!` script on the writable `/opt` mount at
   `execve` time (I argue it does not — no kernel script function exists and
   the interpreter-side library re-implements the RW-path rule itself — but
   it is not provable read-only). The §5 test isolates exactly this.

---

## 1. How systemd executes `AT-HUB.service`

### 1.1 The unit

```console
$ ssh root@192.0.2.10 'cat -n /usr/lib/systemd/system/AT-HUB.service'
     1	[Unit]
     2	After=buxton2.service systemd-udevd.service connman.service net-config.service deviced.service
     3	Description=Launch AT-HUB 
     4	
     5	[Service]
     6	ExecStart=/opt/etc/parseinfo.sh &
     7	
     8	[Install]
     9	WantedBy=starter.target
```

It is enabled by an **image-shipped symlink** (the unit itself is on the RO
rootfs):

```console
$ find /usr/lib/systemd /etc/systemd -name '*AT-HUB*'
/usr/lib/systemd/system/AT-HUB.service
/usr/lib/systemd/system/starter.target.wants/AT-HUB.service      # -> ../AT-HUB.service
```

### 1.2 What systemd actually does with the trailing `&`

systemd **does not run a shell** for `ExecStart`, and it has **no `&`
backgrounding syntax**. The `&` is parsed as an ordinary argument. This is
visible in the resolved argv systemd records:

```console
$ systemctl show AT-HUB.service -p ExecStart -p ExecStartEx -p Type -p User -p Group -p SmackProcessLabel
Type=simple
ExecStart={ path=/opt/etc/parseinfo.sh ; argv[]=/opt/etc/parseinfo.sh & ; ignore_errors=no ; ... ; pid=1666 ; code=exited ; status=0 }
ExecStartEx={ path=/opt/etc/parseinfo.sh ; argv[]=/opt/etc/parseinfo.sh & ; ... }
User=
Group=
SmackProcessLabel=
```

So the syscall systemd makes is effectively:

```
execve("/opt/etc/parseinfo.sh", ["/opt/etc/parseinfo.sh", "&"], env)
```

* `path=` is the **script itself**, not `/bin/sh` — systemd calls `execve` on
  the file, and the **kernel** performs the `#!` handling.
* The kernel `binfmt_script` handler then runs
  `execve("/bin/bash", ["/bin/bash", "/opt/etc/parseinfo.sh", "&"], env)`.
* Bash therefore treats the trailing `&` as a **positional parameter**
  (`$1`), *not* as a shell background operator. There is no sub-shell and no
  backgrounding.

Confirmed by the boot-time execution record (it really ran, as root, once,
and exited cleanly):

```console
$ systemctl status AT-HUB.service --no-pager -l
● AT-HUB.service - Launch AT-HUB
     Loaded: loaded (/usr/lib/systemd/system/AT-HUB.service; disabled; vendor preset: enabled)
     Active: inactive (dead) since Sun 2026-10-25 21:32:34 CET; 1 weeks 2 days ago
    Process: 1666 ExecStart=/opt/etc/parseinfo.sh & (code=exited, status=0/SUCCESS)
   Main PID: 1666 (code=exited, status=0/SUCCESS)
```

`ExecMainStartTimestamp=…21:32:30` → `ExecMainExitTimestamp=…21:32:34`
(≈4 s). `Type=simple`, `RemainAfterExit=no` → the unit is considered started
immediately after `execve`, and becomes `inactive (dead)` when the script
exits.

### 1.3 uid and SMACK label of the process

* **uid/gid 0/0** — no `User=`/`Group=` anywhere in the unit (`User=` and
  `Group=` are `[not set]`), and systemd was PID 1.
* **SMACK label `System::Privileged`** — PID 1 itself is `System::Privileged`,
  `AT-HUB.service` has **no** `SmackProcessLabel=`, and the script has **no**
  `SMACK64EXEC` xattr, so the child simply inherits the manager's label. The
  pattern is confirmed against sibling root services:

```console
$ ps -e -o pid,ppid,label,comm | grep -E '^ *1 |systemd-udevd'
    1     0 System::Privileged              systemd
  887     1 System::Privileged              systemd-udevd

$ ps -eZ | grep -E 'deviced|storaged|sfpmd|fconfigd|buxton2d'
System::Privileged   671 ?  deviced          # no SMACK64EXEC -> inherits mgr label
System::Privileged  1712 ?  storaged
System::Privileged  2813 ?  sfpmd
System             273 ?  fconfigd          # has SMACK64EXEC=System
System             300 ?  buxton2d_worker
```

```console
$ chsmack /usr/bin/deviced  -> access="_"
$ chsmack /usr/bin/fconfigd -> access="_" execute="System"
$ chsmack /usr/bin/sfpmd    -> access="_"
$ chsmack /opt/etc/parseinfo.sh -> access="_"        # no execute= (SMACK64EXEC) label
```

The boot chain that reaches it (all on the RO rootfs):

```
/usr/lib/systemd/system/default.target -> bootmode.target            (symlink)
bootmode.target   Requires=basic.target  Wants=pkgmgr-info.service
                     bootmode-graphical.service bootmode-swu-standalone.service
                     bootmode-system-update.service
bootmode-graphical.service
     ConditionPathExists=!/system_update   ConditionPathExists=!/run/.standalone
     ExecStart=/usr/bin/systemctl start starter.target
starter.target    Requires=basic.target   Alias=default.target
starter.target.wants/AT-HUB.service -> ../AT-HUB.service
```

Raw evidence:

```console
$ systemctl get-default
bootmode.target
$ ls -l /usr/lib/systemd/system/default.target
default.target -> bootmode.target
$ systemctl show bootmode-graphical.service -p WantedBy -p ExecStart
WantedBy=bootmode.target
ExecStart={ path=/usr/bin/systemctl ; argv[]=/usr/bin/systemctl start starter.target ; ... status=0 }
$ systemctl list-dependencies starter.target | head -4
starter.target
● ├─allshare-svc.service
● ├─app-control-service.service
● ├─AT-HUB.service
```

---

## 2. Is the script UEP check in the interpreter (not the kernel)?

### 2.1 The pieces

```console
$ for f in /usr/bin/bash /usr/bin/gawk /usr/bin/node /usr/bin/sh /usr/bin/awk; do
    printf "%s kUEP=%s uep_verify=%s\n" "$f" "$(grep -a -c kUEP $f)" "$(grep -a -c uep_verify $f)"; done
/usr/bin/bash  kUEP=1 uep_verify=1
/usr/bin/gawk  kUEP=0 uep_verify=0
/usr/bin/node  kUEP=1 uep_verify=1
/usr/bin/sh    kUEP=1 uep_verify=1     # symlink -> bash
/usr/bin/awk   kUEP=0 uep_verify=0     # symlink -> gawk
```

```console
$ tr -c '[:print:]' '\n' < /usr/bin/bash | grep -a uep
uep_verify
/usr/lib/libkUEPUser.so
[1;31m[uep][bash] the file is NOT signed!! : %s

$ ldd /usr/bin/bash        # libkUEPUser is NOT a DT_NEEDED; it is dlopen()'d
	libdl.so.2 => /lib/libdl.so.2
	libgcc_s.so.1 => /lib/libgcc_s.so.1
	libc.so.6 => /lib/libc.so.6

$ ldd /usr/bin/gawk        # no libkUEPUser, no libdl even
	libdl.so.2 libm.so.6 libgcc_s.so.1 libc.so.6
```

`/usr/bin/node` has `_ZN4node10uep_verifyEPKc` and
`[uep][nodejs] the file is NOT signed!!`, so JS scripts are checked too.
`gawk` has nothing.

### 2.2 `bash`'s `uep_verify` wrapper

Decompiled offline with Ghidra 12.2 headless (binary pulled read-only):

```c
undefined4 uep_verify(undefined4 path /* param_1 */) {
  /* lazy dlopen("/usr/lib/libkUEPUser.so", RTLD_LAZY) */
  h = dlopen("/usr/lib/libkUEPUser.so", 1);
  if (h == 0) return 1;                       // library missing -> ALLOW
  fn = dlsym(h, "<symbol>");
  if (dlerror()) { dlclose(h); return 1; }    // symbol missing -> ALLOW
  return fn(<const label>, path, <const>);    // -> BSContext::verifyFileWihtoutKey()
}
```

(In the ARM build the three register arguments are `r0=<label const>`,
`r1=path`, `r2=<const>`; the library entry point is the exported C symbol
`verifyFileWihtoutKey`.)

### 2.3 `bash main()` — when the check is invoked

In `main()` (the script-file execution path, i.e. `bash <file> [args]`, which
is exactly what a `#!/bin/bash` shebang produces):

```c
pcVar9 = local_118[local_f0];               // the script filename argument
pcVar8 = xmalloc(strlen(pcVar9)+1); strcpy(pcVar8, pcVar9);
iVar4 = uep_verify(pcVar8);
if (iVar4 == 0) {
    __fprintf_chk(stderr, 1,
        "[1;31m[uep][bash] the file is NOT signed!! : %s", pcVar8);
    exit(1);                                 // hard fail
}
```

So **return 0 = deny**, non-zero = allow. Sourcing (`source file` / `. file`)
reads and parses the file directly and never calls `uep_verify` — exactly the
work-around recorded in `dropbear/README.md`:

```text
Use `source`, **not**:  bash /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
The latter trips Samsung's script-signature check on this firmware:
[uep][bash] the file is NOT signed!!
```

### 2.4 `libkUEPUser` — the path rule and what gets verified

Exported symbols / strings (read-only extraction):

```text
verifyFileWihtoutKey            BSContext::checkFileIsInRWUser(char const*, char*)
BSContext::verifyFile(...)      BSContext::verifyFileWithDeviceId(...)
Signature::loadFromFile / loadSignatureContent / getSignatureInfo
Signature::checkSignatureFootprint / composeSignature
VerifyContext::check/checkSignature/checkMPIERK (OpenSSL EVP_sha256, EVP_VerifyFinal)
/proc/modules      /proc/self/zone      db/comss/hwduid      VDUEPOFF.cer
[1;35mDisable Script UEP
[1;31mNot Signed [ file_path : %s ]   [1;32mCorrect Signed [ file_path : %s ]
```

Decompiled `BSContext::verifyFileWihtoutKey(path, label)` (Ghidra, cleaned):

```c
int verifyFileWihtoutKey(char *path, char *label) {
  BSContext ctx;
  if (access(path, F_OK) != 0) return 0;                    // deny
  char real[4100];
  if (BSContext::checkFileIsInRWUser(ctx, path, real) == 0) {   // file IS on a RW mount
      FILE *f = fopen64("/proc/modules", "r");              // (verified literal)
      if (f) {                                              // search every 5-char window
          while (fgets(buf, 6, f))                          // for the token "micom"
              if (strstr(buf, "micom")) goto enforce;
          return 1;                                         // token absent -> ALLOW
      }
      return 1;                                             // /proc/modules absent -> ALLOW
   enforce:
      if (FindUEPDiableFile()) { ... verifyFileWithDeviceId(ctx, <DUID path>, label) ... }
      if (isRootUID()==0 && isIsolatedContainer()) return 1; // zone exemption
      if (verifyFile(ctx, real, label) == 5) return 1;      // 5 == Correct Signed -> ALLOW
      SendRequestNotification(ctx, real, label, rc);
      return 0;                                             // deny
  }
  return 1;                                                 // NOT on a RW mount -> ALLOW
}
```

`checkFileIsInRWUser()` does `realpath(path,out)` then
`statvfs64(out,&st)` and returns the sign of `st.f_flag & ST_RDONLY`
(`(f_flag<<31)>>31` → `0` for a writable mount, `-1` for a read-only one).
Hence **only files that live on a writable mount are UEP-script-verified**;
anything on the RO rootfs is allowed unconditionally.

Two consequences the evidence settles:

* **The interpreter, not the kernel, decides.** If a `#!`-script check were a
  kernel `execve` policy, this library would not need to re-implement the
  RW-mount rule in userspace.
* The verification target is the **`realpath()`-resolved** file, so a
  symlink into the RO rootfs would be treated as RO (a potential, but
  payload-less, bypass).

The `"micom"` token resolves the on-device "does the check fire?" question:
the search file is `/proc/modules` and the token is the platform marker
`micom`. On this TV it **is present**:

```console
$ grep -i micom /proc/modules
tztv_micom 69632 57 tztv_hdmi_switch,tztv_hdmi_switch_api,system_drv, Live 0x00000000 (O)
```

→ on the S95B (a `micom` platform) the RW-script UEP verification **is
enforced**. (On a chassis without `tztv_micom` loaded the check would
short-circuit to allow — that is presumably why the bypass is
build/platform-dependent.)

### 2.5 How the `##UEP…:UEP` trailer is parsed

From the same library (Ghidra):

```c
/* Signature::loadFromFile -> getSignatureInfo(offset) + checkSignatureFootprint + loadSignatureContent */
readFromFile(this, f, end_off, buf, 4);          // checkSignatureFootprint
bool ok = (buf[0]==':' && buf[1]=='U' && buf[2]=='E' && buf[3]=='P');   // trailer ends ":UEP"
...
readFromFile(this, f, off, eight, 8);            // loadSignatureContent
base64_decode(eight, 8, &len, &n);               // 8 base64 chars -> 4-byte big-endian length
readFromFile(this, f, off - len, sig, len);      // the signature blob
```

and the writer counterpart:

```c
/* Signature::composeSignature */
append(vector, sig);                                   // signature bytes
base64_encode(&len_be, 4, tmp, 8); append(vector, tmp); // base64(length)
append(vector, 0x5045553a);                            // ":UEP"
```

So the trailing marker is `[signature][base64(length)][":UEP"]`; the script
file carries it as the last line `##UEP<base64>:UEP` (the stock script's last
line is exactly 533 chars: `##UEP` + 524 base64 + `:UEP`, matching the doc).
The RSA public keys are the two embedded `SubjectPublicKeyInfo` PEM blocks in
`libkUEPUser.so`, selected by the metadata type byte (`'L'`/`'P'`) via
`SelectPubKey`.

**Editing the body of a signed `/opt` script invalidates the signature**, so
the stock `#!/bin/bash` shebang cannot be kept.

### 2.6 Kernel side — no script gate

Strings in the kernel image (`dd if=/dev/mmcblk0p6 …`), symbol set:

```text
SfdUepCheckFileType   SfdUepCheckFileSignature   SfdUepVerifyFileSignature
SfdUepReadFile        SfdUepReadSignatureFromFile
SfdUepVerifyExecutableBinary   SfdUepVerifyDynamicLibrary
SfdUepHandleVerificationResult SfdUepPacketHandler  SfdOpenUepContext/SfdCloseUepContext
UEP has been disabled      [%s] Blocked! cur pid:… kuep:%c … sig:%d
[%s] Not supported UEP version(%c)
```

There is **no script-related kernel function**. Also, unsigned executables
run unhindered on this box: the running dropbear is our own unsigned static
build —

```console
$ sha256sum /run/tv-sshd/dropbearmulti
4ef0d338a255b8015db375890c2a0217a5d3b8b092335b92e9be8a7f7e58b536  /run/tv-sshd/dropbearmulti
```

(matching the repo's unsigned `dropbear/out/dropbear`), and `/usr/bin/bash`
and `/usr/bin/gawk` — both on the RO rootfs, neither carrying a `##UEP`
trailer — execute normally. `grep -c '##UEP' /usr/bin/bash /usr/bin/gawk` = 0
for both.

The only UEP dmesg line on the box is tangential:

```text
[85112.828875] [SfdUepCheckFileType] unknown file type(#RPA)
```

`#RPA` is the 4-byte magic of the `#RPAC=…` anti-tamper comment that 93 of
372 image unit files carry as their first line (e.g.
`/usr/lib/systemd/system/app-boot-manager.service`). It has nothing to do
with `parseinfo.sh` (which starts `[Unit` / `#!/bin/bash`) and is inert for
systemd.

---

## 3. Boot-hook candidates: root `ExecStart` into a writable path

`UnitPath=/etc/systemd/system /run/systemd/system /run/systemd/generator
/usr/lib/systemd/system /run/systemd/generator.late`. The only writable
storage on the box is `/opt` (+`/var`,`/home`), `/mnt/systemrw`, `/mnt/drm`
and tmpfs; `/etc/systemd/system` is on the RO rootfs:

```console
$ ls -ld /etc/systemd/system
drwxr-xr-x 1 root root 7 /etc/systemd/system         # touch -> Read-only file system
```

Full sweep (all unit dirs) of services whose `ExecStart*` names a writable
path, with their `User=`:

| Unit | User= | ExecStart (writable path) | Root? | Reaches boot? |
|---|---|---|---|---|
| **`AT-HUB.service`** | *(none)* | `/opt/etc/parseinfo.sh &` | **root** | **yes** (`starter.target.wants`) |
| `iot-hub-control-setup.service` | *(none)* | `/bin/sh -c '… /opt/usr/apps/com.samsung.tv.iot-hub-control/bin/{matter-ip-forward,hub-init}.sh …'` | root | yes (multi-user + wanted-by starter) |
| `slive-provider-daemon.service` | owner | `/opt/usr/apps/com.samsung.tv.slive-daemon/bin/slive-provider-daemon` | no | yes |
| `adagent-service.service` | owner | `/opt/usr/apps/com.samsung.tv.pisa-service/bin/adagent-service` | no | basic.target |
| `pisa-service.service` | owner | `/opt/usr/apps/com.samsung.tv.pisa-service/bin/pisa-service` | no | multi-user |
| `canalysis-service.service` | owner | `/opt/usr/apps/com.samsung.tv.canalysis-service/bin/canalysis-daemon` | no | multi-user/PNP-starter |
| `com.samsung.tv.context-aware-agent.service` | owner | `…/context-aware-agent` | no | multi-user |
| `com.samsung.tv.context-aware-service.service` | owner | `…/context-aware-service` | no | multi-user |
| `com.samsung.tv.airplay-service-2.service` | owner | `…/airplay-service-2` | no | graphical |
| `com.samsung.tv.googlecast-service.service` | owner | `…/cast_cell_service` | no | graphical |
| `samsunghealth-service-tv-dbus-activation.service` | owner | `…/samsunghealth-service-tv-dbus-activation` | no | — |
| `bms.service` | owner | `…/bms-service` (via `/bin/sh -c`) | no | multi-user |
| `wpa_supplicant.service` | *(none)* | `/usr/sbin/wpa_supplicant … -F /opt/var/log/wpa_supplicant.log` | root | sysinit (log path only) |
| `igmp_service.service` | *(none)* | `/bin/sh -c 'if test -f /opt/data/iptv_igmp_v2 …'` | root | sysinit (no exec from writable) |
| `com.samsung.tv.alexa-setcap.service` | *(none)* | `/bin/sh -c '… setcap … /opt/usr/apps/…/bixbycapsuleviewer'` | root | multi-user (setcap only) |
| `com.samsung.tv.wifi-motion-mgr-setcap.service` | *(none)* | `… setcap … /opt/usr/apps/…` | root | — |
| `display-manager.service` | *(none)* | `/bin/sh -c '/usr/bin/awaiter … /run/.wm_ready'` | root | graphical |
| `tizen-generate-env.service` | *(none)* | `/bin/sh -c '… ln -sf /opt/tizen-mobile-ui-sh /run/tizen-mobile-ui'` | root | basic (symlink only) |
| `org.tizen.remotepc.service` | root | `/usr/bin/zone-template-launcher …` (fixed paths) | root | multi-user |
| `playready_hdssmack.service` | root | `chsmack …` (fixed) | root | multi-user |
| `svc_db_backup.service` | owner | `/bin/sh -c '… sqlite3 /opt/dbspace/…'` | no | — |
| `system-*-target-done.service` | system_fw | `touch /run/...` | no | — |

**Ranked boot-hook candidates (root + executes a *file* from writable storage):**

1. **`AT-HUB.service` → `/opt/etc/parseinfo.sh`** — the only clean match.
   Stock-enabled at boot, root, no `User=`, file is a text script on the
   writable `/opt` VDFS, currently UEP-signed (so the *signed* check is
   satisfied today; it just has to be replaced in a way bash won't check).
2. `iot-hub-control-setup.service` — root, `WantedBy=starter.target`, and it
   *would* run `/opt/usr/apps/com.samsung.tv.iot-hub-control/bin/*.sh`
   (which bash/shebang would UEP-check as RW scripts). **On this unit those
   files do not exist** (`ls` → `No such file or directory`), so it falls
   back to the RO `/usr/bin/iot-hub/*.sh`; it is also gated on
   `/tmp/matter_setup`, absent. Not usable as-is, but the same class.
3. The `User=owner` app services execute **Samsung-signed ELF** payloads on
   `/opt`; they are not root and would trip the kernel ELF/`UEP` and SMACK
   paths. (`/opt/usr/share/systemd/system/` is an empty dir used only for
   `ConditionPathExists` debloat markers.)

---

## 4. SMACK assessment

```console
$ chsmack /opt                  -> access="_"
$ chsmack /opt/etc              -> access="System::Shared" transmute="TRUE"
$ chsmack /opt/etc/parseinfo.sh -> access="_"

$ ls -la /opt/etc      # world-writable dir, 0777 file, root:root
drwxrwxrwx 1 root system_share 25 ... .
-rwxrwxrwx 1 root root 805 ... parseinfo.sh

$ cat /proc/mounts | grep opt
/dev/mmcblk0p21 /opt vdfs rw,relatime,dncs 0 0
```

**Transmute is real and observable.** Files created at runtime in `/opt/etc`
take the directory's label:

```console
$ chsmack /opt/etc/resolv.conf      -> access="System::Shared"   # written 2026-11-04
$ chsmack /opt/etc/localtime        -> access="System::Shared"
$ chsmack /opt/etc/parseinfo.sh     -> access="_"                 # shipped in image
$ chsmack /opt/etc/dotnet_flag.ini  -> access="_"                 # rewritten in place
```

So:

* **Edit in place** (`cp`/`dd`/`sed -i` on the existing inode) → file keeps
  `_` (floor).
* **Replace** (`mv`/`rm`+create) → new inode inherits `System::Shared`.

Either way the exec works, because the process is `System::Privileged`:

```console
$ smackaccess System::Privileged System::Shared x   -> 1
$ smackaccess System::Privileged _ x                -> 1
$ grep -E '^System::Privileged (System::Shared|_) ' /sys/fs/smackfs/load2
System::Privileged System::Shared rwxat
System::Privileged _ rwxa
```

Note our SSH shell runs as SMACK `User`; `User → System::Shared` is only
`r-x--l` (no write), so a naive create in `/opt/etc` would be denied for a
`User`-labelled process — but our root shell holds full capabilities
(`CapEff=0x…3fffffffff`, incl. `CAP_MAC_OVERRIDE`/`CAP_MAC_ADMIN`), which
bypasses the decision. For a systemd-run payload this is moot (it is
`System::Privileged`).

**SMACK is therefore not a blocker for the AT-HUB route.**

---

## 5. Minimal, reversible test plan (NOT run — describe only)

Goal: prove (a) an edited `/opt/etc/parseinfo.sh` under `#!/bin/bash` is
*denied* by the interpreter UEP check, (b) the same body under
`#!/usr/bin/gawk -f` *runs*, and (c) the kernel does not gate the `#!` exec
itself. All steps are mutations and must be an explicit later decision.

**Step 0 — capture the pristine state (read-only, safe now).**

```sh
sha256sum /opt/etc/parseinfo.sh        # c6369e7…c71d61  (805 bytes, 7 lines)
cp -p /opt/etc/parseinfo.sh /opt/etc/parseinfo.sh.orig   # backup ON /opt (persistent)
# also keep an off-box copy in the repo for belt-and-braces:
#   ssh root@TV 'cat /opt/etc/parseinfo.sh' > re/qn90b-core-daemons/rootfs/opt/etc/parseinfo.sh.orig
```

**Step 1 — establish the baseline denial (bash, unsigned).** Rewrite the body
in place, keeping `#!/bin/bash`, adding a one-line marker write and a unique
`/proc/target_version` value:

```sh
cat > /opt/etc/parseinfo.sh <<'EOF'
#!/bin/bash
echo "at-hub-bash-$(date +%s)" >> /opt/et/at-hub-test.log 2>/dev/null || true
echo "at-hub-bash-$(date +%s)" > /opt/at-hub-test.log
echo "__BASHTEST__" > /proc/target_version
EOF
# (edit in place to preserve the "_" label: `cat > file` re-opens the same inode)
```

Reboot, then observe:

```sh
cat /proc/target_version            # expect the OLD/blank value (script was denied)
ls -l /opt/at-hub-test.log 2>&1     # expect "No such file"
systemctl show AT-HUB.service -p ExecMainStatus -p Result   # expect status=1 / failure
# UEP denial is logged to the dlog ring by libkUEPUser (__dlog_print); the
# unit's stdout/stderr are /dev/null, so use dlog if available:
timeout 5 dlogutil -b main -v uep,kuepuser -d | tail
dmesg | grep -i SfdUep
```

**Step 2 — the gawk rewrite.** Same body, new interpreter; BEGIN-only program
so the kernel-supplied trailing `&` (argv[1]) is never used as an input file:

```sh
cat > /opt/etc/parseinfo.sh <<'EOF'
#!/usr/bin/gawk -f
BEGIN {
    repo = ""; model = "";
    while ((getline line < "/etc/prd_info.ini") > 0) {
        if (line ~ /^Build=/) { split(line,a,";"); split(a[1],b,"="); repo=b[2]; }
        if (line ~ /^Model=/) { split(line,a,";"); split(a[1],b,"="); model=b[2]; }
    }
    close("/etc/prd_info.ini");
    printf "__%s__%s\n", repo, model > "/proc/target_version";
    close("/proc/target_version");
    system("echo at-hub-gawk-$(date +%s) > /opt/at-hub-test.log");
    # (payload goes here; keep the process alive if a long-lived child is wanted, see §6.4)
}
EOF
```

Reboot, then observe:

```sh
cat /proc/target_version            # expect "__TIZEN-TRUNK2022-PontusM-RELEASE_20260225.1__T-PTMDEUC"
ls -l /opt/at-hub-test.log          # expect the marker -> gawk path ran
ps -eZ | grep -i gawk               # label should be System::Privileged, uid 0
dmesg | grep -iE 'SfdUep|Blocked'   # expect nothing new
```

Interpretation:

* marker appears → **kernel does not gate the `#!` exec; the gawk bypass works.
  Route confirmed viable**;
* marker absent *and* `dmesg` shows a UEP `Blocked!`/`unknown file type` for
  the script → the kernel *does* gate RW scripts; this route fails and the
  fallback (below) applies.

**Step 3 — rollback (no reboot needed if you can still reach a shell; else
reboot and use Dev-Mode `sdb`):**

```sh
cp -p /opt/etc/parseinfo.sh.orig /opt/etc/parseinfo.sh   # restores 805-byte signed file
sync
# optional immediate effect: systemctl restart AT-HUB.service (mutation)
# verify: sha256sum /opt/etc/parseinfo.sh == c6369e7…c71d61
```

The service can also be re-run without a full reboot via
`systemctl restart AT-HUB.service` to observe results quickly.

**Brick risk / recovery.** `AT-HUB.service` is **not** on the critical boot
path: nothing `Requires=` it, `Type=simple` marks it started at `execve`, and
it only writes `/proc/target_version`. A malformed script therefore fails the
unit (status ≠ 0) without stalling boot (worst case `TimeoutStartUSec=1m30s`,
and even that applies only to the start job). `starter.target` never waits on
it. If the TV ever does fail to reach a usable state, recovery is via the
**existing sdb/Developer-Mode channel** (the same one that got root here) to
restore `/opt/etc/parseinfo.sh.orig`. Keep the off-box copy *before*
touching anything.

---

## 6. Blockers and caveats

1. **Kernel ELF UEP gate vs. the interpreter.** The kernel module gates ELF
   exec (`SfdUepVerifyExecutableBinary`, `SfdUepVerifyDynamicLibrary`) but has
   **no script path**. `/usr/bin/gawk` and `/usr/bin/bash` are unsigned, on
   the RO rootfs, carry no `##UEP`, and run today. Even an unsigned static
   ELF (our dropbear) runs from `/run`. So the kernel gate does not stop the
   interpreter. *The one thing not provable read-only is whether the kernel
   also pre-checks a `#!` script on the writable `/opt` mount at `execve`;
   §5 Step 2 decides it.* If it does, the gawk rewrite fails closed and the
   fallback routes are the .NET-IL boot path (`re/dotnet-*`) or a
   startup-slot TPK.
2. **IMA is not a factor.** `/sys/kernel/security/` exposes only `integrity`
   and `lsm`; there is **no** `/sys/kernel/security/ima`, no `ima_policy=` on
   the cmdline and no IMA dmesg. systemd's `+IMA` build flag is not backed by
   an active policy here.
3. **No persistence through systemd itself.** `/etc/systemd/system` is RO
   (`EROFS`), the rest of `UnitPath` is tmpfs. Units can only be *disabled*
   via the writable `ConditionPathExists` markers in
   `/opt/usr/share/systemd/system/` (empty on this unit) — that is debloat,
   not code execution.
4. **cgroup / lifetime — matters for a long-lived daemon.** `KillMode=control-group`
   (default) means systemd tears down the whole cgroup when the `ExecStart`
   process exits (`RemainAfterExit=no`, `Type=simple`). A gawk program that
   forks an sshd and exits would have its child killed. To keep a process
   alive you must keep the **main** `ExecStart` process alive (e.g. a
   `BEGIN { while (1) { system("…"); system("sleep 30") } }` loop), or have
   the boot hook do a one-shot action (write a file, start a unit, etc.)
   rather than launch a daemon. The stock script just exits after ~4 s.
5. **Observability is limited.** `StandardOutput=null`,
   `StandardError=null` (from `systemctl show`), so bash's
   `[uep][bash] the file is NOT signed!!` never reaches a log through
   systemd. `libkUEPUser` also logs/sends a report via `libdlog`
   (`__dlog_print`) and to `sfpmd` over a UNIX socket, so the dlog ring /
   `/dev/log_main` is the place to look; `/var/log/journal` holds no journal
   files on this unit and `/var/log/dlog` is empty. The most reliable signal
   is a **side-effect marker file** written by the script (as in §5), or the
   `/proc/target_version` value.
6. **The `&` is harmless but must stay harmless.** With bash it is `$1`; with
   gawk it becomes `ARGV[1] = "&"`. A BEGIN-only gawk program (no main
   pattern rules) never opens it. Do **not** write a gawk program with a
   main rule, or it will try to open a file named `&`.
7. **`#RPA` noise.** The recurring `[SfdUepCheckFileType] unknown file type(#RPA)`
   corresponds to the `#RPAC=…` first-line tag on ~93 image unit files and is
   unrelated to this route.

---

## 7. Evidence & method

Everything below was gathered **read-only** over SSH from `root@192.0.2.10`
and by pulling a few files off the box for offline analysis (no on-TV
mutation). Key commands:

* **systemd:** `cat /usr/lib/systemd/system/AT-HUB.service`,
  `systemctl show/status AT-HUB.service` (`ExecStart`, `ExecStartEx`, `User`,
  `SmackProcessLabel`, `KillMode`, `StandardOutput/Error`,
  `ExecMain*Timestamp`), `systemctl get-default`, `list-dependencies
  starter.target`, `cat bootmode.target / bootmode-graphical.service /
  starter.target`, `find /usr/lib/systemd -name '*AT-HUB*'`, full sweep of
  `*.service` for `ExecStart` → `/opt|/home|/mnt|/var|/run`.
* **Process/uid/SMACK:** `ps -eZ`, `ps -e -o pid,ppid,label,comm`,
  `chsmack {/opt,/opt/etc,/opt/etc/parseinfo.sh,/usr/bin/{bash,gawk,node,deviced,fconfigd,sfpmd}}`,
  `smackaccess …`, `grep -E '^System::Privileged ' /sys/fs/smackfs/load2`.
* **UEP:** `grep -a -c kUEP/uep_verify` on the interpreters, `ldd`,
  `tr -c '[:print:]' '\n'`, `cat /sys/module/uep/version`,
  `dmesg | grep -i SfdUep`, kernel strings from `dd if=/dev/mmcblk0p6`,
  `grep -i micom /proc/modules`.
* **Offline RE:** `/usr/lib/libkUEPUser.so`, `/usr/bin/bash`,
  `/usr/bin/gawk` pulled with `ssh … cat` and decompiled with **Ghidra 12.2
  headless** (`re/qn90b-core-daemons/notes/ghidra-decompile.sh` +
  throwaway `DumpCallers`/`DumpByString`/`DumpListing` post-scripts, since
  removed). Functions examined: bash `uep_verify` and its caller `main`;
  libkUEPUser `BSContext::verifyFileWihtoutKey`,
  `BSContext::checkFileIsInRWUser`, `BSContext::verifyFile*`,
  `Signature::loadSignatureContent/getSignatureInfo/checkSignatureFootprint/composeSignature`,
  `FindUEPDiableFile`.
* **Script:** `sha256sum`, `wc -c`, `wc -l`, `od -c`, `tail -c`, byte-exact
  comparison with the repo copy at
  `re/qn90b-core-daemons/rootfs/opt/etc/parseinfo.sh`
  (both `c6369e7dec69408e176daa252d436acf6f8e265f560504d8bd831c8023c71d61`,
  805 bytes, 7 lines).

### Cross-references

* [`docs/samsung-tv-secure-boot.md`](../../docs/samsung-tv-secure-boot.md) —
  boot/secure-boot chain, §6.2/§6.6 (this route, ranked #1).
* [`docs/samsung-tv-uep.md`](../../docs/samsung-tv-uep.md) — UEP components;
  this note resolves its open questions #1 (`#RPA`), #4 (script marker) and
  partly #5 ("Disable Script UEP").
* [`docs/samsung-tv-smack-policies.md`](../../docs/samsung-tv-smack-policies.md) —
  labels, transmute, live policy.
* [`re/qn90b-core-daemons/notes/parseinfo.sh.md`](parseinfo.sh.md) and
  [`re/qn90b-core-daemons/README.md`](README.md) — the original persistence lead.
* [`docs/samsung-tv-root-ecosystem.md`](../../docs/samsung-tv-root-ecosystem.md)
  §5 — danielyc #17 (AT-HUB/gawk), the empirical basis of the bypass.

### Remaining uncertainties (ranked)

1. **Does the kernel gate a `#!` script on a RW mount at `execve`?**
   (Decisive for the gawk route; §5 Step 2.) Argument it does not: no kernel
   script function exists, and the RW-path rule is implemented in the
   userspace library (`checkFileIsInRWUser`), which would be redundant if the
   kernel already did it.
2. Will the `micom` token check actually evaluate true at the exact moment
   the first `/opt` script runs? (`tztv_micom` is built-in-as-module here and
   present in `/proc/modules` now, but module-load order at early boot is not
   proven.) Even if false, the result is *allow*, i.e. the route still works.
3. Exact `_UepSigMeta` byte layout (type byte `'L'`/`'P'` + length) — enough
   to know how the trailer is validated, not needed to bypass it.
4. Why `SfdUepCheckFileType` sees `#RPA`-tagged unit files at all (dmesg
   noise; not on this route).
