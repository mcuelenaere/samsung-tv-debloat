# SDB package-name foothold: tv-living blocker

Date: 2026-11-11
Target: `T-PTMDEUC-REL-202602250226`, `sdbd 2.2.31`, plugin `3.8.5_TV_REL`

## Observed behavior

Both our native client and Samsung's `sdb` send the same service:

```text
shell:0 appinstall tpk new2.tpk`printf${IFS}%s${IFS}<base64>|base64${IFS}-d|bash`.tpk
```

The daemon closes the channel immediately. Plain
`0 appinstall tpk nonexistent.tpk` still reaches pkgcmd, so the transport and
developer-mode authorization are working.

Character probes on the same TV:

| Input | Result |
|---|---|
| plain `test.tpk` | pkgcmd runs |
| backtick | service refused |
| `$`, `$(...)` | service refused |
| pipe / `<` / `>` / `&` | service refused |
| `'` | service refused |
| `;` | accepted by validation, but the package name is truncated at `;` |
| `"` | reaches `/bin/sh -c`, but produces an unmatched-quote parse error |
| `(`, `)` | reach the generated path, but produce a shell syntax error |
| `\` | the package name is truncated at the backslash |

## Why the reference payload fails here

The exported `_verify_shell_cmd` in
`/usr/lib/libsdbd_plugin.so` rejects `'`, `|`, `<`, `>`, `` ` ``, `&`, `$`,
newline, carriage return, tab, `#`, `[`, `]`, `{` and `}`. The reference
payload requires backticks and `$`.

The accepted semicolon never survives conversion. Calling the plugin's
`_convert_shell_cmd` directly on the TV produced:

```text
input:  0 appinstall tpk x;id;.tpk
output: /usr/bin/pkgcmd -i -t tpk -p /home/owner/share/tmp/sdk_tools/x -q -G
```

The name is truncated at the first `;`. Quotes show the same truncation, for
example `x";/path/probe.sh;".tpk` converts to
`.../sdk_tools/x" -q -G`; the resulting shell command has an unmatched quote.
A backslash is also a truncation point. Parentheses are not truncation points
but become shell syntax errors in the generated `pkgcmd` argument.

The other appcmd shapes behave the same way:

```text
0 vd_appinstall tpk x;id;          -> /usr/bin/wascmd -i tpk -p x
0 appuninstall x;id;               -> ...;/usr/bin/pkgcmd -u -n x
0 appinstall tpk /path/x;id;       -> .../pkgcmd -i -t tpk -p /path/x -q -G
```

Every form truncates the user-controlled field at the first separator before
the generated command is executed.

## Conclusion

The package-name injection is not usable on this unit with the installed
`sdbd`/plugin. No safe validator bypass was found in the characters that pass
`_verify_shell_cmd`; the remaining metacharacters either truncate the package
name before it reaches the generated command or only produce shell parse
errors.

`tv-root` now runs a dedicated injection foothold probe before the
compatibility fingerprint. On this TV it fails closed immediately with:

```text
SDB is connected, but the package-name shell injection is blocked by this TV's
sdbd plugin (...); no root route can start
```

Restoring one-shot FDET root requires either a build whose sdbd plugin accepts
the injection characters, a different reviewed acquisition route, or an
operator-controlled downgrade of that daemon. Do not weaken the validation or
persistence guards to work around it.
