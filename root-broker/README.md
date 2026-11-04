# Tizen root authorization broker

This directory contains the implementation work for the SuperSU-like root broker proposed in [`../docs/tv-authd-plan.md`](../docs/tv-authd-plan.md).

The M0 platform spikes are complete. The next milestone is the M1 headless broker. Spike sources:

- `spikes/armv7-hello/`: static ARMv7 Rust build and execution smoke test;
- `spikes/peer-identity/`: Unix-socket `SO_PEERCRED` and `SO_PEERSEC` probe;
- `spikes/efl-popup/`: native Rust/EFL probe (UEP-rejected) and the validated managed .NET/EFL popup.

Each spike records its own build commands and validation results. Host builds and emulation do not count as on-TV validation. No production daemon or approval policy is implemented yet.

## Current status

| Spike | Host/emulated result | Artifact size | Still requires TV validation |
|---|---|---:|---|
| ARMv7 Rust | Static build, Docker/QEMU, direct TV execution, and TV transient-systemd execution passed | 323,652 bytes | Runtime RSS and persistent-unit behavior |
| Peer identity | Host/emulated tests passed; TV returned `System::Privileged` for root clients and distinct `User::Pkg::*` labels for two packaged .NET apps | 393,976 bytes server | Remaining app types and failure/restart cases |
| EFL popup | Native Rust ELF rejected by UEP; managed .NET/EFL popup set notification TOP, appeared over YouTube, and dismissed | 14,568-byte signed TPK | Spoofing/occlusion/focus hardening, manager UI, and RSS |

The development containers returned `ENOPROTOOPT` for `SO_PEERSEC`, while the TV successfully returned SMACK labels for root and packaged app domains. Managed .NET is now the validated UI path; native TPK execution is blocked by UEP. All temporary test packages, `/run/tvauth-spike` files, and temporary logs were removed. All work is uncommitted.

Contributors working in this directory should read [`AGENTS.md`](AGENTS.md) first. In particular, do not access the TV unless the user explicitly makes it available; treat access as read-only unless a new specific write authorization is given.
