//! Word-diff edit inference, replicated from delta's `src/edits.rs` and the
//! `align` module. Given ordered minus and plus lines it produces, for each
//! line, a sequence of (is_emph, text) sections; emph sections are the changed
//! tokens highlighted per `minus/plus-emph-style`.
//!
//! The tokenization regex (hardcoded `\w+`) and all costs/thresholds mirror the
//! oracle config.

use crate::align::{Alignment, Operation};

/// is a Unicode `\w` word char (approximation of regex `\w`, exact for ASCII).
fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Find `\w+` matches (ASCII-faithful approximation).
fn regex_word_runs(line: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if is_word_char(chars[i].1) {
            let start = chars[i].0;
            let mut end = start;
            while i < n && is_word_char(chars[i].1) {
                end = chars[i].0 + chars[i].1.len_utf8();
                i += 1;
            }
            out.push((start, end));
        } else {
            i += 1;
        }
    }
    out
}

/// Split line into tokens for alignment, replicating delta's `tokenize`.
fn tokenize<'a>(line: &'a str) -> Vec<&'a str> {
    let mut tokens = vec![""];
    let mut offset = 0;
    for (start, end) in regex_word_runs(line) {
        if offset == 0 && start > 0 {
            tokens.push("");
        }
        for t in graphemes(&line[offset..start]) {
            tokens.push(t);
        }
        tokens.push(&line[start..end]);
        offset = end;
    }
    if offset < line.len() {
        if offset == 0 {
            tokens.push("");
        }
        for t in graphemes(&line[offset..]) {
            tokens.push(t);
        }
    }
    tokens
}

/// Char-based grapheme splitter (exact for ASCII; graphemes == chars).
fn graphemes(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let end = match iter.peek() {
            Some((j, _)) => *j,
            None => s.len(),
        };
        out.push(&s[i..end]);
        let _ = c;
    }
    if s.is_empty() {
        return out;
    }
    out
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

/// Annotate a paired minus/plus line into sections of (is_emph, text).
fn annotate<'a>(
    alignment: &Alignment<'a>,
    minus_line: &'a str,
    plus_line: &'a str,
) -> (Vec<(bool, &'a str)>, Vec<(bool, &'a str)>, f64) {
    let mut annotated_minus = Vec::new();
    let mut annotated_plus = Vec::new();

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
    for (op, n) in alignment.coalesced_operations() {
        match op {
            Operation::Deletion => {
                let ms = get_section(n, &mut minus_line_offset, &mut x_offset, &alignment.x, minus_line);
                let n_d = width(ms.trim()) as f64;
                d_denom += n_d;
                d_numer += n_d;
                annotated_minus.push((true, ms));
                minus_op_prev = true;
            }
            Operation::NoOp => {
                let ms = get_section(n, &mut minus_line_offset, &mut x_offset, &alignment.x, minus_line);
                let n_d = width(ms.trim()) as f64;
                d_denom += 2.0 * n_d;
                let is_space = ms.trim().is_empty();
                let coalesce = is_space
                    && ((minus_op_prev && plus_op_prev
                        && (x_offset < alignment.x.len() - 1
                            || y_offset < alignment.y.len() - 1))
                        || (!minus_op_prev && !plus_op_prev));
                annotated_minus.push((
                    if coalesce { minus_op_prev } else { false },
                    ms,
                ));
                let ps = get_section(n, &mut plus_line_offset, &mut y_offset, &alignment.y, plus_line);
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
                let ps = get_section(n, &mut plus_line_offset, &mut y_offset, &alignment.y, plus_line);
                let n_d = width(ps.trim()) as f64;
                d_denom += n_d;
                d_numer += n_d;
                annotated_plus.push((true, ps));
                plus_op_prev = true;
            }
        }
    }
    let distance = if d_denom > 0.0 {
        d_numer / d_denom
    } else {
        0.0
    };
    (annotated_minus, annotated_plus, distance)
}

const MAX_LINE_DISTANCE: f64 = 0.6;
const MAX_LINE_DISTANCE_NAIVE: f64 = 0.0;

pub struct EditResult<'a> {
    pub minus_sections: Vec<Vec<(bool, &'a str)>>,
    pub plus_sections: Vec<Vec<(bool, &'a str)>>,
    pub alignment: Vec<(Option<usize>, Option<usize>)>,
}

/// Pair and annotate buffered minus/plus lines (order preserved).
pub fn infer_edits<'a>(minus_lines: &[&'a str], plus_lines: &[&'a str]) -> EditResult<'a> {
    let mut annotated_minus = Vec::new();
    let mut annotated_plus = Vec::new();
    let mut line_alignment = Vec::new();

    let mut plus_index = 0;

    // Word-diff alignment buffers, reused across every candidate pairing.
    let mut alignment = Alignment::new(Vec::new(), Vec::new());

    'minus_loop: for (minus_index, minus_line) in minus_lines.iter().enumerate() {
        let minus_line: &str = minus_line;
        let mut considered = 0;
        for plus_line in &plus_lines[plus_index..] {
            // Identical lines always align with distance 0.0, so they can be
            // annotated directly without running the NW table (they are the
            // first candidate, so no backtracking is affected).
            if *plus_line == minus_line {
                annotated_minus.push(vec![(false, minus_line)]);
                if let Some(content) = contents_before_trailing_whitespace(minus_line) {
                    annotated_plus
                        .push(vec![(false, content), (false, &minus_line[content.len()..])]);
                } else {
                    annotated_plus.push(vec![(false, minus_line)]);
                }
                line_alignment.push((Some(minus_index), Some(plus_index)));
                plus_index += 1;
                continue 'minus_loop;
            }
            alignment.reset(tokenize(minus_line), tokenize(plus_line));
            let (am, ap, distance) = annotate(&alignment, minus_line, plus_line);
            if (minus_lines.len() == plus_lines.len()
                && distance <= MAX_LINE_DISTANCE_NAIVE)
                || distance <= MAX_LINE_DISTANCE
            {
                for pl in &plus_lines[plus_index..(plus_index + considered)] {
                    let pl: &str = *pl;
                    annotated_plus.push(vec![(false, pl)]);
                    line_alignment.push((None, Some(plus_index)));
                    plus_index += 1;
                }
                annotated_minus.push(am);
                annotated_plus.push(ap);
                line_alignment.push((Some(minus_index), Some(plus_index)));
                plus_index += 1;
                continue 'minus_loop;
            } else {
                considered += 1;
            }
        }
        annotated_minus.push(vec![(false, minus_line)]);
        line_alignment.push((Some(minus_index), None));
    }
    for plus_line in &plus_lines[plus_index..] {
        let plus_line: &str = *plus_line;
        if let Some(content) = contents_before_trailing_whitespace(plus_line) {
            annotated_plus.push(vec![(false, content), (false, &plus_line[content.len()..])]);
        } else {
            annotated_plus.push(vec![(false, plus_line)]);
        }
        line_alignment.push((None, Some(plus_index)));
        plus_index += 1;
    }

    EditResult {
        minus_sections: annotated_minus,
        plus_sections: annotated_plus,
        alignment: line_alignment,
    }
}
