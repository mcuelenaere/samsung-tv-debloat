# tv-authd protocol v1

Transport: Unix `SOCK_STREAM`, one UTF-8 JSON object per line (`\n`).

Limits:

- request line: 64 KiB;
- response or prompt line: 4 MiB;
- 1 MiB each for captured stdout and stderr;
- default command timeout: 10,000 ms;
- maximum command timeout: 120,000 ms;
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
  send `exec` only;
- **approval UI**: exact `User::Pkg::org.tvauth.ui` label and UID 5001 by
  default; may send `ui.hello`, `ui.decision`, `grants.list`, and
  `grants.revoke`, and may not send `exec`.

The UI package and UID are daemon settings (`--ui-package`, `--ui-uid`). Missing,
unexpected, or non-package labels fail closed.

## `exec`

Request:

```json
{
  "version": 1,
  "id": "unique-request-id",
  "method": "exec",
  "command": "/bin/id",
  "timeout_ms": 10000
}
```

If the caller has a persistent grant, it executes immediately. Otherwise the
daemon sends a prompt to the registered UI and waits for a decision. The
connection blocks until the prompt is answered, cancelled, or times out.

Success response:

```json
{
  "version": 1,
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
  "version": 1,
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
| `ui_role` | the approval UI attempted to send `exec` |
| `too_many_requests` | the connection exceeded its request limit; the daemon closes it |
| `approval_wait_timeout` | the daemon-side wait for the approval manager expired |

If the app closes (or half-closes) its socket while waiting for approval, the
daemon cancels the pending prompt and never executes the command; a late UI
decision for it gets `stale_prompt`.

Execution uses `/usr/bin/setsid /bin/sh -c <command>` with a cleared environment
except `PATH`, `HOME=/root`, and `LANG=C`. On timeout the process group receives
`SIGKILL` and the response uses exit code 124 with `timed_out: true`.

## `ui.hello`

Registers this connection as the approval UI. It is required before any other UI
method. A newer registration replaces an older one and cancels/re-prompts the
active request.

```json
{"version": 1, "id": "hello-1", "method": "ui.hello"}
```

Response:

```json
{"version": 1, "id": "hello-1", "status": "ok"}
```

Errors: `not_ui` (wrong label or UID), `already_registered` (this connection
already registered), `ui_register_failed`.

## `ui.prompt` (daemon to UI)

After `ui.hello`, the daemon may send an unsolicited prompt on that connection.
It is the only message without a client request `id`; `type` identifies it.

```json
{
  "version": 1,
  "type": "ui.prompt",
  "prompt_id": "32-hex-characters",
  "nonce": "32-hex-characters",
  "request_id": "unique-request-id",
  "label": "User::Pkg::org.example.app",
  "package": "org.example.app",
  "command": "id",
  "command_bytes": 2,
  "command_truncated": false,
  "timeout_ms": 60000
}
```

`prompt_id` and `nonce` are fresh 128-bit random values per prompt. The UI must
return both. The daemon generates the package identity from the peer label; the
requester cannot supply it. The command is a display copy only: control and
bidirectional formatting characters are replaced with U+FFFD, and long commands
are truncated with `command_truncated: true`.

Only one prompt is visible globally. The UI should treat the prompt as a request
for **full root access**, not as approval for a specific high-level action.

## `ui.decision`

```json
{
  "version": 1,
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
{"version": 1, "id": "decision-1", "status": "ok"}
```

Errors: `not_registered`, `stale_prompt` (wrong nonce, prompt, UI connection, or
expired prompt), `invalid_prompt_token`, `approval_unavailable`.

Semantics:

- `deny` — the pending request returns `status: denied`, `error: user_denied`;
- `once` — the pending request executes once; the next command prompts again;
- `always` — the package label is persisted in the root-only grant file before
  the pending request executes; later requests from that exact label bypass
  prompting until revoked.

## `grants.list`

```json
{"version": 1, "id": "list-1", "method": "grants.list"}
```

Response:

```json
{
  "version": 1,
  "id": "list-1",
  "status": "ok",
  "grants": [
    {
      "label": "User::Pkg::org.example.app",
      "package": "org.example.app",
      "granted_at": 1770000000
    }
  ]
}
```

`granted_at` is Unix seconds. Grants are sorted by exact SMACK label.

## `grants.revoke`

```json
{
  "version": 1,
  "id": "revoke-1",
  "method": "grants.revoke",
  "label": "User::Pkg::org.example.app"
}
```

Response:

```json
{"version": 1, "id": "revoke-1", "status": "ok", "revoked": true}
```

`revoked` is `false` if the grant did not exist. Errors: `invalid_label`,
`grant_save_failed`, `not_registered`.

## Prompt and execution limits

- one visible prompt globally;
- one pending request per package label;
- a bounded global queue;
- a short prompt timeout that defaults to deny;
- no background retry that turns one approval into multiple executions;
- allow-once decisions are valid for exactly the pending request ID and are not
  persisted; a daemon restart discards them.
