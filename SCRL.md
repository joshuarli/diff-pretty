# SCRL implementation plan

## Objective

Extract the current built-in pager into an independent package and executable
named `scrl`. The result must:

- page plain or ANSI text from stdin as a standalone command;
- expose a small Rust library that other applications can embed;
- let diff-pretty link to the library directly, with no subprocess;
- preserve diff-pretty's renderer, streaming, pager, and golden behavior; and
- keep the work reviewable through staged, reversible migrations.

The target architecture and user-visible contract are in [PAGER.md](PAGER.md).
This file is the execution plan: exact work phases, compatibility strategy,
tests, performance checks, and release gates.

## Version scope

### v0: behavior-preserving extraction

v0 is deliberately narrow. It moves the current pager into the independent
`scrl` package, exposes the embeddable library seam, adds the standalone stdin
runner, and keeps diff-pretty behavior unchanged. The v0 judge is the existing
diff-pretty test and golden contract, not new pager features or speculative
performance work.

v0 includes only:

- the current ANSI-aware retained document and builder;
- the current retained/live pager and lazy regex-search state machine;
- the current terminal, key decoder, cleanup, and bounded source behavior;
- the `scrl` library and basic `scrl` executable;
- the diff-pretty compatibility facade and in-process integration; and
- focused moved tests plus the existing renderer/golden checks.

v0 explicitly does not add the lz-inspired optimizations or feature expansion
listed below. A change that requires new search semantics, new terminal
behavior, a new dependency, or a new input mode belongs in v1.

### v1: implemented pager improvements

v1 is implemented as separate, benchmarked changes after the v0 hard-break
migration. It includes:

- reusable whole-frame buffering and a single write per frame;
- a broader ANSI tokenizer, safe non-SGR control handling, and
  `unicode-width`-based cell measurement;
- a fixed key-byte buffer and deadline-based escape decoding;
- a literal-search matcher such as Boyer-Moore-Horspool, preserving exact
  regex semantics for the fallback path;
- lazy seekable line sources and optional file operands, while retaining the
  bounded streaming source for pipes;
- cursor-aware search editing, `?`/`n`/`N` navigation, and optional injected
  search history;
- wrap mode, follow mode, filtering, and a help screen, each with its own
  state and streaming tests; and
- additional terminal lifecycle handling for SIGTERM and suspend/resume.

SIGWINCH resize/redraw remains out of scope. Smart-case search, silent binary
refusal, and silent line truncation are not planned: they would change the
content/search contract rather than merely optimize it. Persistent history must
never become implicit filesystem behavior in the reusable library.

## Decisions to make durable before coding

The implementation should commit to these boundaries first:

1. The new Cargo package is named `scrl` and contains both a library target and
   a binary target named `scrl`.
2. `scrl` has no dependency on diff-pretty. The dependency direction is
   diff-pretty → scrl.
3. `scrl::Document` is generic over plain/ANSI text. It does not know about
   Git, hunks, files, line-number styles, or fixed width 80.
4. `scrl::Session` is terminal-independent and can be driven by document
   chunks and decoded events. Unix terminal handling is an adapter around it.
5. `scrl::ChunkSource` is the live boundary. It emits already-rendered text
   chunks through a bounded producer path and does not parse application
   semantics.
6. The v0 compatibility facade was removed as a deliberate hard break. All
   callers use `scrl` directly.
7. Existing renderer goldens are evidence, not generated migration output. No
   golden is changed merely because code moved.

If implementation discovers a choice that changes one of these contracts, stop
at that phase and update both this plan and `PAGER.md` before proceeding.

## Current source inventory

The extraction starts from these existing boundaries:

- `src/pager.rs` contains paging policy, retained and live viewers, pager state,
  terminal lifecycle, key decoding, viewport/status output, and pager tests.
- `src/pager_search.rs` contains query input, regex compilation, lazy line scan
  caching, initial search, directional navigation, live growth, wrapping, and
  search tests.
- `src/render.rs` contains `RenderedDocument`, its ANSI span storage, visible
  line text, search overlay compositor, and `IncrementalDocumentRenderer`, but
  also contains all diff parsing/styling and therefore must be split carefully.
- `src/main.rs` owns diff-pretty CLI parsing and currently invokes
  `pager::emit_reader` for terminal operation.
- `tests/golden.rs` compares string, streaming, retained, and incremental
  renderer paths byte-for-byte. It does not authorize pager-output churn.
- `benches/bench.rs` exercises real viewport rendering through
  `RenderedDocument::write_viewport` and should remain useful after the type
  moves.
- `Makefile` and the release/PGO workflow currently build one application
  binary and must be extended without adding `scrl` symbols to the wrong PGO
  boundary.

Before editing, record the clean baseline with:

```sh
cargo test --release
make check
```

Do not run pre-commit hooks and do not push a remote branch.

## Target repository shape

Add a workspace member without turning diff-pretty into a workspace-wide
dependency of the new package:

```text
Cargo.toml
scrl/
  Cargo.toml
  src/lib.rs
  src/main.rs
  src/document.rs
  src/search.rs
  src/session.rs
  src/viewport.rs
  src/source.rs
  src/terminal.rs
```

The root manifest remains the diff-pretty package and adds a path dependency on
`scrl`. The `scrl` manifest provides:

- package/library/bin name `scrl`;
- edition matching the repository;
- `regex-lite` as a normal dependency;
- optional Unix `rustix` terminal support behind the default `terminal`
  feature; and
- `required-features = ["terminal"]` for the binary if needed to keep
  `default-features = false` library builds pure.

Do not add another pager dependency. Do not make `scrl` depend on a terminal
framework solely to avoid moving the existing small backend.

## Public contract to implement first

Add the public API in `scrl/src/lib.rs` before moving behavior. The first
version should expose the following stable concepts:

```rust
pub struct Document;
pub struct DocumentBuilder;

impl DocumentBuilder {
    pub fn new() -> Self;
    pub fn push_str(&mut self, chunk: &str);
    pub fn finish(self) -> Document;
}

impl Document {
    pub fn line_count(&self) -> usize;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn line_text(&self, line: usize) -> Option<&str>;
    pub fn write_to<W: std::io::Write + ?Sized>(&self, output: &mut W)
        -> std::io::Result<()>;
}

pub struct Size { pub rows: usize, pub columns: usize }
pub struct SessionOptions { pub title: String }
pub struct Session;
pub enum Event { /* decoded pager events */ }
pub enum Action { Continue { changed: bool }, Quit }

pub trait ChunkSource: Send + 'static {
    fn produce(
        self,
        emit: &mut dyn FnMut(&str) -> std::io::Result<()>,
    ) -> std::io::Result<()>;
}

pub enum PagingMode { Auto, Always, Never }
pub struct RunOptions {
    pub paging: PagingMode,
    pub session: SessionOptions,
}
pub enum ExitReason { Quit, EndOfInput }
```

Do not expose `SearchState`, `PagerState`, ANSI varint storage, raw terminal
file descriptors, or diff-pretty's `Style` as part of the initial public API.
`Session` methods and exact visibility are specified in `PAGER.md`; keep the
public surface smaller if an implementation detail does not need embedding.

Document the input assumptions explicitly: UTF-8 text, SGR-aware retained
styles, logical trailing empty lines, visible-text search, and eight-column tab
stops. Return `io::Result` for output and source failure; do not panic on bad
user queries, missing terminals, or normal EOF.

## Phase 0: characterization and scaffolding

### Work

1. Capture the baseline test/check results and inspect `git status`.
2. Add the workspace member and empty `scrl` package with the public names
   behind compiling stubs.
3. Add a minimal `scrl` binary that accepts the eventual flags but does not yet
   replace diff-pretty's path.
4. Add a path dependency from diff-pretty only after the empty package builds.
5. Keep the old pager fully active while scaffolding.

### Hard judges

```sh
cargo check --workspace --all-targets
cargo test --release
make check
```

The new package must build without importing any diff-pretty module. Use
`cargo tree -p scrl` and a source search for `diff_pretty` to verify the
direction.

## Phase 1: move the generic document representation

### Work

1. Move the pager-oriented `RenderedDocument` storage from `src/render.rs` to
   `scrl/src/document.rs`:
   - contiguous visible text;
   - line endpoints and trailing-line behavior;
   - compact span table and custom SGR table;
   - SGR replay and ordinary serialization;
   - visible `line_text`; and
   - viewport line writing, including horizontal clipping and cell width.
2. Move the generic SGR state replay and search overlay compositor with the
   document, not with diff-specific rendering.
3. Use `DocumentBuilder::push_str` as the renderer sink boundary. Keep
   lower-level span methods private.
4. In diff-pretty, replace the concrete document with `scrl::Document` (a type
   alias is preferred initially) and implement its existing `RenderSink` over
   `scrl::DocumentBuilder`.
5. Keep `IncrementalDocumentRenderer` in diff-pretty, but make it contain a
   `scrl::DocumentBuilder` plus the diff parser state. It remains responsible
   for diff-safe chunk boundaries and parser completion.
6. Keep diff-specific `TextRange` construction out of the renderer if the
   generic viewport owns match ranges; otherwise introduce a small public
   `MatchRange` value in scrl and use it from the session only.

Do not introduce the v1 ANSI tokenizer, Unicode-width dependency, frame buffer,
or lazy seekable line source in this phase. Preserve the current SGR scope,
cell-width behavior, and serialization bytes while moving ownership.

### Compatibility checks

Run the golden test after each move. Compare these paths byte-for-byte:

- `render`;
- `render_to`;
- `render_reader_to`;
- `render_document` followed by `write_to`; and
- `render_reader_document` followed by `write_to`.

No terminal is needed for this phase. Any output difference is a migration bug
until explained by a deliberate contract update.

## Phase 2: move search and session state

### Work

1. Move `src/pager_search.rs` into `scrl/src/search.rs` and replace
   diff-pretty imports with `scrl::Document` and generic match ranges.
2. Preserve the current lazy algorithm and its invariants:
   - query-local `Unscanned`/`NoMatch`/`Matches` cache;
   - one regex evaluation per line per query;
   - initial scan from line zero;
   - expanded viewport highlight windows;
   - directional probes and wrap candidates;
   - pending behavior during live input; and
   - zero-width-match navigation without zero-width highlight loops.
3. Move `PagerState`, key actions, input mode, loading state, and retained/live
   viewer equivalence into `scrl/src/session.rs`.
4. Replace the old private `Key` type with public `Event` at the session seam.
   Keep byte decoding out of the state machine.
5. Move status text and bounded/sanitized status writing into `scrl`.
6. Add a generic title option. The standalone default is `scrl`; the
   diff-pretty compatibility facade may pass `diff-pretty` while existing
   callers are migrated.

### Tests to move unchanged in meaning

Move or recreate the focused tests under `scrl` with names tied to the source
behavior:

- search input treats command characters as query text;
- Unicode scalar backspace and Ctrl-U editing;
- invalid and empty query handling;
- visible-text search excludes ANSI bytes;
- first match, centered viewport, and all ranges on one line;
- previous/next navigation order and wrap;
- zero-width match determinism;
- no rescanning of a cached line;
- exact initial/window/directional scan boundaries;
- pending search while a live document grows;
- completed no-match stability;
- viewport saturation and horizontal shift; and
- highlight overlay restoration across SGR boundaries.

Use a recording matcher or scan trace, not timing thresholds. The behavior
contract is which lines are evaluated and when, not an incidental duration.

### Hard judges

```sh
cargo test -p scrl
cargo test --release
make check
```

The diff-pretty pager must still compile through its compatibility facade, but
the new tests must not reach into scrl's private fields.

## Phase 3: move terminal/session orchestration

### Work

1. Move Unix-only terminal code from `src/pager.rs` to
   `scrl/src/terminal.rs`:
   - `/dev/tty` opening;
   - `tcgetwinsize` and captured dimensions;
   - raw-mode guard and restoration;
   - alternate-screen `Screen` guard;
   - no-wrap/cursor lifecycle;
   - bounded key-reader channel;
   - `select` wake-up pipe; and
   - key byte/escape/UTF-8 decoding.
2. Keep the terminal module dependent only on public `Session` events and
   drawing. It must not inspect `Document` internals or search state.
3. Make cleanup order explicit and testable: stop source, wake key reader,
   leave screen, restore raw mode, join workers, then return the original
   result unless cleanup itself is the only error.
4. Preserve direct fallback when terminal open, size, raw mode, or key setup
   fails. Direct fallback must consume the source and write rendered bytes
   without alternate-screen sequences.
5. Keep non-Unix builds compiling with an unsupported terminal backend and a
   direct-output path.

Do not add v1 signal handling or resize support during extraction. Preserve
the current terminal lifecycle and no-resize contract.

### Terminal tests

The current decoder tests should move to scrl and become independent of tty
files. Add deterministic tests for:

- arrows, Home/End, Page Up/Page Down, Backspace, Enter, Escape, Ctrl-U;
- split escape sequences across reads;
- split UTF-8 scalars across reads;
- malformed UTF-8 and unknown escape sequences;
- cancellation wake-up; and
- exactly-once screen cleanup on normal quit, EOF, source error, and output
  error.

Use a fake event source and byte writer for the latter tests. Do not make CI
depend on an interactive terminal.

## Phase 4: add source adapters and the standalone command

### Work

1. Implement `ReaderSource<R>` in `scrl/src/source.rs`. It should preserve
   complete text and SGR sequences, group input into bounded chunks, and report
   source errors without swallowing them.
2. Implement the retained path by building a `Document`, marking the session
   finished, and running the same `Session`/terminal loop as live input.
3. Implement the live path with a bounded channel of owned chunks and a
   cancellation flag. The pager must be able to draw before EOF and show
   `loading` until `finish` arrives.
4. Implement `run_reader` and `run_source` using the standard stdout and
   controlling terminal. Auto mode must use the captured terminal dimensions;
   it must not consult `COLUMNS` for piped output.
5. Implement `scrl/src/main.rs` with `--paging=auto|always|never` and
   `--no-pager`. Invalid values should have a deterministic error or the
   documented default; do not silently invoke an external pager.
6. Add command-level non-terminal tests that pipe input and assert exact bytes
   and absence of `\x1b[?1049h`/`\x1b[?1049l`.

The v0 command accepts stdin only. File operands, follow mode, binary policy,
and line-length limits are deferred or explicitly rejected; do not introduce
them as incidental behavior.

### Standalone acceptance examples

```sh
printf 'one\ntwo\n' | cargo run -p scrl -- --paging=never
printf '\033[31mred\033[0m\n' | cargo run -p scrl -- --paging=never
printf 'long\ncontent\n' | cargo run -p scrl -- --paging=always > /tmp/scrl.out
```

The redirected `always` case must be direct output, because there is no usable
terminal for an interactive session.

## Phase 5: integrate diff-pretty without subprocesses

### Work

1. Add the `scrl` path dependency to the diff-pretty manifest.
2. Replace the body of `src/pager.rs` with the compatibility facade plus the
   diff-specific source adapter. Keep names used by `src/main.rs`, benches, and
   any downstream callers during the migration.
3. Implement the adapter around the existing `for_each_render_chunk` and
   `IncrementalDocumentRenderer` logic. It must emit complete parser-safe
   chunks and preserve the existing word-diff pairing boundary.
4. Route `emit_reader` through `scrl::run_source` when paging is requested.
   Route the direct path through the existing renderer when paging is disabled
   or when scrl reports no usable terminal.
5. Route retained `emit` through a `scrl::Document` session without converting
   it to a serialized string and reparsing it.
6. Keep `should_use_pager` as a diff-pretty policy helper for source selection,
   or replace it with a thin documented delegate. It must not duplicate scrl's
   terminal lifecycle.
7. Keep `src/main.rs` CLI flags and error wording stable unless a deliberate
   compatibility note is added.

### Prove no subprocess path

Search the final source and dependency graph:

```sh
rg -n "Command::|std::process::Command|PAGER|diff-pretty" scrl
cargo tree -p scrl
```

The first command may find the package name in documentation, but must not find
process spawning, `$PAGER`, or a diff-pretty import. `cargo tree -p scrl` must
not include diff-pretty.

### Diff-pretty regression matrix

Run all of these after the integration:

```sh
cargo test --release
make check
cargo test --workspace --all-targets
```

The golden test must still compare all five renderer paths. Add explicit tests
for the compatibility facade's retained and live pager setup using fake output
and event inputs where possible.

## Phase 6: benchmark and release integration

### Benchmarks

Move the generic viewport/search benchmarks to scrl or add an equivalent scrl
benchmark target. Keep diff-pretty's end-to-end renderer benchmarks in its own
package. Measure at least:

- viewport draw with no search;
- initial search with early match;
- initial search with late/no match;
- one-line scrolling through a large document;
- repeated redraw without movement;
- cached next/previous navigation; and
- live search submitted before EOF.

Report time, allocation count, total bytes, and peak simultaneously-live bytes
when the benchmark harness measures them. Compare a persisted baseline with
`make bench-diff` or the equivalent scrl command. Do not mix scrl library
benchmarks into the diff-pretty PGO workload by accident.

### Makefile and release targets

Extend build targets deliberately:

- `make test` continues to test diff-pretty and adds workspace/scrl tests as
  appropriate;
- `make check` continues to render every diff fixture through diff-pretty;
- add a `scrl` build/check target if it improves discoverability;
- release packaging must state whether it produces `diff-pretty`, `scrl`, or
  both; and
- PGO targets must instrument and profile only the explicitly launched
  application. If both binaries get release artifacts, give each an isolated
  target/profile directory and workload provenance check.

Do not change static/dynamic musl linker behavior merely as part of the code
move. Re-run the existing release verification targets after the package
dependency is in place.

## Phase 7: remove legacy implementation and finalize docs

Only after all checks pass:

1. Delete the old pager/search implementation from diff-pretty rather than
   leaving a second copy that can diverge.
2. Reduce `src/pager.rs` to the documented facade/source adapter, or remove it
   if all callers have migrated and the public compatibility decision is
   recorded.
3. Remove now-unused `rustix` and pager-only code from diff-pretty's manifest
   and modules.
4. Update repository layout and command documentation in the root project
   instructions.
5. Update `PAGER.md` only when the stable design changes; record execution
   details and migration evidence here.
6. Add a changelog/release note if the package is published or installed by
   users.

## Failure modes and mitigations

### Accidental renderer churn

Moving the ANSI document can change reset placement or final-newline handling.
Keep the golden test running after each document change. Compare serialized
bytes before and after; do not use visual inspection as the judge.

### Split ownership of ANSI behavior

If diff-pretty keeps its own search overlay or SGR replay, retained/live paths
will fork again. Move those mechanisms with `Document` and make the diff side
emit only text plus SGR transitions.

### Blocking producer and unbounded memory

Use the existing small bounded channel and cancellation flag. Define the
ownership rule for a reader blocked in `read`: it stays in the source worker
until the read returns. Do not claim stronger cancellation than `BufRead`
provides.

### Public API leaks implementation details

Do not expose `rustix`, file descriptors, internal search phases, varint spans,
or diff renderer types. Embedders should use `DocumentBuilder`, `Session`,
`Event`, `ChunkSource`, and the high-level runners.

### Terminal cleanup regressions

Keep `Screen` and raw mode as guards. Test every exit path with fake output,
including source error and output failure. The terminal must never be left in
raw mode or the alternate screen after a normal process return.

### PGO contamination

The new workspace may cause Cargo to build more binaries than the existing
workload launches. Keep profile flags target-scoped and verify merged profiles
contain only the intended application symbols. Update `PGO.md` if release
artifacts or workloads change.

## v0 final acceptance checklist

The extraction is complete only when:

- `cargo run -p scrl -- < input` works as a standalone executable;
- `scrl` handles plain and SGR-colored text without diff-pretty;
- an embedding crate can depend on `scrl` and drive a `Session` without a
  subprocess or global pager configuration;
- `scrl::run_source` supports bounded live chunks and cancellation;
- retained and live sessions share one viewport/search state machine;
- `/`, lazy search, highlighting, navigation wrapping, invalid queries,
  zero-width matches, and loading behavior are covered by focused tests;
- key decoding and terminal cleanup are covered without requiring an
  interactive CI terminal;
- non-terminal `never`, `auto`, and redirected `always` paths emit direct bytes
  with no alternate-screen sequences;
- diff-pretty links to scrl directly and has no subprocess or `$PAGER` path;
- `cargo test --release`, `cargo test --workspace --all-targets`, and `make
  check` pass;
- diff-pretty fixture goldens remain byte-for-byte unchanged; and
- release, benchmark, and PGO documentation accurately names both binaries
  and their ownership boundaries.

## v1 acceptance checklist

Each v1 item has an isolated change, focused regression tests, and a benchmark
or resource-bound justification where applicable:

- frame buffering proves byte-identical frames and one complete frame write;
- ANSI tokenization and Unicode cell width cover CSI, OSC, tabs, combining
  marks, wide characters, clipping, and style restoration;
- key buffering covers split escape sequences and bare-Escape deadlines;
- literal search proves identical ranges to the regex path for eligible
  patterns and has a dedicated benchmark;
- lazy file sources preserve errors, partial lines, EOF, and bounded memory;
- line editing/history/navigation additions preserve the existing search
  cache and live behavior;
- wrapping, follow mode, filtering, and the help screen have explicit state
  transition tests; and
- SIGTERM plus suspend/resume restore the terminal lifecycle before exit or
  stop.

The focused redraw benchmark uses a 20,000-line realistic corpus. Before v1,
cached search redraw measured about 313 µs and 20,002 allocations per frame;
after frame buffering and cached-range reuse it measures about 14 µs and one
120-byte allocation. The benchmark suite is `scrl/benches/bench.rs` and is run
with `cargo bench -p scrl --bench bench -- --sample-count 10`.
