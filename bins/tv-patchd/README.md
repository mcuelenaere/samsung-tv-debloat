# tv-patchd

TizenTube (ad blocking, SponsorBlock, DeArrow, ...) inside Samsung's own
YouTube app, with nothing installed and nothing in YouTube changed on disk.
This is the approach of Vetro's YouTube plugin
(`~/Projects/personal/vetro/app/examples/plugins/youtube`), as a Rust root
daemon.

## How it works

Samsung's YouTube is Cobalt (`com.samsung.tv.cobalt-yt`), started by the
wrapper `9Ur5IzDKqV.TizenYouTube`. Cobalt accepts `--proxy` and `--content` on
its command line.

1. **Flags.** The Home tile, the remote's YouTube key, voice and casting all
   go through the wrapper. The wrapper passes Cobalt the value of one row of
   webappservice's database (`/opt/dbspace/.webappservice.db`, `widgetinfo`,
   `reserved_string_6` of `9Ur5IzDKqV.TizenYouTube`).
   - tv-patchd writes Samsung's flags plus
     `--proxy=127.0.0.2:8102,direct:// --content=/opt/usr/home/owner/share/tvs`.
   - The wrapper cuts the value at 127 characters, so `--base_url=…/tv`
     (Cobalt's default anyway) makes room for ours.
   - Every write is a compare-and-swap. Afterwards the database and its
     journal get their owner and SMACK label back.
   - Samsung's value comes from the package manifest (`native.userdata` in
     `/opt/share/packages/9Ur5IzDKqV.xml`), so restoring needs nothing saved.
2. **Trust.** A CA of its own (key root-only, in `/opt/tv-suite/var/patchd`)
   and a `www.youtube.com` certificate. The CA is added only to a copy of
   Cobalt's content folder (`…/share/tvs`, 2.6 MB), so no other app and not the
   TV's trust store trusts it.
3. **Proxy** on `127.0.0.2:8102`:
   - Every `CONNECT` is a plain tunnel, except `www.youtube.com:443`.
   - That one is TLS-terminated with our certificate and relayed request by
     request to the real site, with the TV's libssl and CA bundle.
   - The `/tv` page is fetched uncompressed, gets
     `<script src="/tv-suite/tizentube.js">` before `</body>` and a permissive
     CSP (Cobalt closes a page without one). The proxy serves the script itself.
   - `Alt-Svc` is dropped so Cobalt does not move to QUIC around the proxy.
4. **Script.** TizenTube (reisxd, GPL-3.0) from npm via jsdelivr:
   - this build pins `1.15.2` with its SHA-256;
   - the file is checked on every load;
   - `update` installs npm's latest release (trust on download: npm publishes
     no hash of this file).

The proxy list ends in `direct://`, so if tv-patchd is not running YouTube
still opens, unpatched.

## Switching off

`tizentube: false` (or `undo`, or **Undo all changes** in the manager):
- writes Samsung's flags back;
- deletes the content copy;
- closes YouTube, so its next start uses the stock flags.

Without tv-patchd, a reboot leaves the row pointing at a dead proxy (YouTube
falls back to direct) and at the content copy (still on disk), so YouTube keeps
working.

Vetro, uninstalled without its restore, had left the row patched with its own
proxy. tv-patchd recognises that value (`other-proxy`) and replaces it with its
own when switched on. Vetro's files (`…/share/vyt`,
`…/share/vetro-youtube`) are not touched; on tv-living they were deleted by
hand on 2026-11-10.

## Custom Home

`custom_home: true` makes the suite's dock (`org.tvauth.home`, a second app in
the `org.tvauth.ui` package) the Home key's target:
- tvctx-manager launches the default app for
  `http://tizen.org/appcontrol/operation/main`, which tv-patchd sets with
  `app_operation_set_current_app`;
- it is stored in `/opt/dbspace/user/5001/.appsvc.db`;
- it is set only once the dock is installed (a target that names a missing
  app leaves Home doing nothing);
- it is checked on every pass;
- `custom_home: false` or `undo` resets it with
  `app_operation_reset_current_app`.

Uninstalling the package resets it to Samsung's Home as well. See
`docs/tv-launcher-plan.md`.

## Configuration and control

`/opt/tv-suite/etc/patchd.json`: `{"tizentube": false, "custom_home": false}`
by default.
Control socket `/run/tv-suite/patchd.sock` (`root:users 0660`; root or the
`org.tvauth.ui` manager), newline-delimited JSON:
`status`, `set` (`{"config":{"tizentube":true}}`), `undo`, `update`.

```sh
tv-patchd status
tv-patchd set '{"tizentube":true}'
tv-patchd update
tv-patchd undo
```

Log: `/run/tv-suite/patchd.log`.

## Build and install

```sh
bins/tv-patchd/build-armv7.sh            # gnueabi: links libssl, libsqlite3, libcurl
```

Binary at `/opt/tv-suite/bin/tv-patchd` (0755), unit
`systemd/tv-patchd.service` at `/opt/tv-units/tv-patchd.service` (0600). The
sysroot needs `libssl.so` (`sysroot/fetch-tv-sysroot.sh`).

## Not done

- The plan's `cobalt_launcher` bind-mount shim. Cobalt reads its flags from
  the launch bundle the wrapper builds from the database row, so there is no
  argument list for a shim to extend.
- Declarative patch rules for other apps (`patches.d`): no second app needs
  one yet.
- Only `www.youtube.com`/`youtube.com` are opened. If YouTube ever serves its
  TV page from another host, that page tunnels and YouTube runs unpatched.
