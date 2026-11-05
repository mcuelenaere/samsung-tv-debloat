# tv-auth approval and grant-manager UI

This is the M2 managed Tizen .NET UI for `tv-authd`. It has two jobs:

1. show the daemon-generated approval prompt when an application requests root;
2. list and revoke persistent **Always allow** grants.

The UI is a normal UID-5001 package with the exact SMACK label
`User::Pkg::org.tvauth.ui`. It never runs as root and has no network privilege.
It talks directly to `/run/tv-auth/broker.sock`; there is no helper process.

## Why managed .NET

The M0 spike proved that launchpad will not map a custom unsigned native ELF from
writable app storage, even when the TPK has valid author and distributor
signatures. A managed `netcoreapp3.1` Tizen `dotnet` application runs through
Samsung's signed apphost, so UEP stays enforced. Native calls are confined to
`dotnet/Interop.cs`.

## Behavior

### Approval prompt

When an app without a persistent grant connects, the daemon launches
`app_launcher -s org.tvauth.ui` and sends a `ui.prompt` frame. The UI:

- raises a full-screen notification-type EFL window (notification level TOP,
  then HIGH, MEDIUM, DEFAULT) with a dimmed scrim and a solid centered card, so
  the home screen no longer shows through the prompt;
- displays the exact package ID and SMACK label from the daemon, plus the
  daemon-generated **FULL ROOT ACCESS** warning;
- displays a sanitized display copy of the command (control and bidirectional
  formatting characters are replaced; long commands are truncated and marked);
- offers **Deny**, **Allow once**, and **Always allow**, focusing **Deny** by
  default;
- shows a live "Denied automatically in N s" countdown for the daemon timeout;
- sends `ui.decision` with the prompt ID and nonce;
- exits shortly after the daemon acknowledges the decision.

RETURN activates the focused button. BACK denies the pending prompt (it does not
silently dismiss the request). Arrow-key focus movement uses Elementary's normal
focus handling and must be re-verified on the TV for the three-button layout.

### Grant manager

If the UI is opened from the launcher with no pending prompt, or after a decision
or reconnect, it shows the persistent-grant list. Each row has a **Revoke**
button. The list is refreshed after a revoke and via the **Refresh** button.
The first 5 grants are shown.

### Failure behavior

- UI timeout or no decision: the UI enforces the daemon's `timeout_ms` locally,
  logs `prompt expired`, shows a timeout message, and exits after two seconds;
  the daemon independently denies.
- UI crash or socket loss: the daemon sees EOF and denies the active request; the
  UI clears the dead prompt and reconnects in manager mode.
- Broker restart: the UI reconnects every two seconds, re-sends `ui.hello`, and
  refreshes grants.
- After sending a decision, the UI has a five-second safety exit even if the
  broker stops responding.
- The manager list normally appears about two seconds after launch while the
  .NET runtime initializes and the grants round-trip completes.

The UI is a **best-effort consent interface**, not a proven secure-attention
path. Notification priority and key grab do not prevent another app from
imitating or obscuring the prompt. The daemon-side nonce still prevents a
different app from forging the decision message, but visual spoofing is outside
this milestone.

## Files

| File | Purpose |
|---|---|
| `dotnet/Program.cs` | EFL window, prompt/manager views, event-loop dispatch |
| `dotnet/BrokerClient.cs` | direct Unix-socket NDJSON client and reconnect loop |
| `dotnet/Interop.cs` | the complete P/Invoke surface |
| `dotnet/tizen-manifest.xml` | package/app `org.tvauth.ui` |
| `build-ui.sh` | .NET Core 3.1 publish and unsigned TPK assembly |

## Build

```sh
./build-ui.sh
```

This uses `mcr.microsoft.com/dotnet/sdk:3.1` and produces:

```text
out/tv-auth-ui/org.tvauth.ui-0.1.0.tpk
out/tv-auth-ui/pkgroot/bin/TvAuthUi.dll
```

Observed local artifacts:

```text
Managed assembly: 37,376 bytes
Unsigned TPK:     17,995 bytes
Signed TPK:       25,545 bytes
```

## Signing and installation

Sign the unsigned TPK with the existing TV certificate through Tizen Studio, as
with the M1 test packages:

```sh
tizen package -t tpk -s <profile> -- org.tvauth.ui-0.1.0.tpk
```

No certificate material or passwords belong in this repository. The signed TPK
must be installed through `pkgcmd` on the TV:

```sh
/usr/bin/pkgcmd -i -t tpk -p /path/to/org.tvauth.ui-0.1.0-signed.tpk
```

The daemon expects the installed label `User::Pkg::org.tvauth.ui` and UID 5001 by
default. If the package ID changes, update `--ui-package` and `--ui-app-id`.

## TV acceptance results

Completed on 2026-11-05 with `org.tvauth.ui` installed and the M2 daemon running:

1. Signed TPK installed; the process label was
   `User::Pkg::org.tvauth.ui` and notification level TOP was accepted.
2. An ungranted test package triggered the prompt; the UI attached and rendered.
3. **Deny**: `denied`/`user_denied`; the command did not run.
4. **Allow once**: a root `id` result; no grant persisted; the next request
   prompted again.
5. **Always allow**: the grant persisted and executed; later requests ran with no
   prompt, including after a daemon restart and a cold boot.
6. Manager **Revoke** returned `revoked=true`, emptied the grant file, and the
   next request prompted.
7. Prompt timeout returned `denied`/`prompt_timeout`; killing the UI while a
   prompt was pending returned `ui_unavailable`/`ui_disconnected`. Neither
   executed. The UI also logs `prompt expired` and dismisses a stale prompt at
   the daemon's timeout.
8. `/usr/bin/input_keyevent` verified RETURN activation, Right/Left focus
   movement, and BACK. The manager list renders about two seconds after launch,
   so automated tests must wait for the grants response before injecting keys.

Pause/resume and repeat-key hardening remain M4 items.

The card layout was re-verified on the TV on 2026-11-05 using
`enlightenment_info -dump_screen`. The window reported 1920x1080; the scrim
dimmed the home screen to about 12% brightness; the prompt rendered with a live
countdown and timed out to `denied`/`prompt_timeout`; and `input_keyevent
XF86Back` closed the manager. No monospace font is installed, so the command
falls back to the proportional UI font.

## Known limitations

- No friendly package-name resolution: the UI shows the exact package ID and
  SMACK label, which are the security-relevant identities. A display-name lookup
  is a future convenience, not a trust input.
- No scrolling: long command lines are truncated to 12 wrapped lines and manager
  lists to 5 entries.
- Notification TOP, three-button focus movement, RETURN, and BACK were validated
  on the target, but spoofing, occlusion, held/repeat keys, pause/resume,
  compositor restart, and relaunch-while-running remain M4 hardening items.
- The UI can be visually imitated by another app. Only the IPC decision is
  authenticated.
- The daemon records the UI package label and UID, not a signing certificate.
  Package replacement/upgrade policy is an M4 hardening item.
