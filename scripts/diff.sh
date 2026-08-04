#!/usr/bin/env bash
# Show a side-by-side, ANSI-stripped, line-numbered diff of one fixture:
#   golden baseline  vs  our binary output
# Usage: scripts/diff.sh <FIXTURE_NAME> [BIN]
#   FIXTURE_NAME, e.g. show_003, log_000, plain_unified
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${2:-$ROOT/target/release/diff-pretty}"
b="${1:?usage: diff.sh <FIXTURE_NAME>}"
strip() { sed -E 's/\x1b\[[0-9;]*m//g'; }
"$BIN" < "$ROOT/fixtures/$b.patch" | strip > /tmp/dp_ours.txt
strip < "$ROOT/fixtures/oracle/$b.out" > /tmp/dp_oracle.txt
diff -u --label "golden/$b" --label "ours/$b" /tmp/dp_oracle.txt /tmp/dp_ours.txt
