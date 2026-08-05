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
  scrl document construction. Public entry points include `render`,
  `render_to`, `render_reader_to`, `render_document`, and
  `render_reader_document`.
- `src/config.rs` — fixed colors, ANSI constants, number formatting, and layout
  primitives.
- `src/align.rs` — reusable sequence-alignment implementation.
- `src/edits.rs` — word tokenization, pairing, and word-diff inference.
- `src/source.rs` — the diff-pretty rendered-chunk source adapter for `scrl`.
- `scrl/src/document.rs`, `search.rs`, `session.rs`, and `source.rs` — the
  standalone pager library's generic document, search, session, and source
  boundaries; its `scrl/src/main.rs` is the stdin command.
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
- `scripts/pgo-workload.py` — launch the explicit application binary for the
  deterministic PGO scenario and optional repeated timing samples.
- `PGO.md` — PGO contamination audit, workflow boundaries, POC assumptions,
  provenance checks, and deferred workload design.
- `GITOXIDE-INTEGRATION.md` and `NATIVE-INTEGRATION.md` — research notes for
  possible structured integrations; they are not part of the current runtime
  path.
- `TODO.md` — known behavior gaps and explicitly deferred input modes.

## Commands

```sh
make test                     # release-mode Rust tests and golden snapshots
make scrl                     # release build of the standalone scrl command
make check                    # byte-for-byte binary check over every fixture
make diff FIXTURE=show_003    # ANSI-stripped diff for one fixture
make bench                    # run benchmarks and persist a host baseline
make bench-diff AFTER=...     # compare a candidate baseline with the saved one
make release                  # release build (no PGO): build-std + aggressive flags
make pgo-instrument           # build the isolated instrumented application binary
make pgo-instrument-linux     # instrument the dynamic musl application (Linux)
make pgo-instrument-linux-static # instrument the static musl application (Linux)
make pgo-profile              # run the application workload and merge its profiles
make pgo-merge                # merge already-produced application .profraw files
make release-pgo              # PGO-optimized release for the host target
make release-pgo-linux        # PGO + dynamically linked musl (Linux only)
make release-pgo-linux-static # PGO + statically linked musl (Linux only)
make verify-release           # ELF checks for the static musl release
make verify-release-dynamic   # ELF checks for the dynamic musl release
```

PGO release targets:

- `pgo-instrument` builds the actual `diff-pretty` release binary into an
  isolated target directory with target-scoped `-Cprofile-generate` flags.
  `pgo-profile` invokes that explicit binary through
  `scripts/pgo-workload.py`; it does not invoke Cargo, Divan, a benchmark
  binary, or the benchmark allocator. The raw profiles and merged report live
  under `target/pgo-profiles/`.
- `pgo-profile-linux` and `pgo-profile-linux-static` keep dynamic and static
  musl instrumentation separate. Run them inside the matching `Dockerfile`
  image, as the release workflow does.
- `release-pgo` (and the Linux variants) rebuild with `-Cprofile-use` plus
  `-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort` and
  `build-std`. Linux builds need the `diff-pretty-crt` stub objects and the
  clang/musl toolchain provided by the `Dockerfile`; the dynamic variant links
  against the musl loader, the static variant does not.
- The audit, POC scenario, profile provenance checks, comparison procedure, and
  deferred workload design are recorded in `PGO.md`.

The release workflow (`.github/workflows/release.yml`, manual dispatch) builds
macOS and Linux artifacts inside the `Dockerfile` image on each target, runs
`make verify-release[-dynamic]` and `make test-ci`, then uploads the binaries
and creates a pre-release.

Both binaries read from stdin and write to stdout. The diff renderer reads:

```sh
cargo run --release < fixtures/show_003.patch
cargo run --release -- --paging=never < fixtures/show_003.patch
```

The standalone pager accepts the same paging flags and can be run with:

```sh
cargo run -p scrl -- --paging=never < fixtures/show_003.patch
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

- End-to-end rendering of the representative Git-show corpus, metadata-heavy
  cases, multi-commit logs, and plain unified diffs.
- Real multi-commit, colorized, plain unified, streaming, retained-document,
  and pager viewport paths over checked-in fixtures.

When optimizing, run the narrowest relevant benchmark first, then compare a
persisted baseline. Report time, peak simultaneously-live bytes, total bytes,
and allocation count when those measurements are relevant.

## Paging

The pager is linked into diff-pretty and is also distributed as the
standalone `scrl` binary; neither path consults `$PAGER`.

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
  alternate-screen handling, and key input belong in `scrl/src/terminal.rs`.
- Avoid new dependencies unless they are necessary and explicitly justified.
- Use the nearest hard judge first: `cargo test`, `make check`, a focused test,
  or a benchmark. Broaden validation when the change affects shared behavior.
- Do not silently broaden unsupported input modes. Record deliberate scope
  changes in `TODO.md` and add representative fixtures.
