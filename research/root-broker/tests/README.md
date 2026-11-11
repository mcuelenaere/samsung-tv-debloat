# M4 hardening probes

These are disposable TV test packages and small scripts for the M4 acceptance
items. They are not production components and must be removed after use.

## Probes

| Package | Purpose | Expected result |
|---|---|---|
| `org.tvauth.spike.fsprobe` | Runs as an ordinary UID-5001 app and opens/creates against boot inputs | Root-executed inputs are not writable or replaceable; the broker socket is connectable |
| `org.tvauth.spike.uiprobe` | Runs as an ordinary app and sends every UI-only method | Every response is `error` / `not_ui` |

### Filesystem probe

`fsprobe` opens existing files for write **without writing**, and creates/removes
only its own uniquely named probe files. It reports DAC+SMACK results to
`/tmp/tvauth-fsprobe.result` for:

- `/opt/etc/parseinfo.sh`, `/opt/etc/resolv.conf`;
- `/opt/tv-units/*.service`, `/opt/tv-auth/*`;
- `/opt/tv-sshd/bin/dropbear`;
- `/run/systemd/system/tv-authd.service`;
- the read-only `/run/opt/tv-auth` bind mount;
- `/run/tv-auth/tv-authd.log`;
- `/opt/dbspace/.pkgmgr_cert.db`;
- directory create probes for the same root-only parents;
- a Unix-socket connection to `/run/tv-auth/broker.sock`.

Expected: writes/creates denied for root-executed inputs; socket connect allowed.
A writable hook or unit source is a release blocker. `/opt/etc` is DAC `0777`
but SMACK labels it `System::Shared`; the probe is the empirical check.

### UI-impersonation probe

`uiprobe` connects as `User::Pkg::org.tvauth.spike.uiprobe` and sends
`ui.hello`, `grants.list`, `ui.decision`, and `grants.revoke`. It records the raw
request/response pairs in `/tmp/tvauth-uiprobe.result`. Expected: all four are
`error` / `not_ui`; it must never receive a prompt or a grant list.

## Build and sign

```sh
./root-broker/tests/build-probes.sh
```

This produces unsigned TPKs under `root-broker/tests/out/`. Sign them with the
same TV certificate used for the M2 packages; certificate material and passwords
must stay outside the repository:

```sh
tizen package -t tpk -s <profile> -- org.tvauth.spike.fsprobe-0.1.0.tpk
tizen package -t tpk -s <profile> -- org.tvauth.spike.uiprobe-0.1.0.tpk
```

## Run

Do this only in a coordinated window: installing/launching probes does not change
the broker, but M3/Vetro may be using it. The new M4 daemon itself requires a
short `tv-authd` restart.

```sh
mkdir -p /run/tvauth-test
# copy the signed TPKs there
/usr/bin/pkgcmd -i -t tpk -p /run/tvauth-test/org.tvauth.spike.fsprobe-0.1.0-signed.tpk
/usr/bin/pkgcmd -i -t tpk -p /run/tvauth-test/org.tvauth.spike.uiprobe-0.1.0-signed.tpk

rm -f /tmp/tvauth-fsprobe.result /tmp/tvauth-uiprobe.result
/usr/bin/app_launcher -s org.tvauth.spike.fsprobe
/usr/bin/app_launcher -s org.tvauth.spike.uiprobe
# wait for both result files, then inspect
cat /tmp/tvauth-fsprobe.result
cat /tmp/tvauth-uiprobe.result

/usr/bin/pkgcmd -u -n org.tvauth.spike.fsprobe
/usr/bin/pkgcmd -u -n org.tvauth.spike.uiprobe
rm -rf /run/tvauth-test /tmp/tvauth-fsprobe.result /tmp/tvauth-uiprobe.result
```

## Package upgrade / reinstall / signer change

Grants are bound to a SHA-256 of the installed package's `author-signature.xml`
(TPK: package root; WGT: `res/wgt`). The grant file is version 2. Version-1
label-only entries are migrated on first use by binding them to the package
currently installed under that label.

Test with the existing `brokerone` requester. Build the changed-content upgrade
with:

```sh
./bins/tv-authd/test-clients/build-upgrade-client.sh
```

1. Install `org.tvauth.spike.brokerone` 0.1.0, grant **Always allow**.
2. Reinstall an identical 0.1.0 TPK: the signature hash is unchanged, so the
   grant remains valid.
3. Install a changed 0.1.1 build (same package ID and signer, changed DLL and
   signed manifest): the signature hash changes, so the next request must prompt
   again.
4. Uninstall/reinstall the identical TPK: the hash is unchanged, so the grant
   remains.
5. A differently signed package with the same package ID must produce a
   different signature hash and therefore must not inherit the grant. Tizen's
   package manager may also reject that install outright; record which behavior
   is observed.

The unit tests already cover changed, missing, symlinked, and group/world-writable
signature files, plus legacy migration and v2 persistence.

## Memory measurement

Read-only measurement:

```sh
TARGET=root@172.17.2.110 ./root-broker/tests/measure-memory.sh
```

Controlled pressure (creates and removes a RAM-backed tmpfs file; capped at
512 MiB):

```sh
TARGET=root@172.17.2.110 ./root-broker/tests/measure-memory.sh 64
```

To measure the UI, first trigger an approval prompt and keep it visible, then run
the script. Record `VmRSS`/`VmHWM` for `tv-authd` and the UI, plus
`MemAvailable` before/during/after pressure. Do not run pressure tests while a
long-running Vetro command or install is in progress.

## Results

TV run 2026-11-08, while M3/Vetro was active; the probes did not restart the
broker or change grants, and they were removed afterwards.

### fsprobe: pass

Run as `User::Pkg::org.tvauth.spike.fsprobe`, UID 5001:

- `/opt/etc/parseinfo.sh`: `stat=allowed write=denied`;
- `/opt/etc/resolv.conf`: `stat=allowed write=denied`;
- `/opt/etc`: `stat=allowed create=denied`;
- `/opt/tv-units/*`, `/opt/tv-auth/*`, `/opt/tv-sshd/bin/*`: `stat=denied
  write=denied` (or parent denied);
- `/run/systemd/system`, `/run/opt/tv-auth`: not visible to the app;
- `/opt/dbspace/.pkgmgr_cert.db`: `stat=allowed write=denied`;
- `/run/tv-auth`: `stat=allowed create=denied`;
- broker socket: `connect=allowed`;
- `/run/tv-auth/tv-authd.log`: `read=allowed` on the pre-M4 daemon, `read=denied`
  after the M4 daemon made it `0600`;

Post-M4 re-check after deployment: `/run/tv-auth` was `0711`
(`stat=allowed create=denied`), the log was `0600` and `read=denied`, and all
boot-input protections above still held.

### uiprobe: pass

All four UI-only requests from `User::Pkg::org.tvauth.spike.uiprobe` returned
`status:error`, `error:not_ui`:

- `ui.hello`, `grants.list`, `ui.decision`, `grants.revoke`.

### Package upgrade / reinstall: pass

With the M4 daemon deployed:

1. `brokerone` 0.1.0 was granted **Always allow** with fingerprint
   `sha256:e5ea3261…`; a second run executed with no prompt.
2. Installing the changed 0.1.1 build logged
   `grant_stale ... reason=package_identity_changed`, prompted, and Deny
   returned `denied`/`user_denied`. The grant was removed; Vetro's grant was
   untouched.
3. Re-granting 0.1.1 stored fingerprint `sha256:24b93e66…`.
4. Reinstalling the identical 0.1.1 TPK kept the fingerprint and executed with
   no prompt.

The package was uninstalled and its grant removed afterwards.

### Memory: pass (pre-M4 daemon, M3/UI active)

`measure-memory.sh 64` on 2026-11-08 created 64 MiB of tmpfs pressure while
Vetro was active:

| | RSS | VmHWM | Threads |
|---|---:|---:|---:|
| `tv-authd` | 304 kB | 384 kB | 3 |
| UI (toast mode) | 50,288 kB | 53,036 kB | 27 |

`MemAvailable`: 437,588 kB before → 374,316 kB during → 438,120 kB after. Both
processes stayed at the same RSS and PID; no restart or OOM occurred. The UI
number includes the .NET runtime while it lingers for Vetro toasts.

### Notes

- During the manager revoke test, the installed UI's cached list still showed
  only Vetro; the first row was Vetro, so the wrong grant was revoked. Vetro was
  immediately restored from the pre-M4 backup and re-migrated with
  `notify: first`; commands resumed. The worktree UI now refreshes grants when
  the manager opens. That fix was installed in place on 2026-11-08 with the
  Apps2Samsung Partner certificate (the installed UI's author certificate
  matches).
- The M4 daemon is deployed and Vetro is running with the v2 fingerprint grant.

### Pending

### Cold boot: pass

Rebooted to boot ID `c0b55e8d-22a6-4fb6-8c70-a755bbfcab86` on 2026-11-08
19:50:32. AT-HUB logged `start units rc=0`; Dropbear and `tv-authd` started
automatically with 0 restarts; the M4 binary `7903c0ba…` and Partner-signed UI
DLL `53f446fb…` were intact. The v2 Vetro grant was loaded directly with
`notify: first` and fingerprint `ed44a7da…` (no stale/migration events), and
Vetro commands resumed with `exit_code=0`. `/run/tv-auth` was `0711`, the socket
`0660 root:users`, and the log `0600`. Launching the UI after boot showed
**vetrotvapp / Toast: first use** and performed the manager grants refresh.
