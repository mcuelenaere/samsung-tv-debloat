# Root broker working notes

- The authoritative design is `../docs/tv-authd-plan.md`. The current task is the M0 platform spikes, not the full broker.
- Do not commit or otherwise mutate Git history unless the user explicitly asks.
- The TV is on. Read-only access plus the specific temporary M0 test writes/installs were authorized on 2026-11-04/05; all test packages and `/run` artifacts were removed afterwards. Do not make further writes, install packages, execute payloads, restart services, or reboot without new explicit authorization.
- Keep each spike self-contained under its assigned `spikes/<name>/` directory. Do not edit another spike's files or shared build files while agents work in parallel.
- Prefer synchronous/blocking Rust and small, focused crates. Do not introduce Tokio or another async runtime. Pragmatic crates are allowed when they avoid reinventing nontrivial code.
- Daemon-side artifacts target `armv7-unknown-linux-musleabihf`. The approval UI is a managed `netcoreapp3.1` Tizen .NET TPK. Do not package a custom native ELF UI: the M0 spike proved UEP prevents launchpad from executing unsigned native binaries from writable app storage, even with a correctly signed TPK.
- Run host-side builds/tests when possible. Record exact commands, toolchain versions, artifact sizes, and results in the spike README.
- Clearly distinguish host validation, emulation, cross-compilation, and actual TV validation. Never claim an on-TV result that was not observed.
- Build products belong in each spike's `target/` or `out/` directory and must not be committed.
