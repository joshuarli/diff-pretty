//! The delta-subset renderer: parse `git show` output and emit bytes matching
//! the oracle byte-for-byte under the hardcoded config.

use std::borrow::Cow;
use std::io::{self, BufRead, Write};

use crate::config::*;

trait RenderSink {
    fn push_str(&mut self, text: &str);
    fn push(&mut self, character: char);

    fn push_ansi(&mut self, sequence: &str) {
        self.push_str(sequence);
    }

    fn push_style(&mut self, style: Style, scratch: &mut String) {
        scratch.clear();
        style.push_prefix(scratch);
        self.push_ansi(scratch);
    }

    fn reset_style(&mut self) {
        self.push_ansi(sgr::RESET);
    }
}

struct RenderState {
    line_writer: Writer,
    minus_buf: Vec<String>,
    plus_buf: Vec<String>,
    word_diff: crate::edits::WordDiffScratch,
    pending_file: Option<FileInfo>,
}

impl RenderState {
    fn new() -> Self {
        Self {
            line_writer: Writer::with_capacity(256),
            minus_buf: Vec::new(),
            plus_buf: Vec::new(),
            word_diff: crate::edits::WordDiffScratch::new(),
            pending_file: None,
        }
    }
}

impl RenderSink for String {
    fn push_str(&mut self, text: &str) {
        String::push_str(self, text);
    }

    fn push(&mut self, character: char) {
        String::push(self, character);
    }
}

struct IoSink<'a, W> {
    output: &'a mut W,
    error: Option<io::Error>,
}

impl<'a, W: Write> IoSink<'a, W> {
    fn new(output: &'a mut W) -> Self {
        Self {
            output,
            error: None,
        }
    }

    fn finish(self) -> io::Result<()> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<W: Write> RenderSink for IoSink<'_, W> {
    fn push_str(&mut self, text: &str) {
        if self.error.is_none()
            && let Err(error) = self.output.write_all(text.as_bytes())
        {
            self.error = Some(error);
        }
    }

    fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.push_str(character.encode_utf8(&mut encoded));
    }
}

/// Incrementally renders parser-safe diff chunks or Git semantic events into
/// scrl's generic retained document.
pub struct RenderSession {
    document: scrl::DocumentBuilder,
    state: RenderState,
    event_buffer: String,
    complete: bool,
}

impl RenderSession {
    pub fn new() -> Self {
        Self {
            document: scrl::DocumentBuilder::new(),
            state: RenderState::new(),
            event_buffer: String::new(),
            complete: false,
        }
    }

    pub fn push_patch(&mut self, input: &str) -> io::Result<()> {
        if self.complete {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot push input after RenderSession::finish",
            ));
        }
        self.flush_event_buffer();
        render_chunk(input, &mut self.document, &mut self.state, false);
        Ok(())
    }

    /// Accept one Git semantic event. Git's callbacks are line-granular, while
    /// the frozen renderer needs the enclosing file header and hunk together;
    /// buffer only until the next file header, then reuse the same parser.
    pub fn push_event(&mut self, event: crate::event::DiffEvent<'_>) -> io::Result<()> {
        if self.complete {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot push event after RenderSession::finish",
            ));
        }
        if event.kind == crate::event::HEADER && !self.event_buffer.is_empty() {
            self.flush_event_buffer();
        }
        let mut fragment = String::with_capacity(event.data.len() + 8);
        crate::event::append_patch_fragment(event, &mut fragment)?;
        self.event_buffer.push_str(&fragment);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn document(&self) -> scrl::Document {
        self.document.clone().finish()
    }

    pub fn finish(mut self) -> scrl::Document {
        self.flush_event_buffer();
        self.complete();
        self.document.finish()
    }

    fn flush_event_buffer(&mut self) {
        if !self.event_buffer.is_empty() {
            let input = std::mem::take(&mut self.event_buffer);
            render_chunk(&input, &mut self.document, &mut self.state, false);
        }
    }

    fn complete(&mut self) {
        if !self.complete {
            render_chunk("", &mut self.document, &mut self.state, true);
            self.complete = true;
        }
    }
}

impl Default for RenderSession {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderSink for scrl::DocumentBuilder {
    fn push_str(&mut self, text: &str) {
        scrl::DocumentBuilder::push_str(self, text);
    }

    fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.push_str(character.encode_utf8(&mut encoded));
    }
}

/// An ANSI color, mirroring the (subset of) `nu_ansi_term` colors in play.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Red,
    Green,
    Blue,
    Magenta,
    Fixed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    /// Append this color's SGR code (after the `38;5;` / `38;2;` prefix for
    /// fixed/rgb) directly to `out` — the no-allocation form of `fg_code`.
    fn push_code(self, out: &mut String) {
        use std::fmt::Write as _;
        match self {
            Color::Red => out.push_str("31"),
            Color::Green => out.push_str("32"),
            Color::Blue => out.push_str("34"),
            Color::Magenta => out.push_str("35"),
            Color::Fixed(n) => {
                let _ = write!(out, "38;5;{n}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(out, "38;2;{r};{g};{b}");
            }
        }
    }
}

/// A delta style (subset of `nu_ansi_term::Style`). The SGR prefix orders
/// attributes bold..strikethrough then background then foreground.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: Option<Color>,
    pub bold: bool,
    pub reverse: bool,
}

impl Style {
    pub const fn plain() -> Style {
        Style {
            fg: None,
            bold: false,
            reverse: false,
        }
    }
    pub const fn color(fg: Color) -> Style {
        Style {
            fg: Some(fg),
            bold: false,
            reverse: false,
        }
    }
    pub const fn emph(fg: Color) -> Style {
        Style {
            fg: Some(fg),
            bold: true,
            reverse: true,
        }
    }
    pub fn is_plain(&self) -> bool {
        self.fg.is_none() && !self.bold && !self.reverse
    }

    /// Append the full SGR prefix for this style (nothing when plain) directly
    /// to `out`. The hot path's transition writer uses this so per-segment
    /// prefixes never build a temporary `String`.
    pub fn push_prefix(&self, out: &mut String) {
        if self.is_plain() {
            return;
        }
        out.push_str("\x1b[");
        let mut first = true;
        if self.bold {
            out.push('1');
            first = false;
        }
        if self.reverse {
            if !first {
                out.push(';');
            }
            out.push('7');
            first = false;
        }
        if let Some(fg) = self.fg {
            if !first {
                out.push(';');
            }
            fg.push_code(out);
        }
        out.push('m');
    }

    /// The full SGR prefix for this style (empty when plain).
    pub fn prefix(&self) -> String {
        let mut out = String::new();
        self.push_prefix(&mut out);
        out
    }
}

const STYLE_BLUE: Style = Style::color(Color::Blue);
const STYLE_PLAIN: Style = Style::plain();
const STYLE_ZERO: Style = Style::color(Color::Rgb(68, 68, 68));
const STYLE_MINUS_NUM: Style = Style::color(Color::Fixed(88));
const STYLE_PLUS_NUM: Style = Style::color(Color::Fixed(28));
const STYLE_MINUS: Style = Style::color(Color::Red);
const STYLE_PLUS: Style = Style::color(Color::Green);
const STYLE_MINUS_EMPH: Style = Style::emph(Color::Red);
const STYLE_PLUS_EMPH: Style = Style::emph(Color::Green);
const STYLE_WS_ERROR: Style = Style {
    fg: Some(Color::Magenta),
    bold: false,
    reverse: true,
};

pub fn blue() -> Style {
    Style::color(Color::Blue)
}
pub fn zero_num() -> Style {
    Style::color(Color::Rgb(68, 68, 68))
}
pub fn minus_num() -> Style {
    Style::color(Color::Fixed(88))
}
pub fn plus_num() -> Style {
    Style::color(Color::Fixed(28))
}
pub fn minus() -> Style {
    Style::color(Color::Red)
}
pub fn plus() -> Style {
    Style::color(Color::Green)
}
pub fn minus_emph() -> Style {
    Style::emph(Color::Red)
}
pub fn plus_emph() -> Style {
    Style::emph(Color::Green)
}

/// Whether two styles can be transitioned via extra codes (add-only) or must be
/// reset first — mirroring `nu_ansi_term::Difference::between`.
enum Difference {
    Extra(Style),
    Reset,
    Empty,
}

fn difference(first: &Style, next: &Style) -> Difference {
    if first == next {
        return Difference::Empty;
    }
    // Cannot un-set an attribute/color without a reset.
    if (first.bold && !next.bold) || (first.reverse && !next.reverse) {
        return Difference::Reset;
    }
    if first.fg.is_some() && next.fg.is_none() {
        return Difference::Reset;
    }
    let mut extra = Style::plain();
    if first.bold != next.bold {
        extra.bold = true;
    }
    if first.reverse != next.reverse {
        extra.reverse = true;
    }
    if first.fg != next.fg {
        extra.fg = next.fg;
    }
    Difference::Extra(extra)
}

/// The nu-ansi-term `AnsiStrings` renderer: write first prefix, then for each
/// window write the minimal transition (extra codes or reset + new prefix), and
/// a final reset when the last style is non-plain.
pub struct Writer {
    scratch: String,
    cur: Style,
    started: bool,
}

impl Writer {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// A writer with `out` pre-sized for roughly `cap` bytes, avoiding the
    /// per-line growth reallocations on the hunk-line hot path.
    pub fn with_capacity(cap: usize) -> Self {
        Writer {
            scratch: String::with_capacity(cap),
            cur: Style::plain(),
            started: false,
        }
    }

    /// Emit the minimal transition to `style` (first full prefix, else the
    /// add-only difference or a reset + new prefix), writing SGR codes directly
    /// into the buffer without temporary `String`s.
    fn transition(&mut self, out: &mut impl RenderSink, style: Style) {
        if !self.started {
            self.push_prefix(out, style);
            self.started = true;
        } else {
            match difference(&self.cur, &style) {
                Difference::Extra(s) => self.push_prefix(out, s),
                Difference::Reset => {
                    out.reset_style();
                    self.push_prefix(out, style);
                }
                Difference::Empty => {}
            }
        }
        self.cur = style;
    }

    fn push_prefix(&mut self, out: &mut impl RenderSink, style: Style) {
        out.push_style(style, &mut self.scratch);
    }

    fn push(&mut self, out: &mut impl RenderSink, style: Style, text: &str) {
        self.transition(out, style);
        out.push_str(text);
    }

    /// Push a line-number cell in `style`, formatted directly into the buffer
    /// (no intermediate String per cell).
    fn push_num(
        &mut self,
        out: &mut impl RenderSink,
        style: Style,
        number: Option<usize>,
        width: usize,
    ) {
        self.transition(out, style);
        self.scratch.clear();
        crate::config::push_pad_number(&mut self.scratch, number, width);
        out.push_str(&self.scratch);
    }

    /// Flush the buffered line into `out`, applying the final reset rule.
    fn flush(&mut self, out: &mut impl RenderSink) {
        if !self.cur.is_plain() {
            out.reset_style();
        }
        self.cur = Style::plain();
        self.started = false;
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}
/// Write one hunk body line: the two blue-bordered line-number cells (minus /
/// plus) and the content sections, styled and emitted directly into `out`.
///
/// `sections` are the word-diff `(is_emph, text)` runs; `base_emph` /
/// `base_plain` pick the minus or plus palette. When `ws_error` is set, the
/// trailing whitespace-only run of sections gets the whitespace-error style
/// (delta does this for plus lines only). Empty sections are skipped, matching
/// delta's `paint_line`.
///
/// `w` is a single line buffer reused across all hunk lines; `flush` clears it
/// while retaining capacity, so the per-line `String` is allocated once.
#[allow(clippy::too_many_arguments)]
fn write_hunk_line(
    out: &mut impl RenderSink,
    w: &mut Writer,
    width: usize,
    minus_style: Style,
    plus_style: Style,
    minus_n: Option<usize>,
    plus_n: Option<usize>,
    sections: &[(bool, &str)],
    base_emph: Style,
    base_plain: Style,
    ws_error: bool,
) {
    let trailing_ws_start = if ws_error {
        sections
            .iter()
            .rposition(|(_, t)| !t.trim().is_empty())
            .map(|i| i + 1)
            .unwrap_or(0)
    } else {
        sections.len()
    };
    w.push(out, STYLE_BLUE, "");
    w.push_num(out, minus_style, minus_n, width);
    w.push(out, STYLE_BLUE, "");
    w.push_num(out, plus_style, plus_n, width);
    w.push(out, STYLE_BLUE, "");
    for (i, &(emph, text)) in sections.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let style = if i >= trailing_ws_start {
            STYLE_WS_ERROR
        } else if emph {
            base_emph
        } else {
            base_plain
        };
        w.push(out, style, text);
    }
    w.flush(out);
    out.push('\n');
}

/// Write one context ("zero") hunk line: the two line-number cells (both gray)
/// and the body. Tabs are expanded inline into the line Writer (each tab to 8
/// spaces, `--tabs` default) instead of building an intermediate `Cow`, so the
/// context line never heap-allocates.
fn write_zero_line(
    out: &mut impl RenderSink,
    w: &mut Writer,
    width: usize,
    minus_n: usize,
    plus_n: usize,
    body: &str,
) {
    w.push(out, STYLE_BLUE, "");
    w.push_num(out, STYLE_ZERO, Some(minus_n), width);
    w.push(out, STYLE_BLUE, "");
    w.push_num(out, STYLE_ZERO, Some(plus_n), width);
    w.push(out, STYLE_BLUE, "");
    push_expanded_tabs(out, w, STYLE_PLAIN, body);
    w.flush(out);
    out.push('\n');
}

/// Push `s` into `w`, expanding each tab to `TAB_STOP` (8 spaces) directly in
/// the buffer. Tab-free strings are pushed in one piece.
fn push_expanded_tabs(out: &mut impl RenderSink, w: &mut Writer, style: Style, s: &str) {
    let mut parts = s.split('\t');
    if let Some(first) = parts.next() {
        w.push(out, style, first);
        for part in parts {
            w.push(out, style, TAB_STOP);
            w.push(out, style, part);
        }
    }
}
/// Render to a caller-provided sink without materializing the complete output.
pub fn render_to<W: Write>(input: &str, output: &mut W) -> io::Result<()> {
    let mut sink = IoSink::new(output);
    render_into(input, &mut sink);
    sink.finish()
}

/// Incrementally read and render input without retaining the complete input or
/// output. Whole-hunk word-diff context is preserved by splitting only before
/// commit and file boundaries.
pub fn render_reader_to<R: BufRead, W: Write>(mut input: R, output: &mut W) -> io::Result<()> {
    let mut sink = IoSink::new(output);
    let mut state = RenderState::new();
    for_each_render_chunk(&mut input, |chunk| {
        render_chunk(chunk, &mut sink, &mut state, false);
        match sink.error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })?;
    render_chunk("", &mut sink, &mut state, true);
    sink.finish()
}

/// Incrementally read into a retained document. This is the non-terminal form
/// of the live pager pipeline and avoids retaining the complete input.
pub fn render_reader_document<R: BufRead>(mut input: R) -> io::Result<scrl::Document> {
    let mut renderer = RenderSession::new();
    for_each_render_chunk(&mut input, |chunk| renderer.push_patch(chunk))?;
    Ok(renderer.finish())
}

pub(crate) fn for_each_render_chunk<R: BufRead>(
    input: &mut R,
    mut emit: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    let mut chunk = String::with_capacity(64 * 1024);
    let mut line = String::new();
    let mut stripped = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        stripped.clear();
        strip_sgr_append(line.trim_end_matches(['\r', '\n']), &mut stripped);
        let starts_file = stripped.starts_with("diff --git");
        let starts_section = starts_file || is_commit_header(&stripped);
        if starts_section && !chunk.is_empty() {
            emit(&chunk)?;
            chunk.clear();
        }
        chunk.push_str(&line);
    }
    if !chunk.is_empty() {
        emit(&chunk)?;
    }
    Ok(())
}

/// Emit incrementally rendered chunks for the interactive pager source.
/// Unlike the raw parser boundary helper, this also completes the renderer at
/// EOF so the final file decoration and pending state are not left unstyled.
pub(crate) fn for_each_rendered_chunk<R: BufRead>(
    input: &mut R,
    mut emit: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    let mut state = RenderState::new();
    let mut rendered = String::with_capacity(64 * 1024);
    for_each_render_chunk(input, |chunk| {
        rendered.clear();
        render_chunk(chunk, &mut rendered, &mut state, false);
        emit(&rendered)
    })?;
    rendered.clear();
    render_chunk("", &mut rendered, &mut state, true);
    if !rendered.is_empty() {
        emit(&rendered)?;
    }
    Ok(())
}

/// Render into the retained representation consumed directly by the pager.
pub fn render_document(input: &str) -> scrl::Document {
    let mut document = scrl::DocumentBuilder::new();
    render_into(input, &mut document);
    document.finish()
}

/// Renders a `git show` buffer to a String.
pub fn render(input: &str) -> String {
    // Reserve roughly the input size up front: output adds line-number cells
    // and SGR codes on top of the passthrough content. Tab expansion grows the
    // output by 7 bytes per tab, so those are counted in too; this removes most
    // of the geometric reallocation growth of `out`.
    let tab_count = input.as_bytes().iter().filter(|&&b| b == b'\t').count();
    let mut out = String::with_capacity(input.len() + input.len() / 2 + 7 * tab_count);
    render_into(input, &mut out);
    out
}

fn render_into(input: &str, out: &mut impl RenderSink) {
    let mut state = RenderState::new();
    render_chunk(input, out, &mut state, true);
}

fn render_chunk(
    input: &str,
    out: &mut impl RenderSink,
    state: &mut RenderState,
    final_chunk: bool,
) {
    // git colorizes the diff it sends to its pager, so the input carries CSI
    // SGR codes. We keep the raw (possibly colored) lines for passthrough
    // regions that delta reproduces verbatim (commit meta), and a stripped copy
    // for parsing; hunk lines are re-styled by us regardless of input color.
    let raw_lines: Vec<&str> = input.lines().collect();
    // The stripped copy is borrowed where possible. Plain inputs (no ESC) borrow
    // `raw_lines` directly and allocate nothing; colorized inputs are stripped
    // into a single scratch buffer, so the N per-line `String`s of the old
    // approach collapse into one allocation. `scratch` is pre-sized to the
    // input (stripping only removes bytes), so it never reallocates and the
    // recorded byte ranges stay valid.
    let mut scratch = String::new();
    let lines: Cow<'_, [&str]> = if input.as_bytes().contains(&b'\x1b') {
        scratch.reserve(input.len());
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(raw_lines.len());
        for l in &raw_lines {
            let start = scratch.len();
            strip_sgr_append(l, &mut scratch);
            ranges.push((start, scratch.len()));
        }
        Cow::Owned(
            ranges
                .iter()
                .map(|&(s, e)| &scratch[s..e])
                .collect::<Vec<&str>>(),
        )
    } else {
        Cow::Borrowed(&raw_lines)
    };
    let mut i = 0;

    // Single line buffer reused across every hunk line (flush clears it).
    let line_writer = &mut state.line_writer;

    // Buffers for a run of minus/plus lines awaiting word-diff inference,
    // hoisted to render scope so they are allocated once, not once per hunk.
    let minus_buf = &mut state.minus_buf;
    let plus_buf = &mut state.plus_buf;

    // Word-diff inference buffers, hoisted to render scope so per-hunk calls
    // reuse the per-line ranges, alignment table, and operation runs.
    let word_diff = &mut state.word_diff;

    // ---- file / hunk sections ----
    let pending_file = &mut state.pending_file;

    if lines.first().is_some_and(|line| is_commit_header(line))
        && let Some(fi) = pending_file.take()
    {
        emit_file_decoration(out, &fi, None);
    }

    // A plain unified diff (`diff -u`, no `diff --git`) starts with `---`,
    // followed by `+++` and `@@`. Detect it so we don't pass it through as
    // verbatim commit-meta, and seed the file so `---`/`+++` populate paths.
    let plain_unified = !lines.is_empty() && lines[0].starts_with("--- ");
    if plain_unified {
        *pending_file = Some(FileInfo::new_plain());
    } else {
        // ---- commit / leading verbatim block ----
        while i < lines.len() && !lines[i].starts_with("diff --git") {
            out.push_str(raw_lines[i]);
            out.push('\n');
            i += 1;
        }
    }

    while i < lines.len() {
        let line = &lines[i];

        if line.starts_with("diff --git") {
            if !final_chunk && let Some(fi) = pending_file.take() {
                emit_file_decoration(out, &fi, None);
            }
            *pending_file = Some(FileInfo::from_diff_line(line));
            i += 1;
            continue;
        }

        if line.starts_with("Binary files ") && line.ends_with(" differ") {
            // Binary file change: emit the decoration with a "(binary file)"
            // addendum and no hunk. The two paths come from this line itself
            // (binary diffs have no ---/+++ lines).
            if let Some(mut fi) = pending_file.take() {
                let inner = line
                    .trim_start_matches("Binary files ")
                    .trim_end_matches(" differ");
                if let Some((a, b)) = inner.split_once(" and ") {
                    fi.minus_file = strip_ab(a);
                    fi.plus_file = strip_ab(b);
                }
                emit_file_decoration(out, &fi, Some("binary file"));
            }
            i += 1;
            continue;
        }

        if is_commit_header(line) {
            // `git log -p` interleaves a commit header before each diff. A new
            // commit ends any in-progress file and resets to verbatim
            // commit-meta mode (delta does the same; `commit-decoration-style
            // = none` means the header is passed through undecorated).
            if let Some(fi) = pending_file.take() {
                emit_file_decoration(out, &fi, None);
            }
            out.push_str(raw_lines[i]);
            out.push('\n');
            i += 1;
            while i < lines.len() && !lines[i].starts_with("diff --git") {
                out.push_str(raw_lines[i]);
                out.push('\n');
                i += 1;
            }
            continue;
        }

        if line.starts_with("@@") {
            // A hunk header. If this is the first hunk of the file, emit the
            // file decoration first.
            if let Some(fi) = pending_file.take() {
                emit_file_decoration(out, &fi, None);
            }
            // `decorations` feature in the aligned live config sets
            // `hunk-header-style = none`: delta writes a blank line and, when
            // the `@@` line carries a code fragment, a box (bullet + fragment,
            // no line number). No box otherwise.
            out.push('\n');
            emit_hunk_box(out, line_writer, line);
            i += 1;

            // Parse hunk header for line counters.
            let (minus_start, minus_len, plus_start, plus_len) = parse_hunk_numbers(line);
            let hunk_max = minus_start
                .saturating_add(minus_len)
                .max(plus_start.saturating_add(plus_len));
            let cell_width = (4).max(num_digits(hunk_max));

            let mut minus_n = minus_start;
            let mut plus_n = plus_start;

            // Flush a buffered run of minus/plus lines (delta paints all minus
            // then all plus within a run) with word-diff inference. The buffers
            // are passed straight in (no `&str` collection), and the flat
            // section runs are walked by per-line range.
            macro_rules! flush_run {
                () => {{
                    if !minus_buf.is_empty() || !plus_buf.is_empty() {
                        let res = word_diff.infer_edits(minus_buf, plus_buf);
                        for &(start, len) in res.minus_ranges {
                            write_hunk_line(
                                out,
                                line_writer,
                                cell_width,
                                STYLE_MINUS_NUM,
                                STYLE_PLUS_NUM,
                                Some(minus_n),
                                None,
                                &res.minus_sections[start..start + len],
                                STYLE_MINUS_EMPH,
                                STYLE_MINUS,
                                false,
                            );
                            minus_n += 1;
                        }
                        for &(start, len) in res.plus_ranges {
                            // Trailing whitespace on added lines is a whitespace
                            // error, styled with the ws-error style (delta does
                            // this for plus lines only).
                            write_hunk_line(
                                out,
                                line_writer,
                                cell_width,
                                STYLE_MINUS_NUM,
                                STYLE_PLUS_NUM,
                                None,
                                Some(plus_n),
                                &res.plus_sections[start..start + len],
                                STYLE_PLUS_EMPH,
                                STYLE_PLUS,
                                true,
                            );
                            plus_n += 1;
                        }
                        minus_buf.clear();
                        plus_buf.clear();
                    }
                }};
            }

            // Hunk body lines until next @@, the next diff, or a new commit.
            while i < lines.len() {
                let l = &lines[i];
                if l.starts_with("@@") || l.starts_with("diff --git") || is_commit_header(l) {
                    break;
                }
                if let Some(body) = l.strip_prefix(' ') {
                    flush_run!();
                    write_zero_line(out, line_writer, cell_width, minus_n, plus_n, body);
                    minus_n += 1;
                    plus_n += 1;
                    i += 1;
                } else if let Some(body) = l.strip_prefix('-') {
                    minus_buf.push(expand_tabs(body).into_owned());
                    i += 1;
                } else if let Some(body) = l.strip_prefix('+') {
                    plus_buf.push(expand_tabs(body).into_owned());
                    i += 1;
                } else {
                    flush_run!();
                    out.push_str(l);
                    out.push('\n');
                    i += 1;
                }
            }
            flush_run!();
            continue;
        }

        // Header lines (index, mode, ---, +++, rename, etc.): fold into the
        // pending file if there is one (used for the file decoration), else
        // skip. They never produce output on their own.
        if let Some(fi) = pending_file.as_mut() {
            let fi2 = std::mem::replace(fi, FileInfo::empty());
            *fi = fi2.feed(line);
        }
        i += 1;
    }

    // Any file that never produced a hunk (e.g. a pure rename or a file with
    // only metadata changes) still gets its decoration at end of input.
    if final_chunk && let Some(fi) = pending_file.take() {
        emit_file_decoration(out, &fi, None);
    }
}

#[derive(Debug)]
struct FileInfo {
    minus_file: String,
    plus_file: String,
    event: FileEvent,
    /// Plain unified diff (`diff -u`, no `diff --git` header): paths are used
    /// verbatim (no `a/`/`b/` prefix stripping) and the file change is always
    /// rendered in comparing form.
    plain: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FileEvent {
    Change,
    Added,
    Removed,
    Rename,
}

impl FileInfo {
    fn empty() -> Self {
        Self {
            minus_file: String::new(),
            plus_file: String::new(),
            event: FileEvent::Change,
            plain: false,
        }
    }
    fn new_plain() -> Self {
        Self {
            plain: true,
            ..Self::empty()
        }
    }

    fn from_diff_line(line: &str) -> Self {
        // Record the diff-line paths; the authoritative minus/plus come from
        // the ---/+++ lines which we fold in later.
        Self::empty().feed(line)
    }
}

impl FileInfo {
    /// Fold in a non-hunk header line (index/mode/---/+++/rename...).
    fn feed(mut self, line: &str) -> Self {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // remember nothing here; paths come from ---/+++
            let _ = rest;
        } else if let Some(rest) = line.strip_prefix("--- ") {
            self.minus_file = if self.plain {
                strip_path_plain(rest)
            } else {
                strip_ab(rest)
            };
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            self.plus_file = if self.plain {
                strip_path_plain(rest)
            } else {
                strip_ab(rest)
            };
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            self.event = FileEvent::Added;
            let _ = rest;
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            self.event = FileEvent::Removed;
            let _ = rest;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            self.minus_file = rest.to_string();
            self.event = FileEvent::Rename;
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.plus_file = rest.to_string();
        }
        self
    }
}

fn strip_ab(s: &str) -> String {
    let s = s.trim_end_matches('\t');
    let s = s.trim_matches('"');
    for p in ["a/", "b/"] {
        if let Some(r) = s.strip_prefix(p) {
            // but keep /dev/null untouched semantics handled by caller
            return r.to_string();
        }
    }
    s.to_string()
}

/// Strip a path from a plain unified diff (`---`/`+++` with `git_diff_name =
/// false`): unquote, drop a trailing tab, and take the text before any tab.
fn strip_path_plain(s: &str) -> String {
    let s = s.trim_matches('"');
    if s == "/dev/null" {
        return s.to_string();
    }
    s.strip_suffix('\t')
        .unwrap_or(s)
        .split('\t')
        .next()
        .unwrap_or("")
        .to_string()
}

fn parse_hunk_numbers(line: &str) -> (usize, usize, usize, usize) {
    // @@ -a,b +c,d @@ ...
    let mut coords = (0, 1, 0, 1);
    let mut nums: Vec<(usize, usize)> = Vec::new();
    for part in line.split(' ') {
        let mut p = part;
        if let Some(r) = p.strip_prefix('-') {
            p = r;
        } else if let Some(r) = p.strip_prefix('+') {
            p = r;
        } else {
            continue;
        }
        let mut it = p.split(',');
        let start: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let len: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
        nums.push((start, len));
    }
    if !nums.is_empty() {
        coords.0 = nums[0].0;
        coords.1 = nums[0].1;
    }
    if nums.len() >= 2 {
        coords.2 = nums[1].0;
        coords.3 = nums[1].1;
    }
    coords
}

/// The file-decoration underline: a fixed full-width (80) run of `─`.
const BORDER_80: &str =
    "────────────────────────────────────────────────────────────────────────────────";

fn emit_file_decoration(out: &mut impl RenderSink, fi: &FileInfo, addendum: Option<&str>) {
    out.push('\n'); // delta writes a blank line before every file decoration
    out.push_str(sgr::BLUE);
    emit_file_decor(out, fi);
    if let Some(a) = addendum {
        out.push_str(" (");
        out.push_str(a);
        out.push(')');
    }
    out.push_str(sgr::RESET);
    out.push('\n');
    // underline full width
    out.push_str(sgr::BLUE);
    out.push_str(BORDER_80);
    out.push_str(sgr::RESET);
    out.push('\n');
}

/// Write the decoration label+paths directly into `out` (no intermediate
/// `String`). Paths are written verbatim; a leading non-empty label gets a
/// trailing space, matching delta's `format!("{s} ")`.
fn emit_file_decor(out: &mut impl RenderSink, fi: &FileInfo) {
    // Plain unified diffs are shown in comparing form regardless of the paths.
    if fi.plain {
        push_decor_label(out, "Δ");
        out.push_str(&fi.minus_file);
        out.push(' ');
        out.push_str(sgr::RIGHT_ARROW);
        out.push(' ');
        out.push_str(&fi.plus_file);
    } else if fi.minus_file == fi.plus_file {
        push_decor_label(out, "Δ");
        out.push_str(&fi.minus_file);
    } else if fi.plus_file == "/dev/null" {
        push_decor_label(out, "removed:");
        out.push_str(&fi.minus_file);
    } else if fi.minus_file == "/dev/null" {
        push_decor_label(out, "added:");
        out.push_str(&fi.plus_file);
    } else {
        push_decor_label(
            out,
            if fi.event == FileEvent::Rename {
                "renamed:"
            } else {
                "Δ"
            },
        );
        out.push_str(&fi.minus_file);
        out.push(' ');
        out.push_str(sgr::RIGHT_ARROW);
        out.push(' ');
        out.push_str(&fi.plus_file);
    }
}

fn push_decor_label(out: &mut impl RenderSink, s: &str) {
    if !s.is_empty() {
        out.push_str(s);
        out.push(' ');
    }
}

/// Tab expansion width (`--tabs` default 8 spaces).
const TAB_STOP: &str = "        ";

/// Replace each tab with a constant number of spaces (`--tabs` default 8),
/// matching delta's `tabs::expand`: `line.split('\t').join("        ")`. It is
/// not tab-stop alignment. Lines without a tab are returned borrowed, so the
/// common case allocates nothing.
fn expand_tabs(s: &str) -> Cow<'_, str> {
    if !s.as_bytes().contains(&b'\t') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace('\t', TAB_STOP))
}

/// Remove CSI SGR escape sequences (`ESC [ ... m`) from a line, so git's
/// colorized pager input can be parsed the same as plain input. The stripped
/// content is appended to `scratch`, a single buffer shared by every line, so
/// colorized input costs one allocation total.
fn strip_sgr_append(s: &str, scratch: &mut String) {
    if !s.as_bytes().contains(&b'\x1b') {
        scratch.push_str(s);
        return;
    }
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            scratch.push(c);
        }
    }
}

/// Emit the hunk-header box for `hunk-header-style = none` on a `@@` line that
/// carries a code fragment: `┌─ ... ┐` / `• <fragment> │` / `└─ ... ┘`, with no
/// line number. When the `@@` line has no fragment, delta draws no box (the
/// caller has already emitted the blank line).
///
/// `w` is the reused line Writer: the content line is pushed through it (no
/// per-box Writer) and the borders are pushed from the `─` constant (no
/// `repeat` String), so a box allocates nothing.
fn emit_hunk_box(out: &mut impl RenderSink, w: &mut Writer, hunk_line: &str) {
    let fragment = hunk_fragment(hunk_line);
    if fragment.is_empty() {
        return;
    }
    // content = "• " + fragment + " " (trailing space); boxed by ─/┐/│/┘.
    let box_width = fragment.chars().count() + 3;

    // top border
    out.push_str(sgr::BLUE);
    push_border(out, box_width);
    out.push_str(sgr::RESET);
    out.push_str(sgr::BLUE);
    out.push_str(sgr::DOWN_LEFT);
    out.push_str(sgr::RESET);
    out.push('\n');

    // content line: bullet (blue) + " " + fragment + " " + border
    w.push(out, STYLE_BLUE, sgr::BULLET);
    w.push(out, STYLE_PLAIN, " ");
    w.push(out, STYLE_PLAIN, fragment);
    w.push(out, STYLE_PLAIN, " ");
    w.push(out, STYLE_BLUE, sgr::VERTICAL);
    w.flush(out);
    out.push('\n');

    // bottom border
    out.push_str(sgr::BLUE);
    push_border(out, box_width);
    out.push_str(sgr::RESET);
    out.push_str(sgr::BLUE);
    out.push_str(sgr::UP_LEFT);
    out.push_str(sgr::RESET);
    out.push('\n');
}

/// Push `count` `─` characters into `out` (the constant, not a `repeat`
/// allocation).
fn push_border(out: &mut impl RenderSink, count: usize) {
    for _ in 0..count {
        out.push_str(sgr::HORIZONTAL);
    }
}

/// Extract the code fragment (everything after the final `@@`) with leading
/// whitespace preserved and trailing whitespace stripped.
fn hunk_fragment(hunk_line: &str) -> &str {
    let idx = hunk_line
        .rfind("@@")
        .map(|i| i + 2)
        .unwrap_or(hunk_line.len());
    let rest = &hunk_line[idx..];
    rest.trim_end()
}

/// Whether a (SGR-stripped) line starts a git commit header, e.g.
/// `commit 4a3ebfa4...` (possibly followed by ` (HEAD -> main)`). Mirrors
/// delta's commit_regex `^commit [0-9a-f]{7,}`.
fn is_commit_header(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("commit ") else {
        return false;
    };
    match rest.split_whitespace().next() {
        Some(hash) if hash.len() >= 7 => hash.chars().all(|c| c.is_ascii_hexdigit()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_document_exposes_completed_chunks() {
        let input = "commit 0123456\nmessage\ndiff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let mut renderer = RenderSession::new();
        let mut chunks = Vec::new();
        for_each_render_chunk(&mut input.as_bytes(), |chunk| {
            renderer.push_patch(chunk)?;
            chunks.push(renderer.document().line_count());
            Ok(())
        })
        .unwrap();
        let document = renderer.finish();
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();

        assert!(chunks.len() > 1);
        assert!(chunks[0] > 0);
        assert_eq!(output, render(input).as_bytes());
    }

    #[test]
    fn incremental_colorized_chunk_renders_diff_lines_before_eof() {
        let input = concat!(
            "\x1b[33mcommit 0123456789abcdef\x1b[m\n",
            "\x1b[1mdiff --git a/a b/a\x1b[m\n",
            "\x1b[1mindex 1111111..2222222 100644\x1b[m\n",
            "\x1b[1m--- a/a\x1b[m\n",
            "\x1b[1m+++ b/a\x1b[m\n",
            "\x1b[36m@@ -1 +1 @@\x1b[m\n",
            "-old\n",
            "+new\n",
            "\x1b[1mdiff --git a/b b/b\x1b[m\n",
        );
        let mut renderer = RenderSession::new();
        let mut first_file = None;
        for_each_render_chunk(&mut input.as_bytes(), |chunk| {
            renderer.push_patch(chunk)?;
            if chunk.contains("diff --git a/a b/a") {
                let mut snapshot = Vec::new();
                renderer.document().write_to(&mut snapshot).unwrap();
                first_file = Some(snapshot);
            }
            Ok(())
        })
        .unwrap();
        let document = renderer.document();
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();
        assert!(std::str::from_utf8(&output).unwrap().contains("Δ a"));
        assert!(
            !std::str::from_utf8(&output)
                .unwrap()
                .contains("@@ -1 +1 @@")
        );
        let first_file = std::str::from_utf8(first_file.as_deref().unwrap()).unwrap();
        assert!(first_file.contains("\x1b[31mold"));
        assert!(!first_file.contains("@@ -1 +1 @@"));
    }

    #[test]
    fn semantic_events_buffer_file_sections_for_the_existing_renderer() {
        let mut renderer = RenderSession::new();
        let events = [
            crate::event::DiffEvent::new(crate::event::HEADER, 0, b"diff --git a/a b/a\n"),
            crate::event::DiffEvent::new(crate::event::FILEPAIR_MINUS, 0, b"a/a"),
            crate::event::DiffEvent::new(crate::event::FILEPAIR_PLUS, 0, b"b/a"),
            crate::event::DiffEvent::new(crate::event::CONTEXT_FRAGINFO, 0, b"@@ -1 +1 @@\n"),
            crate::event::DiffEvent::new(crate::event::MINUS, 0, b"old\n"),
            crate::event::DiffEvent::new(crate::event::PLUS, 0, b"new\n"),
            crate::event::DiffEvent::new(crate::event::HEADER, 0, b"diff --git a/b b/b\n"),
            crate::event::DiffEvent::new(crate::event::FILEPAIR_MINUS, 0, b"a/b"),
            crate::event::DiffEvent::new(crate::event::FILEPAIR_PLUS, 0, b"b/b"),
            crate::event::DiffEvent::new(crate::event::CONTEXT_FRAGINFO, 0, b"@@ -1 +1 @@\n"),
            crate::event::DiffEvent::new(crate::event::MINUS, 0, b"left\n"),
            crate::event::DiffEvent::new(crate::event::PLUS, 0, b"right\n"),
        ];
        for event in events {
            renderer.push_event(event).unwrap();
        }
        let document = renderer.finish();
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();

        let patch = concat!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
            "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-left\n+right\n",
        );
        assert_eq!(output, render(patch).as_bytes());
    }

    #[test]
    fn interactive_chunks_are_rendered_and_flushed_at_eof() {
        let input = concat!(
            "\x1b[33mcommit 0123456789abcdef\x1b[m\n",
            "\x1b[1mdiff --git a/a b/a\x1b[m\n",
            "\x1b[1mindex 1111111..2222222 100644\x1b[m\n",
            "\x1b[1m--- a/a\x1b[m\n",
            "\x1b[1m+++ b/a\x1b[m\n",
            "\x1b[36m@@ -1 +1 @@\x1b[m\n",
            "-old\n",
            "+new\n",
        );
        let mut chunks = Vec::new();
        for_each_rendered_chunk(&mut input.as_bytes(), |chunk| {
            assert!(!chunk.contains("@@ -1 +1 @@"));
            chunks.push(chunk.to_owned());
            Ok(())
        })
        .unwrap();
        let streamed = chunks.concat();

        assert!(streamed.contains("Δ a"));
        assert!(streamed.contains("\x1b[31mold"));
        assert_eq!(streamed, render(input));
    }

    #[test]
    fn short_commit_text_is_not_a_stream_boundary() {
        assert!(!is_commit_header("commit dead"));
        assert!(is_commit_header("commit deadbee"));
    }

    #[test]
    fn incremental_renderer_flushes_metadata_only_file_before_next_commit() {
        let input = "commit 0123456\nmessage\ndiff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\ncommit 1234567\nnext\n";
        let document = render_reader_document(input.as_bytes()).unwrap();
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();

        assert_eq!(output, render(input).as_bytes());
    }

    #[test]
    fn incremental_large_log_matches_retained_rendering() {
        let mut input = String::new();
        for commit in 0..512 {
            input.push_str(&format!(
                "commit {commit:07x}\nAuthor: Synthetic <synthetic@example.test>\nDate:   Thu Jan 1 00:00:00 1970 +0000\n\n    synthetic commit {commit}\n\n"
            ));
        }
        let document = render_reader_document(input.as_bytes()).unwrap();
        let mut incremental = Vec::new();
        document.write_to(&mut incremental).unwrap();

        assert_eq!(incremental, render(&input).as_bytes());
        assert_eq!(document.line_count(), 512 * 6 + 1);
    }
}
