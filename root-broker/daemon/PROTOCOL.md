# tv-authd protocol v2

Transport: Unix `SOCK_STREAM`, one UTF-8 JSON object per line (`\n`).

Version 1 remains accepted for the v1 methods (`exec`, `ui.hello`,
`ui.decision`, `grants.*`). Package methods require `"version": 2`, and every
response and unsolicited `ui.*` message is written with `"version": 2`.

Limits:

- request line: 64 KiB;
- response or prompt line: 4 MiB;
- 1 MiB each for captured stdout and stderr;
- default command timeout: 10,000 ms;
- maximum command timeout: 120,000 ms;
- default and maximum install timeout: 120,000 ms;
- maximum package source path: 4096 bytes;
- default prompt timeout: 60,000 ms;
- default UI launch/connect timeout: 10,000 ms;
- maximum concurrent clients: 16 by default, plus a separate allowance of 2 for the approval UI;
- a complete request line must arrive within the idle timeout (30 s apps, 600 s UI) of the daemon starting to wait for it;
- at most 1024 distinct request IDs per connection;
- default approval queue: 8 pending requests (including the active one).

Unknown JSON fields, versions, and methods are rejected. A request `id` is
required, at most 128 bytes, and limited to ASCII alphanumerics plus `. _ - :`.
Duplicate request IDs on one connection are rejected without execution.

## Roles

The daemon reads `SO_PEERCRED` and `SO_PEERSEC` before accepting any frame. The
role is fixed for the whole connection:

- **ordinary package**: exact `User::Pkg::<pkgid>` label, any accepted UID; may
  send `exec` and `pkg.install`;
- **approval UI**: exact `User::Pkg::org.tvauth.ui` label and UID 5001 by
  default; may send `ui.hello`, `ui.decision`, `grants.list`,
  `grants.notify`, and `grants.revoke`, and may not send `exec` or `pkg.*`.

The UI package and UID are daemon settings (`--ui-package`, `--ui-uid`). Missing,
unexpected, or non-package labels fail closed.

## `exec`

Request:

```json
{
  "version": 2,
  "id": "unique-request-id",
  "method": "exec",
  "command": "/bin/id",
  "timeout_ms": 10000
}
```

If the caller has a persistent grant, it executes immediately and the daemon
queues a `ui.notice` for the UI (see below). Otherwise the
daemon sends a prompt to the registered UI and waits for a decision. The
connection blocks until the prompt is answered, cancelled, or times out.

Success response:

```json
{
  "version": 2,
  "id": "unique-request-id",
  "status": "ok",
  "exit_code": 0,
  "timed_out": false,
  "stdout_b64": "...",
  "stderr_b64": "...",
  "truncated": false
}
```

Denial or failure:

```json
{
  "version": 2,
  "id": "unique-request-id",
  "status": "denied",
  "error": "user_denied"
}
```

Statuses:

- `ok` — command completed; inspect `exit_code` and `timed_out`;
- `denied` — the user or policy denied the request; the command was not run;
- `ui_unavailable` — no prompt could be obtained; the command was not run;
- `busy` — the approval queue or per-package pending limit was reached;
- `error` — malformed request, unsupported role/method, or execution setup
  failure.

M1 returned `not_granted` for ungranted callers; M2 replaces that with the
approval flow. M2 errors include:

| Error | Meaning |
|---|---|
| `user_denied` | the user chose Deny |
| `prompt_timeout` | no decision arrived before the prompt timeout |
| `ui_disconnected` | the UI connection dropped while the prompt was pending and the retry limit (2 re-shows) was exhausted |
| `ui_unavailable` | no UI connected and launch attempts were exhausted |
| `ui_launch_failed` | the fixed launcher could not be started |
| `prompt_queue_full` | the global approval queue is full |
| `request_already_pending` | this package already has a queued or active request |
| `grant_persist_failed` | Always allow could not be saved; the command did not run |
| `package_identity_unavailable` | Always allow could not bind the grant to the installed package signature; the command did not run |
| `ui_role` | the approval UI attempted to send `exec` |
| `too_many_requests` | the connection exceeded its request limit; the daemon closes it |
| `approval_wait_timeout` | the daemon-side wait for the approval manager expired |

If the app closes (or half-closes) its socket while waiting for approval, the
daemon cancels the pending prompt and never executes the command; a late UI
decision for it gets `stale_prompt`.

Execution uses `/usr/bin/setsid /bin/sh -c <command>` with a cleared environment
except `PATH`, `HOME=/root`, and `LANG=C`. On timeout the process group receives
`SIGKILL` and the response uses exit code 124 with `timed_out: true`.

## `pkg.install`

Ask tv-authd to install a `wgt` or `tpk` package. Requires protocol version 2.

```json
{
  "version": 2,
  "id": "inst-1",
  "method": "pkg.install",
  "pkg_type": "wgt",
  "path": "/opt/usr/home/owner/media/Downloads/org.example.app-1.0.0.wgt",
  "timeout_ms": 120000
}
```

`timeout_ms` bounds the installer run and defaults to 120,000 ms. `pkg.install`
is always interactive: the daemon stages the package, inspects it, and sends a
`ui.prompt` with `kind: "install"` before touching the installer. Only one
install (including its prompt) runs globally; a concurrent request gets
`status: busy`, `error: install_busy`.

Success:

```json
{
  "version": 2,
  "id": "inst-1",
  "status": "ok",
  "pkgid": "org.example.app",
  "pkg_version": "1.0.0",
  "re_signed": false,
  "log_tail_b64": "..."
}
```

`log_tail_b64` is the last 8 KiB of `pkgcmd` output (standard output followed by
standard error), base64-encoded. `re_signed` is reserved for M3 on-device
re-signing and is currently always `false`.

The request lifecycle:

1. `validate_install`: absolute path (≤ 4096 bytes, no NUL) and a valid timeout;
2. path allowlist check and streamed copy into `/opt/tv-auth/staging/<random>`
   (default cap 128 MiB, configurable up to 256 MiB; SHA-256 recorded);
3. zip hygiene and manifest inspection (`config.xml` for wgt,
   `tizen-manifest.xml` for tpk);
4. `ui.prompt` with the package metadata;
5. trust enrollment (`enroll_failed` if it cannot be repaired);
6. `pkgcmd -i -t <type> -p <staged>` with the request timeout;
7. response, then the staged copy is deleted on every path.

Policy refusals use `status: denied`; failures after the user approved use
`status: error`. Every refusal and attempt carries a `detail` string where one
is useful.

| Error | Status | Meaning |
|---|---|---|
| `invalid_path` | `denied` | outside the allowlist, not a regular file, a symlink, or malformed |
| `invalid_timeout` | `error` | timeout is zero or above the maximum |
| `too_large` | `denied` | source or staged copy exceeds the size cap |
| `bad_package` | `error` | not a zip, unsafe entry names, bad or missing manifest |
| `manifest_forbidden` | `denied` | reserved package-id prefix or platform-only privileges; `detail` lists them |
| `user_denied` | `denied` | the user chose Deny |
| `prompt_timeout` | `denied` | no decision before the prompt timeout |
| `enroll_failed` | `error` | trust enrollment could not be established; nothing was installed |
| `install_failed` | `error` | `pkgcmd` failed or timed out; `detail` and `log_tail_b64` are included |
| `install_busy` | `busy` | another install is in progress |
| `package_support_disabled` | `error` | the daemon was started with `--no-package-support` |
| `ui_unavailable`, `prompt_queue_full`, `request_already_pending`, `approval_wait_timeout` | as for `exec` | the prompt could not be completed |

If the requesting app closes its socket while the prompt is pending, the prompt
is cancelled and no package is installed.

`pkg.install` is refused for the UI role (`denied`, `ui_role`).

## `ui.hello`

Registers this connection as the approval UI. It is required before any other UI
method. A newer registration replaces an older one and cancels/re-prompts the
active request.

```json
{"version": 2, "id": "hello-1", "method": "ui.hello"}
```

Response:

```json
{"version": 2, "id": "hello-1", "status": "ok"}
```

When the daemon launched the UI only to show execution notices (no prompt was
waiting), the response carries `"launch": "notice"`. The UI should then stay
out of the way: no full-screen window, no key grabs. The approval manager writes
this response, so it always precedes any `ui.prompt` or `ui.notice`.

Errors: `not_ui` (wrong label or UID), `already_registered` (this connection
already registered), `ui_register_failed`.

## `ui.prompt` (daemon to UI)

After `ui.hello`, the daemon may send an unsolicited prompt on that connection.
It is the only message without a client request `id`; `type` identifies it.

```json
{
  "version": 2,
  "type": "ui.prompt",
  "prompt_id": "32-hex-characters",
  "nonce": "32-hex-characters",
  "request_id": "unique-request-id",
  "label": "User::Pkg::org.example.app",
  "package": "org.example.app",
  "kind": "install",
  "install": {
    "pkgid": "org.example.app",
    "name": "Example",
    "pkg_version": "1.0.0",
    "pkg_type": "wgt",
    "size_bytes": 1234567,
    "privileges": ["http://tizen.org/privilege/internet"],
    "signature": "unsigned_will_resign",
    "is_upgrade": false
  },
  "command": "install wgt /opt/usr/home/owner/media/Downloads/org.example.app-1.0.0.wgt",
  "command_bytes": 76,
  "command_truncated": false,
  "timeout_ms": 60000
}
```

`prompt_id` and `nonce` are fresh 128-bit random values per prompt. The UI must
return both. The daemon generates the package identity from the peer label; the
requester cannot supply it. The command is a display copy only: control and
bidirectional formatting characters are replaced with U+FFFD, and long commands
are truncated with `command_truncated: true`.

`kind` is `exec`, `install`, or `uninstall`; a v1 prompt without the field means
`exec`. Only `kind: "install"` carries the `install` object. Within it:

- `signature` is `unsigned_will_resign`, `foreign_will_resign`, `user_ca`, or
  `samsung` —
  a display label, not a verification result;
- `is_upgrade` is a best-effort hint from the installed-package directories;
- all strings are sanitized and length-limited like the command;
- `privileges` is truncated to 128 entries.

Only one prompt is visible globally. For `kind: "exec"` the UI should treat the
prompt as a request for **full root access**. For package kinds it is a request
for exactly the staged artifact shown: the metadata in the prompt and the file
that will be installed are the same bytes.

## `ui.notice` (daemon to UI)

Granted packages run without prompting, but the user still sees who ran root
commands. After `ui.hello`, the daemon may send:

```json
{
  "version": 2,
  "type": "ui.notice",
  "notices": [
    {
      "label": "User::Pkg::org.example.app",
      "package": "org.example.app",
      "count": 3,
      "command": "id",
      "command_truncated": false
    }
  ]
}
```

Each entry coalesces one package's executions since the previous notice;
`count` is a delta, so the UI adds it to what it is showing. `command` is a
display copy (sanitized like `ui.prompt`, at most 256 bytes) of the most recent
one. Delivery is near real-time:

- a granted exec is sent at once;
- execs within the next 250 ms are coalesced per package into one message,
  which only caps the message rate during bursts;
- notices are held while a prompt is active or queued;
- if no UI is connected the daemon launches it (same launcher and attempt
  limits as prompts); if that fails the notices are dropped and no launch is
  retried for notices for 60 s. Notices never delay or block execution.

Keeping the screen quiet is the UI's job: it shows one toast and updates it in
place.

Execs that were just approved by a prompt do not produce a notice, and the
grant's `notify` setting (see `grants.notify`) can limit or silence them.

## `ui.decision`

```json
{
  "version": 2,
  "id": "decision-1",
  "method": "ui.decision",
  "prompt_id": "32-hex-characters",
  "nonce": "32-hex-characters",
  "decision": "once"
}
```

`decision` is `deny`, `once`, or `always`.

Response:

```json
{"version": 2, "id": "decision-1", "status": "ok"}
```

Errors: `not_registered`, `stale_prompt` (wrong nonce, prompt, UI connection, or
expired prompt), `invalid_prompt_token`, `invalid_decision`, `approval_unavailable`.

Semantics:

- `deny` — the pending request returns `status: denied`, `error: user_denied`;
- `once` — the pending request executes once; the next command prompts again;
- `always` — the package label, a SHA-256 of its installed
  `author-signature.xml`, and its notify mode are persisted before the pending
  request executes; later requests bypass prompting only while the installed
  package identity still matches. A package upgrade, changed reinstall, or
  signer change invalidates the grant and returns to prompting. Version-1
  label-only grants are migrated on first use by binding them to the currently
  installed package; a non-default `notify` mode is preserved. **Only valid for
  `kind: "exec"`.**

For `kind: "install"` and `kind: "uninstall"`, `always` is rejected with
`invalid_decision` and the prompt stays active, so the user can still choose
deny or once. Package operations have no persistent grant at all; an exec grant
never authorizes an install.

## `grants.list`

```json
{"version": 2, "id": "list-1", "method": "grants.list"}
```

Response:

```json
{
  "version": 2,
  "id": "list-1",
  "status": "ok",
  "grants": [
    {
      "label": "User::Pkg::org.example.app",
      "package": "org.example.app",
      "granted_at": 1770000000,
      "notify": "every"
    }
  ]
}
```

`granted_at` is Unix seconds. Grants are sorted by exact SMACK label.

`notify` is the grant's toast setting (see `grants.notify`).

## `grants.notify`

Sets when a granted package's executions produce a `ui.notice`:

```json
{
  "version": 2,
  "id": "notify-1",
  "method": "grants.notify",
  "label": "User::Pkg::org.example.app",
  "notify": "first"
}
```

- `every` (default) — every exec;
- `first` — only the first exec after the package was idle for 30 minutes,
  so a polling app shows once when it becomes active;
- `never` — no notices.

Response: `{"version": 2, "id": "notify-1", "status": "ok"}`. Errors:
`not_found` (no grant for that label), `invalid_label`, `grant_save_failed`,
`not_registered`.

## `grants.revoke`

```json
{
  "version": 2,
  "id": "revoke-1",
  "method": "grants.revoke",
  "label": "User::Pkg::org.example.app"
}
```

Response:

```json
{"version": 2, "id": "revoke-1", "status": "ok", "revoked": true}
```

`revoked` is `false` if the grant did not exist. Errors: `invalid_label`,
`grant_save_failed`, `not_registered`.

## Package installation policy

- Source paths: `/opt/usr/home/owner/**`, `/opt/media/**`, `/tmp/**`; regular
  file, no symlink at the final component, ≤ 128 MiB by default
  (`--max-package-bytes`, maximum 256 MiB).
- Zip: ≤ 4096 entries, ≤ 1 GiB uncompressed, no absolute names and no `..`.
- Package ids starting with `com.samsung.`, `org.tizen.`, or `samsung.` are
  refused, as are platform-only privileges (the installer's
  own privilege check remains authoritative).
- Trust material lives in `/opt/tv-auth/trust/` (`user-root-ca.pem`,
  `fingerprint_list.xml`, `tizen-ca/`). Enrollment bind-mounts the fingerprint
  list and CA overlay over the stock paths inside the daemon's mount namespace;
  `pkgcmd` inherits that namespace. Enrollment is re-verified before every
  approved install.
- Every attempt is logged with the requesting label, UID/PID, package id,
  version, SHA-256 of the staged copy, decision, and result.

## Prompt and execution limits

- one visible prompt globally;
- one pending request per package label;
- one install (staging, prompt, and `pkgcmd`) globally;
- a bounded global queue;
- a short prompt timeout that defaults to deny;
- no background retry that turns one approval into multiple executions;
- allow-once decisions are valid for exactly the pending request ID and are not
  persisted; a daemon restart discards them.
