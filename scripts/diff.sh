#!/usr/bin/env bash
# Show a side-by-side, ANSI-stripped, line-numbered diff of one patch:
#   oracle output  vs  our binary output
# Usage: scripts/diff.sh <NNN> [BIN]
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${2:-$ROOT/target/release/diff-pretty}"
b="${1:?usage: diff.sh <NNN>}"
strip() { sed -E 's/\x1b\[[0-9;]*m//g'; }
"$BIN" < "$ROOT/patches/$b.patch" | strip > /tmp/dp_ours.txt
strip < "$ROOT/oracle/$b.out" > /tmp/dp_oracle.txt
diff -u --label "oracle/$b" --label "ours/$b" /tmp/dp_oracle.txt /tmp/dp_ours.txt
