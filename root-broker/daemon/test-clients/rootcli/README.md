# tvauth-cli (root-role client)

A dependency-free static ARMv7 client for tv-authd's uid-0 role. A root shell
on the TV runs as `System::Privileged`, so it may call `pkg.install`,
`pkg.uninstall`, and `pkg.list` without a prompt (and gets a `ui.notice` toast
on success). This exists because the TV's shell tooling is gated by UEP:
`node` refuses to run scripts from `/tmp`, and the VDFS in `/opt` rejects
unsigned ELFs.

```sh
./build-armv7.sh
# copy out/tvauth-cli to the TV, then run it from /run:
cat > /run/tvauth-cli          # or scp-less: ssh 'cat > /run/tvauth-cli' < out/tvauth-cli
chmod 0755 /run/tvauth-cli

/run/tvauth-cli list
/run/tvauth-cli install tpk /tmp/org.example.app-0.1.0.tpk
/run/tvauth-cli uninstall org.example.app
```

It sends one version-2 request to `/run/tv-auth/broker.sock` (override with
`--socket PATH`), prints the raw response line, and exits 0 only for
`"status":"ok"`. `exec` is deliberately unsupported: root clients cannot run
commands.

`cargo test` covers request building and JSON string escaping on the host.
