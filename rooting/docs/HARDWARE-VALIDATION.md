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
