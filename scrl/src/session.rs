use regex_lite::Regex;
use std::io::{self, Write};
#[cfg(feature = "terminal")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
#[cfg(feature = "terminal")]
use std::thread;
#[cfg(feature = "terminal")]
use std::time::{Duration, Instant};

use crate::document::{Document, Range};
use crate::search::SearchState;
const RESET: &str = "\x1b[0m";
#[cfg(all(feature = "terminal", unix))]
use crate::terminal::{
    RawMode, SignalGuard, read_event_tty, suspend, suspend_requested, terminated,
};
#[cfg(feature = "terminal")]
use crate::{ChunkSource, ExitReason, RunOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub rows: usize,
    pub columns: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SessionOptions {
    pub title: String,
    pub search_history: Vec<String>,
    pub wrap: bool,
    pub follow: bool,
    pub filter: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    Delete,
    CtrlU,
    Text(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Continue { changed: bool },
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourcePull {
    None,
    One,
    All,
}

pub struct Session {
    size: Size,
    options: SessionOptions,
    document: Document,
    source_document: Option<Document>,
    search: SearchState,
    top: usize,
    horizontal_offset: usize,
    wrap: bool,
    follow: bool,
    filter: Option<Regex>,
    help: bool,
    finished: bool,
    frame: Vec<u8>,
    search_ranges: Vec<Vec<Range>>,
}

impl Session {
    pub fn new(size: Size, options: SessionOptions) -> Self {
        let history = options.search_history.clone();
        let wrap = options.wrap;
        let follow = options.follow;
        let filter = options
            .filter
            .as_deref()
            .and_then(|query| Regex::new(query).ok());
        Self {
            size,
            options,
            document: Document::default(),
            source_document: filter.as_ref().map(|_| Document::default()),
            search: SearchState::with_history(history),
            top: 0,
            horizontal_offset: 0,
            wrap,
            follow,
            filter,
            help: false,
            finished: false,
            frame: Vec::with_capacity(size.rows.saturating_mul(size.columns).max(1024)),
            search_ranges: Vec::new(),
        }
    }

    #[cfg(all(feature = "terminal", unix))]
    pub(crate) fn from_document(document: Document, size: Size, options: SessionOptions) -> Self {
        let history = options.search_history.clone();
        let wrap = options.wrap;
        let follow = options.follow;
        let filter = options
            .filter
            .as_deref()
            .and_then(|query| Regex::new(query).ok());
        let (display_document, source_document) = if let Some(pattern) = &filter {
            (document.filtered(pattern), Some(document))
        } else {
            (document, None)
        };
        Self {
            size,
            options,
            document: display_document,
            source_document,
            search: SearchState::with_history(history),
            top: 0,
            horizontal_offset: 0,
            wrap,
            follow,
            filter,
            help: false,
            finished: true,
            frame: Vec::with_capacity(size.rows.saturating_mul(size.columns).max(1024)),
            search_ranges: Vec::new(),
        }
    }

    pub fn push_chunk(&mut self, chunk: &str) {
        if !self.finished {
            if let Some(source_document) = &mut self.source_document {
                source_document.append_for_session(chunk);
                if let Some(pattern) = &self.filter {
                    self.document = source_document.filtered(pattern);
                    self.search.session = None;
                }
            } else {
                self.document.append_for_session(chunk);
            }
            if self.follow {
                self.top = self.max_top();
            }
        }
        if !self.follow {
            self.clamp_top();
        }
    }

    pub fn finish(&mut self) {
        self.finished = true;
        if self.follow {
            self.top = self.max_top();
        } else {
            self.clamp_top();
        }
    }

    pub fn handle(&mut self, event: Event) -> Action {
        if self.help {
            match event {
                Event::Interrupt => return Action::Quit,
                Event::Escape | Event::Text('h' | 'H' | 'q' | 'Q') => {
                    self.help = false;
                    return Action::Continue { changed: true };
                }
                _ => return Action::Continue { changed: false },
            }
        }
        if self.search.input.is_some() {
            match event {
                Event::Interrupt => return Action::Quit,
                Event::Escape => self.search.cancel(),
                Event::Enter => self.search.submit(&self.document, self.finished),
                Event::Backspace => self.search.backspace(),
                Event::Delete => self.search.delete(),
                Event::CtrlU => self.search.clear_input(),
                Event::Left => self.search.move_cursor(false),
                Event::Right => self.search.move_cursor(true),
                Event::Home => self.search.cursor_start(),
                Event::End => self.search.cursor_end(),
                Event::Up => self.search.history_previous(),
                Event::Down => self.search.history_next(),
                Event::Text(character) => self.search.insert(character),
                _ => return Action::Continue { changed: false },
            }
            return Action::Continue { changed: true };
        }
        match event {
            Event::Interrupt | Event::Text('q' | 'Q') => return Action::Quit,
            Event::Text('/') => {
                self.search.begin(true);
                return Action::Continue { changed: true };
            }
            Event::Text('?') => {
                self.search.begin(false);
                return Action::Continue { changed: true };
            }
            Event::Text('h' | 'H') => {
                self.help = true;
                return Action::Continue { changed: true };
            }
            Event::Text('n') => {
                self.move_match(self.search.forward);
            }
            Event::Text('N') => {
                self.move_match(!self.search.forward);
            }
            Event::Up => {
                if !self.move_match(false) {
                    self.top = self.top.saturating_sub(1);
                }
            }
            Event::Down => {
                if !self.move_match(true) {
                    self.top = self.top.saturating_add(1);
                }
            }
            Event::Text('k') => self.top = self.top.saturating_sub(1),
            Event::Text('j') => self.top = self.top.saturating_add(1),
            Event::Text('b') | Event::PageUp => {
                self.top = self.top.saturating_sub(self.content_rows())
            }
            Event::Text(' ') | Event::PageDown => {
                self.top = self.top.saturating_add(self.content_rows())
            }
            Event::Text('g') | Event::Home => self.top = 0,
            Event::Text('G') | Event::End => self.top = self.max_top(),
            Event::Left => {
                self.horizontal_offset = self
                    .horizontal_offset
                    .saturating_sub(self.horizontal_shift())
            }
            Event::Right => {
                self.horizontal_offset = self
                    .horizontal_offset
                    .saturating_add(self.horizontal_shift())
            }
            _ => return Action::Continue { changed: false },
        }
        self.clamp_top();
        Action::Continue { changed: true }
    }

    pub fn advance(&mut self) -> bool {
        let before = self.top;
        self.ensure_search();
        before != self.top
    }

    pub fn draw<W: Write + ?Sized>(&mut self, output: &mut W) -> io::Result<()> {
        if self.help {
            self.draw_help(output)?;
            return Ok(());
        }
        self.ensure_search();
        let top = self.top;
        let content_rows = self.content_rows();
        let finished = self.finished;
        let ranges = if let Some(session) = self.search.session.as_mut() {
            session.ensure_cache(&self.document, top, content_rows, finished);
            self.search_ranges.resize_with(content_rows, Vec::new);
            for (row, ranges) in self.search_ranges.iter_mut().enumerate() {
                ranges.clear();
                ranges.extend_from_slice(session.ranges(&self.document, top + row));
            }
            Some((top, self.search_ranges.as_slice()))
        } else {
            None
        };
        let command = self.search.input.as_ref().map(|input| {
            let prefix = if self.search.forward { '/' } else { '?' };
            let mut command = String::with_capacity(input.text.len() + 1);
            command.push(' ');
            command.push(prefix);
            command.push_str(&input.text);
            if let Some(error) = &self.search.error {
                command.push_str("  ");
                command.push_str(error);
            }
            command
        });
        self.frame.clear();
        self.document.write_viewport_search(
            &mut self.frame,
            self.top,
            self.size.rows,
            true,
            self.horizontal_offset,
            ranges,
            &self.options.title,
            self.wrap,
            self.size.columns,
            command.as_deref(),
        )?;
        output.write_all(&self.frame)?;
        output.flush()
    }

    fn draw_help<W: Write + ?Sized>(&mut self, output: &mut W) -> io::Result<()> {
        const HELP: &[&str] = &[
            " scrl help",
            "",
            " j/k or arrows       scroll",
            " PgUp/PgDn, b/space  page",
            " g/Home, G/End       jump to top/bottom",
            " Left/Right          shift horizontally",
            " / or ?              search forward/backward",
            " n/N                 next/previous match",
            " h or Escape         close help",
            " q or Ctrl-C         quit",
        ];
        self.frame.clear();
        self.frame.extend_from_slice(b"\x1b[H");
        for row in 0..self.content_rows() {
            self.frame.extend_from_slice(b"\x1b[2K\r");
            if let Some(line) = HELP.get(row) {
                self.frame.extend_from_slice(line.as_bytes());
            }
            if row + 1 < self.content_rows() {
                self.frame.push(b'\n');
            }
        }
        if self.size.rows > 1 {
            self.frame
                .extend_from_slice(b"\n\x1b[2K\r\x1b[7m help  press h or Escape to return ");
            self.frame.extend_from_slice(RESET.as_bytes());
        }
        output.write_all(&self.frame)?;
        output.flush()
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    fn ensure_search(&mut self) {
        let top = self.top;
        let content_rows = self.content_rows();
        let finished = self.finished;
        let max_top = self.max_top();
        if let Some(session) = self.search.session.as_mut() {
            session.ensure_cache(&self.document, top, content_rows, finished);
            if let Some(line) = session.selected_line() {
                self.top = line.saturating_sub(content_rows / 2).min(max_top);
            }
        }
    }

    fn move_match(&mut self, forward: bool) -> bool {
        let Some(session) = self.search.session.as_mut() else {
            return false;
        };
        let changed = session.next(&self.document, forward);
        self.ensure_search();
        changed
    }

    fn content_rows(&self) -> usize {
        self.size.rows.saturating_sub(1).max(1)
    }
    fn max_top(&self) -> usize {
        self.document
            .visual_line_count(self.wrap, self.size.columns)
            .saturating_sub(self.content_rows())
    }
    fn clamp_top(&mut self) {
        self.top = self.top.min(self.max_top());
    }
    fn horizontal_shift(&self) -> usize {
        (self.size.columns / 2).max(1)
    }
}

// This entry point intentionally contains only the generic runner policy. A
// host can use Session directly for a richer terminal adapter; the built-in
// adapter keeps lifecycle bytes paired and falls back to direct output when a
// controlling terminal cannot be opened.
#[cfg(all(feature = "terminal", unix))]
pub(crate) fn run_terminal<S: ChunkSource>(
    source: S,
    options: RunOptions,
    output: &mut dyn Write,
) -> io::Result<ExitReason> {
    if options.session.follow
        || matches!(
            options.paging,
            crate::PagingMode::Always | crate::PagingMode::Auto
        )
    {
        return run_terminal_live(source, options, output);
    }
    #[cfg(unix)]
    let tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(tty) => tty,
        Err(_) => {
            let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
            source.produce(&mut emit)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    #[cfg(not(unix))]
    let mut tty = None::<std::fs::File>;

    #[cfg(unix)]
    let size = rustix::termios::tcgetwinsize(&tty)
        .map(|size| Size {
            rows: usize::from(size.ws_row).max(1),
            columns: usize::from(size.ws_col).max(1),
        })
        .unwrap_or(Size {
            rows: 24,
            columns: 80,
        });
    #[cfg(not(unix))]
    let size = Size {
        rows: 24,
        columns: 80,
    };
    let mut session = Session::new(size, options.session);
    let mut emit = |chunk: &str| {
        session.push_chunk(chunk);
        Ok(())
    };
    source.produce(&mut emit)?;
    session.finish();
    if matches!(options.paging, crate::PagingMode::Auto)
        && session.document.line_count() <= size.rows
    {
        session.document.write_to(output)?;
        output.flush()?;
        return Ok(ExitReason::EndOfInput);
    }
    #[cfg(unix)]
    let raw = match RawMode::enter(&tty) {
        Ok(raw) => raw,
        Err(_) => {
            session.document.write_to(output)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    let _signals = SignalGuard::install();
    output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
    let mut result = session.draw(output);
    if result.is_ok() {
        loop {
            if terminated() {
                break;
            }
            if suspend_requested() {
                suspend(output, &raw)?;
                session.draw(output)?;
                continue;
            }
            match read_event_tty(&mut &tty)? {
                None => break,
                Some(event) => match session.handle(event) {
                    Action::Quit => break,
                    Action::Continue { changed: true } => {
                        if let Err(error) = session.draw(output) {
                            result = Err(error);
                            break;
                        }
                    }
                    Action::Continue { changed: false } => {}
                },
            }
        }
    }
    let cleanup = output
        .write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
        .and_then(|()| output.flush());
    #[cfg(unix)]
    drop(raw);
    result.and(cleanup).map(|()| ExitReason::EndOfInput)
}

#[cfg(all(feature = "terminal", not(unix)))]
pub(crate) fn run_terminal<S: ChunkSource>(
    source: S,
    _options: RunOptions,
    output: &mut dyn Write,
) -> io::Result<ExitReason> {
    let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
    source.produce(&mut emit)?;
    output.flush()?;
    Ok(ExitReason::EndOfInput)
}

#[cfg(all(feature = "terminal", unix))]
pub(crate) fn run_retained_terminal(
    document: Document,
    options: RunOptions,
    output: &mut dyn Write,
) -> io::Result<ExitReason> {
    let tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(tty) => tty,
        Err(_) => {
            document.write_to(output)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    let size = rustix::termios::tcgetwinsize(&tty)
        .map(|size| Size {
            rows: usize::from(size.ws_row).max(1),
            columns: usize::from(size.ws_col).max(1),
        })
        .unwrap_or(Size {
            rows: 24,
            columns: 80,
        });
    if matches!(options.paging, crate::PagingMode::Auto) && document.line_count() <= size.rows {
        document.write_to(output)?;
        output.flush()?;
        return Ok(ExitReason::EndOfInput);
    }
    let raw = match RawMode::enter(&tty) {
        Ok(raw) => raw,
        Err(_) => {
            document.write_to(output)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    let _signals = SignalGuard::install();
    let key_tty = tty.try_clone()?;
    let (key_sender, key_receiver) = mpsc::sync_channel(8);
    thread::spawn(move || {
        let input = key_tty;
        loop {
            match read_event_tty(&mut &input) {
                Ok(Some(event)) => {
                    if key_sender.send(event).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    });
    let mut session = Session::from_document(document, size, options.session);
    output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
    let mut result = session.draw(output);
    while result.is_ok() {
        if terminated() {
            break;
        }
        if suspend_requested() {
            suspend(output, &raw)?;
            result = session.draw(output);
            continue;
        }
        match key_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => match session.handle(event) {
                Action::Quit => break,
                Action::Continue { changed: true } => result = session.draw(output),
                Action::Continue { changed: false } => {}
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if session.advance() {
                    result = session.draw(output);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let cleanup = output
        .write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
        .and_then(|()| output.flush());
    drop(raw);
    result.and(cleanup).map(|()| ExitReason::Quit)
}

#[cfg(all(feature = "terminal", unix))]
fn run_terminal_live<S: ChunkSource>(
    source: S,
    options: RunOptions,
    output: &mut dyn Write,
) -> io::Result<ExitReason> {
    #[cfg(unix)]
    let tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(tty) => tty,
        Err(_) => {
            let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
            source.produce(&mut emit)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    #[cfg(not(unix))]
    {
        let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
        source.produce(&mut emit)?;
        output.flush()?;
        return Ok(ExitReason::EndOfInput);
    }
    #[cfg(unix)]
    let size = rustix::termios::tcgetwinsize(&tty)
        .map(|size| Size {
            rows: usize::from(size.ws_row).max(1),
            columns: usize::from(size.ws_col).max(1),
        })
        .unwrap_or(Size {
            rows: 24,
            columns: 80,
        });
    #[cfg(unix)]
    let raw = match RawMode::enter(&tty) {
        Ok(raw) => raw,
        Err(_) => {
            let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
            source.produce(&mut emit)?;
            output.flush()?;
            return Ok(ExitReason::EndOfInput);
        }
    };
    let _signals = SignalGuard::install();

    enum LoadEvent {
        Chunk(String),
        Finished(io::Result<()>),
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_cancelled = Arc::clone(&cancelled);
    thread::spawn(move || {
        let result = source.produce(&mut |chunk: &str| {
            if worker_cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "pager quit"));
            }
            sender
                .send(LoadEvent::Chunk(chunk.to_owned()))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pager stopped reading"))
        });
        let _ = sender.send(LoadEvent::Finished(result));
    });

    let (key_sender, key_receiver) = mpsc::sync_channel(8);
    let key_cancelled = Arc::clone(&cancelled);
    let key_tty = tty.try_clone()?;
    thread::spawn(move || {
        let input = key_tty;
        loop {
            if key_cancelled.load(Ordering::Relaxed) {
                break;
            }
            match read_event_tty(&mut &input) {
                Ok(Some(event)) => {
                    if key_sender.send(event).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    });

    let mut session = Session::new(size, options.session);
    let auto = matches!(options.paging, crate::PagingMode::Auto) && !session.follow;
    let mut pager_started = !auto;
    let mut last_draw = None;
    if pager_started {
        output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
    }
    let mut result = if pager_started {
        let result = session.draw(output);
        last_draw = Some(Instant::now());
        result
    } else {
        Ok(())
    };
    let mut finished = false;
    let mut quit = false;
    let receive = |session: &mut Session| -> io::Result<bool> {
        match receiver.recv() {
            Ok(LoadEvent::Chunk(chunk)) => {
                session.push_chunk(&chunk);
                Ok(false)
            }
            Ok(LoadEvent::Finished(source_result)) => {
                source_result?;
                session.finish();
                Ok(true)
            }
            Err(_) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source stopped before EOF",
            )),
        }
    };

    // Pull only enough source data to make the first viewport useful. Once
    // this loop ends, the producer is blocked on the one-slot channel until a
    // navigation or search action asks for more data.
    while result.is_ok()
        && !finished
        && (session.follow || !pager_started || session.document().line_count() <= size.rows)
    {
        match receive(&mut session) {
            Ok(done) => finished = done,
            Err(error) => {
                result = Err(error);
                break;
            }
        }
        if !pager_started && session.document().line_count() > size.rows {
            pager_started = true;
            output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
        }
        if session.follow
            && pager_started
            && last_draw.is_none_or(|last| last.elapsed() >= Duration::from_millis(16))
        {
            result = session.draw(output);
            last_draw = Some(Instant::now());
        }
    }

    if result.is_ok() && pager_started {
        result = session.draw(output);
        last_draw = Some(Instant::now());
    } else if result.is_ok() && !pager_started {
        session.document().write_to(output)?;
        output.flush()?;
    }

    while pager_started && result.is_ok() && !quit {
        if terminated() {
            quit = true;
            cancelled.store(true, Ordering::Relaxed);
            break;
        }
        if suspend_requested() {
            suspend(output, &raw)?;
            result = session.draw(output);
            last_draw = Some(Instant::now());
            continue;
        }
        if session.follow && !finished {
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(LoadEvent::Chunk(chunk)) => {
                    session.push_chunk(&chunk);
                    if last_draw.is_none_or(|last| last.elapsed() >= Duration::from_millis(16)) {
                        result = session.draw(output);
                        last_draw = Some(Instant::now());
                    }
                }
                Ok(LoadEvent::Finished(source_result)) => {
                    result = source_result;
                    if result.is_ok() {
                        session.finish();
                        result = session.draw(output);
                        last_draw = Some(Instant::now());
                    }
                    finished = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    result = Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "source stopped before EOF",
                    ));
                }
            }
            continue;
        }

        match key_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                let pull = source_pull_request(&session, finished, event);
                let mut loaded = false;
                if !matches!(pull, SourcePull::None) {
                    loop {
                        match receive(&mut session) {
                            Ok(done) => {
                                loaded = true;
                                finished = done;
                            }
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                        if finished || matches!(pull, SourcePull::One) {
                            break;
                        }
                    }
                }
                if result.is_ok() {
                    match session.handle(event) {
                        Action::Quit => quit = true,
                        Action::Continue { changed } => {
                            if changed || loaded {
                                result = session.draw(output);
                            }
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if finished && session.advance() {
                    result = session.draw(output);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    cancelled.store(true, Ordering::Relaxed);
    let cleanup = if pager_started {
        output
            .write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
            .and_then(|()| output.flush())
    } else {
        Ok(())
    };
    #[cfg(unix)]
    drop(raw);
    result.and(cleanup).map(|()| {
        if quit {
            ExitReason::Quit
        } else {
            ExitReason::EndOfInput
        }
    })
}

fn source_pull_request(session: &Session, finished: bool, event: Event) -> SourcePull {
    if finished {
        return SourcePull::None;
    }
    if session.search.input.is_some() {
        return if event == Event::Enter {
            SourcePull::All
        } else {
            SourcePull::None
        };
    }
    if (session.search.session.is_some() && matches!(event, Event::Text('n' | 'N')))
        || matches!(event, Event::Text('G') | Event::End)
    {
        return SourcePull::All;
    }
    if matches!(
        event,
        Event::Down | Event::Text('j') | Event::PageDown | Event::Text(' ')
    ) && session.top.saturating_add(session.content_rows())
        >= session
            .document
            .visual_line_count(session.wrap, session.size.columns)
    {
        SourcePull::One
    } else {
        SourcePull::None
    }
}

#[cfg(all(feature = "terminal", not(unix)))]
fn run_terminal_live<S: ChunkSource>(
    source: S,
    _options: RunOptions,
    output: &mut dyn Write,
) -> io::Result<ExitReason> {
    let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
    source.produce(&mut emit)?;
    output.flush()?;
    Ok(ExitReason::EndOfInput)
}

impl Document {
    pub(crate) fn append_for_session(&mut self, chunk: &str) {
        self.append(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct WriteCounter {
        writes: usize,
        flushes: usize,
    }

    impl Write for WriteCounter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn session() -> Session {
        Session::new(
            Size {
                rows: 4,
                columns: 40,
            },
            SessionOptions {
                title: "test".into(),
                search_history: Vec::new(),
                wrap: false,
                follow: false,
                filter: None,
            },
        )
    }

    #[test]
    fn source_pull_waits_at_loaded_viewport_until_navigation_needs_more() {
        let mut pager = session();
        pager.push_chunk("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n");

        assert_eq!(
            source_pull_request(&pager, false, Event::Text('j')),
            SourcePull::None
        );

        pager.handle(Event::End);
        assert_eq!(
            source_pull_request(&pager, false, Event::Text('j')),
            SourcePull::One
        );
        assert_eq!(
            source_pull_request(&pager, false, Event::End),
            SourcePull::All
        );

        pager.handle(Event::Text('/'));
        assert_eq!(
            source_pull_request(&pager, false, Event::Enter),
            SourcePull::All
        );
    }

    #[test]
    fn query_characters_are_not_commands() {
        let mut pager = session();
        pager.push_chunk("alpha q / beta\n");
        pager.finish();
        pager.handle(Event::Text('/'));
        for c in "q/".chars() {
            pager.handle(Event::Text(c));
        }
        assert!(matches!(
            pager.handle(Event::Enter),
            Action::Continue { changed: true }
        ));
    }

    #[test]
    fn invalid_query_is_editable_and_empty_query_is_inactive() {
        let mut pager = session();
        pager.push_chunk("text\n");
        pager.finish();
        pager.handle(Event::Text('/'));
        pager.handle(Event::Text('['));
        pager.handle(Event::Enter);
        pager.handle(Event::Backspace);
        pager.handle(Event::Enter);
        assert!(pager.search.session.is_none());
    }

    #[test]
    fn search_uses_visible_text() {
        let mut pager = session();
        pager.push_chunk("\x1b[31mred\x1b[0m\n");
        pager.finish();
        pager.handle(Event::Text('/'));
        pager.handle(Event::Text('r'));
        pager.handle(Event::Text('e'));
        pager.handle(Event::Text('d'));
        pager.handle(Event::Enter);
        let mut output = Vec::new();
        pager.draw(&mut output).unwrap();
        assert!(output.windows(3).any(|window| window == b"red"));
    }

    #[test]
    fn draw_emits_one_complete_frame_write() {
        let mut pager = session();
        pager.push_chunk("one\ntwo\n");
        pager.finish();
        let mut output = WriteCounter {
            writes: 0,
            flushes: 0,
        };
        pager.draw(&mut output).unwrap();
        assert_eq!(output.writes, 1);
        assert_eq!(output.flushes, 1);
    }

    #[test]
    fn search_editing_tracks_unicode_cursor_and_history() {
        let mut pager = Session::new(
            Size {
                rows: 4,
                columns: 40,
            },
            SessionOptions {
                title: "test".into(),
                search_history: vec!["previous".into()],
                wrap: false,
                follow: false,
                filter: None,
            },
        );
        pager.handle(Event::Text('/'));
        for character in "éx".chars() {
            pager.handle(Event::Text(character));
        }
        pager.handle(Event::Left);
        pager.handle(Event::Text('!'));
        assert_eq!(pager.search.input.as_ref().unwrap().text, "é!x");
        pager.handle(Event::Delete);
        assert_eq!(pager.search.input.as_ref().unwrap().text, "é!");
        pager.handle(Event::Backspace);
        assert_eq!(pager.search.input.as_ref().unwrap().text, "é");
        pager.handle(Event::CtrlU);
        pager.handle(Event::Up);
        assert_eq!(pager.search.input.as_ref().unwrap().text, "previous");
        pager.handle(Event::Down);
        assert_eq!(pager.search.input.as_ref().unwrap().text, "");
    }

    #[test]
    fn reverse_search_and_n_navigation_use_the_same_cached_matches() {
        let mut pager = session();
        pager.push_chunk("alpha\nbeta alpha\nomega\n");
        pager.finish();
        pager.handle(Event::Text('?'));
        for character in "alpha".chars() {
            pager.handle(Event::Text(character));
        }
        pager.handle(Event::Enter);
        assert_eq!(
            pager.search.session.as_ref().unwrap().selected_line(),
            Some(1)
        );
        pager.handle(Event::Text('n'));
        assert_eq!(
            pager.search.session.as_ref().unwrap().selected_line(),
            Some(0)
        );
        pager.handle(Event::Text('N'));
        assert_eq!(
            pager.search.session.as_ref().unwrap().selected_line(),
            Some(1)
        );
    }

    #[test]
    fn wrap_mode_scrolls_visual_rows() {
        let mut pager = Session::new(
            Size {
                rows: 4,
                columns: 4,
            },
            SessionOptions {
                title: "test".into(),
                search_history: Vec::new(),
                wrap: true,
                follow: false,
                filter: None,
            },
        );
        pager.push_chunk("abcdefghij\nx\n");
        pager.finish();
        assert_eq!(pager.max_top(), 2);
        pager.handle(Event::Down);
        assert_eq!(pager.top, 1);
    }

    #[test]
    fn filter_rebuilds_the_display_without_losing_source_chunks() {
        let mut pager = Session::new(
            Size {
                rows: 4,
                columns: 40,
            },
            SessionOptions {
                title: "test".into(),
                search_history: Vec::new(),
                wrap: false,
                follow: false,
                filter: Some("keep".into()),
            },
        );
        pager.push_chunk("drop one\nkeep first\n");
        pager.push_chunk("keep second\ndrop two\n");
        assert_eq!(pager.document.line_count(), 3);
        assert_eq!(pager.document.line_text(0), Some("keep first"));
        assert_eq!(pager.document.line_text(1), Some("keep second"));
        assert_eq!(pager.document.line_text(2), Some(""));
    }

    #[test]
    fn follow_mode_pins_new_chunks_to_the_bottom() {
        let mut pager = Session::new(
            Size {
                rows: 3,
                columns: 40,
            },
            SessionOptions {
                title: "test".into(),
                search_history: Vec::new(),
                wrap: false,
                follow: true,
                filter: None,
            },
        );
        pager.push_chunk("one\ntwo\nthree\n");
        assert_eq!(pager.top, 2);
        pager.push_chunk("four\n");
        assert_eq!(pager.top, 3);
    }

    #[test]
    fn help_is_a_reversible_screen_state() {
        let mut pager = session();
        assert!(matches!(
            pager.handle(Event::Text('h')),
            Action::Continue { changed: true }
        ));
        let mut output = Vec::new();
        pager.draw(&mut output).unwrap();
        assert!(output.windows(9).any(|window| window == b"scrl help"));
        assert!(matches!(
            pager.handle(Event::Escape),
            Action::Continue { changed: true }
        ));
        output.clear();
        pager.draw(&mut output).unwrap();
        assert!(!output.windows(9).any(|window| window == b"scrl help"));
    }
}
