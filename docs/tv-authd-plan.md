# `tv-authd`: a small SuperSU-like root broker for Samsung Tizen TVs

*Status: M0 platform spikes, M1 headless broker, and M2 approval UI are complete; M1 and M2 are cold-boot validated on the target. Next milestone: M3 Vetro cutover. Initial target: the S95B/PontusM, Tizen 6.5, build `T-PTMDEUC-1720.7` documented in this repository.*

## 1. Summary

Build a persistent root daemon that lets TV applications execute root shell commands after the user approves the requesting application on the TV.

The authorization model is deliberately close to SuperSU/Magisk:

- an application asks for root;
- the TV displays an approval prompt;
- the user selects **Deny**, **Allow once**, or **Always allow**;
- **Allow once** authorizes one command;
- **Always allow** authorizes subsequent root commands from the same application until revoked.

This is **not** a fine-grained permissions framework. The daemon does not need to know what “disable a package”, “apply DNS settings”, or “change picture settings” means. Those remain ordinary shell commands issued by the consumer.

The main components are:

| Component | Proposed language | Purpose |
|---|---|---|
| `tv-authd` | **Rust** | Persistent root broker, caller identity, grants, prompts, shell execution |
| Approval/manager UI | **Managed C#/.NET with minimal EFL P/Invoke** | On-TV prompt and grant revocation, running without root |
| Vetro adapter | **JavaScript in Vetro's existing Node process** | Replace root-agent transport with direct Unix-socket calls |
| Wire protocol | Newline-delimited JSON over a Unix stream socket | Easy interoperation between Rust and Node |
| Boot integration | Existing gawk hook plus a systemd unit | Start and restart the daemon independently of Vetro/SDB |

Rust remains the choice for the privileged daemon: it can be built as a static ARMv7 binary and run from `/run` with systemd, as validated on the target.

The approval UI cannot use a custom native ELF packaged in `/opt/usr/apps`: UEP prevents launchpad from executing unsigned native binaries from writable app storage, even when the TPK itself has valid author/distributor signatures. The UI must therefore be a managed Tizen .NET application (`netcoreapp3.1`, `type="dotnet"`), which runs through Samsung's signed apphost. Its small C# layer calls the required EFL functions directly; this path was installed, launched, and displayed above YouTube on the target.

## 2. Goals and non-goals

### Goals

1. Start a root broker automatically through the repository's existing AT-HUB persistence mechanism.
2. Let a local application request execution of an arbitrary root shell command.
3. Identify the requesting application using kernel-provided credentials, principally its SMACK package label.
4. Ask on the TV before the first request, with allow-once and persistent allow decisions.
5. Provide a small manager UI to list and revoke persistent grants.
6. Let Vetro keep using shell commands while replacing its TV-local SDB/root-agent transport.
7. Leave SDB Host PC IP set to a real computer; routine Vetro operation must not need SDB or a connected PC.

### Non-goals

- Per-operation permissions, action providers, plugin APIs, or a generic policy language.
- A network-facing root service.
- Interactive shells, PTY forwarding, or a complete `su` command-line clone in the first version.
- Protecting against an already-root process or an application that was itself granted root.
- A compositor-backed, provably spoof-proof secure-attention UI.
- Removing every other privilege-escalation/confused-deputy path in Samsung's firmware.
- Rooting the TV or inventing a new persistence exploit. Root acquisition/installation is a prerequisite and remains separate.

## 3. Prior art and why the model fits

SuperSU and Magisk primarily provide **application-level authorization for a root shell**, not Android-style runtime permissions for individual shell commands.

Magisk's current implementation is useful prior art:

- its `su` client connects to `magiskd` over a local Unix socket;
- the daemon obtains kernel peer credentials and checks per-application policy;
- policy defaults to deny and is stored in a root-protected database;
- after approval, Magisk executes the requested shell command rather than translating it into high-level operations.

Sources:

- [Magisk internal details](https://topjohnwu.github.io/Magisk/details.html)
- [`su.cpp`: client and root-shell execution](https://github.com/topjohnwu/Magisk/blob/master/native/src/core/su/su.cpp)
- [`daemon.rs`: peer credentials, policy check, execution](https://github.com/topjohnwu/Magisk/blob/master/native/src/core/su/daemon.rs)
- [`db.rs`: per-application root policy](https://github.com/topjohnwu/Magisk/blob/master/native/src/core/su/db.rs)

Linux polkit is adjacent but solves a broader problem: privileged mechanisms expose named actions, while polkit and authentication agents authorize those actions over the system D-Bus. The daemon/authentication-agent split is useful inspiration; polkit's action/policy framework is more than this project needs.

Source: [polkit architecture](https://www.freedesktop.org/software/polkit/docs/0.107/polkit.8.html).

## 4. Architecture

```text
 Vetro service                  Another TV application
      │                                  │
      └────────────────┬─────────────────┘
                       │ exec(command, timeout)
                       ▼
               /run/tv-auth/broker.sock
                       │
                       ▼
                ┌─────────────┐
                │  tv-authd   │  root, System::Privileged
                │             │  owned by systemd
                │ peer identity│
                │ allow/deny   │──────► /opt/tv-auth/grants.json
                │ prompt queue │
                │ shell exec   │
                └──────┬───────┘
                       │ no persistent grant
                       │ fixed AUL launch
                       ▼
              ┌──────────────────┐
              │  tv-auth-ui      │  UID 5001, managed .NET/EFL, not root
              │  approval prompt │
              │  grant manager   │
              └────────┬─────────┘
                       │ same socket, UI-only messages
                       ▼
                    tv-authd
                       │
                       ▼
             /usr/bin/setsid /bin/sh -c <command>
```

There is one Unix socket. The daemon determines the caller's role from its peer SMACK label:

- ordinary packages may send `exec` requests;
- only the exact approval-ui package may send `ui.decision`, `grants.list`, and `grants.revoke` messages;
- root administration remains outside the broker's threat model.

A single socket is sufficient. Separate request/UI sockets can be added later only if implementation shows a concrete benefit.

## 5. Minimal protocol

Use newline-delimited JSON (NDJSON): each compact JSON object is followed by `\n`. JSON serializers escape literal newlines inside strings, so the delimiter is unambiguous. Keep maximum buffered frame sizes, initially 64 KiB for requests and 4 MiB for results (enough for two base64-encoded 1 MiB output streams). Reject malformed, oversized, or unknown-version frames. Readers must handle partial lines and multiple lines returned by one read; writers to a shared connection must serialize complete frames.

A length prefix would also work, but is unnecessary here. `SOCK_STREAM` does not preserve message boundaries, even when using `sendmsg()`/`recvmsg()`; those APIs add scatter/gather I/O and ancillary data such as file descriptors or credentials, not record framing.

Linux also has `SOCK_SEQPACKET` and `SOCK_DGRAM`, which do preserve message boundaries. They are a poor fit for this project because Node's built-in Unix-socket support is stream-only. Using them would require a native Node add-on, a helper process, or separate transports for Vetro and the UI. NDJSON over one stream socket is simpler, remains easy to inspect with ordinary socket tools, and works with Node's standard library.

The version-1 request API needs only:

```json
{
  "version": 1,
  "id": "client-generated-unique-id",
  "method": "exec",
  "command": "id",
  "timeout_ms": 10000
}
```

A successful execution response should map directly to Vetro's current contract:

```json
{
  "version": 1,
  "id": "client-generated-unique-id",
  "status": "ok",
  "exit_code": 0,
  "timed_out": false,
  "stdout_b64": "dWlkPTAocm9vdCkK",
  "stderr_b64": ""
}
```

Other `status` values are limited to:

- `denied` — user or policy denied the request;
- `ui_unavailable` — no approval could be obtained;
- `timeout` — execution timed out;
- `busy` — queue/concurrency limit reached;
- `error` — malformed request or execution setup failure;
- `unknown` — execution may have started but its outcome could not be established.

Command output is base64 so arbitrary bytes do not get corrupted by JSON UTF-8 conversion. This also matches Vetro's existing root-agent conventions.

UI-only messages:

- `ui.hello` — registers the approval UI connection;
- `ui.prompt` — daemon to UI: prompt ID, nonce, request ID, exact package ID and SMACK label, and command;
- `ui.decision` — `deny`, `once`, or `always`, with the prompt ID and unguessable prompt nonce;
- `grants.list` / `grants.revoke` — manager functionality.

The implemented v1 shapes are:

```json
{"version":1,"id":"hello-1","method":"ui.hello"}
{"version":1,"id":"decision-1","method":"ui.decision",
 "prompt_id":"32-hex","nonce":"32-hex","decision":"once"}
{"version":1,"id":"list-1","method":"grants.list"}
{"version":1,"id":"revoke-1","method":"grants.revoke",
 "label":"User::Pkg::org.example.app"}
```

The daemon sends prompts only to the authenticated UI connection:

```json
{"version":1,"type":"ui.prompt","prompt_id":"32-hex","nonce":"32-hex",
 "request_id":"app-request-1","label":"User::Pkg::org.example.app",
 "package":"org.example.app","command":"id","command_bytes":2,
 "command_truncated":false,"timeout_ms":60000}
```

The daemon generates prompt wording and derives the package identity from the
kernel-provided SMACK label. The requester cannot supply a trusted-looking app
name or approval description. The command is a sanitized display copy: control
and bidirectional formatting characters are replaced and long commands are
clearly marked as truncated. The prompt always says that the application is
requesting **full root access**. M2 shows the exact package ID and SMACK label
rather than a friendly package name; friendly names are not a trust input.

### UI IPC security

The Unix socket is local transport, not proof of identity by itself. Secure the UI channel as follows:

1. Keep `/run/tv-auth/` root-owned and non-writable by apps so another process cannot remove or replace `broker.sock`. The socket may be connectable by the shared `users` group; DAC only controls who can connect, not who is trusted.
2. On every accepted connection, read `SO_PEERCRED` and `SO_PEERSEC` **before accepting any frames**. Bind that identity to the connection for its entire lifetime.
3. Accept `ui.hello`, `ui.decision`, `grants.list`, and `grants.revoke` only from the exact installed approval package's `User::Pkg::<ui-pkgid>` label and expected UID. Do not authorize these methods for ordinary requester labels; conversely, the UI package should not need `exec`.
4. Let only the daemon initiate the fixed UI launch. The UI connects directly to the socket—never through a requester, proxy, or helper that would change the peer identity.
5. For each prompt, generate a fresh random nonce of at least 128 bits in the daemon. A decision is valid only for that nonce, the currently pending request, the registered UI connection, and the allowed time window. Consume or expire it after one decision.
6. Send prompts only to the authenticated UI connection. Other apps may open their own connections and request approval for themselves, but cannot read another connection's traffic or submit UI decisions.
7. Treat the UI package as security-critical: no network privilege, no requester-controlled rich text, minimal permissions, and a small protocol/parser surface.

No encryption or shared password is needed on this local channel. SMACK supplies the UI identity, filesystem permissions protect the socket endpoint, and the nonce prevents stale/replayed decisions. Root can bypass all of this and is explicitly outside the threat model. A malicious app might still imitate the UI visually; IPC authentication prevents it from forging the corresponding approval message, but does not solve UI spoofing by itself.

Execution semantics:

- fixed executable: `/usr/bin/setsid /bin/sh -c <command>`;
- sanitized initial environment, with consumer commands free to set additional variables themselves;
- stdout/stderr capture bounded initially to 1 MiB each;
- timeout bounded initially to 120 seconds;
- kill the command process group on timeout;
- no automatic retry after an unknown outcome.

## 6. Identity and grant policy

### Caller identity

Use both:

- `SO_PEERCRED` for PID/UID/GID sanity checks;
- `SO_PEERSEC` for the caller's SMACK label.

The label must be an installed package-specific `User::Pkg::<pkgid>` label. UID 5001 is shared and is not an application identity. Do not accept a caller-supplied package ID as proof.

The MVP grant key is the exact SMACK package label, relying on Tizen's package manager and security-manager to maintain package identity. Revalidate that the package is still installed. Test same-package upgrades, uninstall/reinstall, and different-signer replacement before relying on persistent grants. If security-manager exposes a signing-author label through a straightforward trusted query, record and compare it as an additional check; do not build a certificate-management subsystem.

The M0 spike verified `SO_PEERSEC` on the TV for two separately installed UID-5001 managed applications: the server observed `User::Pkg::org.tvauth.spike.peerone` and `User::Pkg::org.tvauth.spike.peertwo`. The same check must be repeated for Vetro's actual service process and the final approval UI. Missing or unexpected labels fail closed.

### Stored decisions

Only persistent **allow** grants need to be stored in the MVP:

```json
{
  "version": 1,
  "grants": {
    "User::Pkg::example.vetro": {
      "granted_at": "2026-11-04T21:00:00Z"
    }
  }
}
```

No grant means “ask”. Deny affects only the current request. Revocation removes the stored grant.

Allow-once decisions remain in memory and authorize exactly one request ID. After a daemon restart, an unfinished request must require new approval rather than silently execute.

### Prompt and execution limits

Keep these deliberately simple:

- one visible prompt globally;
- one waiting request per caller, with a small global queue;
- a short prompt timeout, defaulting to deny;
- bounded connections, request size, output size, and execution concurrency;
- no background retry that turns one approval into multiple executions.

If an app chooses “Always allow” on its first request, later commands run without prompts. This is the intended SuperSU-like behavior, not a policy loophole.

## 7. Approval UI

Write the approval UI as a small **managed Tizen .NET application** (`.NET Core 3.1`, `type="dotnet"` in the TPK manifest). Do not put Wayland/EFL code in the root daemon and do not run the UI as root.

### Why the native ELF path was rejected

The first UI spike was a Rust executable cross-linked against the TV's soft-float EFL libraries. The TPK installed successfully, but launchpad never mapped the executable: only `launchpad-loader` appeared in the process maps and the app entry point never ran. UEP blocks unsigned native ELF execution from writable app storage even when the TPK has valid author and distributor signatures.

System native apps work because they live on the read-only rootfs. A custom native approval app would therefore require UEP signing/bypass or a root-staged read-only-view workaround, neither of which is acceptable for the normal installed UI. The Rust UI source remains as negative-path evidence; it is not the production direction.

### Validated managed path

A managed .NET assembly has no custom native ELF. Tizen's signed `dotnet-launcher` loads it while UEP remains enforced. On the target, the managed UI package:

- installed as a normal signed TPK;
- launched through `app_launcher`;
- ran as `User::Pkg::org.tvauth.spike.popupdotnet`, UID 5001;
- loaded Elementary, EFL extension, and `capi-ui-efl-util`;
- set the Wayland surface type to notification;
- reported `notification level TOP accepted`;
- displayed above YouTube;
- dismissed cleanly with OK/RETURN.

The C# layer uses direct P/Invoke for the Tizen app lifecycle, Elm/Evas widgets, `efl_util_set_notification_window_level`, and Eext key grabbing. This is a small platform binding, not a dependency on a large UI framework.

The implemented M2 UI lives in `root-broker/ui/` and:

1. is launched by the daemon through a fixed AUL/`app_launcher -s org.tvauth.ui` command;
2. creates a notification-type EFL/Wayland window at the highest usable notification level;
3. sets the underlying surface type to notification before showing it;
4. grabs only the TV Back key (`XF86Back`) and relies on normal Elementary focus for the buttons;
5. connects directly to `broker.sock` and registers with `ui.hello`;
6. displays the daemon-supplied exact package ID, SMACK label, full-root warning, and a sanitized command copy;
7. returns `deny`, `once`, or `always` with the pending prompt ID and nonce;
8. exits shortly after a decision, or remains open in the grant-manager view when opened manually.

The window is a best-effort consent surface; the daemon treats every timeout,
disconnect, launcher failure, stale nonce, or malformed decision as a denial. Do
not modify `/opt/dbspace/.syspopup.db` or depend on Samsung's alert-syspopup
package.

M2 UI status: implemented, signed, installed, and TV-validated. The final
managed DLL is 34,816 bytes; the unsigned TPK is 16,535 bytes and the signed TPK
is 24,073 bytes. Notification level TOP was accepted. `input_keyevent` was used
to verify RETURN activation, Right/Left focus movement, and BACK handling without
a physical remote. The manager view renders grants after roughly two seconds, so
test scripts must wait for the grants response before sending keys.

Fallback if notification priority/key-grab fails for a developer-signed app:

1. try a normal full-screen .NET/EFL app;
2. if necessary, require the user to open the manager manually to review pending requests.

The UI is a **best-effort consent interface**, not yet a proven secure-attention path. Notification level and key grab do not prove that another app cannot imitate or obscure the prompt. Test spoofing, occlusion, focus loss, held/repeated keys, and compositor restart. Failure or ambiguity always denies.

## 8. Why not D-Bus?

D-Bus is technically viable, but it is not the simplest transport for this design.

Using the **system bus** would require verifying and possibly changing:

- name-ownership and send policies;
- service-activation configuration on a mostly read-only root filesystem;
- Tizen's additional `dbuspolicy`/Cynara behavior;
- whether `GetConnectionCredentials` exposes `LinuxSecurityLabel` as the required SMACK label on this firmware.

Systemd already handles startup, so bus activation, well-known names, and routing add little value.

**Peer-to-peer D-Bus over a Unix socket** avoids system-bus policy files, but retains D-Bus authentication, bindings, marshalling, and client-library complexity while giving up the bus features that might justify them. Rust `zbus`, C GDBus, and a Node D-Bus client are more moving parts than the small JSON protocol above.

Therefore:

> Use a Unix stream socket with newline-delimited JSON for version 1. Revisit D-Bus only if the project later needs many services, routed signals, or integration with existing D-Bus consumers.

D-Bus versus raw sockets is independent of the authorization model: either transport can implement simple app-level root grants or complex per-action policy.

## 9. Boot and storage integration

The AT-HUB boot hook dynamically discovers every root-owned
`/opt/tv-units/*.service`, copies it into `/run/systemd/system`, and starts each
unit independently. Systemd owns restart and cgroup lifetime; AT-HUB does not
host the long-running daemon.

Validated layout:

```text
/opt/tv-units/
    tv-authd.service         root-only unit source
    dropbear.service         root-only unit source

/opt/tv-auth/
    tv-authd                 persistent ARMv7 binary
    grants.json              persistent allow grants

/opt/tv-sshd/
    bin/dropbear             root-only Dropbear source
    authorized_keys
    dropbear_ed25519_host_key

/run/tv-auth/
    tv-authd                 executable staged on tmpfs
    broker.sock
    tv-authd.log
```

`/opt/tv-units`, `/opt/tv-auth`, and `/opt/tv-sshd/bin` are root-owned mode
`0700`, labeled `System::Shared`. Use symlink-safe file handling and atomic
grant-file replacement. Corrupt or unreadable grants fail closed. Runtime logs
stay bounded in `/run`; do not log command output by default.

### Persistence inputs were hardened

The previous app-writable `/opt/systemd` directory and the app-shared Dropbear
staging binary were retired. `/opt/systemd` was removed; Dropbear now executes a
root-only source at `/opt/tv-sshd/bin/dropbear`. This was validated across a cold
boot. The stock behavior and old unit/hook were backed up before deployment.

## 10. Implementation and binary-size guidance

### Rust daemon

A reasonable baseline is:

- `serde` and `serde_json` for protocol/grants;
- `libc` for Unix credentials and process-group operations.

This is not a prohibition on other useful crates. Prefer small, focused, maintained dependencies and use them where they avoid reinventing nontrivial code. `getrandom`, `nix`/`rustix`, `time`, or `clap` are reasonable if actually needed. Check transitive dependencies, enabled features, and measured artifact impact rather than optimizing dependency count at the expense of correctness. Do not hand-roll unsafe syscall or random-number code merely to save a few KiB.

Avoid large frameworks and infrastructure that solve problems this project does not have: no Tokio/other async runtime, web framework, ORM/database engine, or TLS stack. Use a small blocking design with bounded worker threads. Keep protocol framing, identity, grants, prompting, and execution in separate modules, but do not create a plugin architecture.

Build target:

```text
tv-authd: armv7-unknown-linux-musleabihf   (static, copied to /run)
```

Start with a size-conscious release profile and measure the actual artifact:

```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = "symbols"
debug = 0
```

Only the root executable must be copied into RAM-backed `/run`, so that artifact gets the strictest size scrutiny. Track stripped binary size and runtime RSS in validation notes.

### Managed UI

The UI is a `netcoreapp3.1` managed assembly packaged as a Tizen `dotnet` application. It requires no custom ARM ELF and therefore bypasses the UEP wall while still running as UID 5001 with a package-specific SMACK label. Keep native calls in one small interop class and keep UI/protocol logic managed. Measure the DLL and signed TPK size, process RSS, and launch time.

### Validated M0 evidence

- static ARMv7 Rust hello: runs directly and under a transient systemd unit from `/run`;
- .NET peer probes: two package processes expose distinct `User::Pkg::*` labels over `SO_PEERSEC`;
- managed .NET UI: installs, launches from AUL, sets notification TOP, appears over YouTube, and dismisses with OK/RETURN.

## 11. Vetro integration

Replace Vetro's root-agent transport, not its feature modules.

Current path:

```text
platform.execute()
  -> RootAgentServer
  -> SDB root reacquisition when disconnected
  -> SamsungTvRootAgent
  -> /bin/sh -c
```

New path:

```text
platform.execute()
  -> Unix-socket code in the same Vetro Node process
  -> /run/tv-auth/broker.sock
  -> tv-authd policy check
  -> /bin/sh -c
```

The adapter preserves the existing `{exit_code, timed_out, stdout, stderr}` result shape. Use Node's built-in `net.Socket`/`net.createConnection` Unix-domain-socket support, newline buffering for NDJSON, and `Buffer` for base64 output; no npm dependency is needed. The socket code can live directly in `platform.js` or in a same-process helper module if that improves readability. “Broker client” describes code, not another process.

The Node service must connect directly so the broker sees Vetro's own PID and SMACK label. A small Rust CLI is unnecessary: the kernel identifies whichever process connects, so it would see the CLI's PID. The CLI would normally inherit Vetro's SMACK label unless an exec transition changes it, but spawning it adds process, framing, and lifecycle complexity without improving identity.

On startup, Vetro no longer stages or relaunches its root agent. If the broker is unavailable, report `no-root-agent`/equivalent without attempting TV-local SDB root acquisition.

Vetro's `install()` is a separate local-SDB operation. Keep it explicitly PC-driven initially, or test installing through the normal package manager via the broker's ordinary shell API. Do not add a special high-level install permission to the broker.

### Vetro's HTTP API is a separate security boundary

Once Vetro receives a persistent full-root grant, anyone able to make Vetro issue commands inherits that authority. In particular, its unauthenticated `POST /exec` would become a LAN-to-root proxy.

For a trusted-LAN proof of concept this can be an explicitly accepted risk. Before recommending persistent grants for general use, disable `/exec` or put it behind authentication, and review other privileged HTTP endpoints. This does not require a new broker permission system.

## 12. Agent-friendly workstreams

Freeze the small wire protocol and package/label names before parallel implementation. Suggested ownership boundaries:

| Workstream | Ownership | Deliverable |
|---|---|---|
| A. Platform spikes | `root-broker/spikes/` and small test programs | Rust ARMv7 execution, `SO_PEERSEC`, EFL popup evidence |
| B. Daemon | `root-broker/daemon/` | Rust broker, protocol, grants, execution, unit tests |
| C. Approval UI | `root-broker/ui/` | Managed .NET prompt/grant manager with isolated EFL P/Invoke |
| D. Vetro adapter | Separate Vetro worktree only | Same-process, dependency-free Unix-socket implementation behind `platform.execute()` |
| E. Persistence/build | `root-broker/systemd/`, `root-broker/build/`, minimal `persistence/` edits | Reproducible ARMv7 build and boot integration |
| F. End-to-end tests | `root-broker/tests/` | Multi-app, failure, reboot, and Vetro acceptance tests |

Avoid having multiple agents edit the protocol definition or the same persistence hook concurrently. Merge protocol changes explicitly.

## 13. Milestones and acceptance tests

### M0 — Three small spikes (validated 2026-11-04/05)

1. **Passed:** a static Rust ARMv7 hello ran directly and under a transient systemd unit from `/run`; stripped size 323,652 bytes.
2. **Passed:** two separately packaged managed apps connected from UID 5001 and the server observed `User::Pkg::org.tvauth.spike.peerone` and `User::Pkg::org.tvauth.spike.peertwo` over `SO_PEERSEC`.
3. **Passed with managed .NET:** the native Rust/EFL UI cross-linked and installed but UEP prevented launchpad from executing its ELF. A managed `.NET`/EFL app installed, launched through AUL, set notification TOP, appeared over YouTube, and dismissed with OK/RETURN.

The native-UI failure is why the design now uses managed .NET for the approval UI. Root daemon execution from `/run` remains native Rust.

### M1 — Headless broker (complete)

- Rust core implemented under `root-broker/daemon/` with NDJSON protocol, strict package-label identity, exact-label grants, bounded execution, and ARMv7 build.
- Deployed as `/opt/tv-auth/tv-authd`, unit source `/opt/tv-units/tv-authd.service`, runtime socket `/run/tv-auth/broker.sock`.
- Dynamic boot hook stages and starts every `/opt/tv-units/*.service` independently.
- A granted managed .NET package executed a root command and received a valid response; an ungranted package was denied.
- `systemctl restart tv-authd.service` recovered the socket and accepted the next request.
- Test packages, grants, and temporary files were removed after validation.
- Cold boot passed: new boot ID, AT-HUB `start units rc=0`, Dropbear and `tv-authd` automatically active from `/opt/tv-units`, broker socket recreated, and empty grants loaded.
- The root-only Dropbear source path was validated on the cold boot.

### M2 — Approval and persistent grants (complete)

- Daemon protocol extended with `ui.hello`, `ui.prompt`, `ui.decision`,
  `grants.list`, and `grants.revoke`; exact UI label + UID role separation.
- Approval manager implements one global prompt, one pending request per package,
  a bounded queue, 128-bit prompt IDs/nonces, prompt timeout/default-deny,
  fixed `app_launcher` execution, and bounded relaunch attempts.
- Deny does not execute; Allow once executes exactly the pending request; Always
  persists the exact label before execution; revoke restores prompting.
- UI timeout, disconnect, launcher failure, and stale/expired decisions fail
  closed.
- Managed .NET approval/grant-manager UI implemented under `root-broker/ui/`.
- 35 Linux tests pass, including queueing, nonce rejection, timeout, disconnect,
  persistence, revoke, rollback on failed grant writes, and role separation.
  ARMv7 broker build: 546,388 bytes, SHA-256
  `d93890d3f4104fee7fc537e927010dc05b8c06777944a671c2262a558c4a5ae1`.
- TV acceptance on 2026-11-05:
  - signed `org.tvauth.ui` installed; process label verified as
    `User::Pkg::org.tvauth.ui`; notification level TOP accepted;
  - Deny returned `denied`/`user_denied` and did not execute;
  - Allow once returned a root `id` result, did not persist a grant, and the next
    request prompted again;
  - Always allow persisted the exact label and executed; later requests ran with
    no prompt, including after a daemon restart and a cold boot;
  - manager revocation returned `revoked=true`, emptied the grant file, and the
    next request prompted again;
  - prompt timeout returned `denied`/`prompt_timeout` and did not execute; the UI
    logged `prompt expired` and exited;
  - killing the UI during a pending prompt returned
    `ui_unavailable`/`ui_disconnected` and did not execute;
  - RETURN, Right/Left focus movement, and BACK were exercised with
    `/usr/bin/input_keyevent`; the manager takes about two seconds to render the
    grant list after launch.
- Cold boot on 2026-11-05: new boot ID
  `7687b045-4e4b-4a7e-9416-68ecc3fc6efc`, AT-HUB logged `start units rc=0`,
  Dropbear and `tv-authd` were active, the unit reported
  `Environment=XDG_RUNTIME_DIR=/run HOME=/root`, the grant survived, and the
  requester executed without launching the UI.
- The M0 spike already validated the same notification-window mechanism above a
  foreground YouTube session; M2 revalidated TOP acceptance and the consent
  flow itself.

### M3 — Vetro cutover

- Normal Vetro features work through `platform.execute()` with no shell-command rewrites.
- No runtime SDB connection is attempted.
- Host PC IP remains a real LAN address.
- Vetro service restart does not reacquire root or kill the broker.
- A cold boot with no PC connected works once Vetro has its persistent grant.

### M4 — Hardening and multi-app tests

- Two same-UID apps cannot impersonate each other or the UI.
- Package upgrade/reinstall/signer-change behavior is documented and tested.
- Oversized frames, output floods, prompt floods, timeouts, disconnects, and corrupt grant files fail safely.
- Broker and UI memory use are measured under TV memory pressure.
- Root-executed boot inputs are not writable by ordinary apps.

## 14. Explicit simplifications relative to the earlier proposal

- One authorization scope: **full root shell per application**.
- One socket and three approval choices, not a capability taxonomy.
- No high-level Vetro operations in the daemon.
- No shell-command parsing or translation.
- No D-Bus service/policy integration.
- No Tokio, SQLite, dynamic providers, or certificate-management subsystem in the MVP.
- No root Wayland client and no root UI.
- No custom native ELF for the approval UI; use a managed .NET TPK to stay compatible with UEP.
- No need to port Vetro feature modules; replace only its transport adapter.

This should provide the requested safety—users know which app receives root and can revoke it—without turning the project into a general-purpose authorization framework.

## 15. Plan maintenance

Treat the spike results and integration measurements as inputs to this document, not as separate folklore. After the spikes and each validation milestone, update this plan in place:

- replace assumptions with exact observed firmware, ABI, identity, UI, binary-size, and RSS results;
- remove rejected alternatives and stale fallback instructions rather than accumulating outdated proposals;
- reconcile the wire protocol, workstream ownership, and acceptance tests with what was actually built;
- link the relevant validation logs/source instead of claiming unverified behavior.

M0 was completed on 2026-11-04/05, M1 was deployed and cold-boot validated on
2026-11-05, and M2 was implemented, deployed, and cold-boot validated on
2026-11-05. The plan now reflects the observed soft-float EFL ABI, successful
`SO_PEERSEC` package labels, native-ELF UEP rejection, the validated managed .NET
notification popup, the live broker layout, the M2 approval protocol, and the
interactive denial/once/always/revoke/timeout/crash/reboot results. Raw evidence
and build commands live in:

- `root-broker/spikes/armv7-hello/`
- `root-broker/spikes/peer-identity/`
- `root-broker/spikes/efl-popup/`
- `root-broker/sysroot/`
- `root-broker/daemon/`
- `root-broker/ui/`

Future milestones should continue updating this document rather than preserving
superseded assumptions.
