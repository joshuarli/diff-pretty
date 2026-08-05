# scrl pager design

## Status and purpose

This is the target design for extracting the native pager from diff-pretty into
an independent Rust package and executable named `scrl`. It replaces the old
document that described adding search to the diff-pretty pager. Search is now
part of the pager contract and must move with the rest of the pager.

The design has two consumers:

1. `scrl`, a standalone executable that reads ANSI or plain text from stdin and
   pages it on a terminal; and
2. diff-pretty, which links the `scrl` library and continues to render patches
   in-process. It must never invoke the `scrl` executable.

The package has no dependency on diff-pretty. The dependency direction is:

```text
diff-pretty renderer ────────┐
                             ├──> scrl library
scrl executable ─────────────┘
```

`diff-pretty` remains responsible for interpreting Git patches and producing
its fixed presentation. `scrl` is responsible only for displaying an already
rendered text stream.

## Version scope

The v0 design was an extraction contract, not a feature expansion. It preserves
the current pager's SGR scope, cell-width implementation, key decoding,
retained/live behavior, and no-resize terminal lifecycle while moving ownership
into `scrl`. The lz-inspired optimizations and additional less-like features
were intentionally left for the separately committed v1 changes below.

The v1 pager includes reusable frame buffering, broader ANSI/token-cell
handling, a fixed key-byte buffer, literal-search acceleration, lazy seekable
sources and file operands, richer search editing/history/navigation, wrapping,
follow mode, filtering, help, and SIGTERM/suspend-resume lifecycle handling.
SIGWINCH resize/redraw remains out of scope. Smart-case search, silent binary
refusal, and silent line truncation are not part of the design.

## Package and module boundary

The repository becomes a Cargo workspace with the existing package and a new
member:

```text
Cargo.toml                    workspace declaration plus diff-pretty package
scrl/
  Cargo.toml                  package `scrl`, library target and binary target
  src/
    lib.rs                    public API and contracts
    document.rs               ANSI-aware retained document and builder
    search.rs                 lazy regex search and match cache
    session.rs                viewport state machine and key actions
    viewport.rs               ANSI viewport/status compositor
    source.rs                 bounded live-source adapters
    terminal.rs               Unix tty, raw mode, screen, and key decoder
    main.rs                   `scrl` command-line entry point
```

The package name and library crate name are both `scrl`. The binary target is
also named `scrl`, so users can either install the command with
`cargo install scrl` or add `scrl` as a normal Rust dependency. The package
must not contain a feature, module, or dependency that imports diff-pretty.

The default feature enables the real terminal backend. A `default-features =
false` dependency remains useful for document/session tests and non-terminal
integrations; the pure state and document code must not require Unix APIs.
`regex-lite` is the only required runtime dependency. `rustix` is optional and
is enabled by the terminal feature on Unix.

v0 should not add `unicode-width` or another optimization dependency. Revisit
that choice only as part of the separately benchmarked v1 cell-width work.

## Responsibilities

### scrl owns

- the ANSI-aware document representation;
- UTF-8 line indexing and visible-text access;
- preservation of SGR sequences while drawing a viewport;
- line clipping, terminal cell-width calculation, and tab stops;
- the pager session state machine;
- `/` regular-expression search, highlighting, lazy scanning, and wrapping;
- retained and incrementally growing documents;
- bounded producer-to-pager channels and cancellation;
- terminal dimensions captured at session start;
- raw terminal mode, alternate-screen entry/cleanup, key decoding, and status
  rendering; and
- the standalone `scrl` CLI.

### diff-pretty owns

- Git metadata and patch parsing;
- hunk pairing and word-diff inference;
- diff-specific styles, decorations, line numbers, and fixed width 80 output;
- the decision to render a patch directly when paging is disabled; and
- the adapter that turns its rendered patch chunks into the generic `scrl`
  source interface.

The pager must not know what a hunk, commit, file header, or diff line means.
The renderer must not know about `/dev/tty`, raw mode, alternate screens, key
decoding, or pager search.

## Public library surface

The public surface should be small and intentional. Names below are proposed
contract names; implementation helpers remain private to the package.

### Document construction

```rust
pub struct Document { /* retained ANSI-aware logical lines */ }
pub struct DocumentBuilder { /* incrementally populated Document */ }

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
    pub fn write_viewport<W: std::io::Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
    ) -> std::io::Result<()>;
}
```

`DocumentBuilder` accepts UTF-8 text containing plain content and CSI SGR
sequences (`ESC [ ... m`). SGR bytes are retained as style transitions but do
not appear in `line_text`. Unsupported escape sequences are preserved as raw
input only if they are explicitly admitted by the builder contract; the first
implementation should preserve the current behavior and scope to SGR.

Logical lines include the trailing empty line after a final newline. `line_text`
returns visible UTF-8 text without the line terminator or SGR bytes. It returns
the original text bytes for ordinary content, including tabs; viewport width
calculation interprets tabs at eight-column stops. Match offsets are UTF-8 byte
offsets into this exact returned string.

The builder is the generic sink boundary. It must support both of these uses:

- `scrl` reads raw stdin and pushes chunks directly; and
- diff-pretty's renderer emits ANSI transitions and styled text into the
  builder without serializing the entire rendered document first.

`Document` keeps the current compact representation: one contiguous visible
text buffer, line endpoints, and deduplicated ANSI transitions. Search and
highlight drawing use the visible buffer; normal serialization replays the
stored transitions. No pager path may search the serialized ANSI stream.

The v0 builder intentionally keeps the current SGR-only parsing boundary and
cell-width behavior. General CSI/OSC tokenization and Unicode-width handling
are v1 work; they must not alter diff-pretty's serialized bytes during the
extraction.

### Session and events

The reusable interactive unit is a terminal-independent session. This gives
Rust embedders a way to drive the pager with their own event loop or terminal
adapter, while the default runner supplies the Unix implementation.

```rust
pub struct Session { /* document, viewport, search, loading state */ }

pub struct SessionOptions {
    pub title: String,
}

pub struct Size {
    pub rows: usize,
    pub columns: usize,
}

pub enum Event {
    Interrupt,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Backspace,
    CtrlU,
    Text(char),
}

pub enum Action {
    Continue { changed: bool },
    Quit,
}

impl Session {
    pub fn new(size: Size, options: SessionOptions) -> Self;
    pub fn push_chunk(&mut self, chunk: &str);
    pub fn finish(&mut self);
    pub fn handle(&mut self, event: Event) -> Action;
    pub fn advance(&mut self) -> bool;
    pub fn draw<W: std::io::Write + ?Sized>(
        &mut self,
        output: &mut W,
    ) -> std::io::Result<()>;
    pub fn document(&self) -> &Document;
}
```

`push_chunk` is valid until `finish`; it makes complete logical lines visible
as soon as their chunks arrive. `advance` progresses a pending initial or
directional search and returns whether drawing may change. `handle` owns the
input-mode rules, so `/`, `q`, `j`, `k`, and other command characters are query
text while search input is active.

`Session::draw` writes one complete fixed-height frame, including the status
row when there is one. It does not enter raw mode, open `/dev/tty`, or emit
alternate-screen lifecycle bytes. This is the seam that makes it testable and
embeddable.

### Sources and runners

The live runner uses a bounded source abstraction rather than making the pager
depend on a particular renderer:

```rust
pub trait ChunkSource: Send + 'static {
    fn produce(
        self,
        emit: &mut dyn FnMut(&str) -> std::io::Result<()>,
    ) -> std::io::Result<()>;
}

pub struct ReaderSource<R>(R);

pub enum PagingMode { Auto, Always, Never }

pub struct RunOptions {
    pub paging: PagingMode,
    pub session: SessionOptions,
}

pub enum ExitReason { Quit, EndOfInput }

pub fn run_reader<R: std::io::BufRead + Send + 'static>(
    input: R,
    options: RunOptions,
) -> std::io::Result<ExitReason>;

pub fn run_source<S: ChunkSource>(
    source: S,
    options: RunOptions,
) -> std::io::Result<ExitReason>;
```

The standard runners own stdout and the process terminal. For embedders that
need a different output sink or event loop, `Session` is the lower-level API;
the source trait is provided for integrations that want the same bounded live
loading behavior as the command-line runner.

`ChunkSource::produce` is deliberately callback-based. A diff renderer can
stream each completed render unit into the bounded channel without an
intermediate complete ANSI string. The runner must use a small bounded channel,
check cancellation between chunks, and never let a large input make the
channel or retained document grow solely because the user has not consumed
output.

## Command-line contract

The `scrl` binary reads stdin and writes stdout. It treats stdin as content and
opens the controlling terminal separately for keys, matching the current pager
behavior when stdin is a pipe.

The command accepts stdin or file operands. `--follow`, `--wrap`, and
`--filter=REGEX` configure the corresponding session modes. Binary detection
and line-length policies remain outside the contract; the reusable library
must not silently refuse or mutate content.

Supported flags are intentionally few:

```text
scrl [--paging=auto|always|never] [--no-pager]
     [--wrap] [--follow] [--filter=REGEX] [FILE ...]
```

`auto` pages only when stdout is a terminal and the content exceeds one screen;
`always` uses the pager whenever a usable terminal exists; `never` copies the
input unchanged. When stdout is not a terminal, all modes write content
directly and emit no alternate-screen sequences. The CLI must not consult
`$PAGER` or invoke another pager.

The status title is `scrl`. It must not contain diff-pretty-specific labels or
line-count assumptions beyond the generic logical document.

## Interactive behavior

The extracted pager preserves the current behavior unless this document calls
out a generic correction.

### Viewport

- Terminal dimensions are captured when the session starts. Resize signals are
  unsupported for the first extracted version.
- One status row is reserved when the terminal has more than one row; content
  has at least one row.
- Long lines are clipped at the right edge. The terminal is put into no-wrap
  mode while frames are drawn.
- Left and Right shift horizontally by half the terminal width, with a minimum
  shift of one column.
- `j`/`k`, arrow scrolling without an active search, Page Up/Page Down, `b`,
  Space, Home, End, `g`, and `G` retain their current meanings.
- `q`, `Q`, and Ctrl-C quit. EOF from the terminal input also exits cleanly.

### Search

`/` starts a new search input session. Enter compiles the complete query once
with `regex-lite`. Search is over `Document::line_text`, not serialized ANSI.
Matches are UTF-8 byte ranges and are highlighted with the existing reverse
video overlay while preserving each line's foreground/background style.

The current search contract moves unchanged into `scrl`:

- search input starts empty; printable characters are literal query input;
- Backspace/Delete removes one Unicode scalar value and Ctrl-U clears input;
- Esc cancels input; Enter submits; invalid expressions remain editable;
- an empty query does not create a search session;
- the first match is found from line zero and centered when found;
- every non-empty match on a line is highlighted;
- Up/Down visit matches left-to-right, then line-to-line, and wrap;
- j/k and page movement scroll without changing the selected match;
- zero-width matches are retained for navigation but produce no visible overlay;
- a completed no-match search is final and repeated arrows do no work; and
- a live no-match search waits for more input before declaring failure.

The cache is query-local and indexed by line:

```text
Unscanned | NoMatch | Matches(Vec<MatchRange>)
```

No line is regex-evaluated twice for one query. Initial search scans forward in
viewport-sized steps. Highlighting scans an expanded window around the current
viewport. Directional navigation scans only the next required expanded window,
retains a wrap candidate where necessary, and remains pending when EOF has not
arrived. Search work is advanced between frames so a large input cannot freeze
key handling.

### Status and safety

The status row reports the current line range, loading state, and generic help.
It uses bounded terminal-cell output. Query text and regex errors are sanitized
before being written; control characters, escape sequences, and bidi/invisible
formatting characters must not be allowed to alter the terminal. Long status
text is clipped rather than wrapped.

### ANSI style overlay

Search highlighting is a temporary overlay. The compositor must:

1. replay the input SGR state up to a match;
2. enter the search style;
3. write the matched visible bytes;
4. restore the exact prior SGR state; and
5. continue with the original transitions.

If a prior style cannot be represented by the supported SGR state machine, the
compositor must use a reset followed by the known complete replacement style,
never rely on terminal state from a previous line, and finish every non-plain
line with one reset. Existing no-search viewport bytes are a compatibility
contract and must remain unchanged.

## diff-pretty integration

The integration is a library call, not a child process and not an environment
lookup.

### Type and module migration

The generic `scrl::Document` replaces the pager-oriented portion of
diff-pretty's `RenderedDocument`. During migration, diff-pretty should keep:

```rust
pub type RenderedDocument = scrl::Document;
```

or an equivalent compatibility wrapper if Rust-version or documentation
constraints make a type alias impractical. Existing methods used by
`tests/golden.rs` and `benches/bench.rs` must continue to produce the same
bytes.

`diff-pretty::pager` becomes a small compatibility facade. It re-exports or
adapts `scrl::PagingMode` and retains `emit`, `emit_reader`, and
`should_use_pager` for current callers. The facade owns the diff renderer
adapter; it must not duplicate `Session`, search, terminal, or ANSI viewport
logic.

### Render path

For non-pager output, diff-pretty keeps its existing direct render path. For a
pager invocation:

1. `diff-pretty` constructs a `ChunkSource` whose producer runs the existing
   patch parser and incremental render state;
2. each complete commit/file-safe render chunk is emitted to `scrl`;
3. `scrl` builds its generic `Document` and starts the pager once the current
   auto/always policy requires it;
4. the same source is used for direct fallback if no usable terminal exists;
5. quitting cancels the source between chunks; and
6. on source completion, `scrl` calls `Session::finish` and continues in the
   retained viewer.

The chunk boundary must remain outside a diff file because diff-pretty's word
pairing requires a complete hunk/file context. `scrl` must not infer or split
Git structures. The renderer's existing `for_each_render_chunk` remains a
diff-pretty concern and feeds the generic source callback.

The diff-pretty status title may remain `diff-pretty` during a compatibility
period only if the status API explicitly accepts a caller label. The preferred
design is a generic `SessionOptions { title: String }` with `scrl` defaulting to
`scrl` and diff-pretty passing `diff-pretty`; this is presentation metadata, not
a dependency on either application.

## Error and cleanup contract

- A missing or unusable controlling terminal falls back to direct output when
  the source can still be consumed safely.
- Raw mode is guarded and always restored, including panic-safe drop paths where
  possible.
- Alternate-screen exit, cursor-show, wrap-enable, and SGR reset are attempted
  exactly once on every entered session.
- A source error is returned after terminal cleanup; it is never replaced by a
  misleading successful quit.
- A broken output returns an I/O error and still attempts cleanup.
- A user quit is a successful `ExitReason::Quit`, not an error.
- A blocked custom reader cannot be forcibly interrupted through `BufRead`; the
  source worker remains responsible for that read, as in the current design.

## Compatibility invariants

The extraction is successful only if all of these remain true:

- `diff_pretty::render`, `render_to`, `render_reader_to`,
  `render_document`, and `render_reader_document` produce byte-identical
  output for every checked-in fixture;
- `fixtures/oracle/*.out` require no incidental updates;
- `--paging=never` and direct-output fallback are unchanged;
- diff-pretty's 80-column renderer remains independent of terminal dimensions;
- retained and live pager sessions have the same search and viewport behavior;
- search never sees ANSI bytes or diff metadata semantics; and
- diff-pretty has no runtime subprocess or `$PAGER` dependency.

Intentional changes to generic `scrl` status wording or CLI behavior belong in
scrl tests and documentation. They must not be disguised as renderer golden
updates.
