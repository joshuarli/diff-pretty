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
enum ProbeKind {
    Highlight,
    Down,
    DownWrap,
    Up,
    UpWrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectionPhase {
    Down,
    DownWrap,
    Up,
    UpWrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectionalSearch {
    direction: Direction,
    phase: DirectionPhase,
    cursor: usize,
    probe_top: usize,
    original_top: usize,
    wrap_candidate: Option<MatchLocation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingSearch {
    Initial { next: usize, original_top: usize },
    Direction(DirectionalSearch),
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanReason {
    Initial,
    Window {
        kind: ProbeKind,
        top: usize,
        start: usize,
        end: usize,
        line_count: usize,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScanEvaluation {
    line: usize,
    reason: ScanReason,
}

pub(crate) struct SearchInput {
    query: String,
    display_prefix: Option<char>,
    discard_first_character: bool,
    compile_error: Option<String>,
}

impl SearchInput {
    fn new() -> Self {
        Self {
            query: String::new(),
            display_prefix: None,
            discard_first_character: true,
            compile_error: None,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn display_prefix(&self) -> Option<char> {
        self.display_prefix
    }

    pub(crate) fn compile_error(&self) -> Option<&str> {
        self.compile_error.as_deref()
    }

    pub(crate) fn push(&mut self, character: char) {
        self.compile_error = None;
        if self.discard_first_character {
            self.display_prefix = Some(character);
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
        let new_top = session.start_initial(document, top, height, finished);
        *self = Self::Active(session);
        new_top
    }
}

trait SearchMatcher {
    fn find_ranges(&mut self, line: usize, text: &str, reason: ScanReasonValue) -> Vec<TextRange>;

    #[cfg(test)]
    fn evaluations(&self) -> &[ScanEvaluation];
}

#[cfg(not(test))]
struct RegexMatcher {
    regex: Regex,
}

#[cfg(not(test))]
impl SearchMatcher for RegexMatcher {
    fn find_ranges(&mut self, _line: usize, text: &str, reason: ScanReasonValue) -> Vec<TextRange> {
        match reason {
            ScanReasonValue::Initial => {}
            ScanReasonValue::Window {
                kind,
                top,
                start,
                end,
                line_count,
            } => {
                let _ = (kind, top, start, end, line_count);
            }
        }
        regex_ranges(&self.regex, text)
    }

    #[cfg(test)]
    pub(crate) fn evaluations(&self) -> &[ScanEvaluation] {
        &[]
    }
}

#[cfg(test)]
struct RecordingMatcher {
    regex: Regex,
    evaluations: Vec<ScanEvaluation>,
}

#[cfg(test)]
impl SearchMatcher for RecordingMatcher {
    fn find_ranges(&mut self, line: usize, text: &str, reason: ScanReasonValue) -> Vec<TextRange> {
        self.evaluations.push(ScanEvaluation {
            line,
            reason: reason.into(),
        });
        regex_ranges(&self.regex, text)
    }

    fn evaluations(&self) -> &[ScanEvaluation] {
        &self.evaluations
    }
}

fn regex_ranges(regex: &Regex, text: &str) -> Vec<TextRange> {
    regex
        .find_iter(text)
        .map(|found| TextRange {
            start: found.start(),
            end: found.end(),
        })
        .collect()
}

pub(crate) struct SearchSession {
    query: String,
    matcher: Box<dyn SearchMatcher>,
    scans: Vec<LineScan>,
    selected: Option<MatchLocation>,
    pending: Option<PendingSearch>,
    final_no_match: bool,
    display_revision: u64,
}

impl SearchSession {
    fn new(query: String, regex: Regex, line_count: usize) -> Self {
        #[cfg(not(test))]
        let matcher: Box<dyn SearchMatcher> = Box::new(RegexMatcher { regex });
        #[cfg(test)]
        let matcher: Box<dyn SearchMatcher> = Box::new(RecordingMatcher {
            regex,
            evaluations: Vec::new(),
        });
        Self::with_matcher(query, matcher, line_count)
    }

    fn with_matcher(query: String, matcher: Box<dyn SearchMatcher>, line_count: usize) -> Self {
        Self {
            query,
            matcher,
            scans: unscanned_lines(line_count).collect(),
            selected: None,
            pending: None,
            final_no_match: false,
            display_revision: 0,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<MatchLocation> {
        self.selected
    }

    #[cfg(test)]
    pub(crate) fn evaluations(&self) -> &[ScanEvaluation] {
        self.matcher.evaluations()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn is_final_no_match(&self) -> bool {
        self.final_no_match
    }

    pub(crate) fn ranges(&self, line: usize) -> &[TextRange] {
        if self.selected.is_none() {
            return &[];
        }
        self.line_ranges(line)
    }

    fn line_ranges(&self, line: usize) -> &[TextRange] {
        match self.scans.get(line) {
            Some(LineScan::Matches(ranges)) => ranges,
            _ => &[],
        }
    }

    pub(crate) fn display_revision(&self) -> u64 {
        self.display_revision
    }

    pub(crate) fn ensure_window(&mut self, document: &RenderedDocument, top: usize, height: usize) {
        self.scan_window(document, top, height, ProbeKind::Highlight);
    }

    pub(crate) fn document_changed(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.resize(document.line_count());
        self.advance_pending(document, top, height, finished)
    }

    pub(crate) fn advance_pending(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.resize(document.line_count());
        match self.pending {
            Some(PendingSearch::Initial { next, original_top }) => {
                self.advance_initial(document, next, original_top, height, finished)
            }
            Some(PendingSearch::Direction(search)) => {
                self.advance_direction(document, search, height, finished)
            }
            None => {
                self.ensure_window(document, top, height);
                None
            }
        }
    }

    pub(crate) fn next(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.start_direction(document, top, height, finished, Direction::Down)
    }

    pub(crate) fn previous(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.start_direction(document, top, height, finished, Direction::Up)
    }

    fn start_initial(
        &mut self,
        document: &RenderedDocument,
        original_top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        self.pending = Some(PendingSearch::Initial {
            next: 0,
            original_top,
        });
        self.advance_pending(document, original_top, height, finished)
    }

    fn advance_initial(
        &mut self,
        document: &RenderedDocument,
        next: usize,
        original_top: usize,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        let end = next.saturating_add(height).min(document.line_count());
        for line in next..end {
            self.scan_line(document, line, ScanReasonValue::Initial);
            if !self.line_ranges(line).is_empty() {
                return self.select(
                    document,
                    MatchLocation {
                        line,
                        range_index: 0,
                    },
                    height,
                );
            }
        }
        if end < document.line_count() || !finished {
            self.pending = Some(PendingSearch::Initial {
                next: end,
                original_top,
            });
        } else {
            self.pending = None;
            self.selected = None;
            self.final_no_match = true;
            self.display_revision = self.display_revision.wrapping_add(1);
        }
        None
    }

    fn start_direction(
        &mut self,
        document: &RenderedDocument,
        original_top: usize,
        height: usize,
        finished: bool,
        direction: Direction,
    ) -> Option<usize> {
        let anchor = self.selected?;
        self.pending = None;
        self.resize(document.line_count());
        let same_line = match direction {
            Direction::Down => self.next_on_line(anchor),
            Direction::Up => self.previous_on_line(anchor),
        };
        if let Some(location) = same_line {
            return self.select(document, location, height);
        }

        let search = match direction {
            Direction::Down => DirectionalSearch {
                direction,
                phase: DirectionPhase::Down,
                cursor: anchor.line.saturating_add(1),
                probe_top: center_top(anchor.line, height, document.line_count()),
                original_top,
                wrap_candidate: None,
            },
            Direction::Up => DirectionalSearch {
                direction,
                phase: DirectionPhase::Up,
                cursor: anchor.line,
                probe_top: center_top(anchor.line, height, document.line_count()),
                original_top,
                wrap_candidate: None,
            },
        };
        self.pending = Some(PendingSearch::Direction(search));
        self.advance_direction(document, search, height, finished)
    }

    fn advance_direction(
        &mut self,
        document: &RenderedDocument,
        mut search: DirectionalSearch,
        height: usize,
        finished: bool,
    ) -> Option<usize> {
        let line_count = document.line_count();
        if !finished
            && matches!(search.phase, DirectionPhase::Down | DirectionPhase::UpWrap)
            && search.cursor >= line_count
        {
            self.pending = Some(PendingSearch::Direction(search));
            return None;
        }
        let kind = match search.phase {
            DirectionPhase::Down => ProbeKind::Down,
            DirectionPhase::DownWrap => ProbeKind::DownWrap,
            DirectionPhase::Up => ProbeKind::Up,
            DirectionPhase::UpWrap => ProbeKind::UpWrap,
        };
        let (start, end) = self.scan_window(document, search.probe_top, height, kind);

        match search.phase {
            DirectionPhase::Down | DirectionPhase::DownWrap => {
                if let Some(location) = self.first_in(search.cursor.max(start), end) {
                    return self.select(document, location, height);
                }
                search.cursor = end;
                if end < line_count {
                    search.probe_top = next_probe_top(search.probe_top, height, line_count);
                } else if search.phase == DirectionPhase::Down && !finished {
                    search.cursor = line_count;
                } else if search.phase == DirectionPhase::Down {
                    search.phase = DirectionPhase::DownWrap;
                    search.cursor = 0;
                    search.probe_top = 0;
                } else {
                    self.pending = None;
                    return None;
                }
            }
            DirectionPhase::Up => {
                if let Some(location) = self.last_in(start, search.cursor.min(end)) {
                    return self.select(document, location, height);
                }
                search.cursor = start;
                if start > 0 {
                    search.probe_top = search.probe_top.saturating_sub(height);
                } else {
                    search.phase = DirectionPhase::UpWrap;
                    search.cursor = 0;
                    search.probe_top = 0;
                    search.wrap_candidate = None;
                }
            }
            DirectionPhase::UpWrap => {
                if let Some(location) = self.last_in(search.cursor.max(start), end) {
                    search.wrap_candidate = Some(location);
                }
                search.cursor = end;
                if end < line_count {
                    search.probe_top = next_probe_top(search.probe_top, height, line_count);
                } else if finished {
                    self.pending = None;
                    return search
                        .wrap_candidate
                        .and_then(|location| self.select(document, location, height));
                }
            }
        }

        self.pending = Some(PendingSearch::Direction(search));
        None
    }

    fn scan_window(
        &mut self,
        document: &RenderedDocument,
        top: usize,
        height: usize,
        kind: ProbeKind,
    ) -> (usize, usize) {
        self.resize(document.line_count());
        let (start, end) = expanded_window(top, height, document.line_count());
        let reason = ScanReasonValue::Window {
            kind,
            top,
            start,
            end,
            line_count: document.line_count(),
        };
        for line in start..end {
            self.scan_line(document, line, reason);
        }
        (start, end)
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
        (anchor.range_index > 0).then_some(MatchLocation {
            line: anchor.line,
            range_index: anchor.range_index.saturating_sub(1),
        })
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
        self.final_no_match = false;
        self.display_revision = self.display_revision.wrapping_add(1);
        let top = center_top(location.line, height, document.line_count());
        self.ensure_window(document, top, height);
        Some(top)
    }

    fn scan_line(&mut self, document: &RenderedDocument, line: usize, reason: ScanReasonValue) {
        if !matches!(self.scans.get(line), Some(LineScan::Unscanned)) {
            return;
        }
        let Some(text) = document.line_text(line) else {
            return;
        };
        let ranges = self.matcher.find_ranges(line, text, reason);
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

#[derive(Clone, Copy)]
enum ScanReasonValue {
    Initial,
    Window {
        kind: ProbeKind,
        top: usize,
        start: usize,
        end: usize,
        line_count: usize,
    },
}

#[cfg(test)]
impl From<ScanReasonValue> for ScanReason {
    fn from(reason: ScanReasonValue) -> Self {
        match reason {
            ScanReasonValue::Initial => Self::Initial,
            ScanReasonValue::Window {
                kind,
                top,
                start,
                end,
                line_count,
            } => Self::Window {
                kind,
                top,
                start,
                end,
                line_count,
            },
        }
    }
}

fn unscanned_lines(count: usize) -> impl Iterator<Item = LineScan> {
    std::iter::repeat_with(|| LineScan::Unscanned).take(count)
}

fn expanded_window(top: usize, height: usize, line_count: usize) -> (usize, usize) {
    debug_assert!(height > 0);
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

fn next_probe_top(top: usize, height: usize, line_count: usize) -> usize {
    top.saturating_add(height).min(max_top(line_count, height))
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
        let mut top = session.start_initial(document, 0, height, true);
        while session.is_pending() && top.is_none() {
            top = session.advance_pending(document, 0, height, true);
        }
        let top = top.expect("test document has a match");
        (session, top)
    }

    fn finish_pending(
        session: &mut SearchSession,
        document: &RenderedDocument,
        top: usize,
        height: usize,
    ) -> Option<usize> {
        let mut selected = None;
        for _ in 0..document.line_count().saturating_add(2) {
            if !session.is_pending() {
                break;
            }
            selected = session
                .advance_pending(document, top, height, true)
                .or(selected);
        }
        assert!(!session.is_pending(), "search did not terminate");
        selected
    }

    fn assert_trace_valid(session: &SearchSession, height: usize) {
        let mut lines: Vec<_> = session
            .evaluations()
            .iter()
            .map(|evaluation| evaluation.line)
            .collect();
        let count = lines.len();
        lines.sort_unstable();
        lines.dedup();
        assert_eq!(lines.len(), count, "a line was regex-evaluated twice");
        for evaluation in session.evaluations() {
            match evaluation.reason {
                ScanReason::Initial => {}
                ScanReason::Window {
                    kind,
                    top,
                    start,
                    end,
                    line_count,
                } => {
                    assert_eq!((start, end), expanded_window(top, height, line_count));
                    assert!((start..end).contains(&evaluation.line));
                    if !matches!(kind, ProbeKind::Highlight) {
                        assert!(top <= max_top(line_count, height));
                    }
                }
            }
        }
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
        assert_eq!(
            session
                .evaluations()
                .iter()
                .map(|evaluation| evaluation.line)
                .collect::<Vec<_>>(),
            (0..11).collect::<Vec<_>>()
        );
        assert_trace_valid(&session, 5);
    }

    #[test]
    fn directional_navigation_runs_one_probe_per_step() {
        let document = lines(500, &[3, 497]);
        let (mut session, top) = session(&document, 7);
        session.next(&document, top, 7, true).unwrap();
        let before = session.evaluations().len();
        assert_eq!(session.next(&document, top, 7, true), None);
        let first_step = session.evaluations().len() - before;
        assert!(first_step <= 21);
        assert!(session.is_pending());
        let selected_top = finish_pending(&mut session, &document, top, 7).unwrap();
        assert_eq!(selected_top, 494);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 497,
                range_index: 0
            })
        );
        let mut probe_tops: Vec<_> = session
            .evaluations()
            .iter()
            .filter_map(|evaluation| match evaluation.reason {
                ScanReason::Window {
                    kind: ProbeKind::Down,
                    top,
                    ..
                } => Some(top),
                _ => None,
            })
            .collect();
        probe_tops.dedup();
        assert!(
            probe_tops
                .windows(2)
                .all(|tops| tops[1] == next_probe_top(tops[0], 7, document.line_count()))
        );
        assert_trace_valid(&session, 7);
    }

    #[test]
    fn navigation_visits_ranges_then_lines_and_wraps() {
        let document = lines(30, &[3, 20]);
        let (mut session, mut top) = session(&document, 5);
        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(session.selected().unwrap().range_index, 1);
        session.next(&document, top, 5, true);
        top = finish_pending(&mut session, &document, top, 5).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 20,
                range_index: 0
            })
        );
        top = session.next(&document, top, 5, true).unwrap();
        assert_eq!(session.selected().unwrap().range_index, 1);
        session.next(&document, top, 5, true);
        top = finish_pending(&mut session, &document, top, 5).unwrap();
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 3,
                range_index: 0
            })
        );
        session.previous(&document, top, 5, true);
        finish_pending(&mut session, &document, top, 5);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 20,
                range_index: 1
            })
        );
        assert_trace_valid(&session, 5);
    }

    #[test]
    fn scrolling_and_disjoint_jumps_scan_only_requested_windows() {
        let document = lines(100, &[3]);
        let (mut session, _) = session(&document, 5);
        session.ensure_window(&document, 95, 5);
        session.ensure_window(&document, 0, 5);
        let evaluated: Vec<_> = session
            .evaluations()
            .iter()
            .map(|evaluation| evaluation.line)
            .collect();
        assert_eq!(evaluated, (0..11).chain(90..101).collect::<Vec<_>>());
        assert_trace_valid(&session, 5);
    }

    #[test]
    fn pending_initial_search_resumes_by_one_viewport_without_rescanning() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("one\ntwo\nthree\nfour\nfive\n");
        let regex = Regex::new("needle").unwrap();
        let mut session =
            SearchSession::new("needle".into(), regex, renderer.document().line_count());
        assert_eq!(
            session.start_initial(renderer.document(), 0, 2, false),
            None
        );
        let before = session.evaluations().len();
        assert_eq!(
            session.advance_pending(renderer.document(), 0, 2, false),
            None
        );
        assert!(session.evaluations().len() - before <= 2);
        renderer.push_chunk("needle\n");
        let mut top = None;
        while session.is_pending() {
            top = session
                .document_changed(renderer.document(), 0, 2, false)
                .or(top);
        }
        assert_eq!(top, Some(4));
        assert_trace_valid(&session, 2);
    }

    #[test]
    fn pending_down_waits_for_growth_and_eof_before_wrapping() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("needle\none\ntwo\n");
        let regex = Regex::new("needle").unwrap();
        let mut session =
            SearchSession::new("needle".into(), regex, renderer.document().line_count());
        let top = session
            .start_initial(renderer.document(), 0, 3, false)
            .unwrap();
        session.next(renderer.document(), top, 3, false);
        assert!(session.is_pending());
        renderer.push_chunk("three\nneedle\n");
        let selected_top = session
            .document_changed(renderer.document(), top, 3, false)
            .unwrap();
        assert_eq!(session.selected().unwrap().line, 4);
        session.next(renderer.document(), selected_top, 3, false);
        assert!(session.is_pending());
        renderer.complete();
        while session.is_pending() {
            session.document_changed(renderer.document(), selected_top, 3, true);
        }
        assert_eq!(session.selected().unwrap().line, 0);
        assert_trace_valid(&session, 3);
    }

    #[test]
    fn pending_up_tracks_the_last_wrap_candidate_as_input_grows() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("needle\none\nneedle\n");
        let regex = Regex::new("needle").unwrap();
        let mut session =
            SearchSession::new("needle".into(), regex, renderer.document().line_count());
        let top = session
            .start_initial(renderer.document(), 0, 2, false)
            .unwrap();
        session.previous(renderer.document(), top, 2, false);
        assert!(session.is_pending());
        renderer.push_chunk("needle latest\n");
        session.document_changed(renderer.document(), top, 2, false);
        assert!(session.is_pending());
        renderer.complete();
        while session.is_pending() {
            session.document_changed(renderer.document(), top, 2, true);
        }
        assert_eq!(session.selected().unwrap().line, 3);
        assert_trace_valid(&session, 2);
    }

    #[test]
    fn completion_created_trailing_line_resolves_pending_search() {
        let mut renderer = crate::render::IncrementalDocumentRenderer::new();
        renderer.push_chunk("one\n");
        let regex = Regex::new("^$").unwrap();
        let mut session = SearchSession::new("^$".into(), regex, renderer.document().line_count());
        assert_eq!(
            session.start_initial(renderer.document(), 0, 2, false),
            None
        );
        assert!(session.is_pending());
        renderer.complete();
        while session.is_pending() {
            session.document_changed(renderer.document(), 0, 2, true);
        }
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 1,
                range_index: 0
            })
        );
    }

    #[test]
    fn completed_no_result_is_final_and_arrows_do_no_work() {
        let document = lines(20, &[]);
        let regex = Regex::new("absent").unwrap();
        let mut session = SearchSession::new("absent".into(), regex, document.line_count());
        assert_eq!(session.start_initial(&document, 7, 4, true), None);
        finish_pending(&mut session, &document, 7, 4);
        assert!(session.is_final_no_match());
        assert_eq!(session.selected(), None);
        let calls = session.evaluations().len();
        assert_eq!(session.next(&document, 7, 4, true), None);
        assert_eq!(session.previous(&document, 7, 4, true), None);
        assert_eq!(session.evaluations().len(), calls);
    }

    #[test]
    fn zero_width_matches_have_deterministic_navigation_and_wrap() {
        let document = crate::render::render_document("abc\ndef\n");
        let regex = Regex::new("^").unwrap();
        let mut session = SearchSession::new("^".into(), regex, document.line_count());
        let mut top = session.start_initial(&document, 0, 2, true).unwrap();
        let mut selected = Vec::new();
        for _ in 0..4 {
            selected.push(session.selected().unwrap());
            session.next(&document, top, 2, true);
            top = finish_pending(&mut session, &document, top, 2).unwrap_or(top);
        }
        assert_eq!(
            selected,
            vec![
                MatchLocation {
                    line: 0,
                    range_index: 0
                },
                MatchLocation {
                    line: 1,
                    range_index: 0
                },
                MatchLocation {
                    line: 2,
                    range_index: 0
                },
                MatchLocation {
                    line: 0,
                    range_index: 0
                },
            ]
        );
    }

    #[test]
    fn viewport_arithmetic_saturates_and_clamps() {
        assert_eq!(expanded_window(usize::MAX, 1, 10), (usize::MAX - 1, 10));
        assert_eq!(expanded_window(0, usize::MAX, 10), (0, 10));
        assert_eq!(center_top(0, 5, 100), 0);
        assert_eq!(center_top(99, 5, 100), 95);
        assert_eq!(center_top(2, 5, 3), 0);
    }

    #[test]
    fn a_new_session_with_the_same_query_has_a_new_cache() {
        let document = lines(20, &[3]);
        let (first, _) = session(&document, 4);
        let (second, _) = session(&document, 4);
        assert_eq!(first.evaluations(), second.evaluations());
        assert!(!first.evaluations().is_empty());
    }

    #[test]
    fn latest_arrow_replaces_a_pending_directional_search() {
        let document = lines(100, &[3, 90]);
        let (mut session, top) = session(&document, 5);
        session.next(&document, top, 5, true).unwrap();
        session.next(&document, top, 5, true);
        assert!(matches!(
            session.pending,
            Some(PendingSearch::Direction(DirectionalSearch {
                direction: Direction::Down,
                ..
            }))
        ));

        session.previous(&document, top, 5, true);
        assert_eq!(session.pending, None);
        assert_eq!(
            session.selected(),
            Some(MatchLocation {
                line: 3,
                range_index: 0
            })
        );
    }
}
