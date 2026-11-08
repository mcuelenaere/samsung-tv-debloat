# Tizen root authorization broker

This directory contains the implementation work for the SuperSU-like root broker proposed in [`../docs/tv-authd-plan.md`](../docs/tv-authd-plan.md).

The M0 platform spikes, M1 headless broker, M2 approval UI, M3/Vetro cutover, and per-app toast branch are live and validated. M4 hardening is deployed on top of the live branch; package upgrade/reinstall, memory pressure, boot-input, and log-permission checks passed. All M4 checks, including cold boot, are complete; the manager-refresh UI is installed. Sources:

- `spikes/armv7-hello/`: static ARMv7 Rust build and execution smoke test;
- `spikes/peer-identity/`: Unix-socket `SO_PEERCRED` and `SO_PEERSEC` probe;
- `spikes/efl-popup/`: native Rust/EFL probe (UEP-rejected) and the validated managed .NET/EFL popup;
- `daemon/`: Rust broker, protocol, approval manager, tests, ARMv7 build, and systemd unit;
- `ui/`: managed Tizen .NET approval prompt and grant manager.

Each spike records its own build commands and validation results. Host builds and emulation do not count as on-TV validation.

## Current status

| Component | Host/emulated result | Artifact size | Still requires TV validation |
|---|---|---:|---|
| ARMv7 Rust spike | Static build, Docker/QEMU, direct TV execution, and TV transient-systemd execution passed | 323,652 bytes | none |
| Peer identity spike | Host/emulated tests passed; TV returned `System::Privileged` for root clients and distinct `User::Pkg::*` labels for two packaged .NET apps | 393,976 bytes server | none |
| EFL popup spike | Native Rust ELF rejected by UEP; managed .NET/EFL popup set notification TOP, appeared over YouTube, and dismissed | 14,568-byte signed TPK | none for the display path |
| M1 broker core | 15 Linux tests plus deployment checks pass; TV validated granted/denied requests, daemon restart, dynamic `/opt/tv-units` startup, and cold boot | 461,940 bytes | none |
| M2 approval UI | 35 Linux tests pass; signed UI installed and TV-validated for deny/once/always/revoke, timeout, crash, focus/keys, daemon restart, and cold boot | 34,816-byte DLL; 24,073-byte signed TPK; 546,388-byte ARM daemon | none |
| M3/toast live branch | Vetro cutover and per-app notify mode deployed and in use | 584,452-byte ARM daemon | none |
| M4 hardening | 66 Linux tests pass; deployed grant identity binding + notify preservation, private runtime log/dir, EINTR handling, failure-path coverage; fsprobe/uiprobe, 64 MiB pressure, upgrade/reinstall, and cold boot passed | 584,452-byte ARM daemon; 46,592-byte UI DLL | none |

The development containers returned `ENOPROTOOPT` for `SO_PEERSEC`, while the TV successfully returned SMACK labels for root and packaged app domains. Managed .NET is the validated UI path; native TPK execution is blocked by UEP. M1/M2 test packages and M4 probes were removed after use. The live TV runs the toast branch plus the M4 daemon with a v2 Vetro grant (`notify: first`). The manager-refresh UI is installed with the Apps2Samsung Partner certificate; use that profile for future UI updates.

Contributors working in this directory should read [`AGENTS.md`](AGENTS.md) first. In particular, do not access the TV unless the user explicitly makes it available; treat access as read-only unless a new specific write authorization is given.
