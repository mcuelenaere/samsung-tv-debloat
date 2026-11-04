# QN90B core daemon RE set

Collected read-only from `root@192.0.2.10` on 2026-11-04. Files retain their
original paths below `rootfs/`; `SHA256SUMS` covers every collected file.

The executables are 32-bit ARM EABI5 binaries for Tizen 6.5 / Linux 5.4.77.
Most are PIE and stripped. Notable exceptions with symbols still present are
`deviced`, `resourced`, `amd`, and `pkgmgr-server`.

## Core services

| Unit | Executable |
| --- | --- |
| `deviced.service` | `/usr/bin/deviced` |
| `storaged.service` | `/usr/bin/storaged` |
| `resourced.service` | `/usr/bin/resourced` |
| `reclaimd.service` | `/usr/bin/reclaimd` |
| `rscmgr-service.service` | `/usr/bin/rscmgr-service` (plus `/usr/bin/gen_rm_msgq`) |
| `resource-center.service` | `/usr/bin/resource-center` |
| `buxton2.service` | `/usr/sbin/buxton2d` |
| `fconfigd.service` | `/usr/bin/fconfigd` |
| `pkgmgr-info.service` | `/usr/bin/pkginfo-server` |
| `eventd.service` | `/usr/bin/eventd` |
| `esd.service` | `/usr/bin/esd` |
| `stability-monitor.service` | `/usr/bin/stability-monitor` |
| `issue_report_agent.service` | `/bin/issue_report_agent` |
| `emergency-log-transmitter.service` | `/usr/bin/emergencyLogTransmitter` |

## Boot/application/package services included for persistence research

| Unit | Executable or script |
| --- | --- |
| `ac.service` | `/usr/bin/amd` |
| `app-boot-manager.service` | `/usr/bin/app-boot-manager` |
| `app-control-service.service` | `/usr/bin/app-control-service` |
| `package-manager.service` | `/usr/bin/pkgmgr-server` |
| `usb-launcher-service.service` | `/usr/bin/usb-launcher-service` |
| `app2sd-server.service` | `/bin/app2sd-server` |
| `AT-HUB.service` | `/opt/etc/parseinfo.sh` |

`tizen-system-env.service` and `tizen-generate-env.service` are also included
because they participate in early boot environment setup.

## Immediate persistence lead

`AT-HUB.service` has `WantedBy=starter.target`, no `User=` directive, and:

```ini
ExecStart=/opt/etc/parseinfo.sh &
```

Thus a stock boot service executes a root-owned script from persistent,
writable `/opt`. The script was collected unchanged at
`rootfs/opt/etc/parseinfo.sh`. This is a more direct candidate than modifying a
daemon binary, but any change still needs to account for SMACK/UEP and should
be tested with a reversible backup.

## Verify

From this directory:

```sh
shasum -a 256 -c SHA256SUMS
```
