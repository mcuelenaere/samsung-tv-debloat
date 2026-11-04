# `tv-authd`: a small SuperSU-like root broker for Samsung Tizen TVs

*Status: proposal, 2026-11-04. No implementation yet. Initial target: the S95B/PontusM, Tizen 6.5, build `T-PTMDEUC-1720.7` documented in this repository.*

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
| Approval/manager UI | **Rust with minimal EFL FFI** | On-TV prompt and grant revocation, running without root |
| Vetro adapter | **JavaScript in Vetro's existing Node process** | Replace root-agent transport with direct Unix-socket calls |
| Wire protocol | Newline-delimited JSON over a Unix stream socket | Easy interoperation between Rust and Node |
| Boot integration | Existing gawk hook plus a systemd unit | Start and restart the daemon independently of Vetro/SDB |

Rust is a good fit for both new executables: memory safety is especially valuable in the privileged daemon, and a small safe wrapper can isolate the UI's unavoidable EFL FFI. Keep the dependency set small and do not use async runtimes. The root daemon targets static musl; the UI links the TV's glibc/EFL libraries. The UI spike must validate the small amount of unsafe lifecycle/callback glue rather than assume that a ready-made EFL crate fits this firmware.

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
              │  tv-auth-ui      │  UID 5001, native EFL, not root
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

Linux also has `SOCK_SEQPACKET` and `SOCK_DGRAM`, which do preserve message boundaries. They are a poor fit for this project because Node's built-in Unix-socket support is stream-only. Using them would require a native Node add-on, a helper process, or separate transports for Vetro and the Rust UI. NDJSON over one stream socket is simpler, remains easy to inspect with ordinary socket tools, and works with Node's standard library.

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
- `ui.prompt` — daemon to UI: request ID, resolved application name, and command;
- `ui.decision` — `deny`, `once`, or `always`, with the request ID and an unguessable prompt nonce;
- `grants.list` / `grants.revoke` — manager functionality.

The daemon generates prompt wording. The requester cannot supply a trusted-looking app name or approval description. Long commands may be shown in a scrollable details view or clearly marked as truncated; the prompt must always say that the application is requesting **full root access**.

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

An early spike must verify that `SO_PEERSEC` returns the expected package label for:

1. two different sideloaded applications sharing UID 5001;
2. Vetro's actual service process;
3. the approval UI.

Missing or unexpected labels fail closed.

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

Write a small native **Rust** application using EFL through a deliberately small FFI module. Do not put Wayland/EFL code in the root daemon and do not run the UI as root.

Rust does not require a complete safe EFL binding. Declare only the needed Tizen app-lifecycle, Ecore/Elementary, notification-level, key-grab, and window functions, then isolate `unsafe` code and C string/callback handling in one `efl` module with a small safe interface for the rest of the UI. This is preferable to scattering raw FFI throughout the application. The spike must verify ABI layouts and callback/lifecycle behavior on the TV.

The existing [`efl` crate](https://crates.io/crates/efl) is not currently a good shortcut: its README requires EFL >= 1.26 and libclang, while this TV has EFL 1.25.1. Hand-writing the few declarations avoids both the version mismatch and a large bindgen/libclang build dependency. C remains a fallback only if the Rust lifecycle/FFI spike reveals a concrete platform blocker; C is not inherently the safer UI language.

Build the UI for `armv7-unknown-linux-gnueabihf`, because it must dynamically link the TV's glibc-based EFL stack. This differs intentionally from the root daemon's static musl target. The UI remains installed under its Tizen package directory in `/opt/usr/apps`; it is not copied to `/run`.

The live TV provides a useful precedent: Samsung's `libsyspopup` uses an EFL/Wayland notification window, `efl_util_set_notification_window_level`, and key grabbing. Root launchers also start ordinary UID-5001 UI apps through `app_launcher`.

The UI should:

1. be launched by the daemon through a fixed AUL/`app_launcher` command;
2. create a notification-type EFL window at the highest usable notification level;
3. grab only the remote keys needed for navigation and confirmation;
4. connect to `broker.sock` and register with `ui.hello`;
5. display the daemon-resolved app name and full-root warning;
6. return a decision with the pending request ID and prompt nonce;
7. exit when idle, or remain open when the user is managing grants.

Initially, call the EFL notification mechanisms directly. Do not modify `/opt/dbspace/.syspopup.db` or depend on Samsung's alert-syspopup package.

Fallback if notification priority/key-grab fails for a developer-signed app:

1. try a normal full-screen EFL app;
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

Reuse the existing AT-HUB boot hook to install and start a transient `tv-authd.service` from `/run/systemd/system`. Systemd owns restart and cgroup lifetime; AT-HUB must not host the long-running daemon.

Proposed paths:

```text
/opt/tv-auth/
    tv-authd                 persistent ARMv7 binary
    tv-authd.service         persistent unit source
    grants.json              persistent allow grants

/run/tv-auth/
    tv-authd                 executable staged on tmpfs
    broker.sock
    pending/                 if runtime files are needed
```

`/opt/tv-auth` must be root-owned, mode `0700`, with a suitable non-app-writable SMACK label and safe parent ancestry. Use symlink-safe file handling and atomic grant-file replacement. Corrupt or unreadable grants fail closed. Runtime logs stay bounded in `/run`; avoid routine flash writes and do not log command output by default.

### Existing persistence inputs need hardening

The live TV currently has `/opt/systemd` labeled `User` and mode `0777`, and the Dropbear source under the app-writable `User::App::Shared` staging area. Those inputs are copied into a root execution path.

Before presenting `tv-authd` as a security feature, place its unit, binary, and grants in the root-only layout above. Harden Dropbear's boot inputs as well; otherwise an unprivileged app can bypass the broker by altering a future root boot payload.

## 10. Rust implementation and binary-size guidance

A reasonable baseline is:

- `serde` and `serde_json` for protocol/grants;
- `libc` for Unix credentials, process-group operations, and the UI's EFL declarations.

This is not a prohibition on other useful crates. Prefer small, focused, maintained dependencies and use them where they avoid reinventing nontrivial code. For example, `getrandom`, `nix`/`rustix`, `time`, or `clap` are all reasonable if the implementation actually needs them. Check transitive dependencies, enabled features, and measured artifact impact rather than optimizing dependency count at the expense of correctness. In particular, do not hand-roll unsafe syscall or random-number code merely to save a few KiB.

Avoid large frameworks and infrastructure that solve problems this project does not have: no Tokio/other async runtime, web framework, ORM/database engine, or TLS stack. Use a small blocking design with bounded worker threads, sufficient for a handful of local clients. Keep protocol framing, identity, grants, prompting, execution, and EFL FFI in separate modules, but do not create a plugin architecture.

Build targets:

```text
tv-authd:  armv7-unknown-linux-musleabihf   (static, copied to /run)
tv-auth-ui: armv7-unknown-linux-gnueabihf   (dynamic glibc/EFL, stays in app package)
```

Use the existing Docker/QEMU approach as a model. Start with a size-conscious release profile and measure the actual artifacts rather than adding extreme size optimizations blindly:

```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = "symbols"
debug = 0
```

Only the root executable must be copied into RAM-backed `/run`, so that artifact gets the strictest size scrutiny. Track stripped binary size and runtime RSS in the validation notes. The first build spike must prove that both the static Rust ARMv7 daemon test binary and the glibc/EFL Rust UI test binary run on this exact TV.

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
| A. Platform spikes | `root-broker/docs/spikes/` and small test programs | Rust ARMv7 execution, `SO_PEERSEC`, EFL popup evidence |
| B. Daemon | `root-broker/daemon/` | Rust broker, protocol, grants, execution, unit tests |
| C. Approval UI | `root-broker/ui/` | Rust prompt/grant manager with isolated EFL FFI |
| D. Vetro adapter | Separate Vetro worktree only | Same-process, dependency-free Unix-socket implementation behind `platform.execute()` |
| E. Persistence/build | `root-broker/systemd/`, `root-broker/build/`, minimal `persistence/` edits | Reproducible ARMv7 build and boot integration |
| F. End-to-end tests | `root-broker/tests/` | Multi-app, failure, reboot, and Vetro acceptance tests |

Avoid having multiple agents edit the protocol definition or the same persistence hook concurrently. Merge protocol changes explicitly.

## 13. Milestones and acceptance tests

### M0 — Three small spikes

1. Execute a static Rust ARMv7 “hello” binary from `/run` under systemd, and record its stripped size.
2. Distinguish two same-UID test apps with `SO_PEERSEC`.
3. Display and dismiss a harmless Rust/EFL prompt launched through AUL, and record its stripped size.

Do not start the full implementation if any of these fails; solve the platform issue first.

### M1 — Headless broker

- Start through the transient systemd unit.
- Pre-seed one test app's allow grant manually.
- Execute a harmless command and return bounded output.
- Reject an unknown test app.
- Restart client processes without restarting the broker.

### M2 — Approval and persistent grants

- Unknown app prompts.
- Deny does not execute.
- Allow once executes once and prompts for the next command.
- Always executes subsequent commands and survives reboot.
- Revoke restores prompting.
- UI timeout/crash/compositor failure does not execute.

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
- No need to port Vetro feature modules; replace only its transport adapter.

This should provide the requested safety—users know which app receives root and can revoke it—without turning the project into a general-purpose authorization framework.

## 15. Plan maintenance

Treat the spike results and integration measurements as inputs to this document, not as separate folklore. After the spikes and each validation milestone, update this plan in place:

- replace assumptions with exact observed firmware, ABI, identity, UI, binary-size, and RSS results;
- remove rejected alternatives and stale fallback instructions rather than accumulating outdated proposals;
- reconcile the wire protocol, workstream ownership, and acceptance tests with what was actually built;
- link the relevant validation logs/source instead of claiming unverified behavior.

Until then, all proposed constants, paths, ABI declarations, and fallback choices remain subject to validation.
