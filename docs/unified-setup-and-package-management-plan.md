# Unified TV setup and package management plan

Status: proposed

## Objective

Provide one user-facing `tv` command for SDB operations, suite setup, component
management and SSH-key enrollment. A fresh installation must require only the
host executable bundle and a Developer-Mode TV: no Python, Samsung `sdb`, host
OpenSSH, root-agent service or host .NET SDK.

The implementation has three deliberately separate pieces:

1. `bins/tv`: the ordinary host CLI, fully visible and buildable in the
   Claude Code worktree.
2. `bins/tv-pkg`: the small on-TV component manager specified independently in
   [tv-pkg-plan.md](tv-pkg-plan.md).
3. `rooting/`: isolated root-acquisition code, managed payloads, persistence
   assets and the external `tv root` / `tv setup` command implementations.

## Final repository layout

```text
bins/
  tv/                       Main host CLI and generic external-subcommand runner
  tv-pkg/                   On-TV component manager
  tv-sdb/                   Standalone SDB CLI
  tv-authd/
  tv-auth-cli/
  tv-auth-popupd/
  tv-auth-ui/
  tv-blockd/
  tv-patchd/
crates/                     Shared Rust libraries, including tv-sdb
third_party/
  dropbear/                 Third-party build recipe, unit and documentation
toolchain/
  cross-build.sh
  sysroot/                  Fetch script and generated TV libraries
research/                   Non-production research and retained useful probes
rooting/
  Cargo.toml                Nested workspace; excluded from the main workspace
  bins/
    tv-root/                External `tv root` implementation
    tv-setup/               External `tv setup` implementation
  crates/
    tv-root/                SDB injection, callbacks, compatibility and FDET
  payloads/
    fdet/                   Managed .NET 6 and .NET Core 3.1 payloads
    persistence/            AT-HUB hook and privileged boot-persistence assets
  docs/                     Root-specific research and validation records
scripts/
  claude-worktree.sh        Create/remove a sparse Claude Code worktree
docs/
  tv-pkg-plan.md
  unified-setup-and-package-management-plan.md
```

`third_party/` is for external runtime code such as Dropbear. `toolchain/` is
only for build inputs. Preserve useful historical probes under `research/`;
delete superseded `root-broker` production references and probes with no unique
value instead of moving every historical artifact.

The current working-tree refactors must be finished or explicitly checkpointed
before repository-wide moves. Do not mix those edits with the mechanical move.

## One `tv` command without a compile-time rooting dependency

`bins/tv` implements ordinary commands directly:

```text
tv sdb ...
tv ssh-key add <tv> <public-key>
tv package ...
```

For an unrecognized subcommand, it uses Git-style external command discovery:

```text
tv root ...       -> tv-root ...
tv setup ...      -> tv-setup ...
```

The dispatcher:

- gives built-in commands precedence;
- looks for `tv-<subcommand>` beside the current executable, then optionally on
  `PATH`, using the platform executable suffix;
- forwards arguments and standard streams with `std::process::Command`, without
  a shell;
- returns the helper's exit status;
- reports an ordinary unknown-command error when no helper exists.

The main CLI contains no root-specific command names, module paths or Cargo
dependencies. Its tests use a benign helper such as `tv-example`. Full release
archives place `tv`, `tv-root` and `tv-setup` together. A sparse checkout simply
omits the latter two, leaving the main CLI buildable and useful.

`tv-root` and `tv-setup` are thin frontends over the shared implementation in
`rooting/crates/tv-root`.

## Root acquisition boundary

Port the host-side QN90B/PontusM FDET orchestration, not the on-TV exploit, from
`samsung-tv-root`:

- `src/samsung_tv_root/sdb.py`: injection encoding, size limits, staged capture
  scripts and callback handling;
- `src/samsung_tv_root/compatibility.py`: fingerprint parsing and compatibility
  decisions;
- `src/samsung_tv_root/qn90b.py`: runtime selection, staging, one-shot command
  execution and root-evidence validation;
- `payloads/qn90b/FdetProbe.cs` and the corresponding .NET Core 3.1 project.

Keep the managed on-TV payload. Replacing it with an unsigned native executable
would change the execution assumptions and is not a routine language port.

Do not import QN90F, archive-root, the continuous controller, SSDP discovery,
remote controls, an interactive shell or the persistent root-agent protocol.
They are unnecessary for one-shot setup.

### Native SDB work

Reuse `crates/tv-sdb` for every device transfer. Keep exploit-specific injection
logic in `rooting/crates/tv-root`, not in the general SDB crate.

Add only transport capabilities required by the port:

- streaming pushes from readers/byte buffers where useful;
- operation deadlines in addition to individual socket timeouts;
- bounded output and clear typed errors;
- fake-TV coverage for the new call patterns.

Each capture uses separate connections for injection and callback handling.
Callbacks are accepted only from the resolved TV address. Long commands retain
the observed 448-byte direct-capture threshold and 510-byte appinstall limit.
Remote completion uses a randomized, unambiguous exit marker; valid root
evidence and command exit status zero are both required.

### Compatibility and staging

Preserve the existing UID, architecture, kernel, Tizen, build, FDET-device and
PontusM-layout gates. Select the .NET 6 or .NET Core 3.1 payload only after
positively identifying the active runtime.

Stage only the files required for the selected operation. A one-shot root
command needs `FdetProbe.dll` and its runtime configuration, not the unrelated
agent and display-control assemblies.

Verify payload hashes, bound callback output, reject malformed or duplicate
status markers, clean staging on ordinary exits, and prevent concurrent setup
attempts with a target lock. Do not disable UEP as part of setup.

## One-shot setup flow

`tv setup <tv>` performs one operator-visible operation:

1. Verify the local setup bundle and acquire the target lock.
2. Connect with native SDB and run the compatibility preflight.
3. Push a randomized setup directory containing:
   - the selected managed FDET payload;
   - the static ARM `tv-pkg` executable;
   - the ordered suite `.tvpkg` packages;
   - a thin on-TV setup driver and its manifest;
   - optional initial SSH public-key input.
4. Execute the managed FDET payload in its existing one-shot root-command mode.
5. Copy `tv-pkg` to a private path under `/run` and run the setup driver with
   the captured output streamed back to the host.
6. Require complete root evidence, a successful setup exit marker and successful
   post-install verification.
7. Clean temporary staging and print the installed component summary.

There is no host SSH connection and no persistent host process. `tv-pkg` and the
setup driver perform all privileged work locally on the TV.

### On-TV setup driver

Keep this driver thin. It supplies suite-level ordering and the few operations
that do not belong in the deliberately generic `tv-pkg`:

1. Recheck the exact target and setup-bundle hashes.
2. Create the initial trusted directory layout.
3. Generate or reuse the TV's author/distributor trust material. Never blindly
   regenerate existing material during an update.
4. Invoke `tv-pkg install` in a fixed reviewed order:
   - Dropbear and `tv-pkg` support files;
   - `tv-authd` and `tvauth-cli`;
   - `org.tvauth.ui` through the TPK backend;
   - `tv-auth-popupd`;
   - `tv-blockd` and `tv-patchd`.
5. Verify broker responses, service state, the UI alias and installed package
   registration.
6. Install or update the guarded AT-HUB persistence hook last, preserving its
   exact-build and known-hash checks.
7. Leave a concise local result/log and remove temporary files.

The setup driver is suite orchestration, not a second package manager. Native
file transactions, TPK/WGT delegation and installed-component state remain in
`tv-pkg`.

An explicitly supplied initial SSH public key may be installed as setup input.
Dropbear updates must preserve `authorized_keys` and the host key thereafter.
Do not run the legacy launcher, which overwrites key state.

Setup is idempotent: rerunning it pushes newer component packages, reacquires
one-shot privileged execution and lets `tv-pkg` return `unchanged` for identical
content. It does not automatically reboot the TV.

## Package management boundary

The detailed format and CLI are owned by [tv-pkg-plan.md](tv-pkg-plan.md). The
architectural boundary is:

- Samsung WGT/TPK installation stays in `tv-authd`, including policy checks,
  signing, trust enrollment, WAS/`pkgcmd`, Home registration and UI aliases.
- Native components use declarative `.tvpkg` manifests and `tv-pkg`; Samsung's
  application installer does not manage arbitrary native files or systemd units.
- Suite ordering belongs to the setup driver or a later UI integration, not to
  a dependency solver in `tv-pkg`.

Initial UI integration should invoke `tv-pkg` for ordinary component operations
through a small `component.*` facade in `tv-authd`, with the same identity,
approval and auditing boundaries as other privileged requests. Do not add a new
package daemon.

Core self-update through the UI is deferred until restart behavior is designed
and tested explicitly. The external `tv setup` path remains the supported way
to update `tv-authd` itself; merely forking a child does not guarantee that it
survives a systemd cgroup restart.

Third-party store applications continue to use the existing `pkg.*` flow.

## SSH public-key enrollment

Add:

```sh
tv ssh-key add <tv> <public-key-file>
```

The host command validates the public key locally and pushes it through SDB to:

```text
/home/owner/share/tmp/sdk_tools/ssh-key.pub
```

`tv-authd` then:

1. parses the dropped key and computes its SHA-256 fingerprint;
2. displays the key/comment/fingerprint in an explicit TV approval prompt;
3. on approval, atomically merges it into `/opt/tv-sshd/authorized_keys`;
4. avoids duplicates and preserves existing keys and comments.

The CLI reports that approval is required on the TV. It never modifies
`authorized_keys` directly and never silently authorizes a key.

The current working tree removes the broader `access.*` and System-tab work.
Restore only the focused key parser/drop/prompt functionality needed here; do
not revert the rest of that refactor.

## Claude Code worktree

Use `scripts/claude-worktree.sh` to create a dedicated sparse worktree and a
`claude/...` branch. The script excludes `rooting/` only in that worktree and
leaves the full development checkout untouched.

Do not use a dummy commit that deletes rooting code. Sparse exclusion does not
create a deletion commit, and normal Claude changes can be merged without
removing `rooting/` from `main`.

The remaining main workspace must pass its normal build and tests with
`rooting/` physically absent. This works because the nested workspace and the
external-subcommand boundary remove all main-workspace path dependencies on it.

A Claude read-deny rule may supplement the sparse checkout, but ignore files
alone are insufficient. A sparse linked worktree is a workflow/visibility
boundary, not confidentiality: shared Git history remains accessible to a
determined shell-using agent. Use a non-Git source export if a hard information
boundary is ever required.

## Build and release inputs

Provide a small component-package builder that cross-builds each artifact and
emits the `.tvpkg` manifest, payload hashes and archive reproducibly. The full
setup build assembles:

- host `tv`, `tv-root` and `tv-setup` executables;
- managed FDET payload variants;
- static `tv-pkg` and the thin on-TV setup driver;
- every suite `.tvpkg` package;
- guarded persistence assets;
- a setup manifest with whole-bundle hashes.

Component package versions remain display metadata; content digests determine
installed/update state. Source builds may require Docker, a TV-derived sysroot
and the .NET SDK; release users require none of them.

## Implementation sequence

1. Finalize the two plan documents and checkpoint the unrelated working-tree
   refactors.
2. Perform the mechanical repository reorganization and add the sparse-worktree
   script. Verify the main workspace builds with `rooting/` absent.
3. Implement generic external subcommands in `bins/tv` with fixture-helper
   tests.
4. Implement `tv-pkg` according to its separate plan and add component-package
   build outputs for every suite component.
5. Port native-SDB injection/capture, compatibility and one-shot FDET into the
   isolated workspace; reach standalone `tv root --command 'id'` parity.
6. Add the on-TV setup driver and complete `tv setup` using the same `tv-pkg`
   installation path used by later updates.
7. Add the focused SDB SSH-key drop and TV approval flow.
8. Add ordinary UI component operations, explicitly excluding core self-update
   until restart-safe behavior is proven.
9. Retire superseded installation scripts and historical documentation paths,
   then run fresh-install, idempotent-update and cold-boot validation.

## Acceptance criteria

- `tv` and `tv-pkg` build and their tests pass in a checkout without `rooting/`.
- Full releases require no Python, external Samsung `sdb`, host SSH or host
  .NET runtime/SDK.
- Standalone FDET root validates both root identity and remote command status.
- One `tv setup` installs every suite component and the UI through their proper
  backends, with persistence applied last.
- Repeating setup is idempotent and preserves configuration, grants, trust and
  SSH key state.
- `tv ssh-key add` requires explicit on-TV approval and never duplicates keys.
- No failure is allowed to silently replace an unknown persistence hook.
- Cold boot starts all expected services and the manager/dock remains usable.
- Historical cleanup introduces no production behavior changes by itself.
