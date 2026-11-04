# Dropbear SSH on the Samsung TV

Tested on the TV at `192.0.2.10`: Tizen 6.5.0, Linux 5.4.77, `armv7l`.
The server is **dropbear 2026.94**, built as a statically linked ARMv7
multi-call binary. Root login uses public-key authentication only.

This document covers only the SSH server, its build, launcher, configuration,
and runtime layout. Acquiring root or arranging for a command to run after a TV
boot is intentionally out of scope; the launcher must be run from an existing
root shell after every power cycle.

## Repository contents

| File | Purpose |
| --- | --- |
| `build-dropbear.sh` | Reproducible Docker/QEMU ARMv7 static build. |
| `install-dropbear.sh` | Pushes the binary and launcher to the TV with `sdb`. |
| `tv-start-sshd.sh` | Idempotent on-TV launcher; contains the authorized public key. |
| `authorized_keys` | Reference copy of the configured public key. |
| `out/dropbear` | Generated binary, intentionally git-ignored. |

No prebuilt binary or private key is stored here.

## TV filesystem layout

### Persistent staging

```text
/home/owner/share/tmp/sdk_tools/dropbear
/home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
```

`/home` resolves into `/opt/usr/home`, so this is on the writable `/opt`
userdata partition. These files survive reboots unless userdata is erased.
The older optional migration source below may also exist:

```text
/home/owner/share/tmp/sdk_tools/dropbear_ed25519_host_key
```

### Persistent SSH state

```text
/opt/tv-sshd/authorized_keys
/opt/tv-sshd/dropbear_ed25519_host_key
```

The Ed25519 **server host key** is generated once on the TV and reused. This
prevents a new SSH host identity after every restart. The client private key is
not stored on the TV or in this repository.

### Volatile runtime

```text
/run/tv-sshd/dropbearmulti
/run/tv-sshd/dropbear       -> /run/tv-sshd/dropbearmulti
/run/tv-sshd/dropbearkey    -> /run/tv-sshd/dropbearmulti
/run/tv-sshd/dropbear.pid
/run/tv-sshd/dropbear.log
```

`/run` is tmpfs and disappears at power-off. The launcher recreates it. An old
`/run/tv-sshd/authorized_keys` may remain from initial testing; it is **not**
used by the current server. The active key file is `/opt/tv-sshd/authorized_keys`.

## Build

Requirements on the host:

- macOS with `curl`, `shasum`, `tar`, and `file`;
- Docker Desktop with multi-platform/QEMU support.

Run:

```bash
cd dropbear
./build-dropbear.sh
```

The script downloads and SHA-256-verifies the pinned 2026.94 source tarball,
then builds inside `alpine:3.20` using `linux/arm/v7`:

```text
https://matt.ucc.asn.au/dropbear/releases/dropbear-2026.94.tar.bz2
SHA-256: e098034a843699200c8c977a991fff73159735bf795d5f72ef672c41a6b1ae81
```

Output:

```text
dropbear/out/dropbear
```

The tested file identification is:

```text
ELF 32-bit LSB executable, ARM, EABI5, statically linked, stripped
```

The packaged build was also verified byte-for-byte against the binary deployed
on the TV:

```text
Size:    325772 bytes
SHA-256: 4ef0d338a255b8015db375890c2a0217a5d3b8b092335b92e9be8a7f7e58b536
```

The build includes `dropbear`, `dropbearkey`, `dbclient`, and `scp` in one
multi-call executable. Only `dropbear` and `dropbearkey` are exposed by the TV
launcher. `LDFLAGS=-static` is intentional: Alpine ARMv7's static CRT objects
conflict with dropbear's default `-Wl,-pie` linker flags.

## Install or update the TV files

Build first, then run:

```bash
./install-dropbear.sh 192.0.2.10
```

If `sdb` is elsewhere:

```bash
SDB=/path/to/tizen-studio/tools/sdb ./install-dropbear.sh 192.0.2.10
```

The installer only stages files. It does not start the server or modify the TV
boot configuration.

Equivalent manual commands:

```bash
SDB=$HOME/tizen-studio/tools/sdb
TV=192.0.2.10:26101
"$SDB" connect "$TV"
"$SDB" -s "$TV" push out/dropbear \
  /home/owner/share/tmp/sdk_tools/dropbear
"$SDB" -s "$TV" push tv-start-sshd.sh \
  /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
```

## Start or restart

From a root shell on the TV:

```bash
source /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
```

Use `source`, **not**:

```bash
bash /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
```

The latter trips Samsung's script-signature check on this firmware:

```text
[uep][bash] the file is NOT signed!!
```

Expected successful output includes:

```text
dropbear pid: <pid>
tcp  0  0 0.0.0.0:22  0.0.0.0:*  LISTEN
tcp6 0  0 :::22       :::*       LISTEN
```

The script is idempotent: it writes the configured public key, stops the PID in
`/run/tv-sshd/dropbear.pid` if present, recreates the volatile runtime, and
starts a new server. Existing host keys are preserved.

## Connect

Use the private key corresponding to the configured `your-key-comment` public
key. With that key available to the normal SSH agent/configuration:

```bash
ssh root@192.0.2.10
```

A successful login is UID/GID 0. No password login is enabled.

## Boot behavior

The binary, launcher, authorized key, and host key are persistent. The running
server is not: it disappears when the TV powers off, and `/run` is cleared.
Run the launcher again as root after each boot. No TV-local systemd unit,
`rc.local`, cron job, or other boot hook is installed by this package.

`setsid` detaches the process from its session but does **not** move it to a new
cgroup. Because root command execution originates through Samsung services,
dropbear can appear in the originating service's cgroup. Restarting that service
can therefore kill dropbear; rerun the launcher after the service and root
access recover.

## Why the launcher has these workarounds

### 1. Executing directly from userdata fails

Running the staged binary directly produced:

```text
Operation not permitted
```

On this TV the userdata/SMACK execution policy is the relevant restriction.
Copying the binary to `/run` (`System::Run`) allows the same unsigned binary to
execute. The launcher therefore always runs `/run/tv-sshd/dropbearmulti`, not
the copy in `sdk_tools`.

### 2. Root's home is read-only

`/root` is on the read-only root filesystem, so `~/.ssh/authorized_keys` cannot
be installed normally. Dropbear's `-D /opt/tv-sshd` selects an alternate
`authorized_keys` directory.

### 3. Dropbear checks every parent directory's permissions

Using `-D /run/tv-sshd` caused public-key authentication to fail with:

```text
/run must be owned by user or root, and not writable by group or others
Exit before auth
```

`/run` is root-owned but group-writable. `/opt/tv-sshd` is mode `0700`, and its
parents `/opt` and `/` have acceptable ownership/modes. The key file itself is
mode `0600`.

### 4. Explicit `-r` host-key files must exist

Current dropbear does not reliably auto-generate an explicitly named `-r` file;
without one it exits with:

```text
No hostkeys available. 'dropbear -R' may be useful or run dropbearkey.
```

The launcher creates an Ed25519 key with `dropbearkey` on first run. `-R` would
instead create delayed/in-memory keys, changing the host identity across
restarts, so it is not used.

### 5. The multi-call binary dispatches on `argv[0]`

Calling a binary named `dropbear` with `dropbearkey` as an argument invokes the
server, not the key generator. The symlinks in `/run/tv-sshd` select the correct
sub-program through the executable's basename. The clean build also ensures
`dbmulti.o` contains the `dropbearkey` dispatch entry.

### 6. Inherited descriptors can keep the caller open

The root/exploit process supplies more than ordinary stdin/stdout/stderr. A
naively launched dropbear inherited duplicate output pipes plus descriptors for
items such as `/dev/sdp_pqe_fdet`, sockets, and other TV services. The duplicate
pipes prevented the caller from seeing EOF and caused a 45-second timeout.

Before `exec`, the launcher:

- redirects fds 0–2 to `/dev/null` and the dropbear log;
- closes fds 3 through 255 in a subshell;
- uses `setsid` to detach the new session.

The resulting server retains only its listening sockets, log, `/dev/null`, and
its own executable descriptor—not the exploit device or caller pipes.

## Configuration and security

The active public key is present in both:

```text
tv-start-sshd.sh             # PUBKEY=...; consumed by the launcher
authorized_keys              # reference copy
```

Keep both copies in sync when changing the key, push the updated launcher, and
rerun it. Remove the old key rather than appending if you intend to revoke it;
the launcher replaces `/opt/tv-sshd/authorized_keys` on every run.

Current security properties:

- root SSH login is enabled;
- only the configured public key is authorized;
- password authentication is disabled with `-s`;
- the server listens on TCP port 22 on all IPv4/IPv6 interfaces;
- no TV firewall rules are changed;
- the server host key is private and mode `0600`;
- no client private key is included in this repository.

Treat port 22 as trusted-LAN-only exposure. Restrict network access externally
if the TV is connected to an untrusted network.

## Logs and diagnostics

```bash
cat /run/tv-sshd/dropbear.pid
netstat -tln | grep ':22 '
tail -50 /run/tv-sshd/dropbear.log
cat /opt/tv-sshd/authorized_keys
```

Common messages:

| Message | Meaning / action |
| --- | --- |
| `Permission denied (publickey)` | Check the configured public key and read the log immediately after the attempt. |
| `/run must be owned ...` | The server is using the wrong `-D` directory; use `/opt/tv-sshd`. |
| `No hostkeys available` | The persistent host key is missing/corrupt; the launcher generates it when absent. |
| `Dropbear SSH multi-purpose ...` | The multi-binary was invoked with the wrong basename; check the symlinks. |
| `wtmp_write ... /dev/null/wtmp` or `lastlog_openseek` | Harmless accounting-file warnings on this minimal TV userland. |

## Remove the installation

From a root shell, after replacing the example paths if customized:

```bash
kill "$(cat /run/tv-sshd/dropbear.pid)"
rm -rf /run/tv-sshd
rm -rf /opt/tv-sshd
rm -f /home/owner/share/tmp/sdk_tools/dropbear
rm -f /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
rm -f /home/owner/share/tmp/sdk_tools/dropbear_ed25519_host_key
```

This removes the server, host key, and authorized-key configuration. It does not
change the root filesystem or any boot service because none was installed.
