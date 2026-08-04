#!/usr/bin/env bash
# Vendor the corpus of patches into ./patches as raw `git show` output.
#
# The corpus is the 100 oldest commits of ~/d/delta (the construction history:
# 0 merges, ~109 .rs + 20 .md + 10 .toml, 2 binary changes excluded).
#
# Usage: scripts/vendor-patches.sh [SRC_REPO]
#   SRC_REPO  path to the delta source checkout (default: ~/d/delta)
set -euo pipefail

SRC="${1:-$HOME/d/delta}"
OUT="$(cd "$(dirname "$0")/.." && pwd)/patches"

[ -d "$SRC/.git" ] || { echo "not a git repo: $SRC" >&2; exit 1; }

STAMP="$(git -C "$SRC" rev-parse HEAD)"

# 100 oldest commits, oldest first.
mapfile -t COMMITS < <(git -C "$SRC" rev-list --reverse HEAD | head -100)

i=0
for c in "${COMMITS[@]}"; do
  # Deterministic fuller header + patch body (text only; no --stat).
  git -C "$SRC" show --format=fuller "$c" \
    > "$OUT/$(printf '%03d' "$i").patch"
  i=$((i+1))
done

echo "vendored $i patches from $SRC (HEAD $STAMP) -> $OUT"
