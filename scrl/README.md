# scrl

`scrl` is a small standalone ANSI-aware terminal pager and reusable Rust
library. It reads UTF-8 text from stdin or file operands, retains logical
lines, searches visible text, and renders a deterministic terminal viewport.
It has no application-specific parser and does not invoke another pager.

## Package shape

The package contains both a library and a binary named `scrl`:

```text
scrl/
  src/lib.rs       public API and runners
  src/document.rs  retained ANSI document and viewport compositor
  src/search.rs    lazy search cache and matchers
  src/session.rs   terminal-independent pager state machine
  src/source.rs    bounded reader and file sources
  src/terminal.rs  Unix raw mode, signals, screen, and key decoding
  src/main.rs      standalone command
```

The reusable state and document code can be built without the terminal
feature. The default `terminal` feature enables the Unix terminal adapter and
the command-line binary's interactive behavior.

## Library API

The core document boundary accepts UTF-8 chunks containing plain text and SGR
style transitions:

```rust
use scrl::{DocumentBuilder, Size, Session, SessionOptions};

let mut builder = DocumentBuilder::new();
builder.push_str("plain\n");
builder.push_str("\x1b[31mred\x1b[0m\n");
let document = builder.finish();

assert_eq!(document.line_count(), 3);
assert_eq!(document.line_text(1), Some("red"));

let mut session = Session::new(
    Size { rows: 24, columns: 80 },
    SessionOptions::default(),
);
session.push_chunk("one\ntwo\n");
session.finish();
```

`Document` provides `line_count`, `len`, `is_empty`, `line_text`, `write_to`,
and `write_viewport`. `DocumentBuilder` supports incremental `push_str` and
`finish`.

Logical lines include the trailing empty line after a final newline.
`line_text` omits line terminators and SGR bytes but preserves ordinary UTF-8
content, including tabs. Search ranges are UTF-8 byte ranges into that exact
visible text.

`Session` is terminal-independent and exposes:

```rust
pub struct SessionOptions {
    pub title: String,
    pub search_history: Vec<String>,
    pub wrap: bool,
    pub follow: bool,
    pub filter: Option<String>,
}

pub struct Size {
    pub rows: usize,
    pub columns: usize,
}

pub enum Event {
    Interrupt, Up, Down, Left, Right,
    PageUp, PageDown, Home, End,
    Enter, Escape, Backspace, Delete, CtrlU,
    Text(char),
}

pub enum Action {
    Continue { changed: bool },
    Quit,
}
```

The session methods are `new`, `push_chunk`, `finish`, `handle`, `advance`,
`draw`, and `document`. `draw` builds and writes one complete fixed-height
frame, then flushes it so the final status row is immediately visible on
line-buffered terminal output.

## Sources and runners

The live boundary is callback-based and bounded by the runner:

```rust
pub trait ChunkSource: Send + 'static {
    fn produce(
        self,
        emit: &mut dyn FnMut(&str) -> std::io::Result<()>,
    ) -> std::io::Result<()>;
}
```

Available sources are:

- `ReaderSource<R>` — bounded UTF-8 chunks from a `BufRead`;
- `FileSource` — lazily opens one file when production starts; and
- `FilesSource` — lazily opens and concatenates multiple file operands.

`PagingMode` is `Auto`, `Always`, or `Never`. `RunOptions` combines the mode
with `SessionOptions`. `run_reader` and `run_source` own stdout and use the
controlling terminal when available; `run_document` handles an already
retained document. Redirected output is always written directly without
alternate-screen controls.

The producer-to-session path uses a one-chunk channel and cancellation flag.
The runner initially pulls only enough chunks to fill the first viewport, then
stops requesting source data while the user is idle. Forward navigation pulls
one more chunk; `End` and a submitted search pull through EOF. Already loaded
content is retained, so scrolling backward never requires replaying the pipe,
but an untouched large input does not become a resident in-memory document.

## Command line

```text
scrl [--paging=auto|always|never] [--no-pager]
     [--wrap] [--follow] [--filter=REGEX] [FILE ...]
```

- `--paging=auto` pages only when stdout is a terminal and the content exceeds
  one screen;
- `--paging=always` uses the pager when a usable terminal exists;
- `--paging=never` and `--no-pager` write directly;
- `--wrap` wraps logical lines at terminal cell boundaries;
- `--follow` keeps the viewport at the newest content while a source grows;
- `--filter=REGEX` displays only matching logical lines; and
- file operands are opened lazily and streamed with bounded memory; and
- live paging retains the loaded prefix but applies backpressure at the
  viewport instead of reading the entire source eagerly.

The command never consults `$PAGER` or invokes an external pager. Invalid
paging values are rejected. Input is not silently treated as binary and
there is no implicit line truncation policy.

## Viewport and terminal behavior

Terminal dimensions are captured when the session starts. The first version
does not implement SIGWINCH resizing. Long unwrapped lines are clipped at the
right edge; wrapped lines use Unicode cell widths and eight-column tab stops.
Combining characters occupy zero cells and wide characters occupy two cells.

Navigation includes arrows, `j`/`k`, Page Up/Page Down, `b`, Space, Home, End,
`g`, `G`, and horizontal Left/Right movement. `q`, `Q`, and Ctrl-C quit. `h`
opens the help screen; `h` or Escape closes it.

Raw mode and alternate-screen state are restored on normal exit, source/output
errors, SIGTERM, and suspend/resume. Suspend restores cooked terminal mode and
leaves the alternate screen before stopping, then reapplies raw mode and
redraws after continuation.

## Search

`/` starts forward search and `?` starts backward search. Search is performed
against visible line text, never against serialized ANSI bytes. Enter submits
the expression; Escape cancels it; invalid expressions remain editable.

Search input supports Unicode-aware cursor movement, Home/End, Backspace,
Delete, Ctrl-U, and explicitly injected in-memory history. `n` and `N`
navigate matches in the current direction and its inverse. Search caches each
line once per query and remains incremental while a source is still loading.

Queries without regular-expression metacharacters use a
Boyer-Moore-Horspool literal matcher. All other queries use `regex-lite`,
preserving regular-expression semantics exactly for the fallback path. Zero-
width matches remain navigable but do not produce a visible highlight.

Viewport highlighting replays the current SGR state, applies a temporary
reverse-video style, restores the prior style, and finishes non-plain lines
with a reset. Non-SGR CSI and OSC controls are tokenized out of viewport text
and are not replayed, so input cannot move the cursor or rewrite terminal
metadata during a redraw.

## Performance and tests

The focused benchmark suite is `benches/bench.rs`:

```sh
cargo test -p scrl --release
cargo bench -p scrl --bench bench -- --sample-count 10
```

It uses a 20,000-line corpus and reports redraw time, allocation count, and
allocated bytes. Cached-search redraw is approximately 14 µs with one
120-byte allocation after frame buffering and cached-range reuse. The
pre-optimization comparison was approximately 313 µs with 20,002 allocations.

The suite covers plain, styled, horizontally clipped, cached-search, initial
search, regex fallback, and live-chunk redraw paths. Unit tests cover document
serialization, ANSI safety, search behavior, sources, session transitions,
key decoding, frame writes/flushes, and terminal-independent help behavior.
