#!/usr/bin/env bash
# Regression test for scripts/claude-worktree.sh in a throw-away repository.
set -euo pipefail

HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
TMP=$(mktemp -d "${TMPDIR:-/tmp}/claude-worktree-test.XXXXXX")
trap 'git -C "$TMP/repo" worktree remove --force "$TMP/wt" >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT

mkdir -p "$TMP/repo/rooting" "$TMP/repo/bins/example" "$TMP/repo/scripts"
cp "$HERE/../claude-worktree.sh" "$TMP/repo/scripts/claude-worktree.sh"
chmod +x "$TMP/repo/scripts/claude-worktree.sh"
printf 'root work\n' > "$TMP/repo/rooting/secret.txt"
printf 'main work\n' > "$TMP/repo/bins/example/main.txt"
git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.email test@example.invalid
git -C "$TMP/repo" config user.name Test
git -C "$TMP/repo" add .
git -C "$TMP/repo" commit -qm initial

WORKTREE_ROOT=$TMP "$TMP/repo/scripts/claude-worktree.sh" create demo --path "$TMP/wt"
test -f "$TMP/wt/bins/example/main.txt"
test ! -e "$TMP/wt/rooting"
test -f "$TMP/repo/rooting/secret.txt"
test "$(git -C "$TMP/wt" branch --show-current)" = claude/demo

printf 'change\n' > "$TMP/wt/bins/example/main.txt"
if "$TMP/repo/scripts/claude-worktree.sh" remove "$TMP/wt" 2>/dev/null; then
    echo 'remove should refuse a dirty worktree' >&2
    exit 1
fi
"$TMP/repo/scripts/claude-worktree.sh" remove "$TMP/wt" --force
test ! -e "$TMP/wt"
printf 'claude-worktree test: OK\n'
