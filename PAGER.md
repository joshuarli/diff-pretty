# Built-in pager: lazy regular-expression search

## Status

This document is the implementation design for adding `/` search to the native
pager. It is deliberately more specific than a feature sketch: the search
window, cache ownership, input state machine, rendering boundary, streaming
behavior, and tests are all part of the design contract.

The feature applies only to the native terminal pager. It must not change the
non-interactive renderer, the byte-for-byte renderer goldens, or any output
written when paging is disabled.

## Goals

The pager will support:

- `/query` command entry, submitted with Enter.
- Regular expressions compiled by `regex-lite`.
- A literal backslash passed to the regex compiler, so expressions such as
  `\\.` and `\\+` escape regex metacharacters in the normal regex-lite way.
- A first-character discard after entering `/`.
- Search matches highlighted in the viewport with reverse video.
- Up and Down arrow keys as previous/next-match navigation while a valid search
  is active.
- Ordinary `j`/`k` scrolling even while a search is active.
- Wrapping at both ends of the document.
- Vertical centering of the selected match, clamped at the beginning and end of
  the document.
- Search over logical, ANSI-free rendered line text. Input color sequences
  must neither become searchable text nor interfere with match offsets.
- Lazy search with a bounded search window around the current viewport.
- Incremental search as the live pager receives more rendered lines.
- Invalid-regex and no-match feedback without panicking or terminating the
  pager.

The primary performance goal is to avoid a document-wide search after the
initial search. Regex evaluation must happen only for lines that are needed for
initial discovery, the current viewport/highlight window, or a directional
match-navigation probe. Already scanned lines must never be scanned again for
the same query.

## Non-goals

This feature will not add:

- Case-insensitive search syntax or a `/i` modifier.
- Search across newline boundaries.
- Search of raw ANSI escape sequences.
- Horizontal scrolling or terminal resize support.
- Persistent search history across pager invocations.
- A document-wide match count. A lazy pager cannot honestly display a total
  count without defeating the search contract.
- A second regex engine or a new dependency beyond the direct
  `regex-lite` dependency.
- Search behavior to `render`, `render_to`, `render_reader_to`, or any other
  non-pager API.

## Existing boundaries to preserve

The current pager has two execution paths:

1. `Viewer` consumes an already retained `RenderedDocument`.
2. `LiveViewer` consumes an `IncrementalDocumentRenderer`, whose document grows
   while the input thread produces complete render chunks.

Both viewers currently own the viewport top and use
`RenderedDocument::write_viewport_with_status` to draw. They must use the same
search state machine and the same viewport/highlight rendering path. Search
behavior must not fork between retained and live modes.

`RenderedDocument` already retains visible text separately from ANSI spans.
Its `text` buffer has no SGR bytes, and tabs have already been expanded by the
renderer. This is the correct search haystack. The renderer's ordinary
serialization remains unchanged.

The native pager is Unix-only today. Search state and unit tests should be
portable Rust code where possible; terminal setup and live-pager integration
remain behind the existing Unix gates.

## User-visible interaction contract

### Entering search

- In normal pager mode, `/` enters search input mode.
- Entering search mode immediately clears the previous query, compiled regex,
  match cache, pending search state, and visible search highlights.
- Printable input after `/` is appended to the query exactly as typed. No
  search runs while the query is being edited; Enter is the search trigger.
- The first Unicode scalar value counts as one character, even if its UTF-8
  representation contains multiple bytes.
- Backspace/Delete removes the last Unicode scalar value from the query.
- Ctrl-U clears the current input query.
- Esc cancels search input, restores normal navigation, and leaves the pager in
  the state it had before this search command began. Since entering `/` clears
  the old query immediately, this means Esc leaves the pager with no active
  search and no old highlights.
- Enter submits the current query.
- `q`, `j`, `k`, `b`, space, `/`, and all other ordinary characters are literal
  query input while search input mode is active. They must not invoke pager
  commands in that mode.

The first-character discard is intentional, even though it makes a one-
character query impossible when the user types it as the first character. It
is a compatibility requirement for this feature and must be covered by an
explicit test so a later refactor does not silently remove it.

### Submitting a valid query

- The query is compiled once with `regex_lite::Regex::new`.
- The query is searched from line zero forward until the first occurrence is
  found.
- If a first match is found, the viewport is centered on its line, and the
  search cache is extended through the expanded viewport window described
  below. All matches in the visible lines are highlighted.
- The selected match is the first match in document order. It is the anchor for
  the next Down/previous Up operation.
- The query remains active after submission. Highlights remain visible while
  the user scrolls with `j`, `k`, Page Up, Page Down, Home, or End.
- Scrolling with any navigation key updates the current viewport line counter
  and lazily scans only the newly relevant search window.

### Submitting an invalid query

- The compile error is retained as search status text and shown in the status
  row, including enough of the original query to identify the problem.
- Search input mode remains active so the user can edit the query and press
  Enter again.
- No regex search is attempted, no old match cache is used, and no highlights
  are drawn for the invalid query.
- Esc cancels the invalid search command.

The status renderer must not insert unescaped query bytes into an ANSI control
sequence. Non-printable query characters should be displayed safely or
replaced for status display; the actual query string remains unchanged for
compilation.

### No match

- A valid query with no match in the currently loaded complete document leaves
  the viewport where it was before submission and reports `pattern not found`
  (or an equivalent fixed message) in the status row.
- A valid query with no match in currently loaded data but with an unfinished
  live input remains pending. The status should identify that the search is
  still loading rather than reporting a final failure.
- A valid query with no match has no highlights. Arrow navigation is a no-op
  while the result is final; it must not scan the same lines repeatedly.

### Navigation after submission

When a valid query has a selected match:

- Down arrow means next match.
- Up arrow means previous match.
- Both directions wrap around the document.
- The selected match's line is vertically centered, subject to the normal
  `max_top` clamp.
- If several matches occur on one line, navigation visits their match ranges
  in left-to-right order before moving to another line.
- `j` and `k` remain one-line ordinary scrolling. Page Up/Page Down, Home, and
  End remain ordinary viewport navigation. These keys do not change the
  selected match anchor, but they do update the current viewport line and
  trigger lazy highlighting for the newly visible search window.
- If an arrow navigation operation has to inspect data that has not arrived in
  the live document yet, it remains pending instead of wrapping prematurely.
  Once loading finishes, it either selects the directional result or performs
  the requested wrap.
- If there is no selected match because the search has no result, arrow keys
  do not become ordinary scrolling keys. The user can use `j`/`k` and other
  navigation keys for scrolling.

### Status help

The status row should distinguish the modes without pretending to know a
lazy total:

- Normal, no search: existing `↑/↓ scroll` help.
- Normal, valid active search: `↑/↓ match`, plus the query or a compact
  search-active marker.
- Search input: `/query  Enter search  Esc cancel`.
- Invalid query: the input prompt followed by the compile error.
- Pending live search: retain the existing `loading` marker and add the search
  pending indication where it fits.

The exact human-readable wording is not part of the renderer golden contract,
but status output must remain bounded to the terminal row and must not panic
on a long query or error message.

## Search text and match representation

### Search haystack

Search uses the visible logical text exposed by `RenderedDocument`, not the
serialized ANSI output. The implementation should add a crate-visible line
text accessor rather than reconstructing strings from serialized output:

```text
line_text(line_index) -> Option<&str>
```

The returned slice is the already tab-expanded text in the document's
contiguous visible-text buffer. It contains neither SGR bytes nor the newline
terminator. This has three important properties:

1. A query cannot match an ANSI escape sequence.
2. Byte offsets returned by `regex-lite` are directly usable by the viewport
   highlighter.
3. Matching does not allocate a temporary string per line.

The accessor must be read-only and must not become part of the public
non-pager API unless there is a compelling reason. It is an implementation
boundary for the pager and its tests.

### Match ranges

`regex-lite::Regex::find_iter` returns UTF-8 byte ranges. Store those ranges as
half-open offsets:

```text
MatchRange {
    start: usize,
    end: usize,
}
```

A match is identified by:

```text
MatchLocation {
    line: usize,
    range_index: usize,
}
```

The range index is important because several matches can occur on one line.
Ranges are naturally ordered by line and then by `start` because
`find_iter` yields non-overlapping matches in order.

Zero-width matches are valid regex results. They must not cause an infinite
loop; `find_iter` already handles iteration correctly. They do not produce a
visible reverse-video span when `start == end`, but they still count as a
search occurrence for navigation. If a zero-width match and a non-empty match
have the same logical position, document the deterministic ordering in the
implementation and test it. The recommended ordering is the order yielded by
`find_iter`, with zero-width results retained for navigation but skipped by
highlight emission.

### Per-line scan cache

Use a query-local vector indexed by document line:

```text
LineScan {
    Unscanned,
    NoMatch,
    Matches(Vec<MatchRange>),
}
```

The vector grows when the live document grows. It is never shrunk while a
query remains active. Resetting `/` drops it, which is both simpler and the
correct memory behavior for a new query.

The cache is deliberately per-line rather than a single contiguous high-water
mark. Normal scrolling generally produces contiguous forward growth, but Home,
End, and reverse directional navigation can legitimately request disjoint
windows. Per-line state guarantees both:

- no duplicate regex evaluation for a line during one query; and
- no requirement to scan unrelated lines merely to reach a requested viewport.

The cache stores match ranges only for lines that have actually been scanned.
It does not create a document-wide match list up front. A helper may lazily
iterate cached line ranges when selecting the next or previous match.

The cache is invalidated in its entirety for every new query, including a
query that happens to have the same bytes as the previous query.

## Lazy search window

### Dimensions

Use the same `content_rows()` calculation as the pager, not the total terminal
rows. The final status row is not searchable content:

```text
height = rows.saturating_sub(1).max(1)
```

The live/current viewport is the half-open line interval:

```text
[top, min(top + height, document.line_count())]
```

The expanded search/highlight cache window is the visible viewport plus one
viewport height on either side:

```text
window_start = top.saturating_sub(height)
window_end   = min(
    document.line_count(),
    top.saturating_add(height).saturating_add(height),
)
```

In other words, the normal case is `[top - height, top + 2*height)`, clipped to
the document. This is the simple viewport math that keeps regex work bounded
by roughly three screens per newly visited viewport. The implementation must
use saturating arithmetic and avoid overflow on a pathological line count.

The pager's autoupdating current line counter is the zero-based `top` value
(and the status row displays `top + 1` where appropriate). Every operation that
changes `top`, including centering, clamping after live input, and Home/End,
updates this value before requesting search work. This makes the search window
an explicit function of the current viewport rather than a hidden global scan
position.

A useful invariant is:

> Except for the initial first-match search, production regex evaluation for a
> query may occur only for a line in the expanded window of the current
> viewport or for a line in a viewport-sized directional probe that is made the
> current search viewport before it is scanned.

Tests should assert the concrete line indices, not merely the number of calls.

### Initial submission algorithm

After successful compilation:

1. Reset the query-local scan cache and all match-selection state.
2. Scan loaded lines in order from line zero.
3. Stop immediately at the first line containing at least one match. The first
   range on that line becomes the selected match.
4. If no loaded line matches and input is still loading, record an initial
   search pending state. When more lines arrive, resume at the first
   unscanned line; do not rescan the prefix.
5. If no line matches after loading completes, report a final no-match result.
6. For the selected line, compute `center_top` and assign it to `top`.
7. Extend the cache to the expanded window around the new `top`. Lines already
   scanned in step 2 are cache hits and are not evaluated again.
8. Draw. Highlight every cached match range on visible lines.

Step 2 is the one intentional exception to the bounded current-viewport rule:
the requested first-match semantics require a prefix scan from the beginning.
The scan stops at the first occurrence and must not continue to the end before
the initial viewport is drawn.

### Scrolling algorithm

For each ordinary scroll operation:

1. Apply the existing top change and clamp it to `max_top(document)`.
2. Update the current line counter (`top`).
3. If a valid search is active, call `ensure_window(top)`.
4. `ensure_window` scans only entries marked `Unscanned` in the expanded
   interval. It skips `NoMatch` and `Matches` entries without invoking the
   regex.
5. Draw the visible viewport using cached ranges only.

A one-line scroll at a time therefore scans only the newly exposed tail of the
three-screen window. Page, Home, and End can cause a bounded burst of work for
their new window, but they never trigger a full-document search merely to draw
highlights.

If the document is live and the requested window extends beyond the currently
loaded line count, scan what exists now and scan the remainder when chunks
arrive. No line is marked `NoMatch` merely because it has not arrived yet.

### Directional match navigation

The selected match is the navigation anchor. Navigation must use the last
match position as its starting point, not begin at line zero on every arrow
press.

For Down:

1. Search cached ranges on the anchor line after the anchor range.
2. Search cached ranges on later lines through the current expanded window end.
3. If a candidate is found, select it, center it, update `top`, extend the new
   centered window, and draw.
4. If no candidate is found in the current directional window, advance a probe
   viewport by one content height toward the end. The probe's top becomes the
   current search viewport for purposes of the bounded-window rule. Scan only
   its expanded window, skipping cache hits. Repeat until a match is found or
   the loaded/end boundary is reached.
5. If loading is unfinished at the boundary, retain a pending Down operation
   and wait for more lines. Do not wrap while more input can still contain a
   later match.
6. If loading is complete and no later match exists, wrap to the beginning.
   Search from the first viewport-sized region as needed, using the existing
   cache. Select the first cached/found match in document order.

For Up, mirror the algorithm:

1. Search earlier ranges on the anchor line before the anchor range.
2. Search cached ranges on earlier lines through the current expanded window
   start.
3. If needed, move a probe viewport backward by one content height, scan only
   its expanded window, and continue.
4. Wait rather than wrap if the live document has not finished.
5. After completion, wrap to the end and search backward in viewport-sized
   probes until the last match is found.

The probe implementation should not mutate the displayed viewport until it has
a result, except that its internal search anchor must be treated as the current
viewport for enforcing the scan bound. Once a result is found, normal
`center_top` logic determines the displayed viewport. If no result is found,
restore the original `top` before reporting no result or waiting for input.

A simpler implementation may move `top` through probes internally and redraw
only when the result is selected, but it must preserve the same observable
behavior and scan bounds.

When a wrap lands on a match already cached from the initial prefix or an
 earlier viewport, no regex call is made. Wrapping is a navigation operation,
not permission to clear or rebuild the cache.

### Search state transitions

The pager should model search as explicit state rather than a collection of
flags. The following shape is recommended:

```text
SearchState {
    mode: Inactive | Input(SearchInput) | Active(SearchSession),
}

SearchInput {
    query: String,
    compile_error: Option<String>,
}

SearchSession {
    query: String,
    regex: Regex,
    scans: Vec<LineScan>,
    selected: Option<MatchLocation>,
    pending: Option<PendingSearch>,
    final_no_match: bool,
}
```

`PendingSearch` should distinguish initial discovery from directional Down or
Up. It also records the unscanned/probe boundary needed to resume without
restarting.

Recommended transitions:

```text
Inactive + Slash       -> Input(empty)
Input + Character      -> append to query
Input + Backspace      -> edit query
Input + Enter          -> compile, then Active or remain Input on error
Input + Escape         -> Inactive
Active + Slash         -> Input(empty)
Active + ordinary key -> existing pager behavior
Active + Up/Down       -> directional search
Active + scroll key    -> viewport move + ensure_window
Active + new chunk     -> resize cache + resume pending work
Active + EOF           -> resolve pending search and allow wrapping
```

A slash typed while input mode is active is query text, not a mode transition.
This makes it possible to search for `/` or use it in a larger regex.

## Input decoding design

The current `read_key` maps unknown bytes to `Key::Unknown`, which is enough
for navigation but not for query editing. Extend the key event model without
making search mode a second terminal reader:

- Add semantic events for Enter, Escape, Backspace, Ctrl-U, and ordinary text.
- Preserve the existing arrow, page, Home, End, quit, and `j`/`k` events.
- Decode ordinary UTF-8 input into Unicode scalar values before adding it to
  the query. Invalid UTF-8 bytes can become an unknown event rather than being
  inserted into a Rust `String`.
- Parse terminal escape sequences before emitting ordinary text. An isolated
  Escape is the cancel key; CSI/SS3 sequences continue to produce navigation
  events.
- Do not special-case backslash. It must be appended as an ordinary query
  character and passed unchanged to `Regex::new`.
- In normal mode `/` produces a `BeginSearch` event. In input mode the same
  byte produces ordinary text.

The existing raw-mode reader runs synchronously for `Viewer` and in the key
reader thread for `LiveViewer`. Both must use the same decoder and tests must
exercise both semantic key parsing and mode-specific dispatch.

## Highlight rendering

### Rendering boundary

Search highlighting is a pager overlay. It must not mutate `RenderedDocument`,
its retained ANSI spans, or the ordinary renderer output.

Add a viewport rendering entry point that accepts optional per-line search
ranges, while retaining the existing public methods as no-highlight wrappers:

```text
write_viewport(...)                         // existing behavior
write_viewport_with_search(..., highlights) // pager-only behavior
```

The pager supplies ranges for the visible line indices. The renderer remains
responsible for combining those ranges with retained ANSI transitions and for
writing the output efficiently.

The normal `write_viewport` path must continue to produce exactly the same
bytes as before. Existing golden tests should not change as a consequence of
adding the new method.

### Combining ranges with retained ANSI

A line consists of visible text plus retained ANSI transitions. Search ranges
are offsets into visible text, so the highlighter must walk both streams:

1. Identify the next search boundary and next retained ANSI boundary.
2. Write ordinary visible bytes up to the earliest boundary.
3. At a non-empty match start, enable the search overlay.
4. At a match end, disable the overlay and restore the underlying display
   style.
5. When an underlying ANSI transition occurs inside an active match, apply the
   transition and then reapply the search overlay if the transition disabled
   it.
6. At line end, preserve the existing final-reset behavior.

The preferred overlay is reverse video (`SGR 7`) because that is the requested
visual contract. It is a logical overlay, not a replacement for the line's
foreground color, boldness, or existing reverse state.

Do not implement matching by blindly writing `SGR 7` at the start and `SGR 27`
at the end. Existing changed-word emphasis already uses reverse video, and an
ANSI reset inside a retained line can also erase the overlay. The implementation
must track enough SGR state to restore the base style after each overlay
boundary, or emit a complete safe style transition at the boundary.

The retained renderer currently knows a fixed set of common styles and can
retain arbitrary SGR sequences in passthrough regions. The highlight compositor
should therefore use a small pager-only SGR state machine:

- Parse the SGR attributes needed to preserve reset, reverse, bold, foreground,
  and background state.
- For known renderer styles, use the existing style knowledge directly.
- For unsupported/custom SGR sequences, conservatively reset and reapply the
  complete known state when leaving a match; never allow a search overlay to
  leak into the next line.
- Always emit one final reset for a non-plain line, as the existing viewport
  writer does.

If implementation evidence shows that arbitrary passthrough SGR state cannot
be reconstructed safely, the fallback is to render a search match using a
separate background SGR (`48;5;...`) and restore it with `49`, while preserving
reverse video already present in the base style. This remains a visible
background highlight but should only be used if it is necessary to avoid
corrupting existing ANSI state. The chosen byte sequences and rationale must
be covered by pager rendering tests.

### Highlight scope

Only visible lines are written, so only visible lines need highlight ranges at
draw time. The search cache may contain ranges for the expanded window, but the
viewport writer should not iterate or serialize off-screen lines. This keeps
draw cost proportional to terminal height rather than search-cache size.

A line with no cached matches is written through the existing fast line path.
The common no-search and no-match paths must retain the current allocation
profile. Search highlighting should allocate only when a query has actual
matches or an input/status string needs growth.

## Live/incremental pager behavior

The input thread remains bounded by its existing channel. Search must not force
it to materialize the complete document.

When a `LoadEvent::Chunk` arrives:

1. Push the chunk into `IncrementalDocumentRenderer` as today.
2. Resize the active search cache to cover the new `line_count`.
3. If initial search is pending, resume scanning at the first unscanned line.
4. If directional navigation is pending, resume only the required direction and
   window.
5. If the current viewport expanded window now includes new lines, scan those
   newly available lines once.
6. Clamp `top` if the document's effective end or trailing empty line changed.
7. Draw on the existing frame schedule.

When `Finished` arrives, call `renderer.complete()` first, because completion
adds/finalizes the trailing logical line. Then resize the cache and resolve all
pending search states against the final line count. A pending search must never
mistake the pre-completion line count for the final document boundary.

A live search can be submitted before EOF. If the first loaded prefix has no
match, the pager must remain usable while waiting for more chunks. It must not
block the key loop on a full-document scan. If a first match is found in the
loaded prefix, it may immediately center and draw; later chunks only extend
highlight coverage and navigation availability.

If the user quits, set the existing cancellation flag before returning so the
input thread can stop. Search must not introduce a worker thread or a second
unbounded queue.

## Error and edge-case handling

- Empty submitted query: treat it as no active search, with no regex scan and no
  highlights. This avoids making an empty regex match every line and makes the
  first-character discard behavior predictable.
- Query ending in a lone backslash: let regex-lite report its compile error;
  do not silently append or remove the slash.
- Regex errors are data shown in status, never panics.
- Empty document and a document containing only the trailing empty line must
  center/clamp safely and must not index past `line_count`.
- A match on the first line centers at top zero.
- A match on the last line centers at `max_top`.
- A match longer than the terminal width is highlighted across the visible
  portion; horizontal clipping remains the terminal's responsibility.
- Unicode ranges are byte offsets but always come from regex-lite and are
  therefore valid UTF-8 boundaries. Backspace removes characters, not bytes.
- Tabs are searched after the renderer's eight-column expansion. A query for
  literal tab bytes is not expected to match because tabs are no longer in the
  retained visible text; this is consistent with what the user sees.
- Multiple matches on one line must not cause repeated scans or malformed
  overlapping highlight transitions.
- A zero-width regex must terminate, navigate deterministically, and produce no
  zero-length ANSI highlight pair if there is no visible content to invert.
- Search results must not leak highlights into the status row or the following
  line.
- Returning from the pager must still emit reset, wrapping enable, cursor show,
  and alternate-screen exit in the existing order.

## Suggested implementation decomposition

### 1. Dependency and document access

- Add `regex-lite` as a direct normal dependency in `Cargo.toml`; it is already
  present transitively in the lockfile but must be declared directly because
  pager code uses it.
- Let Cargo update `Cargo.lock`; do not hand-maintain a dependency checksum.
- Add a crate-visible `RenderedDocument::line_text` accessor and the
  highlight-aware viewport writer in `src/render.rs`.
- Keep `write_viewport` and non-pager serialization unchanged.

### 2. Search engine module

Prefer a dedicated `src/pager_search.rs` module, or a clearly separated
section in `src/pager.rs` if avoiding another file is more valuable. The search
engine should not depend on terminal I/O.

Implement:

- query compilation and input-independent search state;
- `LineScan` cache;
- bounded `ensure_window`;
- initial prefix discovery;
- directional next/previous selection;
- center-top calculation;
- live-document growth and pending operations;
- a matcher callback/trait used only by tests to record evaluated line indices.

Production matching is a thin adapter around `Regex::find_iter`.

### 3. Key and command state

- Extend key decoding with text/control events.
- Add the input-mode dispatcher.
- Keep mode-specific meanings out of the low-level escape parser where
  possible: the parser emits events, and `Viewer`/`LiveViewer` interpret them.
- Ensure both viewer paths call one shared `apply_key` implementation or one
  shared navigation/state object.

### 4. Viewport integration

- Replace direct `top` mutation with a method that clamps, updates the current
  line counter, and invokes `ensure_window` when needed.
- Add search-aware draw options used by both `Viewer::draw` and
  `LiveViewer::draw`.
- Update the status row according to mode, query, error, pending, and normal
  navigation.

### 5. ANSI overlay

- Implement and test the smallest correct compositor for retained line spans.
- Keep the no-search path on the existing `write_line` fast path.
- Ensure search overlay state is reset before every new line and before the
  status row.

### 6. Documentation and benchmark notes

- Keep this file as the design/contract reference.
- Update `AGENTS.md` only if the stable output contract or repository layout
  changes; do not add implementation detail there unless it becomes a general
  project rule.
- Add a focused pager benchmark for a large retained document with a query,
  repeated one-line scrolling, and repeated viewport draws. Report whether
  regex calls, allocations, and bytes scale with visited windows rather than
  total document size.

## Test plan

The test suite must prove observable behavior and the lazy-search invariant.
Tests should be unit-level where terminal devices are unavailable and should
not depend on `/dev/tty`.

### Query input tests

1. `/` enters input mode and clears an active previous query/cache.
2. The first ordinary character is retained in the query, and Enter—not text
   input—triggers the search.
3. Control keys edit or cancel input without triggering a search.
4. Backspace removes one Unicode scalar value.
5. Enter submits; Esc cancels; Ctrl-U clears.
6. `q`, `j`, `k`, `/`, backslash, brackets, parentheses, plus, star, and other
   regex punctuation are literal input while in search mode.
7. A backslash is passed unchanged to regex-lite: `\\.` matches a literal dot,
   and `\\+` matches a literal plus.
8. UTF-8 query input survives round-trip into the regex compiler.
9. Invalid UTF-8/unknown terminal bytes do not panic or corrupt the query.

### Regex and line semantics tests

1. A query matches visible text in a colorized rendered document exactly as it
   matches the corresponding uncolored text.
2. A query cannot match SGR bytes.
3. A query does not cross a logical newline.
4. Regex alternation, character classes, repetition, anchors, and escaped
   metacharacters use regex-lite semantics.
5. Multiple matches on one line are returned in order.
6. Zero-width matches terminate and do not emit empty visible highlights.
7. Empty query performs no scan.
8. Invalid regex status is retained and editable.

### Positioning and navigation tests

1. Initial submission scans from line zero and selects the first occurrence, not
   the first occurrence after the current viewport.
2. The first selected line is centered when possible.
3. First/last-line matches clamp at top zero/max_top.
4. Down visits later matches on the same line, then later lines.
5. Up visits earlier matches on the same line, then earlier lines.
6. Down wraps from the final match to the first; Up wraps from the first to the
   final.
7. `j`/`k` continue ordinary one-line scrolling while an active query remains
   highlighted.
8. Page/Home/End update the current line counter and do not reset the selected
   match anchor.
9. Arrow navigation uses the last selected match position as its lookahead
   anchor rather than rescanning from line zero.
10. No-result queries do not move the viewport and arrow presses remain no-ops.

### Lazy-window proof tests

Use a test matcher that records every line index passed to the regex adapter.
The test document should contain substantially more lines than the viewport,
with matches distributed at the beginning, middle, and end.

1. **Initial stop:** submit a query whose first match is on line 3. Assert that
   only lines `0..=3` were evaluated before the first draw, not the rest of the
   document.
2. **Initial lookahead:** with height `H`, after centering the first match,
   assert that only the expanded viewport interval is newly evaluated. Assert
   that the initial prefix lines are cache hits and are not evaluated again.
3. **Visible scrolling:** scroll down one line repeatedly. Assert that each
   evaluation is inside the new expanded window and that each line is evaluated
   at most once for the query.
4. **Large jump:** press End or Page Down. Assert that the new tail window is
   scanned, but unrelated lines between the old and new windows are not scanned
   merely for highlighting.
5. **Backward jump:** press Home after scanning near the end. Assert that the
   home window is scanned only where its entries are `Unscanned`, and cached
   lines receive zero matcher calls.
6. **Directional lookahead:** select a match and press Down. Assert that scans
   begin at/after the selected match position and do not repeat the prefix.
7. **Directional probe bound:** arrange for the next match to be several
   viewport heights away. Assert that each probe evaluates only its own
   expanded window; no single operation evaluates the entire intervening
   document.
8. **Wrapping:** after reaching the last match, press Down and assert that the
   wrap uses cached lines where available and scans only necessary viewport
   windows. Mirror for Up.
9. **Live growth:** submit before EOF, deliver chunks one at a time, and assert
   that newly arriving lines are evaluated once, with no re-evaluation of the
   loaded prefix.
10. **EOF resolution:** assert that a pending no-match search becomes final only
    after completion adds the trailing logical line.
11. **Invalid query:** assert zero matcher calls after a compile error.
12. **Reset:** submit query A, scan several windows, enter `/`, and submit query
    B. Assert that B starts its own line-zero scan and does not use A's cache.

The central assertion helper should verify, for every production scan request,
that either:

- it belongs to the initial prefix before the first match; or
- it is inside the current/probe expanded viewport interval.

This test is the proof that the implementation is lazy by construction, rather
than merely fast for the fixture used by a benchmark.

### Highlight rendering tests

1. A visible match emits reverse-video start/end transitions around the exact
   byte range.
2. Multiple matches on one line produce separate correctly ordered spans.
3. Matches split by existing red/green/bold/reverse styles restore the base style
   after the search overlay ends.
4. A match containing an existing ANSI transition does not leak reverse video
   beyond the match.
5. A final line reset and the following line remain unchanged by a preceding
   highlight.
6. Only visible lines are rendered; off-screen cached matches produce no output.
7. The ordinary no-highlight viewport remains byte-for-byte identical to the
   existing viewport test output.
8. A colorized input document searches and highlights the same visible range as
   an equivalent plain document.
9. Long lines are clipped by the terminal as before; search ranges do not cause
   wrapping.

### Retained/live equivalence tests

Build the same logical document through `render_document` and through
`IncrementalDocumentRenderer` chunks. Drive the same search and navigation
sequence through the shared search state. Assert equivalent:

- selected match locations;
- viewport tops;
- cache scan decisions;
- visible highlight ranges; and
- viewport output, aside from the expected loading status text while the live
  document is incomplete.

## Performance requirements and instrumentation

The implementation should be evaluated against these concrete properties:

- Regex compilation occurs once per successful submission.
- A line is passed to the regex matcher at most once per active query.
- Ordinary redraws do not invoke regex matching.
- Scrolling by one line after the first window scans only newly uncovered lines.
- Rendering scans only terminal-height visible lines, not all cached lines.
- No match list proportional to the entire document is built eagerly.
- No search worker thread or unbounded result channel is introduced.
- Query reset releases the old per-line match ranges.

Tests should use a recording matcher rather than timing thresholds. Benchmarks
may additionally record allocation count and total bytes for:

- initial query with an early match;
- initial query with a late/no match;
- one-line scrolling across a large document;
- repeated redraw without scrolling;
- repeated next/previous navigation with cached matches; and
- a live document whose query is submitted before EOF.

The benchmark should compare regex calls and allocations against document size.
A larger document with the same visited viewport history should not cause
additional regex calls outside the visited windows.

## Compatibility and output policy

Search output is interactive pager output, not a renderer golden. Adding search
must not alter:

- `render()` or `render_to()` bytes;
- checked-in files under `fixtures/oracle/`;
- the output of `--paging=never`;
- the direct-output fallback when no terminal is available; or
- the existing alternate-screen cleanup sequence.

If adding the highlight-aware writer changes the no-highlight viewport bytes,
that is a bug, not an expected fixture update. Any intentional pager status
text change should be covered by focused pager tests rather than golden
fixtures.

## Implementation order and review gates

1. Add the direct dependency and document line-text access without changing
   existing output.
2. Add the testable search cache/matcher abstraction and lazy-window tests
   first. Do not connect terminal input until the scan-bound tests pass.
3. Add regex compilation, first-character-discard input state, editing, and
   invalid-query handling.
4. Add initial search, centering, cache-backed highlights, and navigation
   against the retained `Viewer`.
5. Add the ANSI overlay compositor and its style-boundary tests.
6. Connect the same state to `LiveViewer`, including document growth and EOF
   pending behavior.
7. Add status-row modes and help text.
8. Add retained/live equivalence tests and the focused benchmark.
9. Run the nearest hard judges after each stage:
   - `cargo test` for focused pager/search tests;
   - full `cargo test` for renderer and pager regressions;
   - `make check` to prove all existing fixture outputs are unchanged; and
   - the focused benchmark before and after optimization.

Do not run pre-commit hooks and do not push a remote branch. Report any
intentional changes to pager behavior separately from renderer golden results.

## Final acceptance checklist

The feature is complete only when all of the following are true:

- `/` starts a fresh search command and Enter submits it.
- The first typed character is retained and matched, while Enter remains the
  only search trigger.
- Backslash escaping reaches regex-lite unchanged and is tested.
- Invalid regexes remain editable with a visible error.
- The first match is found from the beginning and centered.
- Search matches are highlighted without searching ANSI bytes.
- Up/Down arrows navigate previous/next matches and wrap.
- `j`/`k` still scroll normally while highlights remain active.
- Every viewport movement updates the current line counter and lazily extends
  the search cache.
- No line is regex-evaluated twice for one query.
- Regex evaluation is bounded to the initial prefix or current/probe expanded
  viewport window, with tests recording and asserting exact line indices.
- Live input can grow the search result without blocking or rescanning.
- Retained and live pagers behave equivalently.
- Existing non-pager output and all renderer goldens remain byte-for-byte
  unchanged.
- The implementation remains allocation-conscious on the no-search and
  no-match paths.
