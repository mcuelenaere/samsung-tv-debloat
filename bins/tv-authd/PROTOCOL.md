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
- default wait for the popup UI to (re)attach: 10,000 ms;
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
  send `exec`, `pkg.install`, `pkg.uninstall`, and `pkg.list`;
- **root**: UID 0 with the exact `System::Privileged` label (local
  administrator: root shell, init, system services); may send
  `pkg.install`, `pkg.uninstall`, and `pkg.list` **without a prompt**, and may
  not send `exec` or any `ui.*` method. Every prompt-free package operation
  queues a `ui.notice`;
- **approval UI**: exact `User::Pkg::org.tvauth.ui` label and UID 5001 by
  default; may send `ui.hello`, `ui.decision`, `grants.list`,
  `grants.notify`, and `grants.revoke`, and may not send `exec` or `pkg.*`.
  Two processes hold this identity: the persistent `tv-auth-popupd` (systemd
  sets the label; it registers with `ui.hello` and receives prompts/notices)
  and the user-launched `org.tvauth.ui` grant manager (`grants.*` only, no
  `ui.hello`).

The UI package and UID are daemon settings (`--ui-package`, `--ui-uid`). Missing,
unexpected, or non-package labels fail closed; UID 0 with any other label is
also refused.

Only the **root** role may install, upgrade, or remove the approval UI's own
package (`--ui-package`, default `org.tvauth.ui`). Ordinary packages receive
`denied`/`manifest_forbidden` for an install and `denied`/`protected_package`
for an uninstall. This prevents a package signed with the same on-device author
key from replacing the consent UI.

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
| `ui_unavailable` | no popup UI attached within the attach timeout |
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
standard error), base64-encoded. `re_signed` is `true` when the daemon replaced
the package's signatures with the on-device author/distributor CAs before
installing (unsigned or foreign-signed input). `pkgcmd` validates the result
like any other package.

The request lifecycle:

1. `validate_install`: absolute path (≤ 4096 bytes, no NUL) and a valid timeout;
2. path allowlist check and streamed copy into `/opt/tv-auth/staging/<random>`
   (default cap 128 MiB, configurable up to 256 MiB; SHA-256 recorded);
3. zip hygiene and manifest inspection (`config.xml` for wgt,
   `tizen-manifest.xml` for tpk);
4. `ui.prompt` with the package metadata;
5. trust enrollment (`enroll_failed` if it cannot be repaired);
6. for `signature` in `unsigned_will_resign`/`foreign_will_resign`: re-sign with
   `xmlsec1` and the on-device keys (`sign_failed` on error); a `user_ca` or
   `samsung` package is installed as-is;
7. install through `wascmd` (`webappservice`), which also registers the app in
   Samsung Home and supports WGT; the stock `wascmd` runs inside a private
   mount namespace with a symlink overlay of `/etc` that has no `release` file. Without the
   WAS backend (`--no-was`) the daemon falls back to
   `pkgcmd -i -t <type> -p <staged>` with no Home registration;
8. response, then the staged copy is deleted on every path.

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
| `author_mismatch_requires_reinstall` | `denied` | an upgrade of a package signed by a different author (Tizen refuses cross-author upgrades); uninstall first |
| `sign_failed` | `error` | on-device re-signing failed or signing keys are unavailable; nothing was installed |
| `user_denied` | `denied` | the user chose Deny |
| `prompt_timeout` | `denied` | no decision before the prompt timeout |
| `enroll_failed` | `error` | trust enrollment could not be established; nothing was installed |
| `install_failed` | `error` | `pkgcmd` failed or timed out; `detail` and `log_tail_b64` are included |
| `install_busy` | `busy` | another install is in progress |
| `package_support_disabled` | `error` | the daemon was started with `--no-package-support` |
| `ui_unavailable`, `prompt_queue_full`, `request_already_pending`, `approval_wait_timeout` | as for `exec` | the prompt could not be completed |

If the requesting app closes its socket while the prompt is pending, the prompt
is cancelled and no package is installed.

`pkg.install` is refused for the UI role (`denied`, `ui_role`). For the root
role it runs the same validation, staging, policy, and trust pipeline with no
prompt, and queues a `kind: "install"` notice.

## `pkg.uninstall`

Ask tv-authd to remove an installed package. Requires protocol version 2.

```json
{
  "version": 2,
  "id": "uninst-1",
  "method": "pkg.uninstall",
  "pkgid": "org.example.app",
  "timeout_ms": 120000
}
```

The daemon validates the id, rejects platform-reserved prefixes, and reads the
installed package's type, version, name, and origin from the package manager
database (`sqlite3 -readonly /opt/dbspace/.pkgmgr_parser.db`, `package_info`
joined with the "No Locale" `package_localized_info` row). Only packages that
are not `system`, not `preload`, `removable`, and of type `wgt`/`tpk` can be
removed. Ordinary packages are always prompted with `kind: "uninstall"`; the
root role removes without a prompt and queues a `kind: "uninstall"` notice.
Installs and removals share one global lock.

Success:

```json
{
  "version": 2,
  "id": "uninst-1",
  "status": "ok",
  "pkgid": "org.example.app",
  "log_tail_b64": "..."
}
```

`log_tail_b64` is the last 8 KiB of `pkgcmd -u -t <type> -n <pkgid>` output.

| Error | Status | Meaning |
|---|---|---|
| `invalid_pkgid` | `error` | empty, oversized, or not a valid package id |
| `invalid_timeout` | `error` | timeout is zero or above the maximum |
| `manifest_forbidden` | `denied` | package id uses a platform-reserved prefix |
| `unknown_package` | `error` | not installed (or not described by the database) |
| `unsupported_package_type` | `denied` | not a `wgt` or `tpk` package |
| `protected_package` | `denied` | system, preload, or non-removable package |
| `package_list_failed` | `error` | the package database query failed |
| `user_denied`, `prompt_timeout`, `ui_unavailable`, `approval_wait_timeout` | as for `exec` | the prompt could not be completed |
| `uninstall_failed` | `error` | `pkgcmd` failed or timed out; `detail` and `log_tail_b64` are included |
| `install_busy` | `busy` | an install or removal is in progress |
| `package_support_disabled` | `error` | the daemon was started with `--no-package-support` |

`pkg.uninstall` is refused for the UI role (`denied`, `ui_role`).

## `pkg.list`

Read-only listing of installed third-party packages. Requires protocol version 2
and has no prompt; any accepted role except the UI may call it.

```json
{"version": 2, "id": "list-1", "method": "pkg.list"}
```

```json
{
  "version": 2,
  "id": "list-1",
  "status": "ok",
  "packages": [
    {
      "pkgid": "org.example.app",
      "name": "Example",
      "pkg_version": "1.0.0",
      "pkg_type": "wgt",
      "main_app_id": "org.example.app.Ui"
    }
  ]
}
```

The same database filter as `pkg.uninstall` applies, so an app store can tell
which packages are installed and upgradable. At most 256 entries are returned,
sorted by package id; `main_app_id` is omitted when the database has none.
Failures return `package_list_failed`; a daemon without package support returns
`package_support_disabled`.

## `ui.hello`

Registers this connection as the prompt/notice UI (`tv-auth-popupd`). It is
required before `ui.decision`; `grants.*` work without it. A newer
registration replaces an older one and re-prompts the active request.

```json
{"version": 2, "id": "hello-1", "method": "ui.hello"}
```

Response:

```json
{"version": 2, "id": "hello-1", "status": "ok"}
```

The daemon never launches a UI. The approval manager writes this response, so
it always precedes any `ui.prompt` or `ui.notice`.

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
`exec`. Only `kind: "install"` carries the `install` object; only
`kind: "uninstall"` carries `uninstall`:

```json
"uninstall": {
  "pkgid": "org.example.app",
  "name": "Example",
  "pkg_version": "1.0.0",
  "pkg_type": "wgt"
}
```

Both objects' strings are sanitized and length-limited like the command. Within
`install`:

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
      "command_truncated": false,
      "kind": "exec"
    }
  ]
}
```

`kind` is `exec`, `install`, or `uninstall`. `exec` entries are granted package
executions; `install`/`uninstall` entries come from prompt-free root package
operations and carry `package: "root"` with `command` reading
`install <pkgid>-<version>` or `remove <pkgid>`. Entries coalesce per label and
kind.

Each entry coalesces one package's executions since the previous notice;
`count` is a delta, so the UI adds it to what it is showing. `command` is a
display copy (sanitized like `ui.prompt`, at most 256 bytes) of the most recent
one. Delivery is near real-time:

- a granted exec is sent at once;
- execs within the next 250 ms are coalesced per package into one message,
  which only caps the message rate during bursts;
- notices are held while a prompt is active or queued;
- if no popup is attached they stay queued (coalesced, at most 16 packages)
  until one attaches; a notice that fails to write twice pauses notices for
  60 s. Notices never delay or block execution.

A queued prompt with no popup attached waits up to the attach timeout
(`--ui-attach-timeout-ms`, default 10 s, covering a systemd restart of
`tv-auth-popupd`) and is then denied with `ui_unavailable`.

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
`not_found` (no grant for that label), `invalid_label`, `grant_save_failed`.

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
`grant_save_failed`.

## Package installation policy

- Source paths: `/opt/usr/home/owner/**`, `/opt/media/**`, `/tmp/**`; regular
  file, no symlink at the final component, ≤ 128 MiB by default
  (`--max-package-bytes`, maximum 256 MiB).
- Zip: ≤ 4096 entries, ≤ 512 MiB uncompressed, no absolute names and no `..`.
- Package ids starting with `com.samsung.`, `org.tizen.`, or `samsung.` are
  refused, as are platform-only privileges (the installer's
  own privilege check remains authoritative).
- Installs and removals go through `webappservice` by default: `wascmd -i`
  (`-u` for removal) runs under `unshare -m` with a top-level symlink overlay of `/etc` minus
  `release`, because the stock binary refuses to run on retail firmware. This
  is what makes TPKs appear in Samsung Home and keeps their tiles clean on
  removal. `--no-was` selects the `pkgcmd` backend instead.
- `wascmd -u` refuses packages that were not installed through the SDK path
  (`is_sdk_app`); for those the daemon falls back to `pkgcmd -u` and then
  deletes the `widgetinfo` row, the cached icon, and sets
  `memory/was/app_change=1`.
- Trust material lives in `/opt/tv-auth/trust/` (`user-root-ca.pem`,
  `author-root-ca.pem`, `fingerprint_list.xml`, `tizen-ca/`, and the M3 signing
  keys). Enrollment bind-mounts the fingerprint list and CA overlay over the
  stock paths in **PID 1's mount namespace** (`nsenter -t 1 -m`), because
  `pkgcmd` only talks to `pkgmgr-server` (`package-manager.service`,
  `PrivateMounts=no`), which validates signatures there. Enrollment is
  re-verified before every approved install.
- Tizen enforces certificate *domains*: the author chain's root must be listed
  in `tizen-developer`, while the distributor chain's root must be listed in
  `tizen-public`/`tizen-partner` and must **not** be in `tizen-developer`. Two
  CAs are therefore enrolled: `author-root-ca.pem` (developer) and
  `user-root-ca.pem` (public/partner).
- Unsigned and foreign-signed packages are re-signed on-device after approval:
  `author-signature.xml` (all files, `role-author`, RSA-SHA512) with the author
  key, then `signature1.xml` (the author signature plus all files,
  `role-distributor`) with the distributor key. Templates are built by the
  daemon; `xmlsec1` computes digests, the C14N form, and the signature value.
  Keys are `0600 root` under `0700` `/opt/tv-auth/trust`.
- Uninstall/list read the same package manager database (`package_info` in
  `/opt/dbspace/.pkgmgr_parser.db`, opened read-only through `sqlite3`). Only
  non-system, non-preload, removable `wgt`/`tpk` packages can be removed;
  reserved package-id prefixes are refused first.
- One global package-operation lock covers installs and removals, including
  their prompts.
- The root role (UID 0, `System::Privileged`) uses the same staging, policy,
  and audit pipeline without a prompt, and queues a `ui.notice` on success.
  It never gets `exec`.
- Every attempt is logged with the requesting label, UID/PID, package id,
  version, SHA-256 of the staged copy (installs), decision, and result.

## Prompt and execution limits

- one visible prompt globally;
- one pending request per package label;
- one install (staging, prompt, and `pkgcmd`) globally;
- a bounded global queue;
- a short prompt timeout that defaults to deny;
- no background retry that turns one approval into multiple executions;
- allow-once decisions are valid for exactly the pending request ID and are not
  persisted; a daemon restart discards them.
