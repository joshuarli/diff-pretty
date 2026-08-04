#!/usr/bin/env bash
# Compare render throughput of our release build vs the oracle.
#
# Oracle: feed the concatenated corpus through /opt/homebrew/bin/delta in ONE
# invocation so process-startup cost is amortized (the corpus is large relative
# to it; this is the closest analog to delta's "in-process" render speed).
#
# Ours: `src/bin/bench.rs` measures the render function in-process over many
# iterations with zero startup.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "=== ours (in-process, release) ==="
(cd "$ROOT" && cargo run --release --quiet --bin bench 200)

echo
echo "=== oracle (/opt/homebrew/bin/delta) ==="
# Build one large unified input by concatenating the patches.
cat "$ROOT"/patches/*.patch > /tmp/dp_corpus.txt
INPUT=$(wc -c < /tmp/dp_corpus.txt)
# delta reads config only from a git repo config; use the harness's synthetic repo.
ORACLE_REPO=$(mktemp -d)
( cd "$ORACLE_REPO" && git init -q && \
  git config delta.features line-numbers && \
  git config delta.syntax-theme none && \
  git config delta.minus-style red && \
  git config delta.plus-style green && \
  git config delta.minus-emph-style 'red bold reverse' && \
  git config delta.plus-emph-style 'green bold reverse' && \
  git config delta.line-numbers-left-format '{nm:^4}' && \
  git config delta.line-numbers-right-format '{np:^4}' && \
  git config delta.navigate true && \
  git config delta.word-diff-regex '\w+' )
/usr/bin/time -p sh -c "cd '$ORACLE_REPO' && /opt/homebrew/bin/delta --width 80 < /tmp/dp_corpus.txt > /dev/null" 2>&1
echo "oracle input bytes: $INPUT"
rm -rf "$ORACLE_REPO"
