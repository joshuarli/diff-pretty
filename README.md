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
- `fixtures/oracle/*.out` — frozen golden baselines (originally rendered by
  delta with the exact config, captured once). The project is now independent:
  nothing invokes delta. The goldens are an intentional-divergence contract —
  update them in `fixtures/oracle/` when you deliberately change output.
- `tests/golden.rs` — golden snapshot test: our render vs the baseline, byte
  for byte.
- `scripts/` — `vendor-patches.sh` (re-vendor the show_* fixtures, needs the
  `~/d/delta` source checkout), `check.sh`, `diff.sh`.

## Commands

```sh
cargo test --release          # golden snapshot test (must pass)
scripts/check.sh              # byte-for-byte check over every fixture
scripts/diff.sh show_003      # ANSI-stripped diff of one fixture vs its golden
scripts/vendor-patches.sh     # (re)vendor the show_* fixtures from ~/d/delta
```

> **Diverging?** The goldens are frozen. Any intentional change to output will
> (correctly) fail `cargo test` / `scripts/check.sh` until you update the
> affected `.out` files under `fixtures/oracle/` — treat those edits as the way
> you record the new contract.

## Paging

The binary pages like delta when attached to a terminal:

- `--paging=auto` *(default)* — page only when stdout is a terminal; stdout is
  unchanged (pipes, redirection, tests, benchmarks) when not.
- `--paging=always` — always spawn the pager (the pager passes bytes through
  when non-interactive).
- `--paging=never` / `--no-pager` — always write to stdout.

The pager is `$PAGER` (default `less -R`; `-R` is added for `less` when missing).

In `--paging=auto` (the default), output that fits on one screen is written
straight to stdout, so it stays in the terminal scrollback like delta. Only
**multi-screen** output pages, and it is wrapped in the alternate screen buffer
(`\x1b[?1049h` / `\x1b[?1049l`, with `less -X` so the pager doesn't manage the
screen itself), so the paged output is discarded on quit and never pollutes the
scrollback. This avoids the "content blinks out and reverts" problem that the
alternate screen alone causes for small diffs (with `less -F` facing-terminal
behavior). Terminal height comes from the `TIOCGWINSZ` ioctl; paging only
happens when a height is available. Piping/redirection (`--paging=always` on a
non-tty) emits no alternate-screen sequences.

**Quitting the pager.** When the user quits the pager, the write to its stdin
gets a broken pipe; we treat that as a clean stop (matching delta's
`BrokenPipe => return Ok(0)`) rather than dumping the remaining output to
stdout. Only a failure to *spawn* the pager (e.g. `$PAGER` pointing at a
missing binary) falls back to writing to stdout.

Paging lives in `src/pager.rs` (`pager::emit`); `render()` itself is pure and
never spawns a process, so the benchmark and the golden tests are pager-free.

## History: where the goldens came from

The baselines in `fixtures/oracle/` were captured once by running delta from
*inside* a throwaway git repo whose `.git/config` carried the config above
(delta reads config only from a repo's `.git/config`, not a bare file), feeding
each fixture on stdin at `--width 80`. That oracle scaffolding
(`render-oracle.sh`) has since been removed: the project no longer invokes
delta. The same config bytes remain hardcoded in `src/config.rs` /
`src/render.rs`.

## Coverage & known gaps

Locked in byte-for-byte (via the `fixtures/oracle/` goldens, run by
`scripts/check.sh` and `tests/golden.rs`):

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
