#!/bin/bash
# Fast static triage for one ARM32 Tizen daemon. Cheap tools only (strings/objdump/nm).
# Usage: notes/triage.sh rootfs/usr/bin/<daemon>
set -u
BIN="${1:?usage: triage.sh <binary>}"
[ -f "$BIN" ] || { echo "no such file: $BIN" >&2; exit 1; }

sec() { printf '\n==== %s ====\n' "$1"; }

sec "FILE"
file "$BIN"

sec "DYNAMIC IMPORTS (capability-relevant)"
# What external functions it calls -> what it can do.
objdump -T "$BIN" 2>/dev/null | grep -iE \
 'exec|system|popen|fork|clone|dlopen|setuid|setgid|setcap|capset|prctl|smack|socket|bind|connect|dbus|bus_|getenv|fopen|open64|unlink|rename|chmod|chown|mount|ptrace|verify|signature|sign|cert|rsa|sha|hmac|crypt' \
 | awk '{print $NF}' | sort -u

sec "LOCAL SYMBOLS (if not stripped)"
nm "$BIN" 2>/dev/null | grep -iE ' [tT] ' | awk '{print $3}' | sort -u | head -200
nm "$BIN" 2>/dev/null | grep -qE ' [tT] ' || echo "(stripped — no local symbols)"

sec "STRINGS: filesystem paths (/opt /tmp writable + configs)"
strings -n 5 "$BIN" | grep -E '^/(opt|tmp|var|run|mnt|media)/' | sort -u | head -80

sec "STRINGS: exec / shell / script targets"
strings -n 5 "$BIN" | grep -iE 'sh -c|/bin/sh|\.sh|gawk|awk|/usr/bin/|pkgcmd|rpm|systemctl|ExecStart|launch' | sort -u | head -60

sec "STRINGS: UEP / signature / verification"
strings -n 4 "$BIN" | grep -iE 'uep|signature|verify|cert|rsa|sha256|hmac|trustzone|tee|secure|integrity|tamper' | sort -u | head -60

sec "STRINGS: D-Bus names / object paths / sockets"
strings -n 5 "$BIN" | grep -iE 'org\.tizen|com\.samsung|/org/|/com/|\.service$|\.socket|@/|unix' | sort -u | head -60

sec "STRINGS: SMACK labels"
strings -n 3 "$BIN" | grep -iE 'smack|System|User::|::App|floor|\^|_::' | sort -u | head -40

sec "STRINGS: buxton / vconf / config keys"
strings -n 5 "$BIN" | grep -iE 'buxton|vconf|db/|memory/|\.ini|fconfig|feature' | sort -u | head -60

echo
echo "# For a function's C: notes/ghidra-decompile.sh $BIN <name-or-0xaddr[,..]> out.c"
echo "# Find the func containing a string xref:  objdump -d $BIN | grep -B40 '<string-addr>' | grep -E '^[0-9a-f]+ <'"
