# `rooting/`: isolated root acquisition and one-shot setup

This nested Cargo workspace is intentionally excluded from the main workspace
and from sparse Claude Code checkouts. The only interface the ordinary host CLI
uses is the external-command boundary:

```text
tv root ...   -> tv-root ...
tv setup ...  -> tv-setup ...
```

## Layout

```text
bins/tv-root/          one-shot FDET root command
bins/tv-setup/         one-shot suite setup
crates/tv-root/        compatibility, injection/capture, FDET, bundle, setup
payloads/fdet/         managed .NET 6 and .NET Core 3.1 FDET payload
payloads/setup/        thin on-TV setup driver
payloads/persistence/  AT-HUB hook, guarded apply script, unit sources
packages/              reviewed component specs and fixed order
build-suite-bundle.sh  assemble component packages and the setup bundle
docs/                  hardware-validation records and remaining checks
```

## Host checks

```sh
cd rooting
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

Tests use temporary directories, a scripted in-process sdbd and a fake setup
transport. They do not contact a TV, run privileged installers or apply
persistence.

## `tv root`

```sh
tv-root <tv> --check
tv-root <tv> --payload-dir <dir> --command 'id'
```

The command connects with native SDB, runs the compatibility probe, selects
the .NET 6 or .NET Core 3.1 payload only after positively identifying the
runtime, stages only `FdetProbe.dll` and `FdetProbe.runtimeconfig.json`, runs
one command through FDET, requires root identity and exit status zero, and
removes staging.

## `tv setup`

```sh
tv-setup <tv> --bundle <dir> [--key <public-key>]
```

The host verifies every bundle hash, takes a per-target lock, runs the
compatibility preflight and stages the managed payload, `tv-pkg`, the packages,
the setup driver, the plan and an optional initial public key. One-shot FDET
root runs the driver; the driver installs components in the fixed order through
`tv-pkg` and applies the guarded persistence hook last. No host SSH connection
or persistent root agent is used.

## Payload builds

The managed payload source is in `payloads/fdet/`. Build the modern and legacy
assemblies separately, then place them in the paths consumed by
`build-suite-bundle.sh`:

```text
payloads/fdet/out/modern/FdetProbe.dll
payloads/fdet/out/modern/FdetProbe.runtimeconfig.json
payloads/fdet/out/legacy/FdetProbe.dll
payloads/fdet/out/legacy/FdetProbe.runtimeconfig.json
```
