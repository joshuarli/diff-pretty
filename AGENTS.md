# diff-pretty

`diff-pretty` is a standalone Rust renderer and native pager for Git-style
unified diffs. It reads patch text from stdin, applies one fixed presentation
contract, and writes ANSI-styled output to stdout. The renderer is deliberately
small: it does not read user configuration, invoke an external pager, or
attempt to reproduce every Git diff mode.

The output contract is defined by this document, the implementation, and the
checked-in golden fixtures. Keep those sources aligned when changing behavior.

## Output contract

The default presentation is fixed and has no configuration-file layer:

- **Terminal width:** 80 columns for non-interactive output. The renderer does
  not consult `COLUMNS` when stdout is a pipe.
- **Input:** Git-style patch streams, including commit headers, file headers,
  unified hunks, additions, deletions, renames, binary-file notices, and plain
  `diff -u` input.
- **Line numbers:** Each hunk line has centered four-column old and new line
  number cells. Context numbers are gray; removed and added numbers use dark
  red and dark green respectively. The cells are separated by blue borders.
- **Content:** Removed lines are red and added lines are green. Changed words
  use bold reverse video over the corresponding content color. Word tokens are
  sequences matching `\w+`.
- **Whitespace:** Tabs in hunk content expand to eight spaces. Trailing
  whitespace on added lines is rendered with the whitespace-error style.
- **Decorations:** File decorations use a blue `Δ` label for ordinary changes,
  `added:`, `removed:`, or `renamed:` when appropriate, and an 80-column blue
  underline. Hunk headers are separated by a blank line. A hunk carrying a
  function/code fragment receives a blue box containing `• <fragment>`; a hunk
  without a fragment receives no box.
- **ANSI:** Style transitions follow the minimal adjacent-segment model. Add
  only attributes that can be added safely; when an attribute or foreground
  color must be removed, emit a reset followed by the complete next style. A
  non-plain line ends with one reset.
- **Metadata:** Commit and other unsupported leading metadata is passed through
  unchanged apart from the renderer's normal line ending handling. Diff
  content is parsed and restyled rather than preserving input color sequences.

The output is intentionally deterministic. Any intentional output change must
update the affected golden files under `fixtures/oracle/` and the explanation
in the relevant documentation or test.

## Repository layout

- `src/lib.rs` — public crate API and the fixed output width.
- `src/render.rs` — patch parser, styling, decorations, streaming sinks, and
  retained `RenderedDocument` support. Public entry points include `render`,
  `render_to`, `render_reader_to`, `render_document`, and
  `render_reader_document`.
- `src/config.rs` — fixed colors, ANSI constants, number formatting, and layout
  primitives.
- `src/align.rs` — reusable sequence-alignment implementation.
- `src/edits.rs` — word tokenization, pairing, and word-diff inference.
- `src/pager.rs` — the built-in terminal pager and viewport renderer.
- `src/pager_search.rs` — lazy regex search state, per-line scan cache, and
  match navigation shared by retained and live pagers.
- `src/main.rs` — command-line stdin/stdout plumbing and paging flags.
- `benches/bench.rs` — curated throughput, allocation, alignment, rendering,
  and viewport benchmarks.
- `fixtures/*.patch` — checked-in representative patch inputs: commit-based
  Git patches, multi-commit patch streams, colorized input, and plain unified
  diffs.
- `fixtures/oracle/*.out` — checked-in expected output for each fixture. These
  are the project's frozen presentation contract, not generated at test time.
- `tests/golden.rs` — compares the string, streaming, and retained-document
  render paths byte-for-byte against every golden.
- `scripts/check.sh` — renders every fixture through the binary and compares it
  with its golden output.
- `scripts/diff.sh` — shows an ANSI-stripped diff for one fixture.
- `scripts/bench-baseline.py` and `scripts/diff-baselines.py` — run and compare
  the curated benchmark baselines.
- `GITOXIDE-INTEGRATION.md` and `NATIVE-INTEGRATION.md` — research notes for
  possible structured integrations; they are not part of the current runtime
  path.
- `TODO.md` — known behavior gaps and explicitly deferred input modes.

## Commands

```sh
make test                     # release-mode Rust tests and golden snapshots
make check                    # byte-for-byte binary check over every fixture
make diff FIXTURE=show_003    # ANSI-stripped diff for one fixture
make bench                    # run benchmarks and persist a host baseline
make bench-diff AFTER=...     # compare a candidate baseline with the saved one
```

The binary reads from stdin and writes to stdout:

```sh
cargo run --release < fixtures/show_003.patch
cargo run --release -- --paging=never < fixtures/show_003.patch
```

Paging flags are:

- `--paging=auto` — default; page only when stdout is a terminal and the output
  exceeds one screen.
- `--paging=always` — use the built-in pager when stdout is a terminal; write
  directly when stdout is not a terminal.
- `--paging=never` and `--no-pager` — always write directly to stdout.

Do not run pre-commit hooks. Do not push to a remote.

## Benchmark expectations

`make bench` runs the suite through the release-built crate with fat LTO. The
suite measures the paths that matter to this project:

- End-to-end rendering of all checked-in input classes and synthetic 100 KB,
  1 MB, and 10 MB diffs.
- Color-sequence stripping and tab-heavy input.
- Word-diff inference for balanced, identical, highly imbalanced, and long-line
  cases.
- Number padding, including the per-cell allocation-sensitive primitive.
- Streaming and retained-document rendering at 1 MB and 10 MB.
- A fixed 24-row pager viewport using a preallocated output sink.

When optimizing, run the narrowest relevant benchmark first, then compare a
persisted baseline. Report time, peak simultaneously-live bytes, total bytes,
and allocation count when those measurements are relevant.

## Paging

The pager is built into the binary and does not consult `$PAGER`.

In `--paging=auto`, output that fits on one screen is written directly to
stdout so it remains in terminal scrollback. Only multi-screen output enters
the alternate screen buffer (`\x1b[?1049h` / `\x1b[?1049l`). Terminal dimensions
are captured when the pager starts; resize signals are intentionally unsupported.
Long lines are clipped at the terminal edge rather than horizontally scrolled.
On a non-terminal, including `--paging=always` with redirected stdout, no
alternate-screen sequences are emitted.

Navigation keys are `q` and Ctrl-C to quit; arrow keys, `j`/`k`, Page Up/Down,
Home, End, `g`, `G`, `b`, and Space provide vertical navigation. The pager
opens the input terminal separately because stdin contains the patch stream.
`/` enters regex search; after submission, Up/Down select matches while `j`/`k`
continue to scroll the viewport.

Non-interactive rendering stays pure and writes through `render_reader_to`
without materializing the complete ANSI output. Terminal invocations use the
incremental reader path: input is split at complete commit or file boundaries,
the pager can start before EOF, and its status line reports `loading` until the
reader finishes. A boundary is never placed inside a file because word-diff
pairing needs the complete hunk context.

## Change guidelines

- Preserve the fixed output contract unless the task explicitly changes it.
- Before editing, inspect the parser boundary, public API, callers, focused
  tests, fixtures, and relevant research notes.
- For behavior fixes, add or update the smallest regression test before fixing
  the implementation. Prefer observable output and invariants over tests of
  private helper details.
- Keep the string, streaming, incremental, retained-document, and pager paths
  behaviorally equivalent. The golden test exercises all render paths.
- Treat changes to ANSI bytes, line numbering, hunk pairing, separators,
  decorations, parser boundaries, or pager lifecycle as contract changes. Update
  tests and goldens deliberately rather than accepting incidental churn.
- Keep the renderer independent of terminal state. Terminal detection, raw mode,
  alternate-screen handling, and key input belong in `src/pager.rs`.
- Avoid new dependencies unless they are necessary and explicitly justified.
- Use the nearest hard judge first: `cargo test`, `make check`, a focused test,
  or a benchmark. Broaden validation when the change affects shared behavior.
- Do not silently broaden unsupported input modes. Record deliberate scope
  changes in `TODO.md` and add representative fixtures.
