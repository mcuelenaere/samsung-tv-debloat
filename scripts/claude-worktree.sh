#!/usr/bin/env bash
# Create or remove a sparse Claude Code worktree.
#
# The dedicated worktree carries a `claude/...` branch and excludes only
# `rooting/`. The full development checkout is left untouched: there is no
# deletion commit, so normal changes merge back without removing root work.
#
#   scripts/claude-worktree.sh create <name> [--path DIR] [--branch BRANCH]
#   scripts/claude-worktree.sh remove <path> [--force]
#   scripts/claude-worktree.sh list
#
# WORKTREE_ROOT overrides the default sibling location. A sparse worktree is a
# visibility/workflow boundary, not a confidentiality boundary: shared Git
# history remains readable from either checkout.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)

usage() {
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//' >&2
}

default_path() {
    local name=$1
    local base=${WORKTREE_ROOT:-$(dirname "$REPO")}
    printf '%s/%s-%s\n' "$base" "$(basename "$REPO")" "$name"
}

create() {
    local name=${1:?create needs a name}
    shift
    local path branch
    path=$(default_path "$name")
    branch=claude/$name
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --path) path=${2:?--path needs a directory}; shift 2 ;;
            --branch) branch=${2:?--branch needs a name}; shift 2 ;;
            *) usage; exit 2 ;;
        esac
    done
    case "$name" in
        *[!A-Za-z0-9._-]*|'') echo "claude-worktree: unsafe name: $name" >&2; exit 2 ;;
    esac
    case "$path" in
        /*) ;;
        *) path=$(cd -- "$(dirname -- "$path")" && pwd)/$(basename -- "$path") ;;
    esac
    [ ! -e "$path" ] || { echo "claude-worktree: exists: $path" >&2; exit 1; }

    if git -C "$REPO" show-ref --quiet "refs/heads/$branch"; then
        git -C "$REPO" worktree add --no-checkout "$path" "$branch"
    else
        git -C "$REPO" worktree add --no-checkout -b "$branch" "$path" HEAD
    fi

    # Cone mode cannot express "everything except one directory", so use the
    # non-cone pattern form. `rooting/` is the only excluded production tree.
    git -C "$path" sparse-checkout init --no-cone
    git -C "$path" sparse-checkout set '/*' '!rooting/'
    git -C "$path" checkout --quiet

    if [ -e "$path/rooting" ]; then
        echo "claude-worktree: rooting/ was not excluded" >&2
        exit 1
    fi
    printf 'worktree=%s\nbranch=%s\nrooting=excluded\n' "$path" "$branch"
}

remove() {
    local path=${1:?remove needs a worktree path}
    shift
    local force=()
    if [ "${1:-}" = --force ]; then
        force=(--force)
        shift
    fi
    [ "$#" -eq 0 ] || { usage; exit 2; }
    git -C "$REPO" worktree remove "${force[@]}" "$path"
}

case "${1:-}" in
    create) shift; create "$@" ;;
    remove) shift; remove "$@" ;;
    list) git -C "$REPO" worktree list ;;
    -h|--help|help) usage ;;
    *) usage; exit 2 ;;
esac
