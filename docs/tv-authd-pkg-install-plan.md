# tv-authd package installation — design plan

Status: proposal, self-contained milestone numbering (M1–M3 below). Builds on
the completed tv-authd broker and the signature-validation analysis of
T-PTMDEUC (Tizen 6.5, armv7).

## 1. Goal

Let packaged apps ask tv-authd to install Tizen packages (wgt/tpk), with a
dedicated, package-aware approval prompt in `org.tvauth.ui` — the same
SuperSU-style UX as root exec, but showing what actually matters for an
install: package id, name, version, requested privileges, signature status,
and the requesting app.

The long-term flow also removes Samsung from the loop: packages signed by a
user-owned CA (or unsigned packages re-signed on-device) install through the
stock `pkgcmd` backend because the user CA is enrolled in the device trust
store.

## 2. Background: why installs need tv-authd at all

Signature validation on this firmware (verified by disassembly):

- `libapp-installers.so` (`ValidateSignatures` → `ValidateSignatureFile`)
  requires every signature file (`author-signature.xml`, `signatureN.xml`) to
  verify in `libcert-svc-vcore.so`:
  - XML-DSig + per-file reference hashes (xmlsec1/OpenSSL);
  - chain completion against `/usr/share/ca-certificates/tizen/` (read-only);
  - root-CA SHA-1 fingerprint must be listed in a domain in
    `/usr/share/ca-certificates/fingerprint/fingerprint_list.xml`
    (`tizen-developer` for the author signature, `tizen-public`+ for the
    distributor signature);
  - revocation errors map to `-8`, which is merely "disregarded".
- Samsung's `libcert-svc-validator-plugin.so` adds a distributor gate:
  `verifyCertChain` (OpenSSL, CA taken from the signature's embedded chain)
  plus a device check that passes if the distributor cert carries **any X.509
  extension containing the TV's DUID** (`db/comss/hwduid`), or a
  Samsung-signed `device-profile.xml` lists it.
- Privilege level (PUBLIC/PARTNER/PLATFORM) is derived from the distributor
  root's fingerprint domain; level 0 aborts the install.

Trust anchors are therefore *files*, and bind-mounts over the read-only
rootfs work (verified). Enrolling a user CA is sufficient to make the stock
installer accept self-signed packages.

## 3. High-level flow

```
app (any User::Pkg label)
   │  1. stages wgt/tpk where it can write, sends pkg.install{path,type}
   ▼
tv-authd (root)
   2. copies to staging /opt/tv-auth/staging/<random> (size cap, sha256)
   3. parses manifest + inspects signatures → InstallMeta
   4. approval manager: ui.prompt kind="install" + metadata
   5. on approve:
      a. if unsigned or signed by an untrusted CA / foreign DUID:
         re-sign in place with enrolled user CA (milestone M3)
      b. ensure trust enrollment in PID 1's mount namespace (§4)
      c. run installer backend (wascmd / pkgcmd + Home registration), stream result
   6. response {status, pkgid, version, re_signed, log} ; cleanup staging
```

## 4. Trust enrollment (one-time, daemon-managed)

New daemon module `trust.rs`:

- Material under `/opt/tv-auth/trust/` (root 0700):
  - `user-root-ca.pem`, `user-author.p12`-equivalents (see §7),
    `fingerprint_list.xml` (copy of stock + user CA SHA-1 added to
    `tizen-developer`, `tizen-public`, and partner distributor domains so
    user-approved homebrew apps can use both public and partner privileges),
    `tizen-ca/` (copy of `/usr/share/ca-certificates/tizen` + user CA).
- **Root mount-namespace requirement (`PID 1`):**
  - `tv-authd.service` uses `BindReadOnlyPaths=/opt/tv-auth:/run/opt/tv-auth`,
    which places `tv-authd` in a private slave mount namespace. Any `mount(2)`
    called directly inside `tv-authd` is invisible to `pkgmgr-server`
    (`package-manager.service`), which runs in the host (`PID 1`) mount
    namespace.
  - `ensure_enrolled()` must therefore inspect `/proc/1/mounts` and perform
    its bind-mounts inside `/proc/1/ns/mnt` (via a short helper thread/child
    that `setns(2)`s into `/proc/1/ns/mnt`, or `/usr/bin/nsenter -t 1 -m --
    /bin/mount --bind ...`).
  - Because `tv-authd` itself performs enrollment in `PID 1`'s mount namespace
    at daemon startup and again before each approved install, no separate
    `tv-auth-trust.service` boot unit is needed.
- Bind-mounts (in `PID 1`'s mount namespace):
  - `/usr/share/ca-certificates/fingerprint/fingerprint_list.xml` ← ours
  - `/usr/share/ca-certificates/tizen` ← `tizen-ca/` overlay dir

Distributor DUID gate: the user distributor certificate embeds the TV's DUID
(`vconftool get db/comss/hwduid`) in a custom X.509 extension
(`1.3.6.1.4.1.99999.1`, ASN1 UTF8String). `CheckDuid2` only substring-matches
extension data, so this satisfies it without Samsung.

## 5. Protocol v2 additions

Version stays JSON-lines over `/run/tv-auth/broker.sock`. Unknown-field
rejection means this must be a coordinated daemon+UI change; bump
`"version": 2` for the new `pkg.*` methods and UI messages while keeping
`parse_request` accepting `"version": 1` for existing `exec` clients (such as
Vetro) untouched.

### 5.1 `pkg.install` (ordinary packages)

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

Success:

```json
{
  "version": 2, "id": "inst-1", "status": "ok",
  "pkgid": "org.example.app", "pkg_version": "1.0.0",
  "re_signed": true,
  "log_tail_b64": "..."
}
```

Errors (new): `invalid_path` (outside allowlist / not a regular file),
`too_large` (staging cap, default 128 MiB), `bad_package` (zip/manifest
invalid), `manifest_forbidden` (see §8 policy),
`author_mismatch_requires_reinstall`, `install_failed` (installer non-zero;
`log_tail_b64` included), `sign_failed`, `user_denied`, `prompt_timeout`,
plus the existing queue/busy errors.

Request limits: the 64 KiB line cap is fine (path, not data). One pending
install per app label (mirrors exec).

### 5.2 `pkg.uninstall` / `pkg.list` (M2)

`pkg.uninstall{pkgid}` prompts with `kind:"uninstall"` — every time, like
installs (§5.4). `pkg.list` is unauthenticated-read and returns installed
user/third-party packages under `/opt/usr/apps` (`pkgid`, `pkg_version`,
`pkg_type`, `name`) so an app store/manager (such as Vetro) can determine
which packages are already installed or upgradable.

### 5.3 Prompt message: new `kind`

`ui.prompt` gains optional fields; v1 consumers ignore them, but daemon and
UI ship together:

```json
{
  "version": 2, "type": "ui.prompt",
  "prompt_id": "...", "nonce": "...", "request_id": "inst-1",
  "label": "User::Pkg::org.example.app", "package": "org.example.app",
  "kind": "install",
  "command": "install wgt /opt/.../org.example.app-1.0.0.wgt",
  "install": {
    "pkgid": "org.example.app",
    "name": "Example",
    "pkg_version": "1.0.0",
    "pkg_type": "wgt",
    "size_bytes": 1234567,
    "privileges": ["http://tizen.org/privilege/network", "..."],
    "signature": "unsigned_will_resign",
    "is_upgrade": false
  },
  "timeout_ms": 60000
}
```

`signature` ∈ `unsigned_will_resign | foreign_will_resign | user_ca | samsung`.
`kind` ∈ `exec | install | uninstall` (absent ⇒ `exec`).

All metadata strings pass through the existing control/bidi sanitizer.

### 5.4 Decisions: no always-allow for package operations

Install and uninstall are **always** interactive. For `kind="install"` and
`kind="uninstall"` prompts, `ui.decision` accepts only `deny` and `once`;
sending `always` is rejected with `invalid_decision`. (The protocol value
stays `once` for symmetry with exec; the UI buttons simply read "Deny" and
"Install" / "Remove" — §9.) The decision covers exactly the staged, hashed
artifact the user was shown — the grant store is never consulted for package
operations.

Exec grants are unchanged: `GrantStore` keeps its v1 shape and only the
`exec` method and `grants.*` UI methods use it. An exec grant never
authorizes an install, and there is no install grant at all.

## 6. Daemon work items

New/changed modules (sync Rust, no async — per AGENTS.md):

| Module | Change |
|---|---|
| `protocol.rs` | v2 structs: `InstallRequest`, `UninstallRequest`, `ListPackagesRequest`, prompt `kind` + `InstallMeta`, `invalid_decision` guard for `always` on non-`exec` kinds, validators, size/character limits; accept both v1 (`exec`) and v2 |
| `pkgstage.rs` (new) | path allowlist check, `lstat` (no symlink), streamed copy to `/opt/tv-auth/staging/<random>` (`0711` dir, `0644` staged file so `pkgmgr-server`/backends running as `root` or `owner` can read it without exhausting `/run` tmpfs RAM), byte cap, sha256 of staged copy, cleanup guard |
| `pkgmeta.rs` (new) | `zip` crate: entry count/uncompressed-size caps; read `tizen-manifest.xml` (tpk) or `config.xml` (wgt); extract pkgid, version, app name, `<privilege>` list, api-version; classify `author-signature.xml`/`signatureN.xml` (`user_ca`, `samsung`, `unsigned_will_resign`, `foreign_will_resign`) |
| `pkgsign.rs` (new, M3) | in-daemon on-device re-signing after user approval (§7.3) |
| `pkginstall.rs` (new) | `trust::ensure_enrolled()`, invoke installer (`/usr/bin/wascmd` or `/usr/bin/pkgcmd` + Samsung Home app-list registration — see note below) with timeout, parse result status, map failure codes (`-12/-14` etc.), tail log |
| `grants.rs` | no change (exec-only by design, §5.4) |
| `approval.rs` | `ApprovalRequest` gains `kind` + `InstallMeta`; prompt serialization; reject `always` for non-`exec` kinds; otherwise unchanged (queue, nonce, hangup-cancel, retry logic all reused) |
| `server.rs` | route `pkg.install`/`pkg.uninstall`/`pkg.list` for ordinary packages; new **root role**: uid 0 clients (e.g. SSH/dropbear, label `System::Privileged`) may call `pkg.*` prompt-free — exec remains package-only; forbid all `pkg.*` for the UI role; per-label pending tracking |
| `trust.rs` (new) | §4 (`PID 1` mount-namespace enrollment) |
| `main.rs` | call `trust::ensure_enrolled()` at startup; allow `uid == 0` + `System::Privileged` through the pre-slot peer identity check alongside `is_valid_package_label` |

Staging lifecycle: copied at request time (before the prompt) to
`/opt/tv-auth/staging/<random>` (on persistent `/opt` flash rather than RAM-backed
`/run` tmpfs, since the TV has ~437 MiB `MemAvailable` and ~922 MB free on `/opt`)
so the file the user approves is the file that gets installed (no TOCTOU on the
source path). On deny/timeout/failure the staged copy is removed and never
reused; stale staging files are wiped at daemon startup.

Installer & Samsung Home registration: a plain `/usr/bin/pkgcmd -i` installs the
package but does not register it under Samsung Home's "My apps" list (`sdb
install` invokes `/usr/bin/wascmd`). Before implementing `pkginstall.rs`, RE
`/usr/bin/wascmd` (and `/etc/package-manager/backend/*`) to use the exact
invocation or post-install registration step that makes installed apps appear in
Samsung Home.

The installer command is invoked via the existing `executor` (setsid, cleared
env, timeout 120 s, 1 MiB log caps).

## 7. Signing

### 7.1 CA and certificates (generated once, on the Mac)

- `tv-user-root-ca` (RSA 4096, 20 y, `basicConstraints=critical,CA:TRUE`,
  `keyUsage=critical,keyCertSign,cRLSign`).
- `tv-user-author` (EE signed by root CA; CN free-form).
- `tv-user-distributor` (EE signed by root CA; extra extension
  `1.3.6.1.4.1.99999.1` = UTF8String `<TV DUID>` for `CheckDuid2`).
- Deploy to `/opt/tv-auth/trust/`; private keys 0600 root.

### 7.2 Build-time signing (M1, on the Mac)

`tools/sign-wgt.sh`: unzip wgt → build `author-signature.xml` (all files,
author cert) and `signature1.xml` (all files except author-signature.xml,
distributor cert), both `xmldsig#` enveloped, exclusive C14N, RSA-SHA256,
full chain in `X509Data` — match the structure Tizen Studio emits
(verified against a reference signed package). Re-zip. This also becomes the
conformance test fixture for the on-device signer.

### 7.3 On-device re-signing (M3)

For packages with `signature == unsigned_will_resign` or
`foreign_will_resign` (e.g. community `.wgt`/`.tpk` bundles pre-signed with a
third-party developer cert for another TV's DUID), the daemon re-signs **after
the user approves** (avoiding heavy unpack/hash/xmlsec/re-zip work on unapproved
or denied requests, while verifying the staged file's SHA-256 before signing):

- Implemented directly in `pkgsign.rs` inside `tv-authd` (no separate `tv-sign`
  ELF binary to build or bind-mount), shelling out via `executor` to the on-TV
  `/usr/bin/xmlsec1` + `/usr/bin/openssl` (both present on the firmware) using
  template XML generated like §7.2. Keeps XML-DSig/C14N implementation out of
  Rust while avoiding a second binary.
- Strip any existing `author-signature.xml` and `signature*.xml` when
  re-signing a `foreign_will_resign` archive, then sign with `tv-user-author`
  and `tv-user-distributor`.
- If `is_upgrade == true` and the already-installed package on disk was signed
  by a different author certificate than `tv-user-author`, reject early with
  `author_mismatch_requires_reinstall` (Tizen rejects cross-author upgrades with
  `-12`).
- Keys stay in `/opt/tv-auth/trust/` (`0600 root`); the enrolled CA is
  the one being trusted, so post-sign install passes.
- The prompt clearly states "unsigned — will be signed with your on-device
  CA" or "third-party signature — will be re-signed with your on-device CA"
  (`re_signed: true` in the response).

## 8. Policy and safety limits

- **Path allowlist**: `/opt/usr/home/owner/**`, `/opt/media/**`, `/tmp/**`;
  must be a regular file, no symlink, ≤ 128 MiB (bounded to fit safely in
  `/opt/tv-auth/staging`).
- **Zip hygiene**: ≤ 4096 entries, ≤ 512 MiB total uncompressed, no absolute
  or `..` names (reject, don't sanitize).
- **Package identity**: reject pkgids starting with `com.samsung.`,
  `org.tizen.`, `samsung.` (no shadowing system apps). Upgrades allowed only
  when the package is already installed *and* the platform same-author rule
  will hold; the stock installer is the backstop, daemon check is UX.
- **Privilege ceiling**: enrolling the user distributor CA in both the public
  and partner fingerprint domains (§4) allows public and partner privileges
  (which homebrew TV apps and `org.tvauth.ui` itself need). Refuse manifests
  requesting platform-only privileges if the user CA is not enrolled in the
  platform domain — show the offending list in the denial response.
- **One active install globally** (serialize with the prompt queue);
  120 s hard timeout; staging dir `/opt/tv-auth/staging` is `0711 root` (with
  `0644` staged files) and is wiped at daemon start.
- **Root role**: uid 0 clients may use `pkg.install`/`pkg.uninstall`/
  `pkg.list` without a prompt (convenient over SSH). They run with the same
  staging, policy, and audit pipeline; the pkgid shadowing ban applies to
  them too. Non-root, non-package labels still fail closed. Every prompt-free
  root operation queues a `ui.notice` so the user sees what was installed or
  removed (§9).
- **Audit**: every install attempt logged to `tv-authd.log` with source
  label, pkgid, version, sha256(staged), decision, result.
- **Failure modes**: pkgcmd failure or timeout ⇒ `install_failed` with log
  tail; trust enrollment failure ⇒ `enroll_failed` and nothing is installed;
  UI crash/hangup ⇒ same semantics as exec (cancel, never install).

## 9. UI work items (`ui/dotnet`)

- `BrokerClient.cs`: parse `kind` + `install` metadata; new events
  `InstallPromptReceived` (or reuse `PromptReceived` with extended
  `PromptInfo`); decision API unchanged.
- `Program.cs`: install prompt layout —
  - Title: "Install package?" / "Remove package?"
  - Source app (label-derived), package name + id + version, size;
  - privilege list (scrollable; raw URIs mapped to friendly names for the
    ~20 common ones);
  - signature badge: "Unsigned (will be signed on-device)" / "Third-party
    signature (will be re-signed on-device)" / "Signed by your CA" / "Signed
    by Samsung store";
  - Buttons: **Deny** and **Install** (or **Remove**) — no "once" wording,
    no Always option for package operations (§5.4); existing timeout
    countdown, key grabs, backdrop.
  - Grant manager: unchanged (exec grants only).
- Toasts: prompt-free root (`uid 0`) installs/uninstalls must still surface
  in the UI (§8) — the daemon queues a `ui.notice` whose `command` reads
  `install <pkgid>-<version>` / `remove <pkgid>`; the existing toast path
  renders it with no UI change. Prompted operations show the prompt itself,
  so they produce no notice.

## 10. Milestones and validation

Host tests (Linux, `cargo test`) precede any TV write. TV validation only
with explicit authorization, on the user's TV.

| Milestone | Content | Host validation | TV validation |
|---|---|---|---|
| M1 | protocol v2 parsing, staging, manifest parser, trust enrollment, `pkg.install` for pre-signed wgt, `kind=install` prompt, approve/deny | unit tests for protocol/staging/manifest; `sign-wgt.sh` output installs in stock flow (dry-run against device files) | enroll trust, install pre-signed test wgt via broker, prompt UI flow, deny path, reboot persistence of enrollment |
| M2 | `pkg.uninstall` (prompted), `pkg.list`, root CLI role with UI notice | uninstall/list protocol round-trips, root-role gating, notice queuing | uninstall prompt flow, root CLI install/uninstall without prompt + toast appears, UI deny path |
| M3 | `pkgsign.rs` on-device re-signing of unsigned and foreign-signed packages | sign/verify round-trip on host with reference wgt; xmlsec1 template conformance vs §7.2 | unsigned / foreign-signed wgt → user approves → broker re-signs → installs → launches; appears in Samsung Home; signature survives reboot |

Out of scope (documented to avoid creep): delta updates, mount-installed
(readonly) packages, RPM/rpk installs, restoring stock trust (manual
`umount` + remove `/opt/tv-auth/trust`).

## 11. Resolved questions

1. ~~Grant capability for uninstall~~ — moot: package operations have no
   grants at all; install and uninstall always prompt (§5.4).
2. Prompt timeout for installs: keep the 60 s default (packages are staged
   before the prompt, so install size does not pressure the timeout).
3. ~~Expose `pkg.install` to root clients?~~ — yes, prompt-free, via the new
   uid-0 role (§6, §8); every such operation raises a `ui.notice` toast so
   the user is informed (§9). Exec remains package-only.
4. ~~Manifest-declared opt-in marker (`org.tvauth.allow-install`)~~ — no: the
   prompt is the whole gate, and any malicious package would just copy the
   marker.
