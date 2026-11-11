# Native SDB client compatibility on tv-living

Date: 2026-11-11
Target: `T-PTMDEUC-REL-202602250226`, `sdbd 2.2.31`, plugin `3.8.5_TV_REL`

## Result

The reference `samsung-tv-root` method works on this TV, and the native
`tv-sdb` path now matches it:

```console
$ tv root 172.17.2.110 --command 'id'
...
uid_after=0
euid_after=0
gid_after=0
egid_after=0
...
uid=0(root) gid=0(root) ... context="User"
```

The native client initially looked like it was being rejected by the TV. The
television was not blocking the foothold; three native-client details were
wrong.

## Fixes

1. **Callback URL syntax.** The TV script opens bash's `/dev/tcp` path, which
   is `host/port`, not `host:port`. The native capture wrapper used a colon,
   so the TV ran the command but could never connect back to the host
   listener.
2. **CLSE during shell open.** sdbd can close a shell channel without an
   `OKAY` when the command's output is redirected. The native client treated
   that as `TV refused service`, discarding a command that had actually run.
   `Device::shell` now treats an early CLSE as an empty shell result; other
   services still fail closed.
3. **Exploit task tag length.** The managed FDET payload searches for its own
   `task_struct` by its 15-byte `comm` tag. The port used an 11-character
   `qnr…` tag; the reference uses `qnroot…` (14 characters). The shorter tag
   produced consistent `self credential not found` failures. The native
   invocation now uses the reference-compatible `qnroot` tag.

The staged script and the injection also now share one SDB connection, and
`tv-root`/`tv-setup` reuse one `SdbTransport` connection for the whole
operation. This matches Samsung sdb's session behavior and avoids sdbd's
connection churn.

## Verification

- Reference `qn90b root --command 'id'`: UID/GID 0 with the .NET Core 3.1
  payload.
- Native `tv root 172.17.2.110 --check`: compatibility fingerprint passes.
- Native `tv root 172.17.2.110 --command 'id'`: UID/GID 0 and `exec_begin`.
- Native `tv set` / `tv root` callback fixture and `tv-sdb` tests pass.

No TV-side validation or persistence guard was weakened.
