//! The delta-subset renderer: parse `git show` output and emit bytes matching
//! the oracle byte-for-byte under the hardcoded config.

use std::borrow::Cow;

use crate::config::*;
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
    out: String,
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
            out: String::with_capacity(cap),
            cur: Style::plain(),
            started: false,
        }
    }

    /// Emit the minimal transition to `style` (first full prefix, else the
    /// add-only difference or a reset + new prefix), writing SGR codes directly
    /// into the buffer without temporary `String`s.
    fn transition(&mut self, style: Style) {
        if !self.started {
            style.push_prefix(&mut self.out);
            self.started = true;
        } else {
            match difference(&self.cur, &style) {
                Difference::Extra(s) => s.push_prefix(&mut self.out),
                Difference::Reset => {
                    self.out.push_str(sgr::RESET);
                    style.push_prefix(&mut self.out);
                }
                Difference::Empty => {}
            }
        }
        self.cur = style;
    }

    pub fn push(&mut self, style: Style, text: &str) {
        self.transition(style);
        self.out.push_str(text);
    }

    /// Push a line-number cell in `style`, formatted directly into the buffer
    /// (no intermediate String per cell).
    pub fn push_num(&mut self, style: Style, number: Option<usize>, width: usize) {
        self.transition(style);
        crate::config::push_pad_number(&mut self.out, number, width);
    }

    /// Flush the buffered line into `out`, applying the final reset rule.
    pub fn flush(&mut self, out: &mut String) {
        out.push_str(&self.out);
        self.out.clear();
        if !self.cur.is_plain() {
            out.push_str(sgr::RESET);
        }
        self.cur = Style::plain();
        self.started = false;
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
fn write_hunk_line(
    out: &mut String,
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
    w.push(STYLE_BLUE, "");
    w.push_num(minus_style, minus_n, width);
    w.push(STYLE_BLUE, "");
    w.push_num(plus_style, plus_n, width);
    w.push(STYLE_BLUE, "");
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
        w.push(style, text);
    }
    w.flush(out);
    out.push('\n');
}

/// Renders a `git show` buffer to a String.
pub fn render(input: &str) -> String {
    // Reserve roughly the input size up front: output adds line-number cells
    // and SGR codes on top of the passthrough content, so this removes most of
    // the geometric reallocation growth of `out`.
    let mut out = String::with_capacity(input.len() + input.len() / 2);
    // git colorizes the diff it sends to its pager, so the input carries CSI
    // SGR codes. We keep the raw (possibly colored) lines for passthrough
    // regions that delta reproduces verbatim (commit meta), and a stripped copy
    // for parsing; hunk lines are re-styled by us regardless of input color.
    let raw_lines: Vec<&str> = input.lines().collect();
    // The stripped copy is borrowed where possible. Plain inputs (no ESC) are
    // borrowed straight from `input` and allocate nothing; colorized inputs are
    // stripped into a single scratch buffer, so the N per-line `String`s of the
    // old approach collapse into one allocation. `scratch` is pre-sized to the
    // input (stripping only removes bytes), so it never reallocates and the
    // recorded byte ranges stay valid.
    let mut scratch = String::new();
    let lines: Vec<&str> = if input.as_bytes().contains(&b'\x1b') {
        scratch.reserve(input.len());
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(raw_lines.len());
        for l in &raw_lines {
            let start = scratch.len();
            strip_sgr_append(l, &mut scratch);
            ranges.push((start, scratch.len()));
        }
        ranges
            .iter()
            .map(|&(s, e)| &scratch[s..e])
            .collect()
    } else {
        raw_lines.clone()
    };
    let mut i = 0;

    // Single line buffer reused across every hunk line (flush clears it).
    let mut line_writer = Writer::with_capacity(256);

    // ---- file / hunk sections ----
    let mut pending_file: Option<FileInfo> = None;

    // A plain unified diff (`diff -u`, no `diff --git`) starts with `---`,
    // followed by `+++` and `@@`. Detect it so we don't pass it through as
    // verbatim commit-meta, and seed the file so `---`/`+++` populate paths.
    let plain_unified = !lines.is_empty() && lines[0].starts_with("--- ");
    if plain_unified {
        pending_file = Some(FileInfo::new_plain());
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
            pending_file = Some(FileInfo::from_diff_line(line));
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
                match inner.split_once(" and ") {
                    Some((a, b)) => {
                        fi.minus_file = strip_ab(a);
                        fi.plus_file = strip_ab(b);
                    }
                    None => {}
                }
                emit_file_decoration(&mut out, &fi, Some("binary file"));
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
                emit_file_decoration(&mut out, &fi, None);
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
                emit_file_decoration(&mut out, &fi, None);
            }
            // `decorations` feature in the aligned live config sets
            // `hunk-header-style = none`: delta writes a blank line and, when
            // the `@@` line carries a code fragment, a box (bullet + fragment,
            // no line number). No box otherwise.
            out.push('\n');
            emit_hunk_box(&mut out, line);
            i += 1;

            // Parse hunk header for line counters.
            let (minus_start, minus_len, plus_start, plus_len) = parse_hunk_numbers(line);
            let hunk_max = minus_start
                .saturating_add(minus_len)
                .max(plus_start.saturating_add(plus_len));
            let cell_width = (4).max(num_digits(hunk_max));

            let mut minus_n = minus_start;
            let mut plus_n = plus_start;
            let mut minus_buf: Vec<Cow<str>> = Vec::new();
            let mut plus_buf: Vec<Cow<str>> = Vec::new();

            // Flush a buffered run of minus/plus lines (delta paints all minus
            // then all plus within a run) with word-diff inference.
            macro_rules! flush_run {
                () => {{
                    if !minus_buf.is_empty() || !plus_buf.is_empty() {
                        let minus_strs: Vec<&str> = minus_buf.iter().map(|s| s.as_ref()).collect();
                        let plus_strs: Vec<&str> = plus_buf.iter().map(|s| s.as_ref()).collect();
                        let res = crate::edits::infer_edits(&minus_strs, &plus_strs);
                        for sections in &res.minus_sections {
                            write_hunk_line(
                                &mut out,
                                &mut line_writer,
                                cell_width,
                                STYLE_MINUS_NUM,
                                STYLE_PLUS_NUM,
                                Some(minus_n),
                                None,
                                sections,
                                STYLE_MINUS_EMPH,
                                STYLE_MINUS,
                                false,
                            );
                            minus_n += 1;
                        }
                        for sections in &res.plus_sections {
                            // Trailing whitespace on added lines is a whitespace
                            // error, styled with the ws-error style (delta does
                            // this for plus lines only).
                            write_hunk_line(
                                &mut out,
                                &mut line_writer,
                                cell_width,
                                STYLE_MINUS_NUM,
                                STYLE_PLUS_NUM,
                                None,
                                Some(plus_n),
                                sections,
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
                    let body = expand_tabs(body);
                    write_hunk_line(
                        &mut out,
                        &mut line_writer,
                        cell_width,
                        STYLE_ZERO,
                        STYLE_ZERO,
                        Some(minus_n),
                        Some(plus_n),
                        &[(false, body.as_ref())],
                        STYLE_PLAIN,
                        STYLE_PLAIN,
                        false,
                    );
                    minus_n += 1;
                    plus_n += 1;
                    i += 1;
                } else if let Some(body) = l.strip_prefix('-') {
                    minus_buf.push(expand_tabs(body));
                    i += 1;
                } else if let Some(body) = l.strip_prefix('+') {
                    plus_buf.push(expand_tabs(body));
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
    if let Some(fi) = pending_file {
        emit_file_decoration(&mut out, &fi, None);
    }

    out
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
        Self { plain: true, ..Self::empty() }
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
    if nums.len() >= 1 {
        coords.0 = nums[0].0;
        coords.1 = nums[0].1;
    }
    if nums.len() >= 2 {
        coords.2 = nums[1].0;
        coords.3 = nums[1].1;
    }
    coords
}

fn emit_file_decoration(out: &mut String, fi: &FileInfo, addendum: Option<&str>) {
    let mut decor = file_decor(fi);
    if let Some(a) = addendum {
        decor.push_str(" (");
        decor.push_str(a);
        decor.push(')');
    }
    out.push('\n'); // delta writes a blank line before every file decoration
    out.push_str(sgr::BLUE);
    out.push_str(&decor);
    out.push_str(sgr::RESET);
    out.push('\n');
    // underline full width
    out.push_str(sgr::BLUE);
    out.push_str(&sgr::HORIZONTAL.repeat(80));
    out.push_str(sgr::RESET);
    out.push('\n');
}

fn file_decor(fi: &FileInfo) -> String {
    let label = |s: &str| {
        if s.is_empty() {
            String::new()
        } else {
            format!("{s} ")
        }
    };
    // Plain unified diffs are shown in comparing form regardless of the paths.
    if fi.plain {
        return format!(
            "{}{} {} {}",
            label("Δ"),
            fi.minus_file,
            sgr::RIGHT_ARROW,
            fi.plus_file
        );
    }
    if fi.minus_file == fi.plus_file {
        format!("{}{}", label("Δ"), fi.minus_file)
    } else if fi.plus_file == "/dev/null" {
        format!("{}{}", label("removed:"), fi.minus_file)
    } else if fi.minus_file == "/dev/null" {
        format!("{}{}", label("added:"), fi.plus_file)
    } else {
        let al = if fi.event == FileEvent::Rename {
            "renamed:"
        } else {
            "Δ"
        };
        format!(
            "{}{} {} {}",
            label(al),
            fi.minus_file,
            sgr::RIGHT_ARROW,
            fi.plus_file
        )
    }
}

/// Replace each tab with a constant number of spaces (`--tabs` default 8),
/// matching delta's `tabs::expand`: `line.split('\t').join("        ")`. It is
/// not tab-stop alignment. Lines without a tab are returned borrowed, so the
/// common case allocates nothing.
fn expand_tabs(s: &str) -> Cow<'_, str> {
    const TAB_STOP: &str = "        ";
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
fn emit_hunk_box(out: &mut String, hunk_line: &str) {
    let fragment = hunk_fragment(hunk_line);
    if fragment.is_empty() {
        return;
    }
    // content = "• " + fragment + " " (trailing space); boxed by ─/┐/│/┘.
    let content = format!("{} {}{} ", sgr::BULLET, fragment, "");
    let box_width = content.chars().count();

    // top border
    out.push_str(sgr::BLUE);
    out.push_str(&sgr::HORIZONTAL.repeat(box_width));
    out.push_str(sgr::RESET);
    out.push_str(sgr::BLUE);
    out.push_str(sgr::DOWN_LEFT);
    out.push_str(sgr::RESET);
    out.push('\n');

    // content line: bullet (blue) + " " + fragment + " " + border
    let mut w = Writer::new();
    w.push(STYLE_BLUE, sgr::BULLET);
    w.push(STYLE_PLAIN, " ");
    w.push(STYLE_PLAIN, fragment);
    w.push(STYLE_PLAIN, " ");
    w.push(STYLE_BLUE, sgr::VERTICAL);
    w.flush(out);
    out.push('\n');

    // bottom border
    out.push_str(sgr::BLUE);
    out.push_str(&sgr::HORIZONTAL.repeat(box_width));
    out.push_str(sgr::RESET);
    out.push_str(sgr::BLUE);
    out.push_str(sgr::UP_LEFT);
    out.push_str(sgr::RESET);
    out.push('\n');
}

/// Extract the code fragment (everything after the final `@@`) with leading
/// whitespace preserved and trailing whitespace stripped.
fn hunk_fragment(hunk_line: &str) -> &str {
    let idx = hunk_line.rfind("@@").map(|i| i + 2).unwrap_or(hunk_line.len());
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
        Some(hash) if !hash.is_empty() => hash.chars().all(|c| c.is_ascii_hexdigit()),
        _ => false,
    }
}
