# `tv-pkg`: minimal component package manager

Status: proposed

## Summary

`tv-pkg` is a small, on-demand command-line tool that installs, reinstalls,
lists and removes the suite's components. One invocation handles one local
package; there is no daemon, network client, catalog or dependency solver.

It supports two package kinds:

- **native**: declared files, directories, symlinks and systemd units;
- **TPK/WGT**: delegates installation and removal to `tv-authd`, retaining its
  validation, signing, WAS/`pkgcmd` and Samsung Home behavior.

The initial implementation lives in `bins/tv-pkg/` as a library plus a thin CLI.
Do not add a shared crate until another program actually needs one.

## Goals

- Install a local component package with one command.
- Make repeated installation of identical content a no-op.
- Allow development reinstalls when the version is unchanged but content differs.
- Update native files atomically and restart their systemd units.
- Preserve every file not explicitly owned by the component.
- Use the existing `tv-authd` package pipeline for TPK/WGT artifacts.
- Keep the core logic host-testable with temporary directories and fake commands.

## Non-goals

- No background service or new IPC protocol.
- No downloading, repositories, catalogs or update checks.
- No dependency resolution; callers install components in the required order.
- No arbitrary install scripts or package-provided shell snippets.
- No new package-signing infrastructure. Hashes verify integrity; the caller
  remains responsible for trusting and approving the package source.
- No general snapshot manager, downgrade policy or configuration editor.
- No support for replacing Samsung platform packages or arbitrary system paths.

UI and host-tool integration can later invoke this CLI. That integration is
intentionally outside this plan.

## Package format

A package is a ZIP archive with the `.tvpkg` suffix:

```text
component.json
payload/...
```

The manifest has a `format` version, a stable component `id`, a display
`version`, and a `kind` of `native`, `tpk` or `wgt`. Versions are informational;
the SHA-256 of the manifest and payload determines whether content changed.

### Native example

```json
{
  "format": 1,
  "id": "tv-blockd",
  "version": "0.1.0",
  "kind": "native",
  "directories": [
    {
      "path": "/opt/tv-suite/bin",
      "mode": "0755",
      "smack": "System::Shared"
    }
  ],
  "files": [
    {
      "source": "payload/tv-blockd",
      "destination": "/opt/tv-suite/bin/tv-blockd",
      "sha256": "<64 lowercase hex characters>",
      "mode": "0755",
      "smack": "System::Shared"
    }
  ],
  "units": [
    {
      "source": "payload/tv-blockd.service",
      "name": "tv-blockd.service",
      "sha256": "<64 lowercase hex characters>"
    }
  ],
  "undo_command": [
    "/run/opt/tv-suite/bin/tv-blockd",
    "undo"
  ]
}
```

Native manifests may also contain:

- `directories`: explicit path, mode and SMACK label;
- `links`: absolute link path and a relative target, needed for Dropbear's
  `dropbearkey -> dropbear` alias;
- `units`: files installed under `/opt/tv-units` with fixed UID/GID 0, mode
  `0600` and label `System::Shared`;
- one optional `undo_command`: an argv array executed without a shell before
  removal. There are no other lifecycle hooks.

All files are owned by UID/GID 0. Destination parents must be real directories,
not symlinks.

### TPK/WGT example

```json
{
  "format": 1,
  "id": "org.tvauth.ui",
  "version": "0.1.0",
  "kind": "tpk",
  "package": {
    "source": "payload/org.tvauth.ui-0.1.0.tpk",
    "sha256": "<64 lowercase hex characters>",
    "pkgid": "org.tvauth.ui"
  }
}
```

The component ID and `pkgid` must match. `tv-pkg` verifies the archive, extracts
the inner package into a private temporary directory accepted by `tv-authd`,
and sends the existing `pkg.install` or `pkg.uninstall` request through
`tv-proto`. It does not inspect, sign or install TPK/WGT contents itself.

## CLI

```text
tv-pkg verify <package.tvpkg> [--json]
tv-pkg install <package.tvpkg> [--adopt-existing] [--json]
tv-pkg list [--json]
tv-pkg status <component-id> [--json]
tv-pkg remove <component-id> [--json]
```

- `verify` parses and validates without changing the device.
- `install` handles fresh installs, content updates and identical-content
  no-ops.
- `--adopt-existing` is a one-time migration aid for files that already exist
  but have no recorded `tv-pkg` ownership. It is never implied by ordinary
  installation.
- `list` reads local state only.
- `status` reports the recorded content digest and current owned-file/unit
  health.
- `remove` refuses unexpectedly modified owned files rather than deleting them.

JSON output is a small versioned object with `status`, `id`, `version`,
`digest`, optional `changed` and optional `error`. Do not add a general RPC
schema beyond what these commands need.

## Installed state

Use one JSON file per component:

```text
/opt/tv-suite/var/tv-pkg/components/<id>.json
```

It records the manifest digest, kind, version, owned destinations and their
installed hashes, unit names, and link/directory declarations. Writes use a
temporary sibling plus atomic rename.

There is no database. State files are UID/GID-0 owned and not writable by
applications. Configuration, grants, keys, certificates and other unlisted
files are never component-owned merely because they share a directory.

## Native installation algorithm

1. Acquire one non-blocking process lock at `/run/tv-pkg.lock`; report `busy`
   rather than queueing.
2. Read the ZIP with entry-count, expanded-size and manifest-size limits.
   Reject absolute paths, `..`, duplicate entries, links and unknown manifest
   fields.
3. Validate every destination against the allowlist:
   `/opt/tv-suite`, `/opt/tv-auth`, `/opt/tv-sshd` and `/opt/tv-units`.
4. Hash every payload file before stopping anything. If the recorded digest is
   unchanged and all owned files and units are healthy, return `unchanged`.
5. Compare existing destinations with the previous state. Missing files on a
   fresh install are fine; unexpected or modified files fail closed unless the
   operator supplied `--adopt-existing`.
6. Create a private transaction directory on `/opt`, stage verified files there,
   and apply final ownership, modes and labels.
7. Stop only the declared units that are running. Rename replaced files into a
   transaction-local backup, then atomically rename staged files into place.
8. Copy changed unit files to `/run/systemd/system`, run `daemon-reload` once,
   and start or restart the declared units.
9. Verify unit state and all installed hashes, then atomically write the new
   component state.
10. Remove the transaction directory. On an ordinary failure after mutation,
    restore the transaction-local backups and previous unit state before
    returning the error.

This is a per-component transaction, not an all-suite transaction.

## TPK/WGT algorithm

1. Validate the manifest and inner archive hash.
2. Return `unchanged` only when the recorded digest matches and `tv-authd`
   reports the expected installed package.
3. Call `pkg.install` for installation or `pkg.uninstall` for removal through
   the existing broker socket.
4. Record or remove local component state only after a successful broker
   response.
5. Clean the temporary extraction directory on every exit path.

Any UI-alias handling, signing, trust enrollment and Samsung Home registration
stays in `tv-authd`.

## Removal

For native components:

1. Validate current owned-file hashes.
2. Run the optional `undo_command`, if declared.
3. Stop declared units and remove their runtime unit copies.
4. Remove only recorded files, links and unit sources. Leave non-empty
   directories and every unlisted file untouched.
5. Run `daemon-reload` once and remove the component state.

For TPK/WGT components, delegate removal to `tv-authd` before removing local
state. Protected-component policy remains the caller's responsibility in the
initial version.

## Implementation layout

```text
bins/tv-pkg/
  Cargo.toml
  src/
    main.rs          argument parsing and output
    lib.rs           host-testable API
    manifest.rs      strict manifest types and validation
    archive.rs       bounded ZIP reading and payload hashing
    state.rs         installed state and health checks
    native.rs        native transactions and filesystem operations
    systemd.rs       small command-backed systemd adapter
    tizen.rs         tv-authd package backend
```

Use synchronous standard-library code and existing workspace dependencies where
possible. The systemd adapter should accept an executable runner trait so tests
can assert commands without a real system manager.

## Tests

Host tests use temporary destination roots and fake systemd/broker adapters:

- manifest and ZIP validation, including traversal and expansion limits;
- fresh native installation and exact metadata;
- identical-content no-op and same-version changed-content reinstall;
- unexpected/modified destination refusal and explicit adoption;
- failure before mutation leaves the live tree untouched;
- injected failure after mutation restores files and units;
- symlink-parent rejection and relative-link validation;
- unlisted configuration and key files survive install and removal;
- native removal executes only declared undo/stop/remove operations;
- TPK/WGT requests use the correct broker method, type, path and package ID;
- state is written only after success and malformed state fails closed;
- concurrent installation returns `busy`.

Run Linux integration tests for filesystem and process behavior in the existing
container-based test setup. Validate representative native and TPK packages on
a TV only after the host suite passes.

## Delivery steps

1. Implement manifest/archive parsing, state, `verify`, `list` and `status`.
2. Implement native installation with fake-runner tests, then systemd support.
3. Implement removal and transaction-local restoration.
4. Add the TPK/WGT broker backend.
5. Build the static ARM binary, test representative components, and document
   the package format with one example per kind.

Each step should leave a small, usable CLI; do not build speculative UI,
repository or orchestration features into `tv-pkg`.
