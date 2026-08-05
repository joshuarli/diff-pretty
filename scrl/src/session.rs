use std::io::{self, Write};
#[cfg(feature = "terminal")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, TryRecvError},
};
#[cfg(feature = "terminal")]
use std::thread;
#[cfg(feature = "terminal")]
use std::time::Duration;

use crate::document::{Document, Range};
use crate::search::SearchState;
#[cfg(all(feature = "terminal", unix))]
use crate::terminal::RawMode;
#[cfg(feature = "terminal")]
use crate::terminal::read_event;
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

pub struct Session {
    size: Size,
    options: SessionOptions,
    document: Document,
    search: SearchState,
    top: usize,
    horizontal_offset: usize,
    finished: bool,
    frame: Vec<u8>,
    search_ranges: Vec<Vec<Range>>,
}

impl Session {
    pub fn new(size: Size, options: SessionOptions) -> Self {
        Self {
            size,
            options,
            document: Document::default(),
            search: SearchState::new(),
            top: 0,
            horizontal_offset: 0,
            finished: false,
            frame: Vec::with_capacity(size.rows.saturating_mul(size.columns).max(1024)),
            search_ranges: Vec::new(),
        }
    }

    #[cfg(all(feature = "terminal", unix))]
    pub(crate) fn from_document(document: Document, size: Size, options: SessionOptions) -> Self {
        Self {
            size,
            options,
            document,
            search: SearchState::new(),
            top: 0,
            horizontal_offset: 0,
            finished: true,
            frame: Vec::with_capacity(size.rows.saturating_mul(size.columns).max(1024)),
            search_ranges: Vec::new(),
        }
    }

    pub fn push_chunk(&mut self, chunk: &str) {
        if !self.finished {
            self.document.append_for_session(chunk);
        }
        self.clamp_top();
    }

    pub fn finish(&mut self) {
        self.finished = true;
        self.clamp_top();
    }

    pub fn handle(&mut self, event: Event) -> Action {
        if let Some(input) = self.search.input.as_mut() {
            match event {
                Event::Interrupt => return Action::Quit,
                Event::Escape => self.search.cancel(),
                Event::Enter => self.search.submit(&self.document, self.finished),
                Event::Backspace => {
                    input.pop();
                    self.search.error = None;
                }
                Event::CtrlU => {
                    input.clear();
                    self.search.error = None;
                }
                Event::Text(character) => {
                    input.push(character);
                    self.search.error = None;
                }
                _ => return Action::Continue { changed: false },
            }
            return Action::Continue { changed: true };
        }
        match event {
            Event::Interrupt | Event::Text('q' | 'Q') => return Action::Quit,
            Event::Text('/') => {
                self.search.begin();
                return Action::Continue { changed: true };
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
        self.frame.clear();
        self.document.write_viewport_search(
            &mut self.frame,
            self.top,
            self.size.rows,
            true,
            self.horizontal_offset,
            ranges,
            &self.options.title,
            !self.finished,
        )?;
        output.write_all(&self.frame)
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
            .line_count()
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
    if matches!(options.paging, crate::PagingMode::Always) {
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
    output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
    let mut result = session.draw(output);
    if result.is_ok() {
        loop {
            match read_event(&mut &tty)? {
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
    let key_tty = tty.try_clone()?;
    let (key_sender, key_receiver) = mpsc::sync_channel(8);
    thread::spawn(move || {
        let input = key_tty;
        loop {
            match read_event(&mut &input) {
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

    enum LoadEvent {
        Chunk(String),
        Finished(io::Result<()>),
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(2);
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
            match read_event(&mut &input) {
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
    output.write_all(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[H")?;
    let mut result = session.draw(output);
    let mut finished = false;
    let mut quit = false;
    while result.is_ok() && !finished {
        loop {
            match key_receiver.try_recv() {
                Ok(event) => {
                    if matches!(session.handle(event), Action::Quit) {
                        finished = true;
                        quit = true;
                        cancelled.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        if finished {
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(LoadEvent::Chunk(chunk)) => {
                session.push_chunk(&chunk);
                result = session.draw(output);
            }
            Ok(LoadEvent::Finished(source_result)) => {
                result = source_result;
                if result.is_ok() {
                    session.finish();
                    result = session.draw(output);
                }
                finished = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if session.advance() {
                    result = session.draw(output);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                result = Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source stopped before EOF",
                ));
                finished = true;
            }
        }
    }
    if result.is_ok() && finished && !cancelled.load(Ordering::Relaxed) {
        loop {
            match key_receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => match session.handle(event) {
                    Action::Quit => {
                        quit = true;
                        break;
                    }
                    Action::Continue { changed: true } => {
                        result = session.draw(output);
                        if result.is_err() {
                            break;
                        }
                    }
                    Action::Continue { changed: false } => {}
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if session.advance() {
                        result = session.draw(output);
                        if result.is_err() {
                            break;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    cancelled.store(true, Ordering::Relaxed);
    let cleanup = output
        .write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l")
        .and_then(|()| output.flush());
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

    struct WriteCounter(usize);

    impl Write for WriteCounter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
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
            },
        )
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
        let mut output = WriteCounter(0);
        pager.draw(&mut output).unwrap();
        assert_eq!(output.0, 1);
    }
}
