# tv-auth-popupd

Persistent prompt and toast overlay for `tv-authd`. It replaces the old
on-demand launch of `org.tvauth.ui` (1–3 s AUL cold start, a notice-only
heuristic, and launch retry/cooldown state in the daemon).

- systemd runs it as uid 5001 with `SmackProcessLabel=User::Pkg::org.tvauth.ui`
  (the identity the daemon accepts for `ui.hello`), outside AUL: plain
  `elm_init`/`elm_run`.
- It stays connected and invisible. Idle: no mapped window, no key grab, no
  timer (broker events wake the loop through an `Ecore_Pipe`), 0 CPU, ~7 MB RSS
  before the first window and ~13 MB after.
- `ui.prompt`: full-screen modal, BACK grabbed only while it is shown.

**Notification levels (fixed 2026-11-09):** the prompt and the toast are at
`E_LAYER_CLIENT_NOTIFICATION_TOP` (layer 800). Two bugs, both inherited from
the managed UI, had kept every earlier prompt lower: `tv-efl` sent
`set_notification_level` values 1–4, but `efl_util_notification_level_e` is
`DEFAULT=10 … TOP=40` and e20 maps unknown values to `NOTIFICATION_LOW`; and
`elm_win_floating_mode_set` moves a window to `E_LAYER_CLIENT_ABOVE` whatever
its level. e20 does not care whether the client was launched by AUL. Check the
layer with `enlightenment_info -connected_clients` (and the real screen with
`enlightenment_info -dump_screen`).

## Behavior

### Approval prompt

When an app without a persistent grant connects, the daemon sends a
`ui.prompt` frame to the attached popup, which:

- raises a full-screen notification-type EFL window (requesting notification
  level TOP, then HIGH, MEDIUM, DEFAULT) with a dimmed scrim and a solid centered card;
- displays the exact package ID and SMACK label from the daemon, plus the
  daemon-generated **FULL ROOT ACCESS** warning;
- displays a sanitized display copy of the command (long commands are
  truncated and marked);
- offers **Deny**, **Allow once**, and **Always allow**, focusing **Deny** by
  default;
- shows a live "Denied automatically in N s" countdown for the daemon timeout;
- sends `ui.decision` with the prompt ID and nonce;
- hides the window and releases BACK shortly after the daemon acknowledges.

**Always allow** is bound to a SHA-256 of the installed package's
`author-signature.xml`. A package upgrade, changed reinstall, or signer change
invalidates the grant and the next request prompts again. If the daemon cannot
verify the package identity, it fails closed and reports
`package_identity_unavailable`.

### SSH key prompt

`kind: "ssh_key"` comes from tv-authd itself when a key is pushed to
`/home/owner/share/tmp/sdk_tools/ssh-key.pub` (requester `sdb-push`, normally
written by `tv ssh-key add`); its
`command` lists the keys as `type fingerprint comment` lines. The popup shows
them with a warning and offers **Deny** (focused) and **Allow** only.

### Package prompt

For `pkg.install` the daemon sends `kind: "install"` with an `install` object.
The UI shows the requester, package name/id/version/type/size, an upgrade note,
a signature badge, and the requested privileges (up to 7; the rest are
summarised). It offers **Deny** and **Install** only; there is deliberately no
**Always allow** for package operations.

For `pkg.uninstall` the daemon sends `kind: "uninstall"` with an `uninstall`
object and a `remove <pkgid>` command. The same layout shows **REMOVE PACKAGE**,
the package facts, **Deny**, and **Remove**. A package request without the
matching object renders "Malformed package request" with only **Deny**.

### Execution toast

A granted app's executions arrive as `ui.notice` within 250 ms. A separate
focus-skip notification window in the top-right corner shows up to three apps,
e.g. "**org.example.app** ran 3 commands as root", plus the last command's first
line, without grabbing keys or taking focus. New notices update the counters in
place and keep the toast up for 6 s after the last one.

Prompt-free root package operations also arrive as `ui.notice` with
`kind: "install"`/`"uninstall"` and `package: "root"`, rendered as
**PACKAGE CHANGE**.

### Failure behavior

- Prompt timeout: the popup mirrors the daemon's `timeout_ms`, logs
  `prompt expired`, shows a timeout message, and hides after two seconds; the
  daemon independently denies.
- Popup crash: systemd restarts it within 2 s; the daemon re-sends an active
  prompt up to twice after a disconnect and waits up to
  `--ui-attach-timeout-ms` (10 s) for a popup before denying with
  `ui_unavailable`. Notices queue until a popup attaches.
- Broker restart: the popup reconnects every two seconds and re-sends
  `ui.hello`; it hides any prompt from the dead connection.
- After sending a decision, the popup hides after five seconds even if the
  broker never answers.

The UI is a **best-effort consent interface**, not a proven secure-attention
path. Notification priority and key grab do not prevent another app from
imitating or obscuring the prompt. The daemon-side nonce prevents a different
app from forging the decision message, but visual spoofing is outside this
milestone.

## Build and deploy

A replaced binary must be relabelled `_` (`chsmack -a _`) before the unit
starts it: a file written as root over SSH gets `System::Privileged`, which
the popup's label cannot read, and systemd reports `203/EXEC`.

```sh
./build-armv7.sh            # ../../toolchain/cross-build.sh gnueabi; needs ../../toolchain/sysroot
```

On the TV (the unit maps `/opt/tv-auth` at `/run/opt/tv-auth` for UEP, like
`tv-authd.service`):

```sh
cat > /opt/tv-auth/tv-auth-popupd < tv-auth-popupd    # 0755
chsmack -a _ /opt/tv-auth/tv-auth-popupd             # mappable by the UI label
install -m 0600 tv-auth-popupd.service /opt/tv-units/ # AT-HUB starts it at boot
cp /opt/tv-units/tv-auth-popupd.service /run/systemd/system/
systemctl daemon-reload && systemctl start tv-auth-popupd
```

The `org.tvauth.ui` package must stay installed: it owns the SMACK rules and
the `window.priority.set` privilege for the label. The log is
`/tmp/tvauth-popupd.log`; EFL's own stderr chatter goes to `/dev/null`.

## Validation (tv-living, 2026-11-09)

- A standalone overlay probe (`crates/tv-efl/examples/overlay-probe.rs`) under
  `systemd-run` with this identity created a notification window and set/unset
  the XF86Back keygrab.
- The unit started as uid 5001 with `User::Pkg::org.tvauth.ui`, mapped
  `/run/opt/tv-auth/tv-auth-popupd`, and registered with both the previous and
  the new daemon; after a `tv-authd` restart it re-attached within 2 s.
- Toasts: a Vetro first-use notice and a root `pkg.install` notice rendered at
  without launching any app.
- Prompts (requester: `tvauth-cli exec id` under `systemd-run` with an installed
  package's label): the window was visible within ~1 s of `prompt_sent`;
  timeout → `prompt_timeout` denial and the window unmapped 2 s later;
  `input_keyevent XF86Back` → `user_denied`; `Right` + `Return` → Allow once,
  `id` ran as root. **Always allow** was not exercised (it would persist a
  grant).
- Idle CPU stayed at 0 ticks over 10 s after a prompt.
- After the level fix: two consecutive prompts and two toasts each at layer
  800; a store install prompt stacked over the manager (layer 650) on the
  composited screen, and an injected BACK reached the prompt (`user_denied`).
