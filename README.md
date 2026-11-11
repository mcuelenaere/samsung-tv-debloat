# Samsung TV debloat & root suite

Tools for Samsung Tizen 6.5 TVs (ARMv7, PontusM). The roadmap is
[`docs/rust-tv-suite-plan.md`](docs/rust-tv-suite-plan.md); setup and package
planning lives in
[`docs/unified-setup-and-package-management-plan.md`](docs/unified-setup-and-package-management-plan.md).
Reverse-engineering notes live in `docs/` and `research/`.

## Repository layout

```text
bins/
  tv/               host CLI: `tv sdb`, `tv ssh-key`, external `tv-<name>`
  tv-bundle/        reproducible .tvpkg and setup-bundle builder
  tv-pkg/           on-TV component manager (implemented separately)
  tv-sdb/           standalone SDB CLI
  tv-authd/         root broker daemon
  tv-auth-cli/      tvauth-cli root-role client
  tv-auth-popupd/   persistent prompt/toast overlay
  tv-auth-ui/       org.tvauth.ui manager TPK
  tv-blockd/        hosts/telemetry/OTN blocker
  tv-patchd/        TizenTube proxy
crates/
  tv-proto/         tv-authd wire protocol and clients
  tv-sdb/           native SDB transport
  tv-ssh-key/       OpenSSH public-key parser and fingerprints
  tv-efl/           EFL/Elementary/Ecore FFI (TV sysroot)
  tv-tizen/         vconf, pkgmgr DB, app launch, libcurl (TV sysroot)
third_party/
  dropbear/         third-party Dropbear build recipe
toolchain/
  cross-build.sh    pinned Rust 1.85 cross-build container
  sysroot/          TV shared-library fetch script and generated sysroot
research/
  re/               reverse-engineering notes
  root-broker/      historical spikes, probes and working notes
rooting/            nested workspace, excluded from the main one
  bins/tv-root/     external `tv root`
  bins/tv-setup/    external `tv setup`
  crates/tv-root/   FDET acquisition, compatibility and setup orchestration
  payloads/fdet/    managed .NET FDET payload
  payloads/persistence/ AT-HUB hook and guarded persistence assets
  packages/         reviewed component package specs and fixed order
scripts/
  claude-worktree.sh sparse Claude Code worktree helper
docs/               plans and platform notes
```

`rooting/` is a nested Cargo workspace. The main workspace has no path
dependency on it, so `cargo build` and `cargo test` still work in a checkout
where `rooting/` is physically absent. `scripts/claude-worktree.sh` creates a
dedicated sparse worktree that excludes only that directory.

## Host checks

```sh
cargo fmt --all --check
cargo clippy -- -D warnings                  # default members
cargo test                                   # default members
bash scripts/tests/claude-worktree-test.sh

cd rooting
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

The default members avoid linking TV libraries. `bins/tv-auth-ui`,
`tv-auth-popupd`, `tv-efl` and `tv-tizen` still need the TV sysroot or a Linux
host for their EFL-specific targets.

## Using `tv`

```sh
cargo build --release -p tv
target/release/tv sdb -s <tv> shell 0 vd_applist
target/release/tv ssh-key add <tv> ~/.ssh/id_ed25519.pub
target/release/tv root <tv> --command 'id'
target/release/tv setup <tv> --bundle <bundle-dir> --key ~/.ssh/id_ed25519.pub
```

`tv` handles `sdb` and `ssh-key` itself. `tv package ...` delegates to a
`tv-pkg` helper, and `tv root` / `tv setup` discover `tv-root` / `tv-setup`
beside the executable (then on `PATH`). Release archives place the helpers
together; a sparse checkout simply omits the rooting ones.

`tv ssh-key add` validates the public key locally and pushes it to
`/home/owner/share/tmp/sdk_tools/ssh-key.pub`. Nothing is authorized until the
user accepts the fingerprint prompt on the TV.

## TV builds

TV builds run in pinned Rust 1.85 containers through `toolchain/cross-build.sh`:

```sh
toolchain/sysroot/fetch-tv-sysroot.sh   # TV_HOST=root@<tv>; needed for gnueabi
bins/tv-authd/build-armv7.sh            # musl
bins/tv-auth-cli/build-armv7.sh         # musl
bins/tv-auth-popupd/build-armv7.sh      # gnueabi
bins/tv-auth-ui/build-ui.sh             # gnueabi + unsigned TPK
bins/tv-blockd/build-armv7.sh           # gnueabi
bins/tv-patchd/build-armv7.sh           # gnueabi
toolchain/cross-build.sh gnueabi build --locked --release -p tv-tizen --example probe
```

Unsigned ELFs only execute from `/run` (UEP); see `docs/samsung-tv-uep.md`.

## Setup bundle

After the component binaries and the separately built static `tv-pkg` exist:

```sh
TARGET_BUILD=T-PTMAKUC-REL-202310071804 rooting/build-suite-bundle.sh
```

This emits every suite `.tvpkg` plus a setup bundle whose `setup-manifest.json`
hashes all payloads, packages, the thin on-TV driver and the guarded
persistence assets. The TV-side driver installs components through `tv-pkg`,
applies persistence last and prints a single `setup-status=ok` marker.

## License and attribution

Original code in this repository is licensed under the [MIT License](LICENSE).
Adapted third-party material remains under its original license; upstream
sources, revisions, and license notices are tracked in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
