# Minimal Cross-Platform SDB Client in Rust

## Summary

Build a small, standalone Rust client that talks directly to a TV over TCP and
covers the operations needed by the initial setup workflow. It should not require
Tizen Studio or a background host service.

A useful first version supporting connection setup, capabilities,
non-interactive commands, and single-file transfer should be roughly
**800–1,500 lines of Rust**.

Estimated effort:

- **2–4 days** for a working version against the current TV.
- **4–7 days** with package installation, automated tests, and releases for
  macOS, Windows, and Linux.
- **1–2 weeks** for broader firmware/model compatibility and polished error
  handling.

Rust and desktop portability are straightforward. The main effort is matching
Samsung's protocol details and connection lifecycle.

## Recommended scope

Provide a `tv-sdb` executable with these core commands:

```text
tv-sdb connect <tv>[:port]
tv-sdb -s <tv>[:port] capability
tv-sdb -s <tv>[:port] shell <command...>
tv-sdb -s <tv>[:port] push <local-file> <remote-file>
tv-sdb -s <tv>[:port] pull <remote-file> <local-file>
```

Then add thin convenience commands:

```text
tv-sdb -s <tv>[:port] install <package.wgt|package.tpk>
tv-sdb -s <tv>[:port] uninstall <app-id>
tv-sdb -s <tv>[:port] launch <app-id>
```

Defaults and options:

- Port `26101` unless specified.
- `--timeout <seconds>`.
- `--verbose` for frame-level diagnostics without dumping transferred files.
- `-s` selects the destination for that invocation.
- `connect` verifies the connection and prints the device banner; subsequent
  commands establish their own connections.

This is enough to support existing setup scripts using the familiar `connect`
followed by `-s … push` pattern.

### Excluded from v1

- USB support.
- Automatic TV discovery.
- A persistent local SDB server.
- Interactive terminal handling.
- Recursive directory transfer.
- Port forwarding.
- Complete command-line compatibility with every SDB option.
- Implementing the TV-side daemon.

Those features account for most of the complexity without helping the initial
setup workflow.

## Repository structure

Add a reusable library and a small CLI:

```text
crates/tv-sdb/
  Cargo.toml
  src/
    lib.rs          # Public Device API and error types
    frame.rs        # Outer packet encoding/decoding
    connection.rs   # TCP connection, session setup, channels
    negotiate.rs    # Client identity and negotiation variants
    sync.rs         # STAT/SEND/RECV/DATA/DONE protocol
    package.rs      # Archive metadata and package workflows

bins/tv-sdb/
  Cargo.toml
  src/main.rs       # Argument parsing, output, exit codes
```

Add both to the workspace's `default-members` so the existing host-side
formatting, linting, and test commands include them.

Keep the public API independent of the CLI:

```rust
Device::connect(target, options)
Device::capabilities()
Device::shell(command, output)
Device::push(local_path, remote_path)
Device::pull(remote_path, local_path)
Device::install(package_path)
Device::uninstall(app_id)
Device::launch(app_id)
```

Use blocking `std::net::TcpStream` and sequential state machines. There is no
need for Tokio, and this matches the existing workspace style.

## Implementation plan

### 1. Start with a connection spike

Before building the full CLI, implement just enough to:

1. Open TCP port `26101`.
2. Complete session negotiation.
3. Read the device banner.
4. Open `capability:`.
5. Print the returned capabilities.
6. Close the channel and socket cleanly.

This confirms compatibility early and determines which negotiation paths are
necessary.

Use two references for different purposes:

- [TizenTeam/sdb](https://github.com/TizenTeam/sdb): canonical framing and
  file-transfer behavior.
- [PatrickSt1991/tizen-sdb](https://github.com/PatrickSt1991/tizen-sdb): a
  direct TV client showing practical connection behavior.

The original C sources should take precedence where the implementations differ.
Preserve the relevant license notices when porting implementation details.

### 2. Implement the outer frame codec

Each frame has a **24-byte header containing six little-endian `u32` fields**:

```text
command
arg0
arg1
data_length
data_checksum
magic
```

Implement:

- `CNXN`, `OPEN`, `OKAY`, `WRTE`, and `CLSE`.
- `data_checksum` as the additive byte sum used by SDB.
- `magic = command ^ 0xffffffff`.
- Exact reads for headers and payloads, since TCP may split them arbitrarily.
- Validation of magic, checksum, command, and payload length.
- A hard payload allocation limit before allocating incoming buffers.
- Protocol version `0x01000000` and an initial maximum payload of `4096`,
  reduced according to the peer's response.

Keep this module independent from TCP so it can be tested with byte slices and
in-memory streams.

### 3. Add session negotiation and channel handling

Encapsulate negotiation in `negotiate.rs`:

- Handle the direct `CNXN` response.
- Port the standard client-identity exchange used by the existing direct
  client.
- Load or create the client identity in a per-user configuration directory.
- Return a clear `UnsupportedNegotiation` error for unrecognized variants rather
  than leaving the connection hanging.

For channels:

- Allocate nonzero local IDs.
- Send `OPEN(local_id, 0, service + NUL)`.
- Accept `OKAY` as success and `CLSE` as rejection.
- Track the peer's channel ID.
- Split outgoing `WRTE` payloads to the negotiated maximum.
- Wait for the corresponding `OKAY` after each outgoing `WRTE`.
- Acknowledge every incoming `WRTE`.
- Treat `CLSE` as end-of-stream and make close handling idempotent.
- Apply connection, negotiation, and operation timeouts.

**Only one active channel per device connection is needed for v1.** Commands and
file transfers can run sequentially. This avoids building a general concurrent
multiplexer.

A fresh-connection retry is reasonable when negotiation ends before any command
starts. Do not automatically repeat a command or transfer after it has begun.

### 4. Implement capabilities and non-interactive commands

For `shell`:

- Open `shell:<command>`.
- Stream returned bytes directly to stdout.
- Finish when the channel closes.
- Keep diagnostics on stderr.
- Do not invent an exit status when the service does not provide one.

For `capability`:

- Open `capability:`.
- Parse `key:value` lines into a map.
- Preserve the raw response for display and diagnostics.
- Use values such as `sdk_toolpath` and transfer capabilities in later
  operations.

The CLI should pass the command through rather than trying to interpret the TV's
supported command vocabulary.

### 5. Implement single-file sync

Open `sync:` and implement the legacy sync records, each consisting of a
four-byte identifier, a little-endian `u32`, and optional payload.

For `push`:

1. Optionally send `STAT` to determine whether the destination is a directory.
2. Send `SEND` with `remote-path,mode`.
3. Use a regular-file mode such as `0o100644`; do not copy platform-specific
   host metadata.
4. Stream the file using `DATA` records of at most 64 KiB.
5. Let the channel layer split those records into outer frames.
6. Send `DONE` with the file modification time.
7. Require `OKAY`, or return the message carried by `FAIL`.
8. Close the channel without waiting indefinitely.

For `pull`:

1. Send `RECV` with the remote path.
2. Accept repeated `DATA` records until `DONE`.
3. Handle `FAIL` and unexpected records explicitly.
4. Write to a temporary local file and rename it only after successful
   completion.

Initially accept files only. Directory recursion, links, timestamp skipping, and
special files can remain unsupported.

One detail to check during TV testing: the C# reference omits the `,mode` suffix
on `SEND`, while the original SDB implementation includes it. **Use the original
form first** rather than copying that shortcut.

### 6. Add package convenience commands

Keep these as compositions of the primitives above.

#### Install

1. Validate the `.wgt` or `.tpk` extension.
2. Open the archive locally.
3. Read the application/package identifier:
   - WGT: `config.xml`.
   - TPK: `tizen-manifest.xml`.
4. Obtain `sdk_toolpath` from capabilities, with the usual SDB staging directory
   as fallback.
5. Normalize the remote basename so whitespace cannot alter command arguments.
6. Push the package.
7. Run the standard `vd_appinstall` request through the command channel.
8. Stream progress and return failure when the response reports an unsuccessful
   installation.

#### Uninstall and launch

- Add `vd_appuninstall` and `was_execute` wrappers.
- Keep them separate from the transfer implementation.
- Do not add signing, profile generation, or certificate management to this
  tool.

These commands should be a second commit after connection, shell, and push are
proven.

## Testing strategy

### Unit tests

- Golden byte tests for every outer frame type.
- Fragmented and coalesced TCP input.
- Invalid magic, checksum, command, and oversized payload.
- Channel ID mapping and close-before-open-completion.
- Write acknowledgement ordering.
- Sync records, empty files, multi-chunk files, and `FAIL` responses.
- Package identifier extraction and remote filename normalization.

### Fake TV server

Build a small test-only `TcpListener` server that performs negotiation and
scripted channel exchanges. Use it to test:

- Successful connection and capability requests.
- Early disconnects and retry behavior.
- Command output split across many frames.
- Large uploads and downloads.
- Remote close during transfer.
- Timeouts without hanging the CLI.

### Real-TV compatibility checks

Compare with Samsung's client on the same TV:

1. Banner and capability output.
2. A simple read-only command.
3. Empty, small, and multi-megabyte file transfers.
4. A known-good test package installation, launch, and removal.
5. An existing setup script using `SDB=/path/to/tv-sdb`, if command-line
   compatibility is included.

Test at least one operation immediately after another; quick reconnects are a
known area where TV-side behavior can differ.

### Desktop CI

Run tests and release builds on:

- `macos-latest`
- `ubuntu-latest`
- `windows-latest`

The implementation should use only portable networking and filesystem APIs,
with small platform-specific handling for local file permissions and
configuration paths.

## Suggested delivery sequence

1. **PR 1:** Frame codec, connection, negotiation, `connect`, and `capability`.
2. **PR 2:** Channels and non-interactive `shell`.
3. **PR 3:** Single-file `push` and `pull`.
4. **PR 4:** `install`, `uninstall`, and `launch`.
5. **PR 5:** Setup-script integration, documentation, and desktop release
   artifacts.

This provides a series of small, testable milestones and gets the useful setup
functionality working before any convenience features are added.

## Implementation notes (2026-11-11)

Implemented as `crates/tv-sdb` (library) and `bins/tv-sdb` (CLI, package
`tv-sdb-cli`). Verified against the QE55S95B (sdbd 2.2.31, plugin 3.8.5) and a
proxy capture of Samsung's own `sdb`. Where the TV disagreed with the plan, the
code follows the TV:

- **No identity exchange.** The TV answers `CNXN` directly (version
  `0x02000000`, max payload 262044). An `AUTH` reply returns an
  `unsupported negotiation` error; RSA key auth is not implemented.
- **Max payload 256 KiB**, like Samsung's client, instead of 4096: each `WRTE`
  waits for an `OKAY`, so small frames make transfers latency-bound (5 MB push
  ≈ 1.2 s).
- **`SEND` mode is `0o100755`**, which is what Samsung's client always sends;
  `dropbear/install-dropbear.sh` relies on pushed files staying executable.
- **Install uses `0 appinstall <tpk|wgt> <basename>`** (pkgcmd) after pushing to
  `/home/owner/share/tmp/sdk_tools/`, then `0 rmfile "<path>"`, exactly as
  `sdb install` does. No archive parsing is needed, so there is no zip
  dependency. Success means pkgcmd printed `key[end] val[ok]`.
- **Uninstall uses `0 appuninstall <pkgid>`** (also what `sdb uninstall`
  sends). **Launch uses `0 execute <appid>`** (app_launcher; success prints
  `successfully launched`). `0 was_execute` remains reachable through `shell`.
- **`pull` is whitelisted by sdbd**: only paths such as
  `/opt/usr/apps/<pkg>/author-signature.xml` are allowed; everything else
  returns `FAIL You cannot pull files from this path.`
- **`shell:` is a verb vocabulary**, not a shell: `shell echo hi` returns
  nothing. Use verbs such as `0 vd_applist` or `0 getduid`.

Not done yet: desktop CI and release artifacts (PR 5); Windows and Linux builds
are untested but use only `std` networking and filesystem APIs.
