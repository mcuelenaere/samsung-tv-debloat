#!/bin/bash
# Decompile selected functions of an ARM32 binary to C via Ghidra headless.
# Each run uses its own throwaway project dir, so parallel invocations never collide.
# Usage: notes/ghidra-decompile.sh <binary> <targets> [outfile]
#   targets = "all"  OR  "name1,0xADDR,name2"   (prefer targeted; "all" is slow on big blobs)
set -eu
# Ghidra needs a JDK 21+; brew's openjdk kegs are keg-only (not on PATH), so point at one.
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@25/libexec/openjdk.jdk/Contents/Home}"
export PATH="$JAVA_HOME/bin:$PATH"
HEADLESS="/opt/homebrew/Cellar/ghidra/12.2/libexec/support/analyzeHeadless"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:?usage: ghidra-decompile.sh <binary> <targets> [outfile]}"
TARGETS="${2:?need targets: \"all\" or comma-separated names/0xaddrs}"
OUT="${3:-$SCRIPT_DIR/$(basename "$BIN").decomp.c}"

[ -x "$HEADLESS" ] || { echo "Ghidra headless not found at $HEADLESS" >&2; exit 1; }
BIN_ABS="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
OUT_ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"

PROJ="$(mktemp -d "${TMPDIR:-/tmp}/ghidra.$(basename "$BIN").XXXXXX")"
trap 'rm -rf "$PROJ"' EXIT

"$HEADLESS" "$PROJ" proj -import "$BIN_ABS" \
  -scriptPath "$SCRIPT_DIR" \
  -postScript DumpDecompile.java "$TARGETS" "$OUT_ABS" \
  -deleteProject >/dev/null 2>"$PROJ/err.log" || { echo "Ghidra run failed:" >&2; tail -20 "$PROJ/err.log" >&2; exit 1; }

echo "wrote: $OUT_ABS"
wc -l "$OUT_ABS"
