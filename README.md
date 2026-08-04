# diff-pretty

A from-scratch, bone-stripped reimplementation of a strict subset of
[`delta`](https://github.com/dandavison/delta). It renders `git show` output and
matches delta **byte-for-byte** under one fixed configuration, which is
hardcoded as the default behavior. Everything else in delta is cut: no syntax
themes, no config-file reading, no decorations feature, no side-by-side, etc.

## The contract

The configuration that is hardcoded (as if it were default behavior) is exactly:

```toml
[delta]
features = line-numbers decorations
syntax-theme = none
minus-style = red
plus-style  = green
minus-emph-style = "red bold reverse"
plus-emph-style  = "green bold reverse"
line-numbers-left-format  = {nm:^4}
line-numbers-right-format = {np:^4}
navigate = true
word-diff-regex = \w+
[delta "decorations"]
commit-decoration-style = none
hunk-header-style = none
```

This is the user's live config (aligned by request). The `decorations` feature's
`hunk-header-style = none` means delta draws the `• <fragment>` box **only when
the `@@` line carries a code fragment**, and never a line number; hunks without
a fragment get just a blank line.

Behavioral facts pinned by the harness:

- **Width 80.** When stdout is a pipe (not a tty) delta uses a fixed width of 80
  (ignores `COLUMNS`); this is hardcoded as `WIDTH`.
- **Input type.** `git show` (commit header + unified diffs), including new /
  deleted / renamed / binary files.
- **`navigate` effects** in this non-tty mode: the `Δ` modified-file label and
  the `• ` hunk box label. `added:`/`removed:`/`renamed:` annotations are
  emitted regardless.
- **Word diff** with `\w+` tokenization; changed words get the `bold reverse`
  emph style over the active minus/plus color.
- **ANSI emission** follows `nu-ansi-term`'s `AnsiStrings` model: only emit the
  minimal style difference between adjacent segments (or a reset + full prefix
  when attributes would need to be removed), plus one reset at end of line.

## Layout

- `src/lib.rs`, `src/render.rs` — the renderer (entry point `render(&str) -> String`).
- `src/config.rs` — hardcoded style SGR constants and line-number padding.
- `src/align.rs`, `src/edits.rs` — Needleman–Wunsch alignment and word-diff edit
  inference, replicated from delta.
- `src/main.rs` — stdin → stdout binary.
- `src/bin/bench.rs` — in-process wall-time benchmark.
- `fixtures/*.patch` — vendored inputs: `show_NNN.patch` = `git show` of the
  100 oldest commits of `~/d/delta` (text-only; not the 100 most recent, which
  are ~92% merges); `log_000*.patch` = multi-commit `git log -p` (plain +
  colorized); `plain_unified.patch` = `diff -u`.
- `fixtures/oracle/*.out` — golden renders from `/opt/homebrew/bin/delta` with
  the exact config, checked in so tests don't require delta.
- `tests/oracle.rs` — differential test: our render vs golden, byte for byte.
- `scripts/` — `vendor-patches.sh`, `render-oracle.sh`, `check.sh`, `diff.sh`,
  `bench.sh`.

## Commands

```sh
cargo test --release          # differential oracle test (must pass)
scripts/check.sh              # byte-for-byte check over every fixture
scripts/diff.sh show_003      # ANSI-stripped diff of one fixture vs oracle
scripts/bench.sh              # our in-process throughput vs the oracle
scripts/render-oracle.sh      # (re)generate goldens from the oracle
scripts/vendor-patches.sh     # (re)vendor the corpus from ~/d/delta
```

## Paging

The binary pages like delta when attached to a terminal:

- `--paging=auto` *(default)* — page only when stdout is a terminal; stdout is
  unchanged (pipes, redirection, tests, benchmarks) when not.
- `--paging=always` — always spawn the pager (the pager passes bytes through
  when non-interactive).
- `--paging=never` / `--no-pager` — always write to stdout.

The pager is `$PAGER` (default `less -R`; `-R` is added for `less` when missing).
When stdout is a terminal, the whole pager session is wrapped in the alternate
screen buffer (`\x1b[?1049h` / `\x1b[?1049l`, with `less -X` so it doesn't manage
the screen itself), so the paged output is discarded on quit and never pollutes
the terminal scrollback. Piping/redirection (`--paging=always` on a non-tty)
emits no such sequences.

**Quitting the pager.** When the user quits the pager, the write to its stdin
gets a broken pipe; we treat that as a clean stop (matching delta's
`BrokenPipe => return Ok(0)`) rather than dumping the remaining output to
stdout. Only a failure to *spawn* the pager (e.g. `$PAGER` pointing at a
missing binary) falls back to writing to stdout.

Paging lives in `src/pager.rs` (`pager::emit`); `render()` itself is pure and
never spawns a process, so the benchmark and the oracle tests are pager-free.

## How the oracle is isolated

delta reads its configuration only from a git repo's `.git/config`; a bare git
config file or `HOME` is not honored. So the harness renders the oracle from
*inside* a throwaway repo whose `.git/config` carries the exact contract above,
feeding each patch on stdin with `--width 80`. This is fully deterministic and
independent of `~/d/delta`'s own config. The same config bytes are hardcoded in
`src/config.rs` / `src/render.rs` for our implementation.

## Coverage & known gaps

Locked in byte-for-byte (via `oracle/` + `fixtures/oracle/` goldens, run by
`scripts/check.sh` and `tests/oracle.rs`):

- `git show` — 100 construction commits (new/renamed/deleted/binary files,
  code/docs, word diff, hunk boxes).
- `git diff` — worktree/staged and between commits, plain and colorized.
- `git log -p` — multi-commit output (13 commits, `fixtures/log_000*.patch`,
  plain + colorized), incl. per-commit header reset.
- Plain unified diffs with no `diff --git` header
  (`diff -u a b` / `git diff --no-index` in that form,
  `fixtures/plain_unified.patch`) — rendered in comparing form (`Δ a ⟶ b`),
  not passed through.

Known residual differences from delta (tracked in `TODO.md`):

- **Extreme minus/plus imbalance.** In a giant deletion/insertion hunk (e.g. a
  76-line removal vs a 4-line addition) our greedy word-diff pairing can assign
  the few homologs differently than delta, so a handful of lines (≈0.06% of a
  19K-line log) differ in emphasis.
- **Other delta modes** — `git blame`, `git grep`/ripgrep, merge/combined
  diffs — are not implemented (inputs without `diff --git` are passed through).
