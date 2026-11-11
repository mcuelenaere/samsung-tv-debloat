# Hardware validation remaining

The host-side implementation and its fixtures are complete. The following
checks still require an authorized TV and are intentionally not run by the
agent or the host test suite.

## Standalone root

- Run `tv root <tv> --check` on each reviewed build and confirm the
  fingerprint gates match the expected UID, build, kernel, Tizen, FDET and
  PontusM capabilities.
- Run `tv root <tv> --command 'id'` and confirm UID/GID 0 and the exit marker
  on the QN90B/S95B .NET 6 payload and on a positively identified .NET Core
  3.1 target.
- Confirm staged files are removed after success and after an ordinary error.
- Verify a concurrent `tv root` / `tv setup` attempt reports `busy`.

## One-shot setup and packages

- Build the static ARM `tv-pkg` (separate implementation) and every suite
  component, then run `rooting/build-suite-bundle.sh` with the exact reviewed
  `TARGET_BUILD`.
- Fresh install: `tv setup <tv> --bundle <dir> [--key <pub>]`; confirm every
  component is registered, `tv-authd` answers on the broker socket, the UI is
  registered, and the five services are active.
- Idempotent update: rerun setup with identical packages (`tv-pkg` should
  report `unchanged`) and with a changed package (only that component should
  change). Confirm grants, trust, configuration, host keys and
  `authorized_keys` survive.
- Confirm the persistence hook is applied last and that an unknown live hook
  fails closed without replacement.
- Cold boot: confirm Dropbear, `tv-authd`, the popup daemon, blocker, proxy and
  UI startup, and that the manager/dock remains usable.

## SSH key enrollment

- `tv ssh-key add <tv> <public-key>` then confirm the TV prompt shows the
  expected fingerprint and comment.
- Deny leaves `authorized_keys` unchanged and deletes the drop file.
- Allow adds the key exactly once; repeating the same key does not duplicate
  it, and existing comments/keys remain intact.
- Confirm the drop is ignored when it is a symlink, too large, not text or
  contains no valid public key.

## Read-only results: tv-living.home, 2026-11-11

Target: `T-PTMDEUC-REL-202602250226`, Linux `5.4.77`, `armv7l`,
`sdbd 2.2.31`, plugin `3.8.5_TV_REL`, developer host `172.17.2.206`.

- SDB connect, `capability:` and ordinary shell verbs work with the native
  client.
- Plain `0 appinstall tpk nonexistent.tpk` reaches pkgcmd and reports
  `Package not found [-5]`.
- The package-name foothold is blocked on this TV now: both the native client
  and Samsung's `sdb` send `shell:0 appinstall tpk a\`id\`.tpk`, and sdbd
  closes the channel. Backtick, `$`, `$(...)`, pipe and redirection variants
  are refused; a bare `;` is accepted by the validator but did not produce
  observable execution.
- Consequently `tv root --check` fails closed before the compatibility
  fingerprint callback, and `tv root --command` cannot run. `tv-root` now
  probes the foothold explicitly and reports
  `package-name shell injection is blocked by this TV's sdbd plugin` instead of
  waiting for the callback deadline. The validator and conversion evidence is
  in [`sdb-foothold-blockers.md`](sdb-foothold-blockers.md).
- Restarting `sdbd.service` on 2026-11-11 did not change the result; the
  validator is part of the installed plugin binary, not transient daemon
  state. A cold boot has not been tried.
- `tv ssh-key add` validated the existing public key, pushed it to
  `/home/owner/share/tmp/sdk_tools/ssh-key.pub` and tv-authd logged
  `approval_queued label=sdb-push ... kind=ssh_key`. The prompt timed out, the
  daemon logged `access=ssh_key_denied ... reason=prompt_timeout`, deleted the
  drop file and left `authorized_keys` unchanged.

- All component artifacts built successfully: static musl `tv-authd` and
  `tvauth-cli`, gnueabi `tv-auth-popupd`, `tv-blockd` and `tv-patchd`, the
  `org.tvauth.ui` TPK, and static Dropbear. The setup bundle was assembled at
  `rooting/out/setup-bundle` with target
  `T-PTMDEUC-REL-202602250226` and bundle digest
  `be7f74398f7b1bd2d7345a1fe29b3fc3cba2bbaaddffdebb3b8e44656b0646fa`.
- The separate `tv-pkg` implementation verified every generated package:
  `org.tvauth.ui`, `tv-auth-popupd`, `tv-authd`, `tv-blockd`, `tv-dropbear`,
  `tv-patchd`, `tv-pkg` and `tvauth-cli` all report `ok`.

No setup, package install, persistence change or reboot was performed. The
bundle was not pushed to the TV because the SDB foothold above is blocked.
