# Root broker working notes

- The authoritative design is `../docs/tv-authd-plan.md`. M0, M1, and M2 are complete and cold-boot validated. The next milestone is M3, the Vetro cutover.
- Do not commit or otherwise mutate Git history unless the user explicitly asks.
- The TV is on. The M2 broker is installed at `/opt/tv-auth`, units are in `/opt/tv-units`, the dynamic boot hook is `/opt/etc/parseinfo.sh`, and the signed `org.tvauth.ui` manager is installed. The grants file is empty and all test packages/artifacts were removed. Do not make writes, install packages, execute payloads, restart services, or reboot without new explicit authorization.
- Keep each spike self-contained under its assigned `spikes/<name>/` directory. Do not edit another spike's files or shared build files while agents work in parallel.
- Prefer synchronous/blocking Rust and small, focused crates. Do not introduce Tokio or another async runtime. Pragmatic crates are allowed when they avoid reinventing nontrivial code.
- Daemon-side artifacts target `armv7-unknown-linux-musleabihf`. The approval UI is a managed `netcoreapp3.1` Tizen .NET TPK. Do not package a custom native ELF UI: the M0 spike proved UEP prevents launchpad from executing unsigned native binaries from writable app storage, even with a correctly signed TPK.
- Run host-side builds/tests when possible. Record exact commands, toolchain versions, artifact sizes, and results in the spike README.
- Clearly distinguish host validation, emulation, cross-compilation, and actual TV validation. Never claim an on-TV result that was not observed.
- Build products belong in each spike's `target/` or `out/` directory and must not be committed.
