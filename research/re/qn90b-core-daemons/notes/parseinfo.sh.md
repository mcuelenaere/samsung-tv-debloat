# parseinfo.sh — RE note (AT-HUB boot hook)

- **Path / unit:** `/opt/etc/parseinfo.sh` — `AT-HUB.service`, `ExecStart=/opt/etc/parseinfo.sh &`, `WantedBy=starter.target`
- **Type:** `#!/bin/bash` script on **writable VDFS `/opt`**  ·  **Runs as:** **root** (unit has no `User=`)

## What it does
Reads `/etc/prd_info.ini` (`Build=`, `Model=`), then `echo "__${REPO}__${MODEL}" > /proc/target_version`. That's the entire functional body. Carries a trailing Samsung UEP signature line:
`##UEP530x<512 hex>…AAACAg==:UEP`

## 1. Persistence surface — **known community persistence route**
A **stock boot service runs a root-owned script from persistent, writable `/opt`**. This is the danielyc #17 lead already in `docs/samsung-tv-root-ecosystem.md` §5: append commands here → boot-persistent root, *if* UEP enforcement on the script can be satisfied or bypassed.

## 2. UEP / signature — the gate
The `##UEP…:UEP` trailer is the file's User-Executable-Protection signature. Samsung's UEP LSM/loader validates script/native integrity **at exec time in the kernel**, not in any daemon here — so the actual enforcement is out of scope of this binary set. Two documented bypasses from the ecosystem notes:
- Interpreter-based bypass: `#!/usr/bin/gawk -f` scripts are reported to skip UEP (danielyc #17). parseinfo.sh is `#!/bin/bash` and signed; a sibling gawk hook avoids the signature requirement entirely.
- Whether editing the bash body invalidates the UEP check depends on the device's UEP policy (enforcing vs permissive). **Verify on-device:** does an edited `/opt/etc/parseinfo.sh` still run (UEP off / not covering `/opt`), or does it get blocked?

## 3. SMACK / privilege
Runs at AT-HUB's label (no `SmackProcessLabel=` in the unit → inherits systemd default; confirm live `ps -Z`). Root uid. `/proc/target_version` write is the only privileged action.

## 4. Debloat
Not telemetry — leave enabled (removing it may affect model/version reporting). Value here is persistence, not debloat.

## Leads worth a deeper look
- On-device: test write-and-run of `/opt/etc/parseinfo.sh` under current UEP; check mount flags of `/opt` and whether UEP is enforcing. If editable → simplest boot-persistent root in this whole set.
- Inventory other `starter.target` units that `ExecStart` from writable paths (same class of hook).
