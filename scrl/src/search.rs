use regex_lite::Regex;

use crate::document::{Document, Range};

#[derive(Clone, Debug)]
pub(crate) struct SearchState {
    pub(crate) input: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) session: Option<SearchSession>,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchSession {
    matcher: Matcher,
    cache: Vec<Option<Vec<Range>>>,
    pub(crate) selected: Option<(usize, usize)>,
    pub(crate) final_no_match: bool,
}

#[derive(Clone, Debug)]
enum Matcher {
    Literal(LiteralMatcher),
    Regex(Regex),
}

#[derive(Clone, Debug)]
struct LiteralMatcher {
    needle: Vec<u8>,
    skip: [usize; 256],
}

impl LiteralMatcher {
    fn new(needle: &str) -> Self {
        let needle = needle.as_bytes().to_vec();
        let mut skip = [needle.len(); 256];
        for (index, byte) in needle
            .iter()
            .enumerate()
            .take(needle.len().saturating_sub(1))
        {
            skip[usize::from(*byte)] = needle.len() - index - 1;
        }
        Self { needle, skip }
    }

    fn find_ranges(&self, text: &str) -> Vec<Range> {
        let haystack = text.as_bytes();
        let needle_len = self.needle.len();
        if needle_len == 0 || needle_len > haystack.len() {
            return Vec::new();
        }
        let mut ranges = Vec::new();
        let mut offset = 0;
        while offset <= haystack.len() - needle_len {
            let last = offset + needle_len - 1;
            if haystack[offset..=last]
                .iter()
                .zip(&self.needle)
                .all(|(left, right)| left == right)
            {
                ranges.push(Range {
                    start: offset,
                    end: offset + needle_len,
                });
                offset += needle_len;
            } else {
                offset += self.skip[usize::from(haystack[last])].max(1);
            }
        }
        ranges
    }
}

impl Matcher {
    fn ranges(&self, text: &str) -> Vec<Range> {
        match self {
            Self::Literal(matcher) => matcher.find_ranges(text),
            Self::Regex(regex) => regex
                .find_iter(text)
                .map(|found| Range {
                    start: found.start(),
                    end: found.end(),
                })
                .collect(),
        }
    }
}

fn is_literal(query: &str) -> bool {
    !query.bytes().any(|byte| b"\\.^$*+?()[]{}|".contains(&byte))
}

impl SearchState {
    pub(crate) fn new() -> Self {
        Self {
            input: None,
            error: None,
            session: None,
        }
    }

    pub(crate) fn begin(&mut self) {
        self.input = Some(String::new());
        self.error = None;
    }

    pub(crate) fn cancel(&mut self) {
        self.input = None;
        self.error = None;
    }

    pub(crate) fn submit(&mut self, document: &Document, finished: bool) {
        let Some(query) = self.input.take() else {
            return;
        };
        if query.is_empty() {
            self.session = None;
            return;
        }
        let matcher = if is_literal(&query) {
            Matcher::Literal(LiteralMatcher::new(&query))
        } else {
            match Regex::new(&query) {
                Ok(regex) => Matcher::Regex(regex),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.input = Some(query);
                    return;
                }
            }
        };
        let mut session = SearchSession {
            matcher,
            cache: vec![None; document.line_count()],
            selected: None,
            final_no_match: false,
        };
        session.scan_until_match(document, 0);
        if session.selected.is_none() && finished && session.cache.iter().all(Option::is_some) {
            session.final_no_match = true;
        }
        self.session = Some(session);
    }
}

impl SearchSession {
    fn scan_line(&mut self, document: &Document, line: usize) -> &[Range] {
        if line >= self.cache.len() {
            self.cache.resize(line + 1, None);
        }
        if self.cache[line].is_none() {
            let ranges = document
                .line_text(line)
                .map(|text| self.matcher.ranges(text))
                .unwrap_or_default();
            self.cache[line] = Some(ranges);
        }
        self.cache[line].as_deref().unwrap_or(&[])
    }

    fn scan_until_match(&mut self, document: &Document, start: usize) {
        for line in start..document.line_count() {
            if !self.scan_line(document, line).is_empty() {
                self.selected = Some((line, 0));
                return;
            }
        }
    }

    pub(crate) fn ranges(&mut self, document: &Document, line: usize) -> &[Range] {
        self.scan_line(document, line)
    }

    pub(crate) fn ensure_cache(
        &mut self,
        document: &Document,
        top: usize,
        rows: usize,
        finished: bool,
    ) {
        let end = top.saturating_add(rows).min(document.line_count());
        for line in top..end {
            self.scan_line(document, line);
        }
        if finished && self.cache.iter().all(Option::is_some) && self.selected.is_none() {
            self.final_no_match = true;
        }
    }

    pub(crate) fn next(&mut self, document: &Document, forward: bool) -> bool {
        let Some((mut line, mut index)) = self.selected else {
            self.scan_until_match(document, 0);
            return self.selected.is_some();
        };
        let count = document.line_count();
        if count == 0 {
            return false;
        }
        for _ in 0..count {
            let line_ranges = self.scan_line(document, line).to_vec();
            if forward {
                if index + 1 < line_ranges.len() {
                    self.selected = Some((line, index + 1));
                    return true;
                }
                line = (line + 1) % count;
                index = usize::MAX;
            } else {
                if index > 0 && index != usize::MAX {
                    self.selected = Some((line, index - 1));
                    return true;
                }
                line = if line == 0 { count - 1 } else { line - 1 };
                index = self.scan_line(document, line).len();
                if index > 0 {
                    index -= 1;
                    self.selected = Some((line, index));
                    return true;
                }
            }
            if line_ranges.is_empty() && forward {
                index = usize::MAX;
            }
            let ranges = self.scan_line(document, line).len();
            if forward && ranges > 0 {
                self.selected = Some((line, 0));
                return true;
            }
        }
        false
    }

    pub(crate) fn selected_line(&self) -> Option<usize> {
        self.selected.map(|(line, _)| line)
    }
}

#[cfg(test)]
mod tests {
    use super::{LiteralMatcher, Matcher};

    #[test]
    fn literal_matcher_returns_non_overlapping_utf8_byte_ranges() {
        let matcher = LiteralMatcher::new("na");
        let ranges = matcher.find_ranges("banana");
        assert_eq!(
            ranges
                .iter()
                .map(|range| (range.start, range.end))
                .collect::<Vec<_>>(),
            [(2, 4), (4, 6)]
        );
        let ranges = matcher.find_ranges("é na");
        assert_eq!(
            ranges
                .iter()
                .map(|range| (range.start, range.end))
                .collect::<Vec<_>>(),
            [(3, 5)]
        );
    }

    #[test]
    fn literal_and_regex_matchers_agree_for_plain_queries() {
        let text = "render render_document render";
        let literal = Matcher::Literal(LiteralMatcher::new("render"));
        let regex = Matcher::Regex(regex_lite::Regex::new("render").unwrap());
        assert_eq!(literal.ranges(text), regex.ranges(text));
    }
}
