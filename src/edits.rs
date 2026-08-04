//! Word-diff edit inference, replicated from delta's `src/edits.rs` and the
//! `align` module. Given ordered minus and plus lines it produces, for each
//! line, a sequence of (is_emph, text) sections; emph sections are the changed
//! tokens highlighted per `minus/plus-emph-style`.
//!
//! The tokenization regex (hardcoded `\w+`) and all costs/thresholds mirror the
//! oracle config.

use crate::align::{Alignment, Cell, Operation};

/// is a Unicode `\w` word char (approximation of regex `\w`, exact for ASCII).
fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Split `line` into tokens for alignment, replicating delta's `tokenize`, but
/// writing into the caller-owned `tokens` buffer (cleared first). Word runs are
/// found in a single pass and gap characters are pushed directly, so nothing is
/// allocated. Token layout, matching delta exactly:
///
/// `tokens` starts as `[""]`; before the first word run an extra `""` is pushed
/// when the run doesn't start at byte 0, each gap is pushed as one token per
/// grapheme (char, for ASCII), then the word; the trailing gap (or the whole
/// line when there are no words) is appended the same way.
pub(crate) fn tokenize_into<'a>(line: &'a str, tokens: &mut Vec<&'a str>) {
    tokens.clear();
    tokens.push("");
    let len = line.len();
    let mut offset = 0usize;
    let mut i = 0usize;
    while i < len {
        let c = line[i..].chars().next().expect("line is valid UTF-8");
        let c_len = c.len_utf8();
        if is_word_char(c) {
            let start = i;
            i += c_len;
            while i < len {
                let c2 = line[i..].chars().next().expect("line is valid UTF-8");
                if is_word_char(c2) {
                    i += c2.len_utf8();
                } else {
                    break;
                }
            }
            let end = i;
            if offset == 0 && start > 0 {
                tokens.push("");
            }
            let gap = &line[offset..start];
            let mut git = gap.char_indices().peekable();
            while let Some((gi, gc)) = git.next() {
                let gend = match git.peek() {
                    Some((gj, _)) => *gj,
                    None => gap.len(),
                };
                tokens.push(&line[offset + gi..offset + gend]);
                let _ = gc;
            }
            tokens.push(&line[start..end]);
            offset = end;
        } else {
            i += c_len;
        }
    }
    if offset < len {
        if offset == 0 {
            tokens.push("");
        }
        let tail = &line[offset..];
        let mut tit = tail.char_indices().peekable();
        while let Some((gi, gc)) = tit.next() {
            let gend = match tit.peek() {
                Some((gj, _)) => *gj,
                None => tail.len(),
            };
            tokens.push(&line[offset + gi..offset + gend]);
            let _ = gc;
        }
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// Returns the content before trailing whitespace, or None if there is no
/// trailing whitespace (excluding a trailing newline, which isn't present in
/// our line model).
fn contents_before_trailing_whitespace(line: &str) -> Option<&str> {
    let content = line.trim_end();
    if !content.is_empty() && content.len() != line.len() {
        Some(content)
    } else {
        None
    }
}

/// Annotate a paired minus/plus line into sections of (is_emph, text), writing
/// into the caller-owned `annotated_minus`/`annotated_plus` buffers (cleared
/// first) so repeated candidate runs allocate nothing. Returns the change
/// distance (fraction of emphasized width) used to decide whether the pairing
/// is a match.
fn annotate<'a>(
    alignment: &Alignment<'a>,
    minus_line: &'a str,
    plus_line: &'a str,
    annotated_minus: &mut Vec<(bool, &'a str)>,
    annotated_plus: &mut Vec<(bool, &'a str)>,
    ops: &mut Vec<(Operation, usize)>,
) -> f64 {
    annotated_minus.clear();
    annotated_plus.clear();

    let (mut x_offset, mut y_offset) = (0, 0);
    let (mut minus_line_offset, mut plus_line_offset) = (0, 0);
    let (mut d_numer, mut d_denom) = (0.0, 0.0);

    let get_section = |n: usize,
                       line_offset: &mut usize,
                       substrings_offset: &mut usize,
                       substrings: &[&str],
                       line: &'a str| {
        let section_length = substrings[*substrings_offset..*substrings_offset + n]
            .iter()
            .fold(0, |acc, s| acc + s.len());
        let old_offset = *line_offset;
        *line_offset += section_length;
        *substrings_offset += n;
        &line[old_offset..*line_offset]
    };

    let (mut minus_op_prev, mut plus_op_prev) = (false, false);
    alignment.coalesced_operations_into(ops);
    for &(op, n) in ops.iter() {
        match op {
            Operation::Deletion => {
                let ms = get_section(
                    n,
                    &mut minus_line_offset,
                    &mut x_offset,
                    &alignment.x,
                    minus_line,
                );
                let n_d = width(ms.trim()) as f64;
                d_denom += n_d;
                d_numer += n_d;
                annotated_minus.push((true, ms));
                minus_op_prev = true;
            }
            Operation::NoOp => {
                let ms = get_section(
                    n,
                    &mut minus_line_offset,
                    &mut x_offset,
                    &alignment.x,
                    minus_line,
                );
                let n_d = width(ms.trim()) as f64;
                d_denom += 2.0 * n_d;
                let is_space = ms.trim().is_empty();
                let coalesce = is_space
                    && ((minus_op_prev
                        && plus_op_prev
                        && (x_offset < alignment.x.len() - 1 || y_offset < alignment.y.len() - 1))
                        || (!minus_op_prev && !plus_op_prev));
                annotated_minus.push((if coalesce { minus_op_prev } else { false }, ms));
                let ps = get_section(
                    n,
                    &mut plus_line_offset,
                    &mut y_offset,
                    &alignment.y,
                    plus_line,
                );
                let op = if coalesce { plus_op_prev } else { false };
                if let Some(non_ws) = contents_before_trailing_whitespace(ps) {
                    annotated_plus.push((op, non_ws));
                    annotated_plus.push((op, &ps[non_ws.len()..]));
                } else {
                    annotated_plus.push((op, ps));
                }
                minus_op_prev = false;
                plus_op_prev = false;
            }
            Operation::Insertion => {
                let ps = get_section(
                    n,
                    &mut plus_line_offset,
                    &mut y_offset,
                    &alignment.y,
                    plus_line,
                );
                let n_d = width(ps.trim()) as f64;
                d_denom += n_d;
                d_numer += n_d;
                annotated_plus.push((true, ps));
                plus_op_prev = true;
            }
        }
    }
    if d_denom > 0.0 {
        d_numer / d_denom
    } else {
        0.0
    }
}

const MAX_LINE_DISTANCE: f64 = 0.6;
const MAX_LINE_DISTANCE_NAIVE: f64 = 0.0;

pub struct EditResult<'a> {
    /// All minus-line sections flattened into one run.
    pub minus_sections: Vec<(bool, &'a str)>,
    /// All plus-line sections flattened into one run.
    pub plus_sections: Vec<(bool, &'a str)>,
    /// Per minus line, the `(start, len)` window into `minus_sections`.
    pub minus_ranges: Vec<(usize, usize)>,
    /// Per plus line, the `(start, len)` window into `plus_sections`.
    pub plus_ranges: Vec<(usize, usize)>,
    pub alignment: Vec<(Option<usize>, Option<usize>)>,
}

/// Result of a `WordDiffScratch::infer_edits` call: the flat section runs are
/// owned (the caller consumes them), while the per-line ranges borrow the
/// scratch and stay valid until the scratch is next reused.
pub struct InferOut<'h, 's> {
    /// All minus-line sections flattened into one run.
    pub minus_sections: Vec<(bool, &'h str)>,
    /// All plus-line sections flattened into one run.
    pub plus_sections: Vec<(bool, &'h str)>,
    /// Per minus line, the `(start, len)` window into `minus_sections`.
    pub minus_ranges: &'s [(usize, usize)],
    /// Per plus line, the `(start, len)` window into `plus_sections`.
    pub plus_ranges: &'s [(usize, usize)],
}

/// Reusable word-diff buffers, hoisted to the caller's scope so per-hunk
/// `infer_edits` calls allocate only the two flat section runs (consumed by
/// value). Everything else — per-line ranges, line alignment, operation runs,
/// and the NW table — is cleared and reused across calls, and the reused
/// buffers are pre-sized from the hunk's known line counts.
pub struct WordDiffScratch {
    minus_ranges: Vec<(usize, usize)>,
    plus_ranges: Vec<(usize, usize)>,
    line_alignment: Vec<(Option<usize>, Option<usize>)>,
    ops: Vec<(Operation, usize)>,
    cells: Vec<Cell>,
}

impl Default for WordDiffScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl WordDiffScratch {
    pub fn new() -> Self {
        Self {
            minus_ranges: Vec::new(),
            plus_ranges: Vec::new(),
            line_alignment: Vec::new(),
            ops: Vec::new(),
            cells: Vec::new(),
        }
    }

    /// Pair and annotate buffered minus/plus lines (order preserved), reusing
    /// the scratch buffers. See [`infer_edits`] for the pairing semantics.
    ///
    /// Generic over any `S: AsRef<str>` element so callers pass `&[&str]`,
    /// `&[String]`, or `&[Cow<str>]` directly — no intermediate `&str` slice is
    /// collected. The section runs are stored flat with per-line `(start, len)`
    /// ranges, so no per-line `Vec` is heap-allocated.
    pub fn infer_edits<'h, 's, S: AsRef<str>>(
        &'s mut self,
        minus_lines: &'h [S],
        plus_lines: &'h [S],
    ) -> InferOut<'h, 's> {
        self.minus_ranges.clear();
        self.plus_ranges.clear();
        self.line_alignment.clear();
        self.ops.clear();

        let mut annotated_minus: Vec<(bool, &'h str)> = Vec::new();
        let mut annotated_plus: Vec<(bool, &'h str)> = Vec::new();
        annotated_minus.reserve(minus_lines.len() * 2);
        annotated_plus.reserve(plus_lines.len() * 2);
        self.minus_ranges.reserve(minus_lines.len());
        self.plus_ranges.reserve(plus_lines.len());
        self.line_alignment
            .reserve(minus_lines.len() + plus_lines.len());

        let mut plus_index = 0;

        // Word-diff alignment and annotation buffers, reused across every
        // candidate pairing so failed candidates allocate nothing. The NW table
        // allocation is carried across hunks via the scratch's `cells`.
        let mut alignment =
            Alignment::with_cells(Vec::new(), Vec::new(), std::mem::take(&mut self.cells));
        let mut am: Vec<(bool, &'h str)> = Vec::new();
        let mut ap: Vec<(bool, &'h str)> = Vec::new();
        am.reserve(8);
        ap.reserve(8);
        self.ops.reserve(8);

        'minus_loop: for (minus_index, minus_line) in minus_lines.iter().enumerate() {
            let minus_line: &'h str = minus_line.as_ref();
            let mut considered = 0;
            for plus_line in &plus_lines[plus_index..] {
                let plus_line: &'h str = plus_line.as_ref();
                // Identical lines always align with distance 0.0, so they can be
                // annotated directly without running the NW table (they are the
                // first candidate, so no backtracking is affected).
                if plus_line == minus_line {
                    let start = annotated_minus.len();
                    annotated_minus.push((false, minus_line));
                    self.minus_ranges.push((start, 1));
                    let start = annotated_plus.len();
                    if let Some(content) = contents_before_trailing_whitespace(minus_line) {
                        annotated_plus.push((false, content));
                        annotated_plus.push((false, &minus_line[content.len()..]));
                        self.plus_ranges.push((start, 2));
                    } else {
                        annotated_plus.push((false, minus_line));
                        self.plus_ranges.push((start, 1));
                    }
                    self.line_alignment
                        .push((Some(minus_index), Some(plus_index)));
                    plus_index += 1;
                    continue 'minus_loop;
                }
                alignment.reset_lines(minus_line, plus_line);
                let distance = annotate(
                    &alignment,
                    minus_line,
                    plus_line,
                    &mut am,
                    &mut ap,
                    &mut self.ops,
                );
                if (minus_lines.len() == plus_lines.len() && distance <= MAX_LINE_DISTANCE_NAIVE)
                    || distance <= MAX_LINE_DISTANCE
                {
                    for pl in &plus_lines[plus_index..(plus_index + considered)] {
                        let pl: &'h str = pl.as_ref();
                        let start = annotated_plus.len();
                        annotated_plus.push((false, pl));
                        self.plus_ranges.push((start, 1));
                        self.line_alignment.push((None, Some(plus_index)));
                        plus_index += 1;
                    }
                    let start = annotated_minus.len();
                    annotated_minus.extend_from_slice(&am);
                    self.minus_ranges.push((start, am.len()));
                    let start = annotated_plus.len();
                    annotated_plus.extend_from_slice(&ap);
                    self.plus_ranges.push((start, ap.len()));
                    self.line_alignment
                        .push((Some(minus_index), Some(plus_index)));
                    plus_index += 1;
                    continue 'minus_loop;
                } else {
                    considered += 1;
                }
            }
            let start = annotated_minus.len();
            annotated_minus.push((false, minus_line));
            self.minus_ranges.push((start, 1));
            self.line_alignment.push((Some(minus_index), None));
        }
        for plus_line in &plus_lines[plus_index..] {
            let plus_line: &'h str = plus_line.as_ref();
            let start = annotated_plus.len();
            if let Some(content) = contents_before_trailing_whitespace(plus_line) {
                annotated_plus.push((false, content));
                annotated_plus.push((false, &plus_line[content.len()..]));
                self.plus_ranges.push((start, 2));
            } else {
                annotated_plus.push((false, plus_line));
                self.plus_ranges.push((start, 1));
            }
            self.line_alignment.push((None, Some(plus_index)));
            plus_index += 1;
        }

        self.cells = alignment.take_cells();

        InferOut {
            minus_sections: annotated_minus,
            plus_sections: annotated_plus,
            minus_ranges: &self.minus_ranges,
            plus_ranges: &self.plus_ranges,
        }
    }
}

/// Pair and annotate buffered minus/plus lines (order preserved) for callers
/// that process a single buffer at a time. The renderer reuses a
/// [`WordDiffScratch`] across hunks instead; this standalone form stays lean
/// (no scratch is built) so single-shot callers do not allocate a scratch.
///
/// This mirrors `WordDiffScratch::infer_edits` byte-for-byte; the
/// `edits_consistent_with_scratch` test locks the two paths together.
///
/// Generic over any `S: AsRef<str>` element so callers pass `&[&str]`,
/// `&[String]`, or `&[Cow<str>]` directly — no intermediate `&str` slice is
/// collected. The section runs are stored flat (one `Vec` total) with
/// per-line `(start, len)` ranges, so no per-line `Vec` is heap-allocated.
pub fn infer_edits<'a, S: AsRef<str>>(minus_lines: &'a [S], plus_lines: &'a [S]) -> EditResult<'a> {
    let mut annotated_minus: Vec<(bool, &'a str)> = Vec::new();
    let mut annotated_plus: Vec<(bool, &'a str)> = Vec::new();
    let mut minus_ranges: Vec<(usize, usize)> = Vec::new();
    let mut plus_ranges: Vec<(usize, usize)> = Vec::new();
    let mut line_alignment = Vec::new();

    let mut plus_index = 0;

    // Word-diff alignment and annotation buffers, reused across every candidate
    // pairing so failed candidates allocate nothing.
    let mut alignment = Alignment::new(Vec::new(), Vec::new());
    let mut am: Vec<(bool, &'a str)> = Vec::new();
    let mut ap: Vec<(bool, &'a str)> = Vec::new();
    let mut ops: Vec<(Operation, usize)> = Vec::new();

    'minus_loop: for (minus_index, minus_line) in minus_lines.iter().enumerate() {
        let minus_line: &'a str = minus_line.as_ref();
        let mut considered = 0;
        for plus_line in &plus_lines[plus_index..] {
            let plus_line: &'a str = plus_line.as_ref();
            // Identical lines always align with distance 0.0, so they can be
            // annotated directly without running the NW table (they are the
            // first candidate, so no backtracking is affected).
            if plus_line == minus_line {
                let start = annotated_minus.len();
                annotated_minus.push((false, minus_line));
                minus_ranges.push((start, 1));
                let start = annotated_plus.len();
                if let Some(content) = contents_before_trailing_whitespace(minus_line) {
                    annotated_plus.push((false, content));
                    annotated_plus.push((false, &minus_line[content.len()..]));
                    plus_ranges.push((start, 2));
                } else {
                    annotated_plus.push((false, minus_line));
                    plus_ranges.push((start, 1));
                }
                line_alignment.push((Some(minus_index), Some(plus_index)));
                plus_index += 1;
                continue 'minus_loop;
            }
            alignment.reset_lines(minus_line, plus_line);
            let distance = annotate(
                &alignment, minus_line, plus_line, &mut am, &mut ap, &mut ops,
            );
            if (minus_lines.len() == plus_lines.len() && distance <= MAX_LINE_DISTANCE_NAIVE)
                || distance <= MAX_LINE_DISTANCE
            {
                for pl in &plus_lines[plus_index..(plus_index + considered)] {
                    let pl: &'a str = pl.as_ref();
                    let start = annotated_plus.len();
                    annotated_plus.push((false, pl));
                    plus_ranges.push((start, 1));
                    line_alignment.push((None, Some(plus_index)));
                    plus_index += 1;
                }
                let start = annotated_minus.len();
                annotated_minus.extend_from_slice(&am);
                minus_ranges.push((start, am.len()));
                let start = annotated_plus.len();
                annotated_plus.extend_from_slice(&ap);
                plus_ranges.push((start, ap.len()));
                line_alignment.push((Some(minus_index), Some(plus_index)));
                plus_index += 1;
                continue 'minus_loop;
            } else {
                considered += 1;
            }
        }
        let start = annotated_minus.len();
        annotated_minus.push((false, minus_line));
        minus_ranges.push((start, 1));
        line_alignment.push((Some(minus_index), None));
    }
    for plus_line in &plus_lines[plus_index..] {
        let plus_line: &'a str = plus_line.as_ref();
        let start = annotated_plus.len();
        if let Some(content) = contents_before_trailing_whitespace(plus_line) {
            annotated_plus.push((false, content));
            annotated_plus.push((false, &plus_line[content.len()..]));
            plus_ranges.push((start, 2));
        } else {
            annotated_plus.push((false, plus_line));
            plus_ranges.push((start, 1));
        }
        line_alignment.push((None, Some(plus_index)));
        plus_index += 1;
    }

    EditResult {
        minus_sections: annotated_minus,
        plus_sections: annotated_plus,
        minus_ranges,
        plus_ranges,
        alignment: line_alignment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> (Vec<&'static str>, Vec<&'static str>) {
        (
            vec![
                "let x = compute();",
                "let y = 42;",
                "  indented line",
                "fn old(a: i32) -> i32 { a + 1 }",
                "keep me",
                "delete me",
            ],
            vec![
                "let x = compute_fast();",
                "  indented line",
                "fn new(a: i32) -> i32 { a * 2 }",
                "keep me",
                "inserted line",
            ],
        )
    }

    #[test]
    fn edits_consistent_with_scratch() {
        // The single-shot `infer_edits` and the renderer's reused scratch must
        // produce identical pairing results; this locks the two paths together.
        let (minus, plus) = corpus();
        let direct = infer_edits(&minus, &plus);

        let mut scratch = WordDiffScratch::new();
        let InferOut {
            minus_sections,
            plus_sections,
            minus_ranges,
            plus_ranges,
        } = scratch.infer_edits(&minus, &plus);
        let via_scratch = EditResult {
            minus_sections,
            plus_sections,
            minus_ranges: minus_ranges.to_vec(),
            plus_ranges: plus_ranges.to_vec(),
            alignment: std::mem::take(&mut scratch.line_alignment),
        };

        assert_eq!(direct.minus_sections, via_scratch.minus_sections);
        assert_eq!(direct.plus_sections, via_scratch.plus_sections);
        assert_eq!(direct.minus_ranges, via_scratch.minus_ranges);
        assert_eq!(direct.plus_ranges, via_scratch.plus_ranges);
        assert_eq!(direct.alignment, via_scratch.alignment);
    }
}
