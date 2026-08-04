use regex_lite::Regex;

use crate::render::{RenderedDocument, TextRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MatchLocation {
    pub(crate) line: usize,
    pub(crate) range_index: usize,
}

#[derive(Debug)]
enum LineScan {
    Unscanned,
    NoMatch,
    Matches(Vec<TextRange>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingSearch {
    Initial,
    Direction { direction: Direction, cursor: usize },
}

pub(crate) struct SearchInput {
    query: String,
    discard_first_character: bool,
    compile_error: Option<String>,
}

impl SearchInput {
    fn new() -> Self {
        Self {
            query: String::new(),
            discard_first_character: true,
            compile_error: None,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn compile_error(&self) -> Option<&str> {
        self.compile_error.as_deref()
    }

    pub(crate) fn push(&mut self, character: char) {
        self.compile_error = None;
        if self.discard_first_character {
            self.discard_first_character = false;
        } else {
            self.query.push(character);
        }
    }

    pub(crate) fn backspace(&mut self) {
        self.compile_error = None;
        self.query.pop();
    }

    pub(crate) fn clear(&mut self) {
        self.compile_error = None;
        self.query.clear();
    }
}

pub(crate) enum SearchState {
    Inactive,
    Input(SearchInput),
    Active(SearchSession),
}

impl SearchState {
    pub(crate) fn begin(&mut self) {
        *self = Self::Input(SearchInput::new());
    }

    pub(crate) fn cancel(&mut self) {
        *self = Self::Inactive;
    }

    pub(crate) fn input_mut(&mut self) -> Option<&mut SearchInput> {
        match self {
            Self::Input(input) => Some(input),
            _ => None,
        }
    }

    pub(crate) fn input(&self) -> Option<&SearchInput> {
        match self {
            Self::Input(input) => Some(input),
            _ => None,
        }
    }

    pub(crate) fn active(&self) -> Option<&SearchSession> {
        match self {
            Self::Active(session) => Some(session),
            _ => None,
        }
    }

    pub(crate) fn active_mut(&mut self) -> Option<&mut SearchSession> {
        match self {
            Self::Active(session) => Some(session),
            _ => None,
        }
    }

    pub(crate) fn submit(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        let Self::Input(input) = self else {
            return None;
        };
        if input.query.is_empty() {
            *self = Self::Inactive;
            return None;
        }
        let regex = match Regex::new(&input.query) {
            Ok(regex) => regex,
            Err(error) => {
                input.compile_error = Some(error.to_string());
                return None;
            }
        };
        let mut session = SearchSession::new(input.query.clone(), regex, document.line_count());
        let new_top = session.discover_initial(document, top, height, finished);
        *self = Self::Active(session);
        new_top
    }
}

pub(crate) struct SearchSession {
    query: String,
    regex: Regex,
    scans: Vec<LineScan>,
    selected: Option<MatchLocation>,
    pending: Option<PendingSearch>,
    final_no_match: bool,
    initial_next: usize,
    #[cfg(test)]
    evaluated_lines: Vec<usize>,
}

impl SearchSession {
    fn new(query: String, regex: Regex, line_count: usize) -> Self {
        Self {
            query,
            regex,
            scans: unscanned_lines(line_count).collect(),
            selected: None,
            pending: None,
            final_no_match: false,
            initial_next: 0,
            #[cfg(test)]
            evaluated_lines: Vec::new(),
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<MatchLocation> {
        self.selected
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn is_final_no_match(&self) -> bool {
        self.final_no_match
    }

    pub(crate) fn ranges(&self, line: usize) -> &[TextRange] {
        match self.scans.get(line) {
            Some(LineScan::Matches(ranges)) => ranges,
            _ => &[],
        }
    }

    pub(crate) fn ensure_window(&mut self, document: &RenderedDocument, top: usize, height: usize) {
        self.resize(document.line_count());
        let (start, end) = expanded_window(top, height, document.line_count());
        for line in start..end {
            self.scan_line(document, line);
        }
    }

    pub(crate) fn document_changed(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.resize(document.line_count());
        let selected_top = match self.pending {
            Some(PendingSearch::Initial) => self.discover_initial(document, top, height, finished),
            Some(PendingSearch::Direction {
                direction: Direction::Down,
                cursor,
            }) => self.resume_down(document, top, height, finished, cursor),
            Some(PendingSearch::Direction {
                direction: Direction::Up,
                ..
            }) if finished => self.wrap(document, height, Direction::Up),
            Some(PendingSearch::Direction { .. }) => None,
            None => None,
        };
        let window_top = selected_top.unwrap_or(top);
        self.ensure_window(document, window_top, height);
        selected_top
    }

    pub(crate) fn next(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.navigate(document, top, height, finished, Direction::Down)
    }

    pub(crate) fn previous(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.navigate(document, top, height, finished, Direction::Up)
    }

    fn discover_initial(
        &mut self,
        document: &RenderedDocument,
        old_top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.resize(document.line_count());
        while self.initial_next < document.line_count() {
            let line = self.initial_next;
            self.initial_next += 1;
            self.scan_line(document, line);
            if !self.ranges(line).is_empty() {
                self.selected = Some(MatchLocation {
                    line,
                    range_index: 0,
                });
                self.pending = None;
                self.final_no_match = false;
                let top = center_top(line, height, document.line_count());
                self.ensure_window(document, top, height);
                return Some(top);
            }
        }
        self.selected = None;
        self.final_no_match = finished;
        self.pending = (!finished).then_some(PendingSearch::Initial);
        self.ensure_window(document, old_top, height);
        None
    }

    fn navigate(
        &mut self,
        document: &RenderedDocument,
        _displayed_top: usize,
        height: usize,
        finished: bool,
        direction: Direction,
    ) -> Option<usize> {
        let Some(anchor) = self.selected else {
            return None;
        };
        self.pending = None;
        self.resize(document.line_count());

        let candidate = match direction {
            Direction::Down => self
                .next_on_line(anchor)
                .or_else(|| self.find_down(document, anchor.line.saturating_add(1), height)),
            Direction::Up => self
                .previous_on_line(anchor)
                .or_else(|| self.find_up(document, anchor.line, height)),
        };
        if let Some(location) = candidate {
            return self.select(document, location, height);
        }

        if !finished {
            self.pending = Some(PendingSearch::Direction {
                direction,
                cursor: match direction {
                    Direction::Down => document.line_count(),
                    Direction::Up => 0,
                },
            });
            return None;
        }

        self.wrap(document, height, direction)
    }

    fn find_down(
        &mut self,
        document: &RenderedDocument,
        start: usize,
        height: usize,
    ) -> Option<MatchLocation> {
        let mut cursor = start.min(document.line_count());
        let mut probe_top = center_top(cursor, height, document.line_count());
        loop {
            self.ensure_window(document, probe_top, height);
            let (_, end) = expanded_window(probe_top, height, document.line_count());
            if let Some(location) = self.first_in(cursor, end) {
                return Some(location);
            }
            if end == document.line_count() {
                return None;
            }
            cursor = end;
            let next = probe_top
                .saturating_add(height)
                .min(max_top(document.line_count(), height));
            if next == probe_top {
                return None;
            }
            probe_top = next;
        }
    }

    fn find_up(
        &mut self,
        document: &RenderedDocument,
        end: usize,
        height: usize,
    ) -> Option<MatchLocation> {
        let mut cursor = end.min(document.line_count());
        let mut probe_top = center_top(cursor.saturating_sub(1), height, document.line_count());
        loop {
            self.ensure_window(document, probe_top, height);
            let (start, _) = expanded_window(probe_top, height, document.line_count());
            if let Some(location) = self.last_in(start, cursor) {
                return Some(location);
            }
            if start == 0 {
                return None;
            }
            cursor = start;
            let next = probe_top.saturating_sub(height);
            if next == probe_top {
                return None;
            }
            probe_top = next;
        }
    }

    fn resume_down(
        &mut self,
        document: &RenderedDocument,
        displayed_top: usize,
        height: usize,
        finished: bool,
        cursor: usize,
    ) -> Option<usize> {
        self.pending = None;
        if let Some(location) = self.find_down(document, cursor, height) {
            return self.select(document, location, height);
        }
        if finished {
            self.wrap(document, height, Direction::Down)
        } else {
            self.pending = Some(PendingSearch::Direction {
                direction: Direction::Down,
                cursor: document.line_count(),
            });
            self.ensure_window(document, displayed_top, height);
            None
        }
    }

    fn wrap(
        &mut self,
        document: &RenderedDocument,
        height: usize,
        direction: Direction,
    ) -> Option<usize> {
        let location = match direction {
            Direction::Down => self.find_down(document, 0, height),
            Direction::Up => self.find_up(document, document.line_count(), height),
        };
        location.and_then(|location| self.select(document, location, height))
    }

    fn next_on_line(&self, anchor: MatchLocation) -> Option<MatchLocation> {
        if let Some(LineScan::Matches(ranges)) = self.scans.get(anchor.line)
            && anchor.range_index + 1 < ranges.len()
        {
            return Some(MatchLocation {
                line: anchor.line,
                range_index: anchor.range_index + 1,
            });
        }
        None
    }

    fn previous_on_line(&self, anchor: MatchLocation) -> Option<MatchLocation> {
        if anchor.range_index > 0 {
            return Some(MatchLocation {
                line: anchor.line,
                range_index: anchor.range_index - 1,
            });
        }
        None
    }

    fn first_in(&self, start: usize, end: usize) -> Option<MatchLocation> {
        (start..end).find_map(|line| match self.scans.get(line) {
            Some(LineScan::Matches(ranges)) if !ranges.is_empty() => Some(MatchLocation {
                line,
                range_index: 0,
            }),
            _ => None,
        })
    }

    fn last_in(&self, start: usize, end: usize) -> Option<MatchLocation> {
        (start..end)
            .rev()
            .find_map(|line| match self.scans.get(line) {
                Some(LineScan::Matches(ranges)) if !ranges.is_empty() => Some(MatchLocation {
                    line,
                    range_index: ranges.len() - 1,
                }),
                _ => None,
            })
    }

    fn select(
        &mut self,
        document: &RenderedDocument,
        location: MatchLocation,
        height: usize,
    ) -> Option<usize> {
        self.selected = Some(location);
        self.pending = None;
        let top = center_top(location.line, height, document.line_count());
        self.ensure_window(document, top, height);
        Some(top)
    }

    fn scan_line(&mut self, document: &RenderedDocument, line: usize) {
        if !matches!(self.scans.get(line), Some(LineScan::Unscanned)) {
            return;
        }
        #[cfg(test)]
        self.evaluated_lines.push(line);
        let Some(text) = document.line_text(line) else {
            return;
        };
        let ranges: Vec<_> = self
            .regex
            .find_iter(text)
            .map(|found| TextRange {
                start: found.start(),
                end: found.end(),
            })
            .collect();
        self.scans[line] = if ranges.is_empty() {
            LineScan::NoMatch
        } else {
            LineScan::Matches(ranges)
        };
    }

    fn resize(&mut self, line_count: usize) {
        if self.scans.len() < line_count {
            self.scans
                .extend(unscanned_lines(line_count - self.scans.len()));
        }
    }
}

fn unscanned_lines(count: usize) -> impl Iterator<Item = LineScan> {
    std::iter::repeat_with(|| LineScan::Unscanned).take(count)
}

fn expanded_window(top: usize, height: usize, line_count: usize) -> (usize, usize) {
    (
        top.saturating_sub(height),
        top.saturating_add(height)
            .saturating_add(height)
            .min(line_count),
    )
}

fn max_top(line_count: usize, height: usize) -> usize {
    line_count.saturating_sub(height)
}

fn center_top(line: usize, height: usize, line_count: usize) -> usize {
    line.saturating_sub(height / 2)
        .min(max_top(line_count, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(count: usize, matches: &[usize]) -> RenderedDocument {
        let mut input = String::new();
        for line in 0..count {
            if matches.contains(&line) {
                input.push_str(&format!("line {line} needle needle\n"));
            } else {
                input.push_str(&format!("line {line}\n"));
            }
        }
        crate::render::render_document(&input)
    }

    fn session(document: &RenderedDocument, height: usize) -> (SearchSession, usize) {
        let regex = Regex::new("needle").unwrap();
        let mut session = SearchSession::new("needle".into(), regex, document.line_count());
        let top = session
            .discover_initial(document, 0, height, true)
            .expect("test document has a match");
        (session, top)
    }

    #[test]
    fn initial_search_stops_then_scans_only_the_centered_window() {
        let document = lines(100, &[3, 50]);
        let (session, top) = session(&document, 5);

        assert_eq!(top, 1);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 3,
                range_index: 0
            })
        );
        assert_eq!(session.evaluated_lines, (0..11).collect::<Vec<_>>());
    }

    #[test]
    fn scrolling_scans_each_new_window_line_once() {
        let document = lines(100, &[3]);
        let (mut session, _) = session(&document, 5);
        session.ensure_window(&document, 2, 5);
        session.ensure_window(&document, 3, 5);

        assert_eq!(session.evaluated_lines, (0..13).collect::<Vec<_>>());
    }

    #[test]
    fn large_jump_does_not_scan_intervening_lines() {
        let document = lines(100, &[3]);
        let (mut session, _) = session(&document, 5);
        session.ensure_window(&document, 95, 5);

        assert_eq!(
            session.evaluated_lines,
            (0..11).chain(90..101).collect::<Vec<_>>()
        );
    }

    #[test]
    fn navigation_visits_ranges_then_lines_and_wraps() {
        let document = lines(30, &[3, 20]);
        let (mut session, mut top) = session(&document, 5);

        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 3,
                range_index: 1
            })
        );
        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 20,
                range_index: 0
            })
        );
        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 20,
                range_index: 1
            })
        );
        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 3,
                range_index: 0
            })
        );
        let _ = session.previous(&document, top, 5, true).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 20,
                range_index: 1
            })
        );
    }

    #[test]
    fn navigation_keeps_the_selected_match_as_anchor_after_scrolling() {
        let document = lines(100, &[3, 50, 90]);
        let (mut session, _) = session(&document, 5);
        session.ensure_window(&document, 95, 5);

        session.next(&document, 95, 5, true).unwrap();
        let top = session.next(&document, 95, 5, true).unwrap();
        assert_eq!(top, 48);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 50,
                range_index: 0
            })
        );
    }

    #[test]
    fn pending_initial_search_resumes_without_rescanning() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("one\ntwo\n");
        let regex = Regex::new("needle").unwrap();
        let mut session =
            SearchSession::new("needle".into(), regex, renderer.document().line_count());
        assert_eq!(
            session.discover_initial(renderer.document(), 0, 4, false),
            None
        );
        assert_eq!(session.evaluated_lines, vec![0, 1]);

        renderer.push_chunk("needle\n");
        renderer.complete();
        assert_eq!(
            session.document_changed(renderer.document(), 0, 4, true),
            Some(0)
        );
        assert_eq!(session.evaluated_lines, vec![0, 1, 2, 3]);
    }

    #[test]
    fn pending_down_resumes_at_new_lines_and_waits_for_eof_before_wrapping() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("needle\none\ntwo\n");
        let regex = Regex::new("needle").unwrap();
        let mut session =
            SearchSession::new("needle".into(), regex, renderer.document().line_count());
        let top = session
            .discover_initial(renderer.document(), 0, 3, false)
            .unwrap();
        session.next(renderer.document(), top, 3, false);
        let prefix_calls = session.evaluated_lines.clone();
        assert!(session.is_pending());

        renderer.push_chunk("three\nneedle\n");
        let selected_top = session
            .document_changed(renderer.document(), top, 3, false)
            .unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 4,
                range_index: 0
            })
        );
        assert!(session.evaluated_lines.starts_with(&prefix_calls));
        assert_unique(&session.evaluated_lines);

        session.next(renderer.document(), selected_top, 3, false);
        assert!(session.is_pending());
        renderer.complete();
        session.document_changed(renderer.document(), selected_top, 3, true);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 0,
                range_index: 0
            })
        );
        assert_unique(&session.evaluated_lines);
    }

    #[test]
    fn directional_probes_evaluate_every_line_at_most_once() {
        let document = lines(500, &[3, 497]);
        let (mut session, top) = session(&document, 7);
        session.next(&document, top, 7, true).unwrap();
        session.previous(&document, 494, 7, true).unwrap();

        assert_unique(&session.evaluated_lines);
    }

    fn assert_unique(lines: &[usize]) {
        let mut sorted = lines.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), lines.len(), "duplicate regex evaluation");
    }

    #[test]
    fn zero_width_matches_are_navigable() {
        let document = crate::render::render_document("abc\n");
        let regex = Regex::new("^").unwrap();
        let mut session = SearchSession::new("^".into(), regex, document.line_count());
        session.discover_initial(&document, 0, 2, true);

        assert_eq!(session.ranges(0), &[TextRange { start: 0, end: 0 }]);
        assert_eq!(session.next(&document, 0, 2, true), Some(0));
    }
}
