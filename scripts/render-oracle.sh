#!/usr/bin/env bash
# Render every vendored fixture with the oracle delta and store golden output.
#
# The oracle is /opt/homebrew/bin/delta, configured EXACTLY as the target
# contract. delta reads its config only from a git repo's config (a bare git
# config file / HOME is NOT honored), so we create a throwaway repo whose
# `.git/config` carries the contract and run delta from inside it.
# stdout is a pipe (not a tty) => terminal width is fixed at 80; we also pass
# --width 80 explicitly for determinism (verified identical to the default).
#
# Inputs:  fixtures/*.patch            (git show / git log / diff -u fixtures)
# Goldens: fixtures/oracle/<name>.out
#
# Usage: scripts/render-oracle.sh [DELTA_BIN]
#   DELTA_BIN  oracle binary (default: /opt/homebrew/bin/delta)
set -euo pipefail

DELTA="${1:-/opt/homebrew/bin/delta}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES="$ROOT/fixtures"
ORACLE="$FIXTURES/oracle"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$ORACLE"

# Synthetic repo that carries the exact target configuration (the user's live
# config, aligned by request).
(
  cd "$TMP"
  git init -q
  git config user.email oracle@example.invalid
  git config user.name oracle
  git config delta.features 'line-numbers decorations'
  git config delta.syntax-theme none
  git config delta.minus-style red
  git config delta.plus-style green
  git config delta.minus-emph-style 'red bold reverse'
  git config delta.plus-emph-style 'green bold reverse'
  git config delta.line-numbers-left-format '{nm:^4}'
  git config delta.line-numbers-right-format '{np:^4}'
  git config delta.navigate true
  git config delta.word-diff-regex '\w+'
  # `decorations` feature (matches the aligned live config).
  git config delta.decorations.commit-decoration-style none
  git config delta.decorations.hunk-header-style none
) >/dev/null

n=0
for p in "$FIXTURES"/*.patch; do
  base="$(basename "$p" .patch)"
  (cd "$TMP" && "$DELTA" --width 80 < "$p") > "$ORACLE/$base.out"
  n=$((n+1))
done

echo "rendered $n oracle outputs from $DELTA -> $ORACLE"
