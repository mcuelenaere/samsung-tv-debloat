# EFL approval-popup spike

**Outcome (2026-11-05): the native Rust ELF is not viable as an installed TV app
under UEP, but the managed .NET/EFL equivalent is validated on-device.** The
managed popup installed, launched through AUL, set notification TOP, appeared
over YouTube, and dismissed with OK/RETURN.

This document preserves both the native cross-link work and the UEP result. The
production approval UI should use the managed implementation in `dotnet/`; the
Rust implementation remains evidence for the rejected path.

**Native status:** cross-links correctly against the TV's soft-float EFL stack,
but launchpad never maps the ELF from writable `/opt`. Valid TPK author and
distributor signatures do not lift UEP for a custom native executable.
**Managed status:** fully displayed/dismissed on the TV; broader spoofing,
occlusion, focus, and manager behavior remain unvalidated.

All temporary test packages and `/run` files were removed after validation.

This is the M0 harmless display test, not an approval implementation. It has no
broker/IPC code, executes no commands, and does not touch Samsung's syspopup
database. The provisional package/application ID is `org.tvauth.spike.popup`.

## Behavior and scope

`src/main.rs` only exits with the status returned by the isolated `efl` module.
`src/efl.rs` contains all raw FFI, C strings, and callbacks. It:

1. enters a Tizen native UI application through `ui_app_main`;
2. creates an `ELM_WIN_NOTIFICATION` window;
3. tries notification levels TOP, HIGH, MEDIUM, then DEFAULT, logging each
   rejection and leaving the platform default if all requests fail;
4. shows a warning that this is a harmless test and that no root access,
   commands, or settings changes are involved;
5. focuses an **OK / Dismiss** button which exits through `ui_app_exit`;
6. registers only the TV Back key (`XF86Back`) as an additional dismiss path;
7. ungrabs that key and explicitly deletes the window during termination.

There is deliberately no Home/Exit/global key grab, network permission, async
runtime, `efl` crate, bindgen/libclang step, or broad EFL binding. A failed
keygrab is logged rather than preventing use of the button. A priority failure
is also non-fatal for this display spike; that is not a security property of a
future approval UI.

The native manifest is `tizen-manifest.xml`. It declares only the public
`http://tizen.org/privilege/window.priority.set` privilege needed by the
notification-level call. `nodisplay="true"` means the spike is intended for an
explicit AUL launch, not as a launcher icon. The manifest still needs validation
with the actual TV packaging tools.

## Device-collected sysroot facts

On 2026-11-04, the five directly referenced libraries were copied read-only from
the target TV into the ignored `root-broker/sysroot/tv-6.5/` directory:

| SONAME | TV file version | SHA-256 |
|---|---|---|
| `libcapi-appfw-application.so.0` | `0.5.27` | `fe033c6b8eaf2db51a0d8f3514c56b61d6df8339613b74b798c64576b5af8fc3` |
| `libcapi-ui-efl-util.so.0` | `0.7.7` | `f48a2b6bd1d20ec59d4b19a73a6fffa0581c85616eb27c7190622f216905fe3f` |
| `libefl-extension.so.0` | `0.1.0` | `77965ee57ca55f08b7a216bb8383aec485954d8e7e24d468094181878ec16493` |
| `libelementary.so.1` | EFL `1.25.1` | `4cc002554bf81679f726c48d3e07a9cfa5c6d3880226d5988125b5ebe737d722` |
| `libevas.so.1` | EFL `1.25.1` | `c03f352e860b78fed4fcd2690003d4db96cf77a412d3df98f239ba2c545d2585` |

The direct libraries have `ELF32 / ARM / EABI5`, soft-float flags
`0x5000200`, and SONAMEs matching the unversioned names used by Rust's `-l`
lookup. ELF symbol inspection found every function declared by `src/efl.rs`.
The libraries require at most `GLIBC_2.29`; the TV provides glibc 2.30 and
`/lib/ld-linux.so.3`.

This establishes link inputs and ABI flags, not runtime behavior. The TV has no
headers, native compiler, `pkg-config` records, or ELF inspection tools.

## Evidence and exact FFI contract

Sources were consulted on 2026-11-04. The declarations in `src/efl.rs` are based
on the following, **not on a local TV header inspection**:

- **[T1]** Samsung's published Tizen 6.5 Doxygen sources: the
  [Application API](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/api/iot-headed/6.5/group__CAPI__APPLICATION__MODULE.html)
  and the actual
  [`ui_app_lifecycle_callback_s` struct reference](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/api/iot-headed/6.5/structui__app__lifecycle__callback__s.html).
  The Application page gives `ui_app_main`, `ui_app_exit`, and all five callback
  typedefs and identifies their source as `app.h`. The struct reference's Public
  Attributes table—not example assignment order—lists the physical declaration
  sequence as `create`, `terminate`, `pause`, `resume`, `app_control`.
- **[T2]** [Tizen 6.5 EFL UTIL window-property API](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/api/iot-headed/6.5/group__CAPI__EFL__UTIL__WIN__PROPERTY__MODULE.html):
  notification-level function, error behavior, levels, and the required
  `window.priority.set` privilege. It says the level call is synchronous since
  Tizen 3.0.
- **[T3]** [Tizen 6.5 EFL extension event API](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/api/iot-headed/6.5/group__EFL__EXTENSION__EVENTS__GROUP.html):
  `eext_object_event_callback_add`, `Eext_Event_Cb`, and the ordered
  `EEXT_CALLBACK_BACK`, `EEXT_CALLBACK_MORE`, `EEXT_CALLBACK_LAST` enum.
- **[T4]** [Tizen 6.5 EFL extension hardware-key API](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/api/iot-headed/6.5/group__EFL__EXTENSION__HARDWARE__KEYEVENT__GROUP.html):
  `eext_win_keygrab_set` and `eext_win_keygrab_unset`.
- **[T5]** [Tizen EFL Utilities guide](https://docs.tizen.org/application/native/guides/ui/efl/efl-util/):
  create the window with `elm_win_add(..., ELM_WIN_NOTIFICATION)` before setting
  its notification level.
- **[E1]** [EFL 1.25 `elm_win_legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_win_legacy.h):
  window type enum and legacy window/keygrab signatures.
- **[E2]** EFL 1.25 legacy widget headers:
  [`elm_bg_legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_bg_legacy.h),
  [`elm_box_legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_box_legacy.h),
  [`elm_label_legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_label_legacy.h),
  [`elm_button_legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_button_legacy.h), and
  [`elm_object.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/elementary/elm_object.h).
  The last source is important because `elm_object_text_set` is a C macro; Rust
  calls its actual function, `elm_object_part_text_set(obj, NULL, text)`.
- **[E3]** EFL 1.25 [`Evas_Legacy.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/evas/Evas_Legacy.h)
  and [`eina_types.h`](https://raw.githubusercontent.com/Enlightenment/efl/efl-1.25/src/lib/eina/eina_types.h):
  Evas object APIs, `Evas_Smart_Cb`, `Evas_Coord=int`, and
  `Eina_Bool=unsigned char` with true/false values 1/0.
- **[L1]** Samsung TizenFX's native interop sources corroborate the exact layout
  and establish the link names: [`Interop.Application.cs`](https://raw.githubusercontent.com/Samsung/TizenFX/b33b8a802abf2d946d7fae343951305a383b1c59/src/Tizen.Applications.UI/Interop/Interop.Application.cs)
  marks `ui_app_lifecycle_callback_s` as `Include="app.h"` and
  `PkgConfig="capi-appfw-application"` and lays out the same five callbacks;
  its [`Interop.Libraries.cs`](https://raw.githubusercontent.com/Samsung/TizenFX/b33b8a802abf2d946d7fae343951305a383b1c59/src/Tizen.Applications.UI/Interop/Interop.Libraries.cs)
  uses `libcapi-appfw-application.so.0`; and ElmSharp's
  [`Interop.Libraries.cs`](https://raw.githubusercontent.com/Samsung/TizenFX/b33b8a802abf2d946d7fae343951305a383b1c59/src/ElmSharp/Interop/Interop.Libraries.cs)
  maps Eext/Eutil to `libefl-extension.so.0` and
  `libcapi-ui-efl-util.so.0`. Rust links the corresponding unversioned sysroot
  names `capi-appfw-application`, `capi-ui-efl-util`, and `efl-extension`.
- **[K1]** [Samsung TV user-interaction key table](https://developer.samsung.com/smarttv/develop/tizen-net-tv/guides/user-interaction.html):
  TV RETURN is `XF86Back`, ENTER is `Return`, and long-press RETURN is
  `XF86Exit`. Only `XF86Back` is grabbed here; normal focused-button handling is
  left to Elementary.
- **[M1]** [Tizen native application overview](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/native/guides/applications/overview.md)
  and [CLI packaging documentation](https://raw.githubusercontent.com/Samsung/tizen-docs/master/docs/application/tizen-studio/common-tools/command-line-interface.md):
  native manifest shape, `type="capp"`, `bin/` layout, and signed TPK packaging.

### Types, enum values, and lifecycle layout

The Rust representations are:

```text
Eina_Bool                         u8
Evas_Coord / C int / C enum       i32 (c_int)
Evas_Object *, app_control_h      opaque void pointers
app_create_cb                     bool (*)(void *)
other lifecycle callbacks         void (*)(...)
Evas_Smart_Cb / Eext_Event_Cb      void (*)(void *, Evas_Object *, void *)
```

`UiAppLifecycleCallback` is `#[repr(C)]` with exactly five function-pointer
fields in the declaration order shown by the actual Tizen 6.5 struct reference
in [T1] and independently by TizenFX in [L1]. The public declarations also
verify `app_create_cb=bool (*)(void *)`, the three void pause/resume/terminate
callbacks, `app_control_cb=void (*)(app_control_h, void *)`, and the four-
argument `ui_app_main` prototype used here. Thus the source declaration order
and callback types are verified public-header facts, not inferred from example
assignments.

On ARMv7 the expected Rust struct size is 20 bytes and field offsets are 0, 4,
8, 12, and 16. C `bool` maps to Rust `bool`; `Eina_Bool` deliberately does not.
The public declarations establish the source contract. The TV libraries and
required symbols were subsequently verified, and the native binary linked with
soft-float ELF flags. Its runtime lifecycle was never reached because UEP
prevented launchpad from mapping the executable.

Enum values used by the manual declarations are:

```text
ELM_WIN_NOTIFICATION = 11
EEXT_CALLBACK_BACK = 0
EFL_UTIL_NOTIFICATION_LEVEL_NONE = 0
EFL_UTIL_NOTIFICATION_LEVEL_DEFAULT = 1
EFL_UTIL_NOTIFICATION_LEVEL_MEDIUM = 2
EFL_UTIL_NOTIFICATION_LEVEL_HIGH = 3
EFL_UTIL_NOTIFICATION_LEVEL_TOP = 4
```

The first value follows the EFL 1.25 enum starting with `ELM_WIN_UNKNOWN=-1`;
the others follow the declared enumerator order in [T2]/[T3]. Treat every value
as provisional until checked against the exact TV sysroot.

### Functions declared by this spike

All functions use C linkage. Pointer typedefs above are omitted here only to
keep the signatures readable; the Rust module spells out the corresponding raw
pointer types.

| Library | Exact C signature used |
|---|---|
| `capi-appfw-application` | `int ui_app_main(int argc, char **argv, ui_app_lifecycle_callback_s *callback, void *user_data)` |
| `capi-appfw-application` | `void ui_app_exit(void)` |
| `elementary` | `Evas_Object *elm_win_add(Evas_Object *parent, const char *name, Elm_Win_Type type)` |
| `elementary` | `void elm_win_title_set(Evas_Object *obj, const char *title)` |
| `elementary` | `void elm_win_resize_object_add(Evas_Object *obj, Evas_Object *subobj)` |
| `elementary` | `void elm_win_center(Evas_Object *obj, Eina_Bool h, Eina_Bool v)` |
| `elementary` | `void elm_win_activate(Evas_Object *obj)` |
| `elementary` | `Evas_Object *elm_bg_add(Evas_Object *parent)` |
| `elementary` | `Evas_Object *elm_box_add(Evas_Object *parent)` |
| `elementary` | `void elm_box_padding_set(Evas_Object *obj, int h, int v)` |
| `elementary` | `void elm_box_pack_end(Evas_Object *obj, Evas_Object *subobj)` |
| `elementary` | `Evas_Object *elm_label_add(Evas_Object *parent)` |
| `elementary` | `Evas_Object *elm_button_add(Evas_Object *parent)` |
| `elementary` | `void elm_object_part_text_set(Evas_Object *obj, const char *part, const char *text)` |
| `elementary` | `void elm_object_focus_set(Evas_Object *obj, Eina_Bool focus)` |
| `evas` | `void evas_object_size_hint_weight_set(Evas_Object *obj, double x, double y)` |
| `evas` | `void evas_object_size_hint_align_set(Evas_Object *obj, double x, double y)` |
| `evas` | `void evas_object_size_hint_min_set(Evas_Object *obj, int w, int h)` |
| `evas` | `void evas_object_resize(Evas_Object *obj, int w, int h)` |
| `evas` | `void evas_object_show(Evas_Object *obj)` |
| `evas` | `void evas_object_del(Evas_Object *obj)` |
| `evas` | `void evas_object_smart_callback_add(Evas_Object *obj, const char *event, Evas_Smart_Cb func, void *data)` |
| `capi-ui-efl-util` | `int efl_util_set_notification_window_level(Evas_Object *window, efl_util_notification_level_e level)` |
| `efl-extension` | `void eext_object_event_callback_add(Evas_Object *obj, Eext_Callback_Type type, Eext_Event_Cb func, void *data)` |
| `efl-extension` | `Eina_Bool eext_win_keygrab_set(Elm_Win *obj, const char *key)` |
| `efl-extension` | `Eina_Bool eext_win_keygrab_unset(Elm_Win *obj, const char *key)` |

Follow-up review corrected the initial draft's three Rust `#[link]` names from
`capi-ui-app`, `efl_util`, and `capi-ui-efl-extension` to the public names above.
Those were concrete link-time mismatches, not lifecycle-layout errors. No other
signature mismatch was found in the cited public declarations. Linking against
the TV-collected libraries now resolves every declared symbol; on-TV loading and
runtime resolution remain untested.

`elm_win_autodel_set` is intentionally not used: the delete-request callback
only exits, and `app_terminate` ungrabs/deletes once. This avoids retaining a
window pointer after automatic deletion.

## Host checks and local cross-link validation

Initial host source validation used macOS `rustc 1.98.1` / `cargo 1.98.1` with
no third-party Rust dependencies. The following passed:

```sh
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo tree
# manifest XML/ID assertions
```

The first pinned cross-build (Rust 1.85) exposed a host-compiler compatibility
issue: Rust 1.85 does not implement `Default` for raw pointers. `AppState` now
has an explicit `Default` implementation. The checks pass after that fix.

### Reproducible cross-link

The TV was accessed read-only to collect the five direct libraries. The actual
build then ran locally against those files; the device was not made to execute
the binary.

```sh
./../../sysroot/fetch-tv-sysroot.sh
./build-armv7-softfloat.sh
```

`build-armv7-softfloat.sh` uses:
`rust:1.85.0-slim-bullseye@sha256:a78439ac2ee14dc1c2c188fef0ff0b197e1cc1918d4b3daf486776ac0f60029a`,
`rustc 1.85.0`, target `armv7-unknown-linux-gnueabi`, and Debian's
soft-float `arm-linux-gnueabi-gcc`. It links against only the TV's five direct
EFL/application libraries and passes `--allow-shlib-undefined`, leaving their
transitive dependencies to the TV runtime.

Observed artifact:

```text
ELF 32-bit LSB pie executable, ARM, EABI5 version 1 (SYSV),
dynamically linked, interpreter /lib/ld-linux.so.3, stripped,
for GNU/Linux 3.2.0

ELF flags: 0x5000200 (Version5 EABI, soft-float ABI)
DT_NEEDED:
  libcapi-appfw-application.so.0
  libelementary.so.1
  libevas.so.1
  libcapi-ui-efl-util.so.0
  libefl-extension.so.0
  libgcc_s.so.1
  libpthread.so.0
  libc.so.6
Maximum GLIBC symbol version: GLIBC_2.28
Size: 300704 bytes
SHA-256: 9f4cf1caf902aad32bd4076aa625145b0ee955357f79a79de7d033b40b706c66
```

This resolves all external symbols and establishes executable-level glibc
compatibility below the TV's 2.30. It does not validate the TV loader/UEP,
application signature, AUL, display, input, or runtime library resolution.

### Native packaging and UEP result

The native TPK was built, signed with the existing Public TV certificate,
installed successfully, and launched through `app_launcher`. The app process
carried the correct `User::Pkg::org.tvauth.spike.popup` label, but its maps
contained only `/usr/bin/launchpad-loader` and `liblaunchpad`; the custom ELF was
never mapped and its entry point never ran. Installing the TPK and re-signing it
through Tizen Studio did not change the binary or lift UEP.

This makes a custom native approval UI unsuitable unless the project takes on a
UEP bypass or partner/platform signing path, both outside the desired design.

## Managed .NET/EFL UI

The working implementation is `dotnet/Program.cs`, built with .NET Core 3.1 and
packaged as a Tizen `dotnet` UI application. It calls the same Tizen lifecycle
and EFL functions through a small C# P/Invoke layer, explicitly sets the Wayland
window type to notification, and then applies the notification level and shows
the prompt.

Build the managed unsigned TPK root:

```sh
./build-dotnet-popup.sh
```

The script uses `mcr.microsoft.com/dotnet/sdk:3.1`, publishes
`netcoreapp3.1`, and assembles `out/popupdotnet/`. Tizen author/distributor
signing was performed with the existing Public certificate through the tested
XML signer; certificate material and passwords are not in this repository.

Validated artifact:

```text
Managed assembly: EflPopupDotnet.dll, 12,288 bytes
Signed TPK: org.tvauth.spike.popupdotnet-0.1.1-signed.tpk, 14,568 bytes
Signed TPK SHA-256: b38fbfd7ea00d058359f5c56f74c9115b35345a0f3b5a9940784159d7fbd8b7d
```

On-device diagnostics:

```text
set Wayland notification type
notification level TOP accepted
window shown/raised/activated
```

Observed app identity: `User::Pkg::org.tvauth.spike.popupdotnet`, UID 5001. With
YouTube foregrounded, the popup appeared above it and dismissed via OK/RETURN.
The test package, native test package, peer packages, `/run` staging, and
temporary log were removed afterwards.

## Remaining TV/sysroot steps

These are future instructions, not actions performed by this spike. Obtain
explicit authorization before installing or launching anything on the TV.

1. **ABI/link proof.** Complete for the native path locally and for the managed
   path on-device. The native ELF itself is rejected by UEP and is no longer a
   production candidate.
2. **Package validation.** Complete for the managed `org.tvauth.spike.popupdotnet`
   package: signed TPK installed with the Public certificate and public
   `window.priority.set` privilege.
3. **AUL launch.** Complete with `/usr/bin/app_launcher -s <appid>`; the process
   ran as the expected UID-5001 package label.
4. **Creation and priority.** Notification TOP was accepted, the window appeared
   above YouTube, and both OK/RETURN dismissal paths worked.
5. **Further hardening tests.** Still required for a production approval UI:
   spoofing/occlusion, focus loss, repeated/held keys, relaunch while running,
   pause/resume, compositor restart, manager screen behavior, and timing/race
   behavior.
6. **Resource measurements.** Record managed UI runtime RSS, launch latency, and
   signed TPK size under TV memory pressure.
7. **No active test artifacts.** The installed test packages and `/run` staging
   were removed; re-running these tests requires explicit authorization.
