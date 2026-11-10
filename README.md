# Samsung TV debloat & root suite

Tools for Samsung Tizen 6.5 TVs (ARMv7, PontusM). The roadmap is
[`docs/rust-tv-suite-plan.md`](docs/rust-tv-suite-plan.md); reverse-engineering
notes live in `docs/` and `re/`.

## Cargo workspace

```text
crates/tv-proto   tv-authd wire protocol: request/response/ui.* types, bounded
                  NDJSON framing, one-shot `call` and reconnecting UI clients,
                  Elementary markup helpers (pure Rust)
crates/tv-efl     EFL/Elementary/Ecore FFI, widgets, UI log (TV sysroot)
crates/tv-tizen   vconf, read-only pkgmgr DB, app launch, libcurl GET (TV sysroot)
bins/tv-authd     root broker daemon (static musl)
bins/tv-auth-cli  `tvauth-cli` root-role client (static musl)
bins/tv-auth-popupd persistent prompt/toast overlay (EFL systemd daemon, gnueabi)
bins/tv-auth-ui   `org.tvauth.ui` manager TPK: store, grants, tweaks (EFL, gnueabi)
bins/tv-blockd    hosts/telemetry/OTN blocker daemon (gnueabi)
bins/tv-patchd    TizenTube in Samsung YouTube via a local proxy (gnueabi)
sysroot/          fetch script for the TV libraries the gnueabi crates link
root-broker/      spikes, probes, and the broker's working notes (AGENTS.md)
```

Host checks (the default members avoid linking TV libraries):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test                          # tv-proto, tv-authd, tv-auth-cli
```

TV builds run in pinned Rust 1.85 containers through `cross-build.sh`:

```sh
sysroot/fetch-tv-sysroot.sh         # TV_HOST=root@<tv>; needed for gnueabi
bins/tv-authd/build-armv7.sh        # musl
bins/tv-auth-cli/build-armv7.sh     # musl
bins/tv-auth-popupd/build-armv7.sh  # gnueabi
bins/tv-auth-ui/build-ui.sh         # gnueabi + unsigned TPK
bins/tv-blockd/build-armv7.sh       # gnueabi
bins/tv-patchd/build-armv7.sh       # gnueabi
./cross-build.sh gnueabi build --locked --release -p tv-tizen --example probe
```

Unsigned ELFs only execute from `/run` (UEP); see `docs/samsung-tv-uep.md`.
