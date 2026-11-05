# Tizen root authorization broker

This directory contains the implementation work for the SuperSU-like root broker proposed in [`../docs/tv-authd-plan.md`](../docs/tv-authd-plan.md).

The M0 platform spikes and M1 headless broker are complete and cold-boot validated. The next milestone is M2, the approval UI. Sources:

- `spikes/armv7-hello/`: static ARMv7 Rust build and execution smoke test;
- `spikes/peer-identity/`: Unix-socket `SO_PEERCRED` and `SO_PEERSEC` probe;
- `spikes/efl-popup/`: native Rust/EFL probe (UEP-rejected) and the validated managed .NET/EFL popup.
- `daemon/`: M1 Rust broker core, protocol, tests, ARMv7 build, and systemd unit.

Each spike records its own build commands and validation results. Host builds and emulation do not count as on-TV validation. No production daemon or approval policy is implemented yet.

## Current status

| Spike | Host/emulated result | Artifact size | Still requires TV validation |
|---|---|---:|---|
| ARMv7 Rust | Static build, Docker/QEMU, direct TV execution, and TV transient-systemd execution passed | 323,652 bytes | Runtime RSS and persistent-unit behavior |
| Peer identity | Host/emulated tests passed; TV returned `System::Privileged` for root clients and distinct `User::Pkg::*` labels for two packaged .NET apps | 393,976 bytes server | Remaining app types and failure/restart cases |
| EFL popup | Native Rust ELF rejected by UEP; managed .NET/EFL popup set notification TOP, appeared over YouTube, and dismissed | 14,568-byte signed TPK | Spoofing/occlusion/focus hardening, manager UI, and RSS |
| M1 broker core | 12 host tests and 15 Linux tests pass; TV validated granted/denied requests, daemon restart, dynamic `/opt/tv-units` startup, and cold boot | 461,940 bytes ARM binary | M2 approval UI integration |

The development containers returned `ENOPROTOOPT` for `SO_PEERSEC`, while the TV successfully returned SMACK labels for root and packaged app domains. Managed .NET is now the validated UI path; native TPK execution is blocked by UEP. Test packages and temporary files were removed after validation. The broker and dynamic boot-service layout remain installed. All work is uncommitted.

Contributors working in this directory should read [`AGENTS.md`](AGENTS.md) first. In particular, do not access the TV unless the user explicitly makes it available; treat access as read-only unless a new specific write authorization is given.
