#!/usr/bin/env bash
# Differential check: render every input (vendored patches + git-log fixtures)
# with our binary and compare byte-for-byte against the oracle goldens.
#
# Usage: scripts/check.sh [BIN]
#   BIN  our binary (default: ./target/release/diff-pretty)
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/diff-pretty}"
PASS=0; FAIL=0; FAILED=()

check_dir() {
  local inputs="$1" goldens="$2"
  for p in "$inputs"/*.patch; do
    b="$(basename "$p" .patch)"
    if "$BIN" < "$p" | cmp -s - "$goldens/$b.out"; then
      PASS=$((PASS+1))
    else
      FAIL=$((FAIL+1)); FAILED+=("$b")
    fi
  done
}

check_dir "$ROOT/patches" "$ROOT/oracle"
[ -d "$ROOT/fixtures" ] && check_dir "$ROOT/fixtures" "$ROOT/fixtures/oracle"

echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
  echo "failing: ${FAILED[*]}"
  exit 1
fi
