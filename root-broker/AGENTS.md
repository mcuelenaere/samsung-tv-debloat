# Root broker working notes

- The authoritative design is `../docs/tv-authd-plan.md`. M0–M3 are complete and validated. M4 hardening is deployed on top of the live toast branch; package upgrade/reinstall, memory pressure, boot-input, and log-permission checks passed. The manager-refresh UI is installed with the Apps2Samsung Partner certificate. The M4 cold boot passed on 2026-11-08; no M4 acceptance items remain.
- A separate track, `../docs/tv-authd-pkg-install-plan.md`, covers package installation. Its M1 daemon and UI work is TV-validated for TPK installs (including cold boot); WGT installs and Samsung Home registration remain M4, and M3 signing is not started.
- Do not commit or otherwise mutate Git history unless the user explicitly asks.
- The TV is on and Vetro is active. The live broker is the M4 binary `7903c0ba…`; `/opt/tv-auth/grants.json` is v2 with a Vetro fingerprint and `notify: first`; `/run/tv-auth` is `0711` and the log is `0600`. No test packages remain. The live UI is signed with the Apps2Samsung Partner certificate and the manager-refresh fix is installed (DLL `53f446fb…`). Use the Partner profile for UI updates. Do not restart `tv-authd`, change grants, install packages, run memory pressure, or reboot without explicit coordination.
- Keep each spike self-contained under its assigned `spikes/<name>/` directory. Do not edit another spike's files or shared build files while agents work in parallel.
- Prefer synchronous/blocking Rust and small, focused crates. Do not introduce Tokio or another async runtime. Pragmatic crates are allowed when they avoid reinventing nontrivial code.
- Daemon-side artifacts target `armv7-unknown-linux-musleabihf`. The approval UI is a managed `netcoreapp3.1` Tizen .NET TPK. Do not package a custom native ELF UI: the M0 spike proved UEP prevents launchpad from executing unsigned native binaries from writable app storage, even with a correctly signed TPK.
- Run host-side builds/tests when possible. Record exact commands, toolchain versions, artifact sizes, and results in the spike README.
- Clearly distinguish host validation, emulation, cross-compilation, and actual TV validation. Never claim an on-TV result that was not observed.
- Build products belong in each spike's `target/` or `out/` directory and must not be committed.
