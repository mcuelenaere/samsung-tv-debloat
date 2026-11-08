# tv-authd package installation — design plan

Status: proposal, self-contained milestone numbering (M1–M3 below). Builds on
the completed tv-authd broker and the signature-validation analysis of
T-PTMDEUC (Tizen 6.5, armv7).

M1–M4 are implemented and TV-validated for the TPK path: M1 covers protocol
v2, staging, manifest inspection, PID-1 trust enrollment, `pkg.install`, the
package prompt, deny/timeout/policy paths, and cold-boot persistence; M2 adds
`pkg.uninstall`, `pkg.list`, and the prompt-free uid-0 root role with package
toasts; M3 adds on-device re-signing of unsigned and foreign-signed packages
with two enrolled CAs; M4 installs through `webappservice` so TPKs appear in
Samsung Home and uninstalls clean them up (§18). WGT installation is blocked by
this firmware for every backend `-4 Load archive info fail`. The work is
rebased onto `main` (M4 + `review-fixes`).

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
  - `user-root-ca.pem` (distributor root) and `author-root-ca.pem` (author
    root), the author/distributor EE keys and certificates (§7),
    `fingerprint_list.xml` (copy of stock with the author root SHA-1 in
    `tizen-developer` and the distributor root SHA-1 in `tizen-public` and
    `tizen-partner` — *not* in `tizen-developer`, see §7.3), and `tizen-ca/`
    (copy of `/usr/share/ca-certificates/tizen` plus both user roots).
- **Root mount-namespace requirement (`PID 1`):**
  - `tv-authd.service` uses `BindReadOnlyPaths=/opt/tv-auth:/run/opt/tv-auth`,
    which places `tv-authd` in a private slave mount namespace. Any `mount(2)`
    called directly inside `tv-authd` is invisible to `pkgmgr-server`
    (`package-manager.service`), which runs in the host (`PID 1`) mount
    namespace.
  - `ensure_enrolled()` must therefore perform its bind-mounts inside
    `/proc/1/ns/mnt` (`/usr/bin/nsenter -t 1 -m -- /bin/mount --bind ...`).
  - Because `tv-authd` itself performs enrollment in `PID 1`'s mount namespace
    at daemon startup and again before each approved install, no separate
    `tv-auth-trust.service` boot unit is needed.
- `ensure_enrolled()` is idempotent: it verifies by content that the target
  fingerprint file contains our CA and that the target `tizen-ca` directory
  contains our CA, then repairs if not. Called at daemon start and again right
  before each approved install (firmware updates / Samsung services may restore
  stock files). This firmware's VDFS reports bind mounts oddly in
  `/proc/self/mounts`, so content, not the mount table, is authoritative.
- Bind-mounts (in `PID 1`'s mount namespace):
  - `/usr/share/ca-certificates/fingerprint/fingerprint_list.xml` ← ours
  - `/usr/share/ca-certificates/tizen` ← `tizen-ca/` overlay dir
- Persistence across reboot rides the existing `/opt/etc/parseinfo.sh`
  dynamic-unit hook: enrollment is re-established by the daemon at startup and
  before each install (implemented in M1; see §12 for why a separate one-shot
  boot unit was dropped).

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
`too_large` (staging cap, default 128 MiB; configurable up to 256 MiB),
`bad_package` (zip/manifest invalid), `manifest_forbidden` (see §8 policy),
`author_mismatch_requires_reinstall`, `install_failed` (installer non-zero;
`log_tail_b64` included), `sign_failed`, `user_denied`, `prompt_timeout`,
plus the existing queue/busy errors.

Request limits: the 64 KiB line cap is fine (path, not data). One pending
install per app label (mirrors exec).

### 5.2 `pkg.uninstall` / `pkg.list` (M2)

```json
{"version":2,"id":"uninst-1","method":"pkg.uninstall","pkgid":"org.example.app","timeout_ms":120000}
```

`pkg.uninstall{pkgid}` validates the id, rejects reserved prefixes, and reads
the installed package's type, version, name, and origin from the package
manager database before prompting with `kind:"uninstall"` — every time, like
installs (§5.4). The prompt carries an `uninstall` object
(`pkgid`, `name`, `pkg_version`, `pkg_type`) so the UI can show exactly what
will be removed. Only non-system, non-preload, removable `wgt`/`tpk` packages
can be removed (`unknown_package`, `protected_package`,
`unsupported_package_type`). Installs and removals share one global lock.

`pkg.list` is unauthenticated-read (any role except the UI) and returns
installed user/third-party packages (`pkgid`, `name`, `pkg_version`,
`pkg_type`, `main_app_id`) so an app store/manager (such as Vetro) can
determine which packages are already installed or upgradable. Both methods use
one read-only SQLite query against `/opt/dbspace/.pkgmgr_parser.db`
(`package_info` joined with the `No Locale` `package_localized_info` row):
`pkginfo --listpkg` enumerates every app over D-Bus and takes ~16 s on this
firmware, while the query takes ~10 ms. `pkgcmd -u` needs the package id via
`-n`; `-p` is a package path and fails with `realpath fail`.

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
`kind` ∈ `exec | install | uninstall` (absent ⇒ `exec`). `kind: "uninstall"`
carries the same-shape object under `uninstall`:

```json
"uninstall": {"pkgid": "org.example.app", "name": "Example",
              "pkg_version": "1.0.0", "pkg_type": "wgt"}
```

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
| `pkgstage.rs` (new) | path allowlist check, `lstat` (no symlink), streamed copy to `/opt/tv-auth/staging/<random>.<wgt\|tpk>` (ensure both `/opt/tv-auth` and `/opt/tv-auth/staging` have `0711` traversal perms, `0644` staged file, and preserved `.wgt`/`.tpk` extension so `webappservice` running as `owner` UID 5001 can copy it and infer package type without exhausting `/run` tmpfs RAM), byte cap, sha256 of staged copy, cleanup guard |
| `pkgmeta.rs` (new) | `zip` crate: entry count/uncompressed-size caps; read `tizen-manifest.xml` (tpk) or `config.xml` (wgt); extract pkgid, main `appid` (`tizen_id`), version, app name, `<privilege>` list, api-version; classify `author-signature.xml`/`signatureN.xml` (`user_ca`, `samsung`, `unsigned_will_resign`, `foreign_will_resign`) |
| `pkgsign.rs` (new, M3) | in-daemon on-device re-signing after user approval (§7.3) |
| `pkginstall.rs` (new) | M1: `pkgcmd -i`/`-u` invocation, timeout, `key[end]` parsing, tail log; retained as the fallback backend. M4 adds `wasinstall.rs` (stock `wascmd` under `unshare -m` with a symlink overlay of `/etc` without `release`) for Home registration and WGT, with a `pkgcmd` + Home-cleanup fallback for `is_sdk_app` refusals (§18) |
| `pkglist.rs` (new, M2) | `pkg.list` / uninstall metadata from `/opt/dbspace/.pkgmgr_parser.db` via one `sqlite3 -readonly` query; row parsing, user-vs-system classification, `main_app_id` |
| `grants.rs` | no change (exec-only by design, §5.4) |
| `approval.rs` | `ApprovalRequest` gains `kind` + `InstallMeta`; prompt serialization; reject `always` for non-`exec` kinds; otherwise unchanged (queue, nonce, hangup-cancel, retry logic all reused) |
| `server.rs` | route `pkg.install`/`pkg.uninstall`/`pkg.list` for ordinary packages; new **root role**: uid 0 clients (label `System::Privileged`) may call `pkg.*` prompt-free — exec remains package-only; forbid all `pkg.*` for the UI role; one global lock for installs and removals |
| `trust.rs` (new) | §4 (`PID 1` mount-namespace enrollment) |
| `main.rs` | call `trust::ensure_enrolled()` at startup; allow `uid == 0` + `System::Privileged` through the pre-slot peer identity check alongside `is_valid_package_label`; `--sqlite3`/`--pkgdb` |

Staging lifecycle: copied at request time (before the prompt) to
`/opt/tv-auth/staging/<random>.<wgt|tpk>` (on persistent `/opt` flash rather than
RAM-backed `/run` tmpfs, since the TV has ~437 MiB `MemAvailable` and ~922 MB
free on `/opt`) so the file the user approves is the file that gets installed
(no TOCTOU on the source path). Because `webappservice` runs as `owner` (UID
5001) and copies the local package file itself (`CAppInstaller::m_CopyLocalAppPkg`),
`/opt/tv-auth` and `/opt/tv-auth/staging` must be `0711` (`o+x` traversal;
`grants.json` stays `0600` and `trust/` stays `0700`) and the staged file must be
`0644` with its `.wgt`/`.tpk` extension preserved. On deny/timeout/failure the
staged copy is removed and never reused; stale staging files are wiped at daemon
startup.

### 6.1 Installer & Samsung Home registration (verified by RE)

Reverse-engineering of `/usr/bin/wascmd`, `/usr/sbin/sdbd`,
`/usr/lib/libsdbd_plugin_release.so`, `/usr/lib/libwas_manager_api.so`,
`/usr/lib/libwas_common.so`, and `/usr/apps/org.tizen.webappservice/bin/webappservice`
on `T-PTMDEUC` established how Samsung Home ("My apps") registration works:

1. **Why plain `pkgcmd -i` does not show apps in Samsung Home**:
   - `/usr/bin/pkgcmd` calls `pkgmgr_client_usr_install` in `libpkgmgr-client.so.0`,
     which installs the package into `/opt/usr/home/owner/apps_rw/<pkgid>/` and
     updates `pkgmgr-info`, but **never** notifies `org.tizen.webappservice` or
     inserts a row into `/opt/dbspace/.webappservice.db` (`table widgetinfo`).
     Samsung Home (`com.samsung.tv.coba.newhome`) reads its "My apps" list from
     `/opt/dbspace/.webappservice.db` (`app_featured = 1`, `app_type = 2`,
     `install_type = 'user'`, `is_display = 1`, `install_state = 3`) and icons
     from `/opt/share/webappservice/apps_icon/all/<appid>/`.
2. **Why `/usr/bin/wascmd` cannot be exec'd directly on release firmware**:
   - At the start of `main` (`0x000243f0`), `/usr/bin/wascmd` checks
     `stat64("/etc/release", &st) == 0`. Because `/etc/release` is present on
     release firmware, `/usr/bin/wascmd` prints `"wascmd cannot working in
     released image"` and exits `0` immediately.
3. **How `sdb install` bypasses `/etc/release` and registers in Samsung Home**:
   - `/usr/sbin/sdbd` loads `/usr/lib/libsdbd_plugin_release.so`
     (`src/plugin_wascmd.cpp`). On `vd_appinstall` (`0x2710`), the plugin
     constructs `argv` for `wascmd -i <pkgid> -p <pkg_path> [-G]` and calls its
     own exported `was_execution(int argc, char **argv, int out_fd)` **in-process**
     (without executing `/usr/bin/wascmd`), which invokes
     `was_install_webapp_with_translated_error_code` (or
     `was_install_webapp_for_debug_mode` with `-G`) in
     `/usr/lib/libwas_manager_api.so`.
   - `libwas_manager_api.so` sends a D-Bus method call on UID 5001's session bus
     (`DBUS_SESSION_BUS_ADDRESS=kernel:path=/sys/fs/kdbus/5001-user/bus;unix:path=/run/user/5001/dbus/user_bus_socket`)
     to `org.tizen.tv.webappservice.server` (`webappservice`, running as `owner`
     UID 5001).
   - `webappservice` (`CTWebAppInstaller` in `libwas_common.so`):
     1. Copies the staged `.wgt`/`.tpk` file as UID 5001 (`m_CopyLocalAppPkg`) into
        `/opt/usr/home/owner/share/tmp/sdk_tools/tmp/<file>.<wgt|tpk>` and
        derives the package type from the `.wgt`/`.tpk` extension.
     2. Calls `pkgmgr_client_usr_install` (`IPkgMgr::Install`).
     3. On completion (`CTWebAppInstaller::t_InstallPost` → `m_SaveAppInfo`),
        queries `pkgmgr-info` for the installed package's `mainappid`
        (`tizen_id`, e.g. `vetrotvapp.app` or `org.tvauth.ui`), copies the app
        icon to `/opt/share/webappservice/apps_icon/all/<appid>/`, inserts/updates
        the `widgetinfo` row in `/opt/dbspace/.webappservice.db`, and calls
        `CWASUtil::NotifyHomeListChanged` (`vconf` key `memory/was/app_change = 1`)
        so Samsung Home immediately lists the app.
   - Similarly, `was_uninstall_webapp` (`wascmd -u <appid>`) runs
     `pkgmgr_client_usr_uninstall`, removes the `widgetinfo` row from
     `/opt/dbspace/.webappservice.db`, deletes the cached icon directory, and
     notifies Samsung Home. Note that `was_uninstall_webapp` expects the
     application's `appid` (`tizen_id` / `mainappid`, such as `vetrotvapp.app`),
     whereas `libsdbd_plugin_release.so`'s `wascmd_uninstall` wrapper checks
     `is_sdk_app()` first (`installed_source_type`), so `pkginstall.rs` should
     call `was_install_webapp_with_translated_error_code` and
     `was_uninstall_webapp` in `libwas_manager_api.so` (or `was_execution` for
     install) in a forked helper process with `DBUS_SESSION_BUS_ADDRESS` set and
     a `GMainLoop` driving the D-Bus completion callback, bounded by the 120 s
     timeout.

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
- Keys stay in `/opt/tv-auth/trust/` (`0600 root`); the enrolled CAs are
  the ones being trusted, so post-sign install passes.
- The prompt clearly states "unsigned — will be signed with your on-device
  CA" or "third-party signature — will be re-signed with your on-device CA"
  (`re_signed: true` in the response).

**Two CAs, not one (verified on the TV).** Tizen's validator
(`libcert-svc-vcore.so`) enforces certificate *domains* from
`fingerprint_list.xml`: `author-signature.xml`'s root must be listed in
`tizen-developer`, while the distributor signature's root must be listed in
`tizen-public`/`tizen-partner` and must **not** be in `tizen-developer`. A
single CA in all three domains fails every re-signed package with
`-12 Invalid certificate chain`. M3 therefore enrolls:

- `author-root-ca.pem` in `tizen-developer`, signing `user-author.pem`;
- `user-root-ca.pem` in `tizen-public` + `tizen-partner`, signing
  `user-distributor.pem`.

`xmlsec1 sign --id-attr Id --privkey-pem key,cert,ca template.xml` fills the
per-file digests (SHA-512 of the raw bytes), the `#prop` c14n-1.1 digest, and
the RSA-SHA512 `SignatureValue`. The templates reference every payload file
(URI-encoded, `/` → `%2F`), exclude old signature files, and carry the
`role-author`/`role-distributor` signature properties. The signed archive is
written back over the staged file.

## 8. Policy and safety limits

- **Path allowlist**: `/opt/usr/home/owner/**`, `/opt/media/**`, `/tmp/**`;
  must be a regular file, no symlink, ≤ 128 MiB by default (up to 256 MiB
  via `--max-package-bytes`), staged under `/opt/tv-auth/staging`.
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
- **Root role**: uid 0 clients with the exact `System::Privileged` label may
  use `pkg.install`/`pkg.uninstall`/`pkg.list` without a prompt (convenient
  over SSH) and never get `exec`. They run with the same staging, policy,
  trust, and audit pipeline; the pkgid shadowing ban applies to them too.
  Non-root, non-package labels still fail closed. Every prompt-free root
  operation queues a `ui.notice` so the user sees what was installed or
  removed (§9).
- **Uninstall policy**: only packages the package manager marks non-system,
  non-preload, removable, and `wgt`/`tpk` can be removed, and reserved package
  ids are refused before the database is queried. The database is opened
  `sqlite3 -readonly`; the daemon never writes it.
- **Audit**: every install attempt logged to `tv-authd.log` with source
  label, pkgid, version, sha256(staged), decision, result.
- **Failure modes**: pkgcmd failure or timeout ⇒ `install_failed` with log
  tail; trust enrollment failure ⇒ `enroll_failed` and nothing is installed;
  UI crash/hangup ⇒ same semantics as exec (cancel, never install).

## 9. UI work items (`ui/rust`, originally `ui/dotnet`)

The approval UI was ported from `ui/dotnet` to the native `ui/rust` crate after
the `/run` alias was validated; the managed implementation remains in git
history at commit `7b39883`.

- `ui/rust/src/client.rs`: parse `kind` + `install` metadata; extend
  `PromptInfo`; decision API unchanged.
- `ui/rust/src/main.rs`: install prompt layout —
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
- `kind: "uninstall"`: same layout with **REMOVE PACKAGE**, the `uninstall`
  object's name/id/version/type, and **Deny**/**Remove**. A package prompt
  without its metadata object renders "Malformed package request" with only
  **Deny**.
- Toasts: prompt-free root (`uid 0`) installs/uninstalls must still surface
  in the UI (§8) — the daemon queues a `ui.notice` whose `command` reads
  `install <pkgid>-<version>` / `remove <pkgid>`. The notice now carries
  `kind`, so the toast reads **PACKAGE CHANGE** / "root installed a package" /
  "root removed a package" instead of the root-command wording (a small UI
  change beyond the original "no UI change" assumption). Prompted operations
  show the prompt itself, so they produce no notice.

## 10. Milestones and validation

Host tests (Linux, `cargo test`) precede any TV write. TV validation only
with explicit authorization, on the user's TV.

| Milestone | Content | Host validation | TV validation |
|---|---|---|---|
| M1 | protocol v2 parsing, staging, manifest parser, trust enrollment, `pkg.install` for pre-signed tpk, `kind=install` prompt, approve/deny | **done (host, rebased on M4 + review-fixes):** 106 Linux tests in `root-broker/daemon`, clippy-clean, ARMv7 cross-build, UI build; `sign-wgt.sh` still outstanding | **validated 2026-11-08 (incl. cold boot):** trust enrollment + PID-1 namespace check, TPK install via broker, prompt UI, deny/timeout, policy refusals. WGT blocked; Home listing needs M4 (§13/§14) |
| M2 | `pkg.uninstall` (prompted), `pkg.list`, root CLI role with UI notice | **done (host):** 120 Linux tests, clippy/fmt clean, ARMv7 cross-build; uninstall/list protocol round-trips, root-role gating, notice kinds | **validated 2026-11-08:** app-role uninstall prompt, Deny and Remove, protected/unknown refusals, `pkg.list` from app and root roles, prompt-free root install/uninstall with `PACKAGE CHANGE` toasts |
| M3 | `pkgsign.rs` on-device re-signing of unsigned and foreign-signed packages | **done (host):** template/reference/signature-file unit tests, archive rewrite, author-mismatch refusal, 127 Linux tests, ARMv7 build | **validated 2026-11-08 (tpk):** foreign-signed tpk → approve → `re_signed: true` → installs; stripped Partner-signed upgrade → `author_mismatch_requires_reinstall`; two-CA domain rule verified. WGT install still blocked by `pkgcmd` (M4) |
| M4 | WAS backend (`wascmd` under a private mount namespace) so installs appear in Samsung Home | **done (host):** `/etc` symlink overlay without `release`, wascmd invocation, pkgcmd+Home fallback, 130 Linux tests | **validated 2026-11-08 (tpk):** broker install creates the Home row/icon; broker uninstall (WAS and pkgcmd fallback) removes them. WGT still fails with `-4 Load archive info fail` through `pkgcmd`, `sdb install`, and `wascmd` (firmware limitation) |

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

## 12. M1 implementation notes (host-validated)

Code (all under `root-broker/`):

- `daemon/src/protocol.rs` — protocol v2 requests and metadata, version gating,
  install validators, `kind`/`install` in `ui.prompt`, `invalid_decision`.
- `daemon/src/pkgstage.rs` — allowlist, `lstat` + `O_NOFOLLOW`, streamed copy
  with byte cap, SHA-256, cleanup guard, startup wipe of the staging root.
- `daemon/src/pkgmeta.rs` — zip hygiene, wgt/tpk manifest parsing, reserved-id
  and privilege policy, signature labelling.
- `daemon/src/trust.rs` — material validation, fingerprint parsing, idempotent
  bind-mount verification/repair.
- `daemon/src/pkginstall.rs` — `pkgcmd` invocation and `key[end] val[...]`
  parsing; `executor::execute_argv` runs it without a shell.
- `daemon/src/approval.rs`, `server.rs`, `main.rs` — prompt kind plumbing,
  `always` rejection, per-install global lock, CLI flags
  (`--staging-dir`, `--max-package-bytes`, `--pkgcmd`, `--trust-root`,
  `--no-trust`, `--no-package-support`, `--enroll-trust`).
- `ui/rust/src/client.rs`, `main.rs` — parses `kind`/`install` and renders
  the package prompt with Deny/Install and no Always option.
- `daemon/PROTOCOL.md`, `daemon/README.md`, `ui/README.md` — wire contract,
  limits, errors, and UI behavior.

Host validation (rebased on the M4 + review-fixes branches): `cargo test` (120 tests on Linux/Docker, 116 on macOS where
installer-spawning tests are skipped),
`cargo clippy --all-targets -D warnings`, ARMv7 musl cross-build (native
`rust-lld`, `--help` under QEMU), and the native UI build via
`ui/build-ui.sh`.

Deviations from the proposal, and why:

1. **No `tv-auth-trust.service` boot unit, and enrollment targets PID 1's
   namespace.** `tv-authd.service` uses `BindReadOnlyPaths=`, so systemd puts it
   in a private mount namespace; a separate oneshot unit would be isolated the
   same way. Enrollment therefore runs inside tv-authd at startup and before
   each install. TV inspection confirmed `pkgcmd` delegates to
   `pkgmgr-server` (`package-manager.service`: `Type=dbus`, `User=app_fw`,
   `PrivateMounts=no`), which runs in the root namespace. The original
   assumption that `pkgcmd` inherited the daemon's mounts was wrong. The daemon
   now enters PID 1's mount namespace with `nsenter -t 1 -m` for both the bind
   mounts and the verification reads; `--enroll-trust` uses the same path.
2. **Signature status is a label, not verification.** The daemon extracts the
   SHA-1 of every certificate embedded in the package's signature XML and
   compares them with the enrolled CA and the listed roots. It does not verify
   the XML-DSig or build the issuer chain. Our own signer embeds the user root,
   so `user_ca` is reliable for our packages; Samsung packages normally embed
   only leaf/intermediate certificates, so they usually label as `foreign_will_resign`
   ("publisher not identified") unless a listed root is embedded. The stock
   installer remains the verifier.
3. **Privilege ceiling is a conservative deny list**, not a copy of Tizen's full
   privilege table. Unknown/partner privileges pass the UX check and are still
   rejected by the installer's `CheckPrivilegeLevel` if needed.
4. **`is_upgrade` is a directory heuristic** over `/opt/usr/apps` and
   `/usr/apps`; it only changes the prompt wording.
5. **`install_busy`** was added so a second concurrent install fails fast
   (`status: busy`) instead of blocking behind the global install lock.
6. **`detail`** was added to responses for the offending privilege/pkgid list,
   bad-package reason, enrollment error, and `pkgcmd` status.
7. **v1 requests remain accepted** for the v1 methods; `pkg.*` requires v2.
   Responses and `ui.*` messages always carry `"version": 2`.
8. **The M1 backend remains `pkgcmd`.** The webappservice/WAS backend described
   in §6/§6.1 is M4; on this firmware it is required for WGT installs and
   Samsung Home registration. M1 validates TPK installs only.
9. **Staging is `/opt/tv-auth/staging`** (`0711`, `0644` files) rather than
   `/run` tmpfs, avoiding RAM pressure and matching the UID-5001
   `webappservice` requirement for the M4 backend.
10. **The fingerprint list enrolls the user CA in `tizen-developer`,
    `tizen-public`, and `tizen-partner`**; the manifest policy refuses only
    platform-only privileges (and reserved package ids). The prompt shows the
    requested privileges before approval.

Remaining validation: cold-boot persistence of enrollment (a restart was
validated; a reboot was still to be scheduled), WGT installs and Samsung Home
listing (M4), and user-CA-signed package installs (needs the signer, M3).

## 13. TV investigation: Samsung Home and WGT installs (2026-11-08)

The UI commit `51aca7b` noted that a plain `pkgcmd` install runs but does not
appear under Samsung Home's "My apps"; only `sdb install` lists it. I verified
and extended that on the TV:

- A signed TPK installed with `pkgcmd -i -t tpk -p …` works and launches, but
  creates **no** row in `/opt/dbspace/.webappservice.db` (`widgetinfo`). The same
  TPK installed with `sdb install` gets a row with `install_type=user`,
  `is_display=1`, `install_state=3`, `app_type=2`, `pkg_type=tpk`. Those rows are
  exactly the difference between being Home-listed and not.
- `pkgcmd -i -q -t … -G` (the command `sdbd` embeds for TPKs) still does not
  create the row; `-G` alone is not enough.
- `sdb install` runs the WAS path: `libsdbd_plugin.so` statically contains
  `plugin_wascmd.cpp` and calls `was_install_webapp_for_debug_mode` from
  `libwas_manager_api.so`. The install output even says
  `spend time for wascmd`.
- The standalone `/usr/bin/wascmd` is stubbed on this retail image
  (`wascmd cannot working in released image`), including `-l`, so it cannot be
  used as a registration helper from the daemon.
- **WGT packages cannot be installed with `pkgcmd` at all** on this image:
  `key[error] val[-4]`, `error message: :Load archive info fail`,
  `processing result : Operation not allowed [-4]`. Reproduced with a minimal
  signed wgt, with `-G`, with `-d`, and with a re-signed package derived from
  the known-good Vetro wgt. `sdb install` for wgt only pushes the file and
  installs nothing here either.

Consequences for the design:

1. `pkg.install` is a **TPK-only** path on this firmware. M4's WAS backend
   (§18) fixed Home registration but WGT still fails with `-4 Load archive info
   fail` through `pkgcmd`, `sdb install`, and `wascmd`/`webappservice`, so the
   restriction is in the firmware's package manager, not the backend.
2. TPKs installed through the broker are not Home-visible unless a WAS
   registration step is added. They still run and can be launched with
   `app_launcher`.
3. New milestone M4: a `tv-was-install`/`tv-was-register` helper that reproduces
   the SDB install semantics. Candidates to investigate: calling
   `libwas_manager_api.so` (`was_install_webapp_for_debug_mode`,
   `was_webapps_sync`) from a small helper, or writing the `widgetinfo` row and
   checking that Home picks it up. The WAS CLI is not an option (see above).

Also fixed while investigating: `pkgmeta` now reads wgt privileges from
`<tizen:privilege name="…"/>` (the real config.xml shape) as well as element
text, instead of element text only.


## 14. TV validation results (2026-11-08)

Deployed: daemon 762,228 bytes, SHA-256
`4b2a51e4d73f498fd0688206bd6b93f55e272029d85e638c847196343efafd62`; UI signed
with the Partner profile (`tvauth-partner`, 36,214 bytes, SHA-256
`5d37e9e4b2a1fbe769093053a5d5d31480321d8635b34658dda332bf4bb005ca`) and installed with
`sdb install`. Trust material was generated on the build Mac (user root CA
SHA-1 `8C:28:28:A4:5E:C0:03:BD:B2:86:EF:97:F4:B7:FC:F2:D9:8D:9F:94`) and
deployed to `/opt/tv-auth/trust/`; the fingerprint list carries the user CA in
`tizen-developer`, `tizen-public`, and `tizen-partner`.

Validated on the target:

- daemon start and idempotent enrollment; the daemon mounts in **PID 1's**
  mount namespace (`nsenter -t 1 -m`), and a subsequent check there shows the
  target fingerprint list contains our CA and the target `tizen-ca` directory
  contains `user-root-ca.pem`. This is the namespace `pkgmgr-server` uses
  (`PrivateMounts=no`), unlike the daemon's private namespace;
- `pkg.install` approve path with a Partner-signed TPK from
  `org.tvauth.pkgprobe`: the prompt showed name, pkgid, version, type, size,
  upgrade state, privileges and signature badge; approving returned
  `status: ok`, `pkgid`, `pkg_version`, `re_signed: false`, and the `pkgcmd`
  log tail; the package installed and launched;
- Deny (`XF86Back`) returned `denied/user_denied`; an unattended prompt
  returned `prompt_timeout`; the staging directory was empty afterwards;
- `invalid_path` for `/etc/shadow`, `bad_package` for a non-zip file, and
  `manifest_forbidden` for a manifest requesting
  `http://tizen.org/privilege/appmanager.admin` (detail lists it);
- the existing Vetro grant continued to execute through the upgrade and
  `/opt/tv-auth/grants.json` was unchanged.

Bugs found and fixed during this validation:

1. `pkgcmd` prefixes its status pairs with `__return_cb …`; `last_end_value`
   now scans the whole line instead of requiring it to start with `key[`.
2. Tizen signatures embed leaf/intermediate certificates, not the root;
   classification now checks the whole chain. Samsung packages therefore
   usually label as "publisher not identified" unless a listed root is
   embedded.
3. wgt privileges use `<tizen:privilege name="…"/>`; `pkgmeta` now accepts both
   that shape and element text.
4. Trust enrollment originally mounted in the daemon's private mount namespace.
   `pkgcmd` delegates validation to `pkgmgr-server` in the root namespace, so
   those mounts were invisible to the validator. Enrollment now enters PID 1's
   namespace with `nsenter -t 1 -m` for mounts and verification.
5. Extracting the stock CA directory on macOS leaves AppleDouble `._*.pem`
   sidecars. Uploading those into `tizen-ca` makes *every* certificate chain
   fail (`-12 Invalid certificate chain`) even for stock-signed packages.
   Clean them before deployment (`find … -name '._*' -delete` or
   `COPYFILE_DISABLE=1 tar`), and keep the CA directory free of non-PEM files.

Cold boot validated on 2026-11-08 at 21:40: the AT-HUB hook started
`tv-authd` (`start units rc=0`), the daemon recreated
`/opt/tv-auth/staging`, re-enrolled trust in PID 1 (fingerprint list has the
user CA in three domains; `tizen-ca/user-root-ca.pem` present), loaded the
existing Vetro grant unchanged, and a broker TPK install after boot returned
`status: ok` with `trust_enrollment=AlreadyEnrolled`. The `org.tvauth.ui`
Home row survived the reboot.

Still pending: WGT installs and Samsung Home registration (M4); approval and
installation of user-CA-signed packages (needs the M3 signer).

## 15. M2 implementation notes and TV validation (2026-11-08)

Code:

- `daemon/src/protocol.rs` — `UninstallRequest`, `ListPackagesRequest`,
  `UninstallMeta`, `PackageView`, prompt `uninstall` object, `NoticeKind`,
  validators (`invalid_pkgid`), response `packages`.
- `daemon/src/pkglist.rs` (new) — one `sqlite3 -readonly -noheader -separator`
  query per listing or lookup against `/opt/dbspace/.pkgmgr_parser.db`
  (`package_info` plus the `No Locale` label row); row parsing and
  user/system classification. Why not `pkginfo`: `--listpkg` took ~16 s on the
  TV, `--pkg` ~30 ms per call, while the SQL query is ~10 ms.
- `daemon/src/pkginstall.rs` — shared `run()` for `pkgcmd`; uninstall uses
  `-u -t <type> -n <pkgid>` (`-p` is a package path and fails with
  `realpath fail: 2`).
- `daemon/src/server.rs` — `ClientRole` (UI/root/package), `handle_uninstall`
  (validation, reserved-prefix check, database lookup, `protected_package`,
  prompt, `pkgcmd`, audit), `handle_list`, prompt-free root paths,
  `notify_package_operation`, and the shared install/removal lock.
- `daemon/src/approval.rs` — `uninstall` metadata on prompts and
  `ApprovalHandle::package_notice` (`PackageNotice` command, coalesced per
  label+kind).
- `daemon/src/identity.rs` — `ROOT_LABEL`/`is_root_identity` (uid 0 **and**
  `System::Privileged`); `main.rs` admits that identity through the pre-slot
  check and adds `--sqlite3`/`--pkgdb`.
- `daemon/test-clients/rootcli/` — dependency-free static ARMv7 client for the
  root role (`list`, `install <type> <path>`, `uninstall <pkgid>`). It must run
  from `/run`: the VDFS in `/opt` rejects unsigned ELFs.
- `ui/rust` — parses `uninstall` and notice `kind`; renders the remove prompt
  from the daemon's metadata and the package-change toast.

Deployed: daemon 787,372 bytes, SHA-256
`50e246fa4275dbd2bf6cedf05dc4b0a080a354d4bf9ef1d2f092ec8f0c37e915`; UI signed
with the Partner profile (36,640 bytes, SHA-256
`e6edecfc6f9507f83c1224488ac14adc09a8a7fc9f5c01134d3b0720c69c21e8`).

Validated on the target:

- `pkg.list` from a uid-0 client returned 126 installed user packages in
  milliseconds; from the probe package it returned 127 (including itself).
  Results include `name`, `pkg_version`, `pkg_type`, and `main_app_id`.
- root `pkg.install` of `/tmp/org.tvauth.pkgtest-0.1.0.tpk` ran without a
  prompt (`approval=root`), returned `status: ok`, and produced a
  **PACKAGE CHANGE** toast "root installed a package" /
  `$ install org.tvauth.pkgtest-0.1.0`.
- root `pkg.uninstall` returned `status: ok` and produced "root removed a
  package" / `$ remove org.tvauth.pkgprobe`.
- app-role `pkg.uninstall` (from `org.tvauth.pkgprobe`) rendered **REMOVE
  PACKAGE** with requester, "Package install test · org.tvauth.pkgtest ·
  version 0.1.0 · TPK", **Deny**/**Remove**, and the countdown. Deny returned
  `denied/user_denied` with the package intact; Remove returned `status: ok`
  and the package was gone.
- `manifest_forbidden` for a reserved id, `protected_package` for a
  non-reserved system package, `unknown_package` for a missing one, and
  `invalid_pkgid` for a malformed id, all without a prompt.
- The UI role is refused for all three `pkg.*` methods; root `exec` is refused
  with `root_role`.

Notes:

- `pkginfo --listpkg` is unusably slow (~16 s) because it enumerates every app
  over D-Bus, hence the direct database read. `sqlite3` is present on this
  firmware and already used by M4 for `widgetinfo`.
- `pkgcmd -u -p <pkgid>` fails; `-n <pkgid>` is correct.
- Uninstalling a package that was installed through `sdb` leaves its Samsung
  Home `widgetinfo` row behind (`pkgcmd` never contacts `webappservice`). M4
  owns registration and that cleanup; the probe's stale row was removed
  manually after validation.
- The fingerprint list still has the user CA in `tizen-developer`,
  `tizen-public`, and `tizen-partner`; no trust changes were needed for M2.

## 16. M3 implementation notes and TV validation (2026-11-08)

Code:

- `daemon/src/pkgsign.rs` (new) — unpack the staged archive (safe names, caps),
  drop old `author-signature.xml`/`signatureN.xml`, build the two templates,
  run `xmlsec1 sign --id-attr Id --privkey-pem <key>,<cert>,<ca>` with the work
  directory as cwd, strip the XML declaration, rewrite the archive in place;
  `installed_author_fingerprint` reads the installed author leaf certificate for
  the upgrade check.
- `daemon/src/trust.rs` — two CAs (`user-root-ca.pem` distributor,
  `author-root-ca.pem` author) with content verification of both in the target
  `tizen-ca` and both fingerprints in the target list; key paths exposed for
  signing.
- `daemon/src/pkgmeta.rs` — `SignatureContext` carries both roots; a chain
  containing either is `user_ca`.
- `daemon/src/server.rs` — re-sign decisions, `sign_failed`, early
  `author_mismatch_requires_reinstall`, `re_signed` in the response.
- `daemon/src/executor.rs` — `execute_argv_in` for `xmlsec1` (relative file
  references resolve against the work directory).
- `daemon/src/main.rs` — `--xmlsec1` (default `/usr/bin/xmlsec1`).
- `ui/rust/src/main.rs` — unsigned/foreign badges now promise re-signing.

Deployed: daemon 855,716 bytes, SHA-256
`ab637c0287f439221a4b545e5a63a1440acaaf45f007ae95176e9f946d0bdcec`; UI signed
with the Partner profile (36,637 bytes, SHA-256
`20358bf4f31587d9da14ee7c808f5faa38677af92ce3596459ecb27f3cd9dd22`).

Validated on the target:

- trust material: `author-root-ca.pem` (`F5:8C:18:D6…`) in `tizen-developer`,
  `user-root-ca.pem` (`8C:28:28:A4…`) in `tizen-public`/`tizen-partner`;
  `--enroll-trust` reports `already enrolled` and the target `tizen-ca` holds
  both roots;
- a foreign-signed TPK installed through the probe prompt returned
  `status: ok`, `re_signed: true`, and the log shows `result=signed files=7`;
  `pkgcmd -l` lists it and the installed `author-signature.xml` has two
  certificates with `role-author`;
- an unsigned (signatures stripped) copy of the Partner-signed UI package was
  refused with `denied/author_mismatch_requires_reinstall` before any prompt,
  because the installed UI has a different author.

Bugs found and fixed during this validation:

1. A single CA cannot sign both chains: the distributor root may not be in
   `tizen-developer`. Added a separate author root CA.
2. `xmlsec1` resolves relative Reference URIs against the working directory,
   not the template's directory; signing now runs with `cwd` set to the work
   directory.
3. `xmlsec1` does not support `--output` for `sign` on this build and requires
   `--id-attr Id` with the value as a separate argument; stdout is used.

Still pending: WGT installs (blocked by `pkgcmd`'s `-4 Load archive info fail`;
M4) — the signer itself is format-agnostic and signs WGTs the same way.

## 17. M4 investigation (2026-11-08)

The webappservice path is reachable from the device without a C helper, but
not from a plain CLI. Findings:

- **The service is on the system bus**, not the UID-5001 session bus:
  `dbus-send --bus=kernel:path=/sys/fs/kdbus/0-system/bus`. The session bus
  (`/sys/fs/kdbus/5001-user/bus`) does not carry the name. Root can open the
  system bus, but kdbus rejected `dbus-send` as root with
  `File descriptor in bad state` for the user bus; running `dbus-send` as UID
  5001 (via the new `test-clients/rootcli` `dbusrun`, since the TV has no
  `su`/`setpriv`) works.
- Service `org.tizen.tv.webappservice.server`, object
  `/org/tizen/tv/webappservice/server`, interface
  `org.tizen.tv.webappservice.server.interface`. Full introspection lists
  `was_install_webapp_with_translated_error_code`, `was_install_app`,
  `was_install_app_common`, `was_uninstall_app`, `was_command`, and the
  `managerclient`/`launcherclient` client interfaces.
- `was_install_webapp_with_translated_error_code(is_deprecated, obj_name,
  thread_id, id_type, app_id, packet_path, install_path, event_key,
  method_call)` has **no out arguments**: the service calls back into a client
  object that the caller must register, e.g.
  `org.tizen.tv.webappservice.managerclient.interface.<obj_name>` with a
  `was_install_result` method (the manager library registers that object on its
  own GDBus connection). `dbus-send` cannot own an object, so a real GDBus
  client (C helper or a Rust D-Bus client) is required to drive this method and
  receive the result.
- `was_command(cmd, app_id, pkg_path, data)` is a simpler candidate: four
  string/int arguments and no callback. The `cmd` enum is still unknown; the
  next step is to map it from `libwas_common.so` (or test candidate values on a
  throwaway package).
- `dbusrun` (`test-clients/rootcli/src/bin/dbusrun.rs`) makes UID-5001 calls
  possible from a root shell, which unblocks experimentation with the system
  bus.

Next options, in order of effort:

1. Map `was_command`'s enum and use `dbus-send` from `pkginstall.rs` after
   install/uninstall (no new binary, but relies on an undocumented enum).
2. Build a dynamically linked helper (C or Rust with `libloading`) that calls
   `was_execution`/`was_install_webapp_with_translated_error_code` and owns the
   callback object. Requires the TV glibc sysroot and an ARM toolchain, neither
   of which is in the repo yet.
3. Speak the SDB wire protocol to localhost `sdbd` and issue `vd_appinstall`
   (most work, but exactly what `sdb install` does).

## 18. M4 implementation and TV validation (2026-11-08)

**Chosen approach: the stock `wascmd` under a private mount namespace.** The
retail-firmware guard is a single `stat("/etc/release")` at the top of
`main`. Running `/usr/bin/wascmd` with a copy of `/etc` that lacks `release`
bound over `/etc` inside `unshare -m` makes it run normally, with no helper
binary and no C toolchain:

```sh
unshare -m -- /bin/sh -c 'mount --bind /run/tv-auth/etc-norelease /etc;     exec /usr/bin/wascmd -i <pkgid> -p <staged>'
```

Options 3 (a dynamically linked FFI helper) and 4 (a D-Bus client) from §17
were not needed: `was_uninstall_webapp` has a multi-argument callback contract,
and the D-Bus install method is asynchronous with a client-owned object.

Code:

- `daemon/src/wasinstall.rs` (new) — builds `/run/tv-auth/etc-norelease`, a
  directory of **top-level symlinks** for every `/etc` entry except `release`
  (existing top-level symlinks such as `mtab -> ../proc/mounts` are preserved;
  everything else points into `/run/tv-auth/etc-real`). No file data is copied:
  the farm is one `read_dir` (~174 entries, ~1 ms). Per operation it runs
  `unshare -m -- sh -c 'mount --bind /etc /run/tv-auth/etc-real; mount --bind
  <farm> /etc; exec <wascmd> …'` through `executor` with the request timeout;
  succeeds only on exit 0 plus `install completed`/`uninstall completed`.
- `daemon/src/server.rs` — `install_package`/`uninstall_package` pick the WAS
  backend when configured, else `pkgcmd`. Uninstall tries `wascmd -u` first
  (which removes the Home tile) and falls back to `pkgcmd -u` plus
  `clean_home_registration` (delete the `widgetinfo` row via `sqlite3`, remove
  the cached icon, set `memory/was/app_change=1`) when `is_sdk_app` refuses.
  `is_upgrade` now comes from the pkgmgr database rather than a leftover app
  directory, which also keeps the M3 author check from reading stale files.
- `daemon/src/main.rs` — `--no-was` (keep the `pkgcmd` backend); builds the
  symlink overlay at startup (`was_ready`) and logs both paths. Because it is
  one directory read, startup is not delayed.
- `daemon/src/pkginstall.rs` — `pkgcmd` remains as the fallback backend.

Deployed: daemon 864,820 bytes, SHA-256
`36ed2cdc1e2e828255b936f0035a12c192309fdfa2e8a9c9a3f810acd74d38b7`.

Validated on the target:

- broker TPK install with re-signing: `result=signed files=7` then
  `backend=was result=installed`; `/opt/dbspace/.webappservice.db` gained
  `org.tvauth.pkgtest` with `app_featured=1, app_type=2, install_type=user,
  is_display=1, install_state=3`, and the icon appeared under
  `/opt/share/webappservice/apps_icon/all/org.tvauth.pkgtest/`;
- broker uninstall: `backend=was result=uninstalled`, package, row, and icon
  all gone;
- fallback: a `pkgcmd`-installed package (with a manually inserted Home row)
  uninstalled as `backend=pkgcmd+home`; the row and icon were removed;
- WGT: a minimal hand-written WGT, a previously built test WGT, and a
  repackaged copy of the installed Vetro WGT (renamed package) all fail with
  `install failed[118004][-4:Operation not allowed:Load archive info fail]`
  through `wascmd`. This matches the earlier `pkgcmd` and `sdb install`
  results, so WGT cannot be sideloaded on this image.

Bugs found and fixed:

1. `wascmd -u` refuses packages that were not installed through the SDK path
   (`is_sdk_app`); a `pkgcmd` + Home-cleanup fallback is required for the
   packages installed by M1–M3.
2. The upgrade heuristic used app directories, so a leftover
   `/opt/usr/apps/<pkgid>` after uninstall made the next install look like an
   upgrade and triggered the M3 author check against stale signature files.
   `is_upgrade` now comes from `package_info`.
3. `xmlsec1`/`wascmd` must run with a working directory or explicit absolute
   paths; the namespace script uses absolute paths throughout.

Cold boot (2026-11-08 22:47) validated the new pieces: the AT-HUB hook started
the daemon, `starting`/`staging_ready`/`was_ready`/`listening` happened within
one second, trust
re-enrolled in PID 1 with both roots (fingerprint list 1 author + 2
distributor, both CA files in `tizen-ca`), the `org.tvauth.ui` Home row
survived, and a broker TPK install/uninstall still used `backend=was` and
created/removed the Home row and icon. Earlier versions copied `/etc` (12 MiB,
1,240 files) and delayed listening by ~20 s on a cold cache; the symlink farm
removed both the copy and the delay.

Known limitation: WGT packages are rejected by the firmware's package manager.
The WAS backend is in place, so if a future firmware accepts WGT, the daemon
needs no change.
