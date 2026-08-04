#!/usr/bin/env bash
# Differential check: render every vendored fixture with our binary and compare
# byte-for-byte against the oracle goldens under fixtures/oracle/.
#
# Usage: scripts/check.sh [BIN]
#   BIN  our binary (default: ./target/release/diff-pretty)
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/diff-pretty}"
FIXTURES="$ROOT/fixtures"
PASS=0; FAIL=0; FAILED=()

for p in "$FIXTURES"/*.patch; do
  b="$(basename "$p" .patch)"
  if "$BIN" < "$p" | cmp -s - "$FIXTURES/oracle/$b.out"; then
    PASS=$((PASS+1))
  else
    FAIL=$((FAIL+1)); FAILED+=("$b")
  fi
done

echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
  echo "failing: ${FAILED[*]}"
  exit 1
fi
