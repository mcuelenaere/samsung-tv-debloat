# pkg.install probe

A managed service app (`org.tvauth.pkgprobe`) that sends one raw broker
request. It exists to drive `pkg.install` on the TV, where an ordinary app
label is required.

```sh
TIZEN_PROFILE=tvauth-partner ./build.sh
sdb connect <tv-ip>
sdb install out/org.tvauth.pkgprobe-0.1.0.tpk
```

Write the request and launch the probe:

```sh
cat > /tmp/tvauth-probe.request <<'JSON'
{"version":2,"id":"inst-1","method":"pkg.install","pkg_type":"tpk",
 "path":"/tmp/org.example.app-0.1.0.tpk","timeout_ms":120000}
JSON
chown owner:users /tmp/tvauth-probe.request
chsmack -a User::App::Shared /tmp/tvauth-probe.request
/usr/bin/app_launcher -s org.tvauth.pkgprobe
cat /tmp/tvauth-probe.response
```

Use `input_keyevent Return` to activate the focused (Deny) button and
`input_keyevent Right` then `Return` to choose Install. The key name is
`Return`, not `RETURN`. `XF86Back` denies.
