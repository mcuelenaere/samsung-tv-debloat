# tv-authd protocol v1

Transport: Unix `SOCK_STREAM`, one UTF-8 JSON object per line (`\n`).

Limits:

- request line: 64 KiB;
- response line: 4 MiB;
- 1 MiB each for captured stdout and stderr;
- default command timeout: 10,000 ms;
- maximum command timeout: 120,000 ms;
- maximum concurrent clients: 16 by default.

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

`id` is required, at most 128 bytes, and limited to ASCII alphanumerics plus `. _ - :`.
Unknown JSON fields, versions, or methods are rejected. Duplicate request IDs on
one connection are rejected without execution.

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

Error or denial response:

```json
{
  "version": 1,
  "id": "unique-request-id",
  "status": "denied",
  "error": "not_granted"
}
```

Statuses:

- `ok` — command completed; inspect `exit_code` and `timed_out`;
- `denied` — caller has no persistent grant; command was not run;
- `error` — malformed request, unsupported method/version, duplicate ID, or execution setup failure.

Execution uses `/usr/bin/setsid /bin/sh -c <command>` with a cleared environment
except `PATH`, `HOME=/root`, and `LANG=C`. On timeout the process group receives
`SIGKILL` and the response uses exit code 124 with `timed_out: true`.

The daemon identifies callers with `SO_PEERCRED` and `SO_PEERSEC` before reading
the first request. Only exact `User::Pkg::<pkgid>` SMACK labels are accepted.
M1 has no UI prompt protocol; grants are pre-seeded in the root-only grant file.
