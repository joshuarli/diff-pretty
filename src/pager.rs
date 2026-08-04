//! Minimal native pager handling.
//!
//! The renderer is pure and never pages; the decision to page and the terminal
//! session live here so the benchmark and byte-for-byte oracle tests remain
//! pager-free. The native pager consumes `RenderedDocument` directly. It keeps
//! fixed terminal dimensions, vertical navigation, lazy regex search, and an
//! alternate screen. It does not handle resize signals or horizontal scrolling.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use crate::pager_search::SearchState;
use crate::render::{IncrementalDocumentRenderer, RenderedDocument};

#[cfg(unix)]
use std::fs::{File, OpenOptions};

#[cfg(unix)]
use rustix::termios::{self, OptionalActions, Termios};
#[cfg(unix)]
use rustix::{event, event::Timespec, fd::AsRawFd};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagingMode {
    /// Page only when stdout is a terminal (delta's `auto`).
    Auto,
    /// Use the native pager when a terminal is available.
    Always,
    /// Never page; write to stdout (delta's `never`).
    Never,
}

/// Whether a pager will be used for this invocation.
pub fn should_use_pager(mode: PagingMode) -> bool {
    match mode {
        PagingMode::Always => std::io::stdout().is_terminal(),
        PagingMode::Never => false,
        PagingMode::Auto => std::io::stdout().is_terminal(),
    }
}

#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod bench_internals {
    use super::*;

    pub struct SearchViewport<'a> {
        document: &'a RenderedDocument,
        state: PagerState,
    }

    pub struct LiveSearchViewport {
        renderer: IncrementalDocumentRenderer,
        state: PagerState,
    }

    impl<'a> SearchViewport<'a> {
        pub fn new(
            document: &'a RenderedDocument,
            query: &str,
            rows: usize,
            columns: usize,
        ) -> Self {
            let mut state = PagerState::new(rows, columns);
            state.search.begin();
            if let Some(input) = state.search.input_mut() {
                input.push('\0');
                for character in query.chars() {
                    input.push(character);
                }
            }
            if let Some(top) = state
                .search
                .submit(document, state.top, state.content_rows(), true)
            {
                state.top = top;
            }
            while state
                .search
                .active()
                .is_some_and(|session| session.is_pending())
            {
                state.advance_search(document, true);
            }
            Self { document, state }
        }

        pub fn set_top(&mut self, top: usize) {
            self.state.top = top.min(self.state.max_top(self.document));
            let height = self.state.content_rows();
            if let Some(session) = self.state.search.active_mut() {
                session.ensure_window(self.document, self.state.top, height);
            }
        }

        pub fn draw<W: Write>(&self, output: &mut W) -> io::Result<()> {
            self.document.write_pager_viewport(
                output,
                self.state.top,
                self.state.rows,
                self.state.columns,
                false,
                &self.state.search,
            )
        }

        pub fn scroll_down(&mut self) {
            let _ = self.state.apply_key(Key::Text('j'), self.document, true);
        }

        pub fn next_match(&mut self) {
            let _ = self.state.apply_key(Key::Down, self.document, true);
            while self
                .state
                .search
                .active()
                .is_some_and(|session| session.is_pending())
            {
                self.state.advance_search(self.document, true);
            }
        }

        pub fn top(&self) -> usize {
            self.state.top
        }
    }

    impl LiveSearchViewport {
        pub fn new(initial: &str, query: &str, rows: usize, columns: usize) -> Self {
            let mut renderer = IncrementalDocumentRenderer::new();
            renderer.push_chunk(initial);
            let mut state = PagerState::new(rows, columns);
            state.search.begin();
            if let Some(input) = state.search.input_mut() {
                input.push('\0');
                for character in query.chars() {
                    input.push(character);
                }
            }
            let _ =
                state
                    .search
                    .submit(renderer.document(), state.top, state.content_rows(), false);
            Self { renderer, state }
        }

        pub fn push_chunk_and_advance(&mut self, chunk: &str) -> usize {
            self.renderer.push_chunk(chunk);
            self.state.document_changed(self.renderer.document(), false);
            let _ = self.state.advance_search(self.renderer.document(), false);
            self.renderer.document().line_count()
        }
    }
}

/// Emit a retained document, optionally through the native pager. If the
/// terminal cannot be opened or configured, write it directly to stdout.
pub fn emit(document: &RenderedDocument, mode: PagingMode) -> io::Result<()> {
    if !should_use_pager(mode) {
        return write_stdout(document);
    }

    // In `auto` mode, a diff that fits on one screen is written straight to
    // stdout so it stays in terminal scrollback. Only multi-screen output gets
    // the alternate-screen pager.
    if mode == PagingMode::Auto && fits_on_one_screen(document) {
        return write_stdout(document);
    }

    match run_native_pager(document) {
        Ok(()) => Ok(()),
        Err(_) => write_stdout(document),
    }
}

/// Stream input into the pager. The reader is bounded to a small number of
/// complete render units, so rendering can begin before EOF without allowing
/// either the input or output channel to grow with a large `git log`.
///
/// Quitting cancels work between reads and complete render chunks. Rust's
/// `BufRead` contract has no interruption primitive, so a custom reader blocked
/// inside `read` remains owned by its worker until that read returns.
pub fn emit_reader<R: BufRead + Send + 'static>(input: R, mode: PagingMode) -> io::Result<()> {
    if !should_use_pager(mode) {
        return write_reader_stdout(input);
    }

    run_native_pager_reader(input, mode)
}

fn write_reader_stdout<R: BufRead>(input: R) -> io::Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    crate::render::render_reader_to(input, &mut handle).and_then(|()| handle.flush())
}

enum LoadEvent {
    Chunk(String),
    Finished(io::Result<()>),
}

fn spawn_reader_with_cancel<R: BufRead + Send + 'static>(
    input: R,
    cancelled: Arc<AtomicBool>,
) -> Receiver<LoadEvent> {
    let (sender, receiver) = mpsc::sync_channel(2);
    thread::spawn(move || {
        let mut input = input;
        let result = crate::render::for_each_render_chunk(&mut input, |chunk| {
            if cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "pager quit"));
            }
            sender
                .send(LoadEvent::Chunk(chunk.to_owned()))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pager stopped reading"))
        });
        let _ = sender.send(LoadEvent::Finished(result));
    });
    receiver
}

fn write_stdout(document: &RenderedDocument) -> io::Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    document.write_to(&mut handle)?;
    handle.flush()
}

/// Whether the rendered output is short enough to display without paging. We
/// estimate the display height as the number of logical lines and compare it
/// with the terminal's row count.
fn fits_on_one_screen(document: &RenderedDocument) -> bool {
    let Some((rows, _)) = terminal_size() else {
        return false;
    };
    if rows == 0 {
        return false;
    }
    document.line_count() <= rows
}

/// Number of rows and columns in stdout's terminal.
#[cfg(unix)]
fn terminal_size() -> Option<(usize, usize)> {
    let stdout = std::io::stdout();
    let size = termios::tcgetwinsize(&stdout).ok()?;
    Some((size.ws_row as usize, size.ws_col as usize))
}

#[cfg(not(unix))]
fn terminal_size() -> Option<(usize, usize)> {
    let rows = std::env::var("LINES").ok()?.parse().ok()?;
    let columns = std::env::var("COLUMNS").ok()?.parse().ok()?;
    Some((rows, columns))
}

/// Enter / leave the alternate screen buffer, so the pager's content never
/// pollutes the terminal scrollback and nothing lingers after quitting.
pub const ACS_ENTER: &str = "\x1b[?1049h";
pub const ACS_EXIT: &str = "\x1b[?1049l";

const ANSI_CLEAR_LINE: &[u8] = b"\x1b[2K\r";
const ANSI_CURSOR_HOME: &[u8] = b"\x1b[H";
const ANSI_CURSOR_HIDE: &[u8] = b"\x1b[?25l";
const ANSI_CURSOR_SHOW: &[u8] = b"\x1b[?25h";
const ANSI_RESET: &[u8] = b"\x1b[0m";
const ANSI_WRAP_DISABLE: &[u8] = b"\x1b[?7l";
const ANSI_WRAP_ENABLE: &[u8] = b"\x1b[?7h";
const ANSI_STATUS: &[u8] = b"\x1b[7m";

#[cfg(unix)]
fn run_native_pager(document: &RenderedDocument) -> io::Result<()> {
    let tty = OpenOptions::new().read(true).open("/dev/tty")?;
    let (rows, columns) = terminal_size_for(&tty)?;
    if rows == 0 || columns == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal has no usable dimensions",
        ));
    }

    let _raw_mode = RawMode::enter(&tty)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut screen = Screen::enter(&mut handle)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let keys = spawn_key_reader(tty.try_clone()?, Arc::clone(&cancelled));
    let mut viewer = Viewer::new(document, rows, columns);
    let result = viewer.run(&mut screen, &keys.receiver);
    cancelled.store(true, Ordering::Relaxed);
    let _ = keys.thread.join();
    let leave_result = screen.leave();
    result.and(leave_result)
}

#[cfg(unix)]
fn run_native_pager_reader<R: BufRead + Send + 'static>(
    input: R,
    mode: PagingMode,
) -> io::Result<()> {
    let tty = match OpenOptions::new().read(true).open("/dev/tty") {
        Ok(tty) => tty,
        Err(_) => return write_reader_stdout(input),
    };
    let (rows, columns) = match terminal_size_for(&tty) {
        Ok(size) => size,
        Err(_) => return write_reader_stdout(input),
    };
    if rows == 0 || columns == 0 {
        return write_reader_stdout(input);
    }
    let key_input = match tty.try_clone() {
        Ok(key_input) => key_input,
        Err(_) => return write_reader_stdout(input),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let load = spawn_reader_with_cancel(input, Arc::clone(&cancelled));
    let mut renderer = IncrementalDocumentRenderer::new();
    loop {
        match load.recv() {
            Ok(LoadEvent::Chunk(chunk)) => {
                renderer.push_chunk(&chunk);
                if mode == PagingMode::Always || renderer.document().line_count() > rows {
                    break;
                }
            }
            Ok(LoadEvent::Finished(result)) => {
                result?;
                let document = renderer.finish();
                return write_stdout(&document);
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "input renderer stopped before EOF",
                ));
            }
        }
    }
    let _raw_mode = RawMode::enter(&tty)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut screen = Screen::enter(&mut handle)?;
    let keys = spawn_key_reader(key_input, Arc::clone(&cancelled));
    let result = run_live_viewer(
        &mut screen,
        rows,
        columns,
        renderer,
        load,
        keys.receiver,
        &cancelled,
    );
    cancelled.store(true, Ordering::Relaxed);
    let _ = keys.thread.join();
    let leave_result = screen.leave();
    result.and(leave_result)
}

#[cfg(unix)]
struct KeyReader {
    receiver: Receiver<KeyEvent>,
    thread: thread::JoinHandle<()>,
}

#[cfg(unix)]
fn spawn_key_reader(tty: File, cancelled: Arc<AtomicBool>) -> KeyReader {
    let (sender, receiver) = mpsc::sync_channel(64);
    let thread = thread::spawn(move || {
        let mut input = tty;
        let mut decoder = KeyDecoder::new();
        loop {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            match read_terminal_bytes(&mut input, !decoder.is_empty()) {
                Ok(Some(bytes)) if bytes.is_empty() => {
                    let _ = send_key_event(&sender, KeyEvent::Eof, &cancelled);
                    break;
                }
                Ok(Some(bytes)) => decoder.push(&bytes),
                Ok(None) => {
                    if let Some(key) = decoder.next(true)
                        && !send_key_event(&sender, KeyEvent::Key(key), &cancelled)
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = send_key_event(&sender, KeyEvent::Error(error), &cancelled);
                    break;
                }
            }
            while let Some(key) = decoder.next(false) {
                if !send_key_event(&sender, KeyEvent::Key(key), &cancelled) {
                    return;
                }
            }
        }
    });
    KeyReader { receiver, thread }
}

#[cfg(unix)]
fn send_key_event(
    sender: &SyncSender<KeyEvent>,
    mut event: KeyEvent,
    cancelled: &AtomicBool,
) -> bool {
    loop {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) if !cancelled.load(Ordering::Relaxed) => {
                event = returned;
                thread::sleep(Duration::from_millis(2));
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
        }
    }
}

#[cfg(unix)]
fn read_terminal_bytes(input: &mut File, partial: bool) -> io::Result<Option<Vec<u8>>> {
    let fd = input.as_raw_fd();
    let mut read_fds = vec![event::FdSetElement::default(); event::fd_set_num_elements(1, fd + 1)];
    event::fd_set_insert(&mut read_fds, fd);
    let timeout = if partial {
        Timespec {
            tv_sec: 0,
            tv_nsec: 25_000_000,
        }
    } else {
        Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        }
    };
    let ready = unsafe { event::select(fd + 1, Some(&mut read_fds), None, None, Some(&timeout)) }
        .map_err(io::Error::from)?;
    if ready == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0; 64];
    let count = input.read(&mut bytes)?;
    bytes.truncate(count);
    Ok(Some(bytes))
}

#[cfg(unix)]
fn run_live_viewer(
    screen: &mut Screen<'_>,
    rows: usize,
    columns: usize,
    mut renderer: IncrementalDocumentRenderer,
    load: Receiver<LoadEvent>,
    keys: Receiver<KeyEvent>,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    const FRAME_INTERVAL: Duration = Duration::from_millis(16);

    let mut viewer = LiveViewer::new(rows, columns);
    let mut finished = false;
    viewer.draw(screen, renderer.document(), false)?;
    let mut last_draw = Instant::now();

    while !finished {
        let mut key_changed = false;
        if viewer.advance_search(renderer.document(), finished) {
            key_changed = true;
        }
        for _ in 0..64 {
            match keys.try_recv() {
                Ok(KeyEvent::Key(key)) => match viewer.apply_key(key, renderer.document(), false) {
                    ApplyResult::Quit => {
                        cancelled.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                    ApplyResult::Changed => key_changed = true,
                    ApplyResult::Unchanged => {}
                },
                Ok(KeyEvent::Eof) => {
                    cancelled.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(key_disconnect_error());
                }
                Ok(KeyEvent::Error(error)) => return Err(error),
                Err(TryRecvError::Empty) => break,
            }
        }
        if key_changed {
            viewer.draw(screen, renderer.document(), false)?;
            last_draw = Instant::now();
        }

        let timeout = FRAME_INTERVAL.saturating_sub(last_draw.elapsed());
        match load.recv_timeout(timeout) {
            Ok(LoadEvent::Chunk(chunk)) => {
                renderer.push_chunk(&chunk);
                for _ in 0..64 {
                    match load.try_recv() {
                        Ok(LoadEvent::Chunk(chunk)) => renderer.push_chunk(&chunk),
                        Ok(LoadEvent::Finished(result)) => {
                            result?;
                            renderer.complete();
                            finished = true;
                            break;
                        }
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                viewer.document_changed(renderer.document(), finished);
            }
            Ok(LoadEvent::Finished(result)) => {
                result?;
                renderer.complete();
                finished = true;
                viewer.document_changed(renderer.document(), true);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "input renderer stopped before EOF",
                ));
            }
        }
        if finished || last_draw.elapsed() >= FRAME_INTERVAL {
            viewer.draw(screen, renderer.document(), finished)?;
            last_draw = Instant::now();
        }
    }

    loop {
        match keys.recv_timeout(Duration::from_millis(1)) {
            Ok(KeyEvent::Key(key)) => match viewer.apply_key(key, renderer.document(), true) {
                ApplyResult::Quit => {
                    cancelled.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                ApplyResult::Changed => viewer.draw(screen, renderer.document(), true)?,
                ApplyResult::Unchanged => {}
            },
            Ok(KeyEvent::Eof) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(key_disconnect_error());
            }
            Ok(KeyEvent::Error(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if viewer.advance_search(renderer.document(), true) {
                    viewer.draw(screen, renderer.document(), true)?;
                }
            }
        }
    }
}

#[cfg(not(unix))]
fn run_native_pager_reader<R: BufRead + Send + 'static>(
    _input: R,
    _mode: PagingMode,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native pager is only implemented on Unix",
    ))
}

#[cfg(not(unix))]
fn run_native_pager(_document: &RenderedDocument) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native pager is only implemented on Unix",
    ))
}

#[cfg(unix)]
fn terminal_size_for(tty: &File) -> io::Result<(usize, usize)> {
    let size = termios::tcgetwinsize(tty)?;
    Ok((size.ws_row as usize, size.ws_col as usize))
}

#[cfg(unix)]
struct RawMode<'a> {
    tty: &'a File,
    original: Termios,
}

#[cfg(unix)]
impl<'a> RawMode<'a> {
    fn enter(tty: &'a File) -> io::Result<Self> {
        let original = termios::tcgetattr(tty)?;
        let mut raw = original.clone();
        raw.make_raw();
        raw.special_codes[termios::SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[termios::SpecialCodeIndex::VTIME] = 0;
        termios::tcsetattr(tty, OptionalActions::Now, &raw)?;
        Ok(Self { tty, original })
    }
}

#[cfg(unix)]
impl Drop for RawMode<'_> {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(self.tty, OptionalActions::Now, &self.original);
    }
}

struct Screen<'a> {
    output: &'a mut dyn Write,
    active: bool,
}

impl<'a> Screen<'a> {
    fn enter(output: &'a mut dyn Write) -> io::Result<Self> {
        output.write_all(ACS_ENTER.as_bytes())?;
        let screen = Self {
            output,
            active: true,
        };
        screen.output.write_all(ANSI_WRAP_DISABLE)?;
        screen.output.write_all(ANSI_CURSOR_HIDE)?;
        screen.output.write_all(ANSI_CURSOR_HOME)?;
        screen.output.flush()?;
        Ok(screen)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }

    fn leave(mut self) -> io::Result<()> {
        self.restore()
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.output.write_all(ANSI_RESET)?;
        self.output.write_all(ANSI_WRAP_ENABLE)?;
        self.output.write_all(ANSI_CURSOR_SHOW)?;
        self.output.write_all(ACS_EXIT.as_bytes())?;
        self.output.flush()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Screen<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct PagerState {
    rows: usize,
    columns: usize,
    top: usize,
    search: SearchState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplyResult {
    Quit,
    Changed,
    Unchanged,
}

impl PagerState {
    fn new(rows: usize, columns: usize) -> Self {
        Self {
            rows,
            columns,
            top: 0,
            search: SearchState::Inactive,
        }
    }

    fn content_rows(&self) -> usize {
        self.rows.saturating_sub(1).max(1)
    }

    fn max_top(&self, document: &RenderedDocument) -> usize {
        document.line_count().saturating_sub(self.content_rows())
    }

    fn apply_key(&mut self, key: Key, document: &RenderedDocument, finished: bool) -> ApplyResult {
        let height = self.content_rows();
        if let Some(input) = self.search.input_mut() {
            match key {
                Key::Interrupt => return ApplyResult::Quit,
                Key::Escape => self.search.cancel(),
                Key::Enter => {
                    if let Some(top) = self.search.submit(document, self.top, height, finished) {
                        self.top = top;
                    }
                }
                Key::Backspace => input.backspace(),
                Key::CtrlU => input.clear(),
                Key::Text(character) => input.push(character),
                _ => return ApplyResult::Unchanged,
            }
            return ApplyResult::Changed;
        }

        let old_top = self.top;
        match key {
            Key::Interrupt | Key::Text('q' | 'Q') => return ApplyResult::Quit,
            Key::Text('/') => self.search.begin(),
            Key::Up => {
                if let Some(session) = self.search.active_mut() {
                    if let Some(top) = session.previous(document, self.top, height, finished) {
                        self.top = top;
                    }
                } else {
                    self.top = self.top.saturating_sub(1);
                }
            }
            Key::Down => {
                if let Some(session) = self.search.active_mut() {
                    if let Some(top) = session.next(document, self.top, height, finished) {
                        self.top = top;
                    }
                } else {
                    self.top = self.top.saturating_add(1);
                }
            }
            Key::Text('k') => self.top = self.top.saturating_sub(1),
            Key::Text('j') => self.top = self.top.saturating_add(1),
            Key::Text('b') => self.top = self.top.saturating_sub(height),
            Key::Text(' ') => self.top = self.top.saturating_add(height),
            Key::Text('g') | Key::Home => self.top = 0,
            Key::Text('G') | Key::End => self.top = self.max_top(document),
            Key::PageUp => self.top = self.top.saturating_sub(height),
            Key::PageDown => self.top = self.top.saturating_add(height),
            _ => return ApplyResult::Unchanged,
        }
        self.top = self.top.min(self.max_top(document));
        if self.top != old_top
            && let Some(session) = self.search.active_mut()
        {
            session.ensure_window(document, self.top, height);
        }
        ApplyResult::Changed
    }

    fn document_changed(&mut self, document: &RenderedDocument, finished: bool) {
        self.top = self.top.min(self.max_top(document));
        let height = self.content_rows();
        if let Some(session) = self.search.active_mut()
            && let Some(top) = session.document_changed(document, self.top, height, finished)
        {
            self.top = top.min(self.max_top(document));
        }
    }

    fn advance_search(&mut self, document: &RenderedDocument, finished: bool) -> bool {
        let height = self.content_rows();
        let old_top = self.top;
        let revision = self
            .search
            .active()
            .map_or(0, |session| session.display_revision());
        if let Some(session) = self.search.active_mut()
            && let Some(top) = session.advance_pending(document, self.top, height, finished)
        {
            self.top = top.min(self.max_top(document));
        }
        let next_revision = self
            .search
            .active()
            .map_or(0, |session| session.display_revision());
        revision != next_revision || self.top != old_top
    }

    fn draw(
        &mut self,
        screen: &mut Screen<'_>,
        document: &RenderedDocument,
        finished: bool,
    ) -> io::Result<()> {
        self.top = self.top.min(self.max_top(document));
        let height = self.content_rows();
        if let Some(session) = self.search.active_mut() {
            session.ensure_window(document, self.top, height);
        }
        document.write_pager_viewport(
            screen.output,
            self.top,
            self.rows,
            self.columns,
            !finished,
            &self.search,
        )?;
        screen.flush()
    }
}

struct Viewer<'a> {
    document: &'a RenderedDocument,
    state: PagerState,
}

struct LiveViewer {
    state: PagerState,
}

impl LiveViewer {
    fn new(rows: usize, columns: usize) -> Self {
        Self {
            state: PagerState::new(rows, columns),
        }
    }

    fn apply_key(&mut self, key: Key, document: &RenderedDocument, finished: bool) -> ApplyResult {
        self.state.apply_key(key, document, finished)
    }

    fn advance_search(&mut self, document: &RenderedDocument, finished: bool) -> bool {
        self.state.advance_search(document, finished)
    }

    fn document_changed(&mut self, document: &RenderedDocument, finished: bool) {
        self.state.document_changed(document, finished);
    }

    fn draw(
        &mut self,
        screen: &mut Screen<'_>,
        document: &RenderedDocument,
        finished: bool,
    ) -> io::Result<()> {
        self.state.draw(screen, document, finished)
    }
}

impl<'a> Viewer<'a> {
    fn new(document: &'a RenderedDocument, rows: usize, columns: usize) -> Self {
        Self {
            document,
            state: PagerState::new(rows, columns),
        }
    }

    fn run(&mut self, screen: &mut Screen<'_>, keys: &Receiver<KeyEvent>) -> io::Result<()> {
        const SEARCH_INTERVAL: Duration = Duration::from_millis(1);
        self.draw(screen)?;
        loop {
            match keys.recv_timeout(SEARCH_INTERVAL) {
                Ok(KeyEvent::Key(key)) => match self.state.apply_key(key, self.document, true) {
                    ApplyResult::Quit => return Ok(()),
                    ApplyResult::Changed => self.draw(screen)?,
                    ApplyResult::Unchanged => {}
                },
                Ok(KeyEvent::Eof) => return Ok(()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(key_disconnect_error());
                }
                Ok(KeyEvent::Error(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.state.advance_search(self.document, true) {
                        self.draw(screen)?;
                    }
                }
            }
        }
    }

    fn draw(&mut self, screen: &mut Screen<'_>) -> io::Result<()> {
        self.state.draw(screen, self.document, true)
    }
}

impl RenderedDocument {
    /// Draw one fixed-height viewport to `output`.
    ///
    /// The native pager disables terminal wrapping before calling this method,
    /// so long lines are clipped by the terminal rather than wrapped.
    pub fn write_viewport<W: Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
    ) -> io::Result<()> {
        self.write_viewport_with_status(output, top, rows, false)
    }

    fn write_viewport_with_status<W: Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
        loading: bool,
    ) -> io::Result<()> {
        output.write_all(ANSI_CURSOR_HOME)?;
        let content_rows = if rows > 1 { rows - 1 } else { 1 };
        for row in 0..content_rows {
            output.write_all(ANSI_CLEAR_LINE)?;
            if self.write_line(output, top + row)? {
                output.write_all(ANSI_RESET)?;
            }
            if row + 1 < content_rows {
                output.write_all(b"\n")?;
            }
        }

        if rows > 1 {
            output.write_all(b"\n")?;
            output.write_all(ANSI_CLEAR_LINE)?;
            output.write_all(ANSI_STATUS)?;
            let first = if self.line_count() == 0 { 0 } else { top + 1 };
            let last = (top + content_rows).min(self.line_count());
            write!(
                output,
                " diff-pretty  {first}-{last}/{}{}  ↑/↓ scroll  PgUp/PgDn page  q quit",
                self.line_count(),
                if loading { "  loading" } else { "" }
            )?;
            output.write_all(ANSI_RESET)?;
        }
        Ok(())
    }

    fn write_pager_viewport<W: Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
        columns: usize,
        loading: bool,
        search: &SearchState,
    ) -> io::Result<()> {
        output.write_all(ANSI_CURSOR_HOME)?;
        let content_rows = rows.saturating_sub(1).max(1);
        let active = search.active();
        for row in 0..content_rows {
            output.write_all(ANSI_CLEAR_LINE)?;
            let line = top + row;
            let written = if let Some(session) = active {
                self.write_line_with_search(output, line, session.ranges(line))?
            } else {
                self.write_line(output, line)?
            };
            if written {
                output.write_all(ANSI_RESET)?;
            }
            if row + 1 < content_rows {
                output.write_all(b"\n")?;
            }
        }

        if rows > 1 {
            output.write_all(b"\n")?;
            output.write_all(ANSI_CLEAR_LINE)?;
            output.write_all(ANSI_STATUS)?;
            write_pager_status(output, self, top, content_rows, columns, loading, search)?;
            output.write_all(ANSI_RESET)?;
        }
        Ok(())
    }
}

fn write_pager_status<W: Write + ?Sized>(
    output: &mut W,
    document: &RenderedDocument,
    top: usize,
    content_rows: usize,
    columns: usize,
    loading: bool,
    search: &SearchState,
) -> io::Result<()> {
    let first = if document.line_count() == 0 {
        0
    } else {
        top + 1
    };
    let last = top.saturating_add(content_rows).min(document.line_count());
    let loading = if loading { "  loading" } else { "" };
    let mut status = StatusWriter::new(output, columns);
    if let Some(input) = search.input() {
        let _ = write!(status, " /");
        if let Some(prefix) = input.display_prefix() {
            let _ = status.write_char(prefix);
        }
        let _ = write!(status, "{}  Enter search  Esc cancel", input.query());
        if let Some(error) = input.compile_error() {
            let _ = write!(status, "  {error}");
        }
        let _ = write!(status, "{loading}");
    } else if let Some(session) = search.active() {
        let result = if session.is_pending() {
            "  search pending"
        } else if session.is_final_no_match() {
            "  pattern not found"
        } else {
            ""
        };
        let _ = write!(
            status,
            " diff-pretty  {first}-{last}/{}{loading}  /{}{result}  ↑/↓ match  j/k scroll  q quit",
            document.line_count(),
            session.query(),
        );
    } else {
        let _ = write!(
            status,
            " diff-pretty  {first}-{last}/{}{loading}  ↑/↓ scroll  PgUp/PgDn page  q quit",
            document.line_count(),
        );
    }
    status.finish()
}

struct StatusWriter<'a, W: Write + ?Sized> {
    output: &'a mut W,
    columns: usize,
    used: usize,
    error: Option<io::Error>,
}

impl<'a, W: Write + ?Sized> StatusWriter<'a, W> {
    fn new(output: &'a mut W, columns: usize) -> Self {
        Self {
            output,
            columns,
            used: 0,
            error: None,
        }
    }

    fn finish(self) -> io::Result<()> {
        self.error.map_or(Ok(()), Err)
    }
}

impl<W: Write + ?Sized> std::fmt::Write for StatusWriter<'_, W> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.error.is_some() {
            return Err(std::fmt::Error);
        }
        for mut character in text.chars() {
            if unsafe_status_character(character) {
                character = '�';
            }
            let width = terminal_cell_width(character);
            if self.used.saturating_add(width) > self.columns {
                return Ok(());
            }
            let mut encoded = [0; 4];
            if let Err(error) = self
                .output
                .write_all(character.encode_utf8(&mut encoded).as_bytes())
            {
                self.error = Some(error);
                return Err(std::fmt::Error);
            }
            self.used += width;
        }
        Ok(())
    }
}

fn unsafe_status_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

fn terminal_cell_width(character: char) -> usize {
    let code = character as u32;
    if matches!(
        code,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
    ) {
        0
    } else if matches!(
        code,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f000..=0x1faff
            | 0x20000..=0x3fffd
    ) {
        2
    } else {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Interrupt,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Backspace,
    CtrlU,
    Text(char),
    Unknown,
}

enum KeyEvent {
    Key(Key),
    Eof,
    Error(io::Error),
}

fn key_disconnect_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "terminal key reader stopped unexpectedly",
    )
}

struct KeyDecoder {
    bytes: VecDeque<u8>,
}

impl KeyDecoder {
    fn new() -> Self {
        Self {
            bytes: VecDeque::with_capacity(32),
        }
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes);
    }

    fn next(&mut self, timed_out: bool) -> Option<Key> {
        let first = *self.bytes.front()?;
        match first {
            3 => self.consume(1, Key::Interrupt),
            b'\r' | b'\n' => self.consume(1, Key::Enter),
            0x08 | 0x7f => self.consume(1, Key::Backspace),
            0x15 => self.consume(1, Key::CtrlU),
            0x1b => self.escape(timed_out),
            0x20..=0x7e => self.consume(1, Key::Text(first as char)),
            0x80..=0xff => self.utf8(timed_out),
            _ => self.consume(1, Key::Unknown),
        }
    }

    fn escape(&mut self, timed_out: bool) -> Option<Key> {
        let Some(second) = self.bytes.get(1).copied() else {
            return timed_out.then(|| self.consume(1, Key::Escape)).flatten();
        };
        if !matches!(second, b'[' | b'O') {
            return self.consume(1, Key::Escape);
        }
        let final_index =
            (2..self.bytes.len().min(34)).find(|&index| matches!(self.bytes[index], 0x40..=0x7e));
        let Some(final_index) = final_index else {
            if timed_out || self.bytes.len() >= 34 {
                return self.consume(self.bytes.len().min(34), Key::Unknown);
            }
            return None;
        };
        let sequence: Vec<_> = self.bytes.iter().take(final_index + 1).copied().collect();
        let key = decode_escape_sequence(&sequence);
        self.consume(final_index + 1, key)
    }

    fn utf8(&mut self, timed_out: bool) -> Option<Key> {
        let first = self.bytes[0];
        let length = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return self.consume(1, Key::Unknown),
        };
        for index in 1..length.min(self.bytes.len()) {
            if !matches!(self.bytes[index], 0x80..=0xbf) {
                return self.consume(1, Key::Unknown);
            }
        }
        if self.bytes.len() < length {
            return timed_out.then(|| self.consume(1, Key::Unknown)).flatten();
        }
        let bytes: Vec<_> = self.bytes.iter().take(length).copied().collect();
        let key = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.chars().next())
            .map_or(Key::Unknown, Key::Text);
        self.consume(length, key)
    }

    fn consume(&mut self, count: usize, key: Key) -> Option<Key> {
        self.bytes.drain(..count);
        Some(key)
    }
}

fn decode_escape_sequence(sequence: &[u8]) -> Key {
    let final_byte = sequence.last().copied().unwrap_or_default();
    match final_byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' if sequence.get(1) == Some(&b'[') => {
            let parameter_end = sequence[2..]
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .map_or(sequence.len() - 1, |offset| offset + 2);
            match &sequence[2..parameter_end] {
                b"1" | b"7" => Key::Home,
                b"3" => Key::Backspace,
                b"4" | b"8" => Key::End,
                b"5" => Key::PageUp,
                b"6" => Key::PageDown,
                _ => Key::Unknown,
            }
        }
        _ => Key::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> Vec<Key> {
        let mut decoder = KeyDecoder::new();
        decoder.push(bytes);
        let mut keys = Vec::new();
        while let Some(key) = decoder.next(true) {
            keys.push(key);
        }
        keys
    }

    #[test]
    fn document_indexes_trailing_empty_line() {
        let document = crate::render::render_document("one\ntwo\n");

        assert_eq!(document.line_count(), 3);
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();
        assert_eq!(output, b"one\ntwo\n");
    }

    #[test]
    fn loading_status_is_replaced_when_input_finishes() {
        let document = crate::render::render_document("one\ntwo\nthree\n");
        let mut loading = Vec::new();
        document
            .write_viewport_with_status(&mut loading, 0, 3, true)
            .unwrap();
        let mut complete = Vec::new();
        document
            .write_viewport_with_status(&mut complete, 0, 3, false)
            .unwrap();

        assert!(String::from_utf8_lossy(&loading).contains("loading"));
        assert!(!String::from_utf8_lossy(&complete).contains("loading"));
        assert!(String::from_utf8_lossy(&loading).contains("1-2/4"));
    }

    #[test]
    fn inactive_search_keeps_the_existing_viewport_bytes() {
        let document = crate::render::render_document("one\ntwo\nthree\n");
        let mut existing = Vec::new();
        document.write_viewport(&mut existing, 0, 3).unwrap();
        let mut search_aware = Vec::new();
        document
            .write_pager_viewport(
                &mut search_aware,
                0,
                3,
                usize::MAX,
                false,
                &SearchState::Inactive,
            )
            .unwrap();

        assert_eq!(search_aware, existing);
    }

    #[test]
    fn reads_navigation_keys() {
        assert_eq!(
            decode_all(b"kj bgGq"),
            vec![
                Key::Text('k'),
                Key::Text('j'),
                Key::Text(' '),
                Key::Text('b'),
                Key::Text('g'),
                Key::Text('G'),
                Key::Text('q'),
            ]
        );
    }

    #[test]
    fn reads_arrow_and_page_escape_sequences() {
        assert_eq!(
            decode_all(b"\x1b[A\x1b[B\x1b[5~\x1b[6~\x1b[H\x1b[F"),
            vec![
                Key::Up,
                Key::Down,
                Key::PageUp,
                Key::PageDown,
                Key::Home,
                Key::End
            ]
        );
    }

    #[test]
    fn reads_search_controls_and_unicode_text() {
        assert_eq!(
            decode_all("/\r\u{8}\u{15}λ".as_bytes()),
            vec![
                Key::Text('/'),
                Key::Enter,
                Key::Backspace,
                Key::CtrlU,
                Key::Text('λ')
            ]
        );
    }

    #[test]
    fn modified_and_unsupported_csi_sequences_do_not_consume_following_keys() {
        assert_eq!(decode_all(b"\x1b[1;5Aq"), vec![Key::Up, Key::Text('q')]);
        assert_eq!(
            decode_all(b"\x1b[?25hq"),
            vec![Key::Unknown, Key::Text('q')]
        );
    }

    #[test]
    fn fragmented_escape_and_utf8_input_waits_then_decodes() {
        let mut decoder = KeyDecoder::new();
        decoder.push(b"\x1b");
        assert_eq!(decoder.next(false), None);
        decoder.push(b"[");
        assert_eq!(decoder.next(false), None);
        decoder.push(b"A");
        assert_eq!(decoder.next(false), Some(Key::Up));

        for byte in "λ".as_bytes() {
            decoder.push(&[*byte]);
            if *byte == "λ".as_bytes()[0] {
                assert_eq!(decoder.next(false), None);
            }
        }
        assert_eq!(decoder.next(false), Some(Key::Text('λ')));
    }

    #[test]
    fn isolated_escape_and_invalid_utf8_preserve_following_keys() {
        let mut decoder = KeyDecoder::new();
        decoder.push(b"\x1b");
        assert_eq!(decoder.next(true), Some(Key::Escape));
        decoder.push(&[0xc2, b'q']);
        assert_eq!(decoder.next(false), Some(Key::Unknown));
        assert_eq!(decoder.next(false), Some(Key::Text('q')));
    }

    #[test]
    fn partial_csi_times_out_without_losing_later_keys() {
        let mut decoder = KeyDecoder::new();
        decoder.push(b"\x1b[1;");
        assert_eq!(decoder.next(true), Some(Key::Unknown));
        decoder.push(b"q");
        assert_eq!(decoder.next(false), Some(Key::Text('q')));
    }

    #[test]
    fn oversized_csi_is_bounded_and_preserves_following_input() {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', 40));
        bytes.extend_from_slice(b"Aq");
        let keys = decode_all(&bytes);
        assert_eq!(keys.first(), Some(&Key::Unknown));
        assert_eq!(keys.last(), Some(&Key::Text('q')));
    }

    #[test]
    fn incomplete_utf8_sequences_timeout_one_lead_byte_at_a_time() {
        for bytes in [&[0xc2][..], &[0xe2, 0x82][..], &[0xf0, 0x9f, 0x92][..]] {
            let mut decoder = KeyDecoder::new();
            decoder.push(bytes);
            assert_eq!(decoder.next(false), None);
            assert_eq!(decoder.next(true), Some(Key::Unknown));
        }
    }

    #[test]
    fn search_input_discards_first_character_and_treats_commands_as_text() {
        let document = crate::render::render_document("literal j/q plus+ dot. λ\n");
        let mut state = PagerState::new(5, 80);

        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "j/q".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }

        assert_eq!(state.search.input().unwrap().query(), "j/q");
        assert_eq!(state.search.input().unwrap().display_prefix(), Some('x'));
        assert_eq!(
            state.apply_key(Key::Text('q'), &document, true),
            ApplyResult::Changed
        );
        assert_eq!(state.search.input().unwrap().query(), "j/qq");
    }

    #[test]
    fn discarded_character_is_visible_but_not_compiled() {
        let document = crate::render::render_document("needle\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "needle".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }

        let input = state.search.input().unwrap();
        assert_eq!(input.display_prefix(), Some('x'));
        assert_eq!(input.query(), "needle");
        let mut status = Vec::new();
        write_pager_status(&mut status, &document, 0, 4, 80, false, &state.search).unwrap();
        assert!(String::from_utf8(status).unwrap().starts_with(" /xneedle"));

        state.apply_key(Key::Enter, &document, true);
        while state.search.active().unwrap().is_pending() {
            state.advance_search(&document, true);
        }
        assert!(state.search.active().unwrap().selected().is_some());
    }

    #[test]
    fn search_input_backspace_removes_the_visible_discarded_character() {
        let document = crate::render::render_document("text\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        state.apply_key(Key::Text('a'), &document, true);

        state.apply_key(Key::Backspace, &document, true);
        let input = state.search.input().unwrap();
        assert_eq!(input.query(), "");
        assert_eq!(input.display_prefix(), Some('x'));

        state.apply_key(Key::Backspace, &document, true);
        let input = state.search.input().unwrap();
        assert_eq!(input.query(), "");
        assert_eq!(input.display_prefix(), None);
    }

    #[test]
    fn search_input_edits_unicode_by_scalar_value() {
        let document = crate::render::render_document("λ\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        state.apply_key(Key::Text('λ'), &document, true);
        state.apply_key(Key::Backspace, &document, true);

        assert_eq!(state.search.input().unwrap().query(), "");
        assert_eq!(state.search.input().unwrap().display_prefix(), Some('x'));
    }

    #[test]
    fn ctrl_u_clears_query_but_keeps_the_visible_discarded_character() {
        let document = crate::render::render_document("text\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('λ'), &document, true);
        state.apply_key(Key::Text('a'), &document, true);
        state.apply_key(Key::CtrlU, &document, true);

        let input = state.search.input().unwrap();
        assert_eq!(input.display_prefix(), Some('λ'));
        assert_eq!(input.query(), "");
    }

    #[test]
    fn escaped_regex_metacharacters_are_passed_through() {
        let document = crate::render::render_document("dot. plus+\n");
        for query in [r"\.", r"\+"] {
            let mut state = PagerState::new(5, 80);
            state.apply_key(Key::Text('/'), &document, true);
            state.apply_key(Key::Text('x'), &document, true);
            for character in query.chars() {
                state.apply_key(Key::Text(character), &document, true);
            }
            state.apply_key(Key::Enter, &document, true);
            assert!(state.search.active().unwrap().selected().is_some());
        }
    }

    #[test]
    fn invalid_regex_stays_editable_and_escape_cancels() {
        let document = crate::render::render_document("text\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        state.apply_key(Key::Text('['), &document, true);
        state.apply_key(Key::Enter, &document, true);

        assert!(state.search.input().unwrap().compile_error().is_some());
        state.apply_key(Key::Escape, &document, true);
        assert!(matches!(state.search, SearchState::Inactive));
    }

    #[test]
    fn empty_and_invalid_queries_never_create_a_search_session() {
        let document = crate::render::render_document("text\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Enter, &document, true);
        assert!(matches!(state.search, SearchState::Inactive));

        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        state.apply_key(Key::Text('['), &document, true);
        state.apply_key(Key::Enter, &document, true);
        assert!(state.search.input().unwrap().compile_error().is_some());
        assert!(state.search.active().is_none());
    }

    #[test]
    fn arrows_navigate_matches_but_j_and_k_scroll() {
        let document = crate::render::render_document("needle\none\ntwo\nneedle\nfour\n");
        let mut state = PagerState::new(3, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "needle".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }
        state.apply_key(Key::Enter, &document, true);
        assert_eq!(state.top, 0);

        state.apply_key(Key::Down, &document, true);
        assert_eq!(state.top, 2);
        state.apply_key(Key::Text('j'), &document, true);
        assert_eq!(state.top, 3);
        state.apply_key(Key::Text('k'), &document, true);
        assert_eq!(state.top, 2);
    }

    #[test]
    fn viewport_highlight_uses_visible_text_and_restores_styles() {
        let document = crate::render::render_document("\x1b[31mred text\x1b[0m plain\n");
        let ranges = [crate::render::TextRange { start: 4, end: 12 }];
        let mut output = Vec::new();
        document
            .write_line_with_search(&mut output, 0, &ranges)
            .unwrap();
        assert_eq!(
            output,
            b"\x1b[31mred \x1b[7mtext\x1b[27m\x1b[0m\x1b[7m pla\x1b[27min"
        );
        assert!(!document.line_text(0).unwrap().contains('\u{1b}'));
    }

    #[test]
    fn zero_width_highlight_writes_no_overlay() {
        let document = crate::render::render_document("text");
        let mut plain = Vec::new();
        let mut highlighted = Vec::new();
        document.write_line(&mut plain, 0).unwrap();
        document
            .write_line_with_search(
                &mut highlighted,
                0,
                &[crate::render::TextRange { start: 2, end: 2 }],
            )
            .unwrap();
        assert_eq!(highlighted, plain);
    }

    #[test]
    fn search_inside_reverse_video_uses_a_visible_background_overlay() {
        let document = crate::render::render_document("\x1b[1;7;31mchanged\x1b[0m\n");
        let mut output = Vec::new();
        document
            .write_line_with_search(
                &mut output,
                0,
                &[crate::render::TextRange { start: 0, end: 7 }],
            )
            .unwrap();

        assert_eq!(output, b"\x1b[1;7;31m\x1b[48;5;240mchanged\x1b[49m\x1b[0m");
    }

    #[test]
    fn extended_color_payloads_do_not_change_reverse_state() {
        let cases = [
            (
                "\x1b[38;5;7mindexed\x1b[0m",
                b"\x1b[38;5;7m\x1b[7mindexed\x1b[27m\x1b[0m".as_slice(),
            ),
            (
                "\x1b[7;38;5;0mreversed\x1b[0m",
                b"\x1b[7;38;5;0m\x1b[48;5;240mreversed\x1b[49m\x1b[0m".as_slice(),
            ),
            (
                "\x1b[38;2;0;7;27mrgb\x1b[0m",
                b"\x1b[38;2;0;7;27m\x1b[7mrgb\x1b[27m\x1b[0m".as_slice(),
            ),
        ];
        for (input, expected) in cases {
            let document = crate::render::render_document(input);
            let mut output = Vec::new();
            document
                .write_line_with_search(
                    &mut output,
                    0,
                    &[crate::render::TextRange {
                        start: 0,
                        end: document.line_text(0).unwrap().len(),
                    }],
                )
                .unwrap();
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn colon_colors_are_parsed_conservatively_without_allocating_fake_styles() {
        let cases = [
            (
                "\x1b[7;48:2::1:2:3mvalid\x1b[0m",
                b"\x1b[7;48:2::1:2:3m\x1b[48;5;240mvalid\x1b[48;2;1;2;3m\x1b[0m".as_slice(),
            ),
            (
                "\x1b[7;48:2:x:1:2:3mmalformed\x1b[0m",
                b"\x1b[7;48:2:x:1:2:3m\x1b[48;5;240mmalformed\x1b[49m\x1b[0m".as_slice(),
            ),
            (
                "\x1b[7;48:2:1:1:2:3mspace\x1b[0m",
                b"\x1b[7;48:2:1:1:2:3m\x1b[48;5;240mspace\x1b[49m\x1b[0m".as_slice(),
            ),
        ];
        for (input, expected) in cases {
            let document = crate::render::render_document(input);
            let mut output = Vec::new();
            document
                .write_line_with_search(
                    &mut output,
                    0,
                    &[crate::render::TextRange {
                        start: 0,
                        end: document.line_text(0).unwrap().len(),
                    }],
                )
                .unwrap();
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn search_restores_existing_background_exactly() {
        let document = crate::render::render_document("\x1b[48;2;1;2;3mtext\x1b[0m");
        let mut output = Vec::new();
        document
            .write_line_with_search(
                &mut output,
                0,
                &[crate::render::TextRange { start: 0, end: 4 }],
            )
            .unwrap();
        assert_eq!(output, b"\x1b[48;2;1;2;3m\x1b[7mtext\x1b[27m\x1b[0m");
    }

    #[test]
    fn adjacent_matches_and_style_boundaries_have_exact_output() {
        let document = crate::render::render_document("ab\x1b[31mcd\x1b[0mef");
        let mut output = Vec::new();
        document
            .write_line_with_search(
                &mut output,
                0,
                &[
                    crate::render::TextRange { start: 0, end: 2 },
                    crate::render::TextRange { start: 2, end: 4 },
                    crate::render::TextRange { start: 4, end: 4 },
                ],
            )
            .unwrap();
        assert_eq!(
            output,
            b"\x1b[7mab\x1b[27m\x1b[31m\x1b[7mcd\x1b[27m\x1b[0mef"
        );
    }

    #[test]
    fn stale_or_overlapping_ranges_cannot_stall_composition() {
        let document = crate::render::render_document("abcdef");
        let mut output = Vec::new();
        document
            .write_line_with_search(
                &mut output,
                0,
                &[
                    crate::render::TextRange { start: 1, end: 4 },
                    crate::render::TextRange { start: 2, end: 3 },
                    crate::render::TextRange { start: 4, end: 6 },
                ],
            )
            .unwrap();
        assert_eq!(output, b"a\x1b[7mbcd\x1b[27m\x1b[7mef\x1b[27m");
    }

    #[test]
    fn a_new_slash_drops_the_active_query_and_highlights() {
        let document = crate::render::render_document("needle\n");
        let mut state = PagerState::new(5, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "needle".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }
        state.apply_key(Key::Enter, &document, true);
        assert!(state.search.active().is_some());

        state.apply_key(Key::Text('/'), &document, true);
        assert_eq!(state.search.input().unwrap().query(), "");
    }

    #[test]
    fn status_is_bounded_and_sanitizes_control_characters() {
        let mut search = SearchState::Inactive;
        search.begin();
        let input = search.input_mut().unwrap();
        input.push('x');
        input.push('\u{1b}');
        input.push('a');
        let document = crate::render::render_document("one\n");
        let mut output = Vec::new();
        write_pager_status(&mut output, &document, 0, 2, 12, false, &search).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.chars().count() <= 12);
        assert!(!output.contains('\u{1b}'));
    }

    #[test]
    fn status_width_handles_narrow_wide_combining_and_bidi_text() {
        let mut output = Vec::new();
        let mut status = StatusWriter::new(&mut output, 6);
        let _ = write!(status, "aλ界e\u{301}\u{202e}z");
        status.finish().unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(output, "aλ界e\u{301}�");
        assert_eq!(output.chars().map(terminal_cell_width).sum::<usize>(), 6);
        assert!(!output.contains('\u{202e}'));
    }

    #[test]
    fn key_event_channel_reports_eof_error_and_disconnection() {
        let (sender, receiver) = mpsc::sync_channel(4);
        sender.send(KeyEvent::Eof).unwrap();
        assert!(matches!(receiver.recv().unwrap(), KeyEvent::Eof));
        sender
            .send(KeyEvent::Error(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "tty failed",
            )))
            .unwrap();
        assert!(matches!(
            receiver.recv().unwrap(),
            KeyEvent::Error(error) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        drop(sender);
        assert!(receiver.recv().is_err());
    }

    #[test]
    fn key_disconnect_maps_to_unexpected_eof() {
        let error = key_disconnect_error();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn reader_cancellation_stops_at_the_next_chunk_boundary() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let receiver = spawn_reader_with_cancel(
            std::io::Cursor::new(b"diff --git a/a b/a\nfirst\ndiff --git a/b b/b\nsecond\n"),
            Arc::clone(&cancelled),
        );
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            LoadEvent::Finished(Err(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::Interrupted)
            }
            _ => panic!("cancelled reader did not stop before emitting a chunk"),
        }
    }

    #[test]
    fn ignored_keys_do_not_request_redraws() {
        let document = crate::render::render_document("one\n");
        let mut state = PagerState::new(5, 80);
        assert_eq!(
            state.apply_key(Key::Unknown, &document, true),
            ApplyResult::Unchanged
        );
    }

    #[test]
    fn retained_and_incremental_documents_produce_the_same_search_viewport() {
        let input = "one\nneedle here\nthree\nneedle again\nfive\n";
        let retained = crate::render::render_document(input);
        let mut incremental = crate::render::IncrementalDocumentRenderer::new();
        incremental.push_chunk("one\nneedle here\nthree\n");
        incremental.push_chunk("needle again\nfive\n");
        incremental.complete();

        let mut retained_state = PagerState::new(4, 80);
        let mut incremental_state = PagerState::new(4, 80);
        for key in [
            Key::Text('/'),
            Key::Text('x'),
            Key::Text('n'),
            Key::Text('e'),
            Key::Text('e'),
            Key::Text('d'),
            Key::Text('l'),
            Key::Text('e'),
            Key::Enter,
            Key::Down,
            Key::Text('j'),
        ] {
            retained_state.apply_key(key, &retained, true);
            incremental_state.apply_key(key, incremental.document(), true);
        }

        assert_eq!(retained_state.top, incremental_state.top);
        assert_eq!(
            retained_state.search.active().unwrap().selected(),
            incremental_state.search.active().unwrap().selected()
        );
        let mut retained_output = Vec::new();
        let mut incremental_output = Vec::new();
        retained
            .write_pager_viewport(
                &mut retained_output,
                retained_state.top,
                4,
                80,
                false,
                &retained_state.search,
            )
            .unwrap();
        incremental
            .document()
            .write_pager_viewport(
                &mut incremental_output,
                incremental_state.top,
                4,
                80,
                false,
                &incremental_state.search,
            )
            .unwrap();
        assert_eq!(retained_output, incremental_output);
        assert_eq!(
            retained_state.search.active().unwrap().evaluations(),
            incremental_state.search.active().unwrap().evaluations()
        );
    }

    #[test]
    fn highlight_does_not_leak_into_following_lines_or_status() {
        let document = crate::render::render_document("needle\nplain\n");
        let mut state = PagerState::new(3, 80);
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "needle".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }
        state.apply_key(Key::Enter, &document, true);
        while state.search.active().unwrap().is_pending() {
            state.advance_search(&document, true);
        }
        let mut output = Vec::new();
        document
            .write_pager_viewport(&mut output, 0, 3, 80, false, &state.search)
            .unwrap();
        let plain = output
            .windows(b"\x1b[2K\rplain\x1b[0m".len())
            .any(|window| window == b"\x1b[2K\rplain\x1b[0m");
        assert!(plain);
        assert!(output.ends_with(b"q quit\x1b[0m"));
    }

    #[test]
    fn completed_no_result_preserves_pager_viewport_and_status() {
        let document = crate::render::render_document("one\ntwo\nthree\nfour\n");
        let mut state = PagerState::new(3, 80);
        state.top = 2;
        state.apply_key(Key::Text('/'), &document, true);
        state.apply_key(Key::Text('x'), &document, true);
        for character in "absent".chars() {
            state.apply_key(Key::Text(character), &document, true);
        }
        state.apply_key(Key::Enter, &document, true);
        while state.search.active().unwrap().is_pending() {
            state.advance_search(&document, true);
        }
        assert_eq!(state.top, 2);
        let calls = state.search.active().unwrap().evaluations().len();
        state.apply_key(Key::Down, &document, true);
        state.apply_key(Key::Up, &document, true);
        assert_eq!(state.top, 2);
        assert_eq!(state.search.active().unwrap().evaluations().len(), calls);
        let mut status = Vec::new();
        write_pager_status(&mut status, &document, 2, 2, 80, false, &state.search).unwrap();
        assert!(
            String::from_utf8(status)
                .unwrap()
                .contains("pattern not found")
        );
    }

    #[test]
    fn live_state_matches_retained_state_through_growth_and_eof() {
        let complete = crate::render::render_document("one\ntwo\nneedle\nfour\n");
        let mut incremental = crate::render::IncrementalDocumentRenderer::new();
        incremental.push_chunk("one\ntwo\n");
        let mut live = PagerState::new(4, 80);
        live.apply_key(Key::Text('/'), incremental.document(), false);
        live.apply_key(Key::Text('x'), incremental.document(), false);
        for character in "needle".chars() {
            live.apply_key(Key::Text(character), incremental.document(), false);
        }
        live.apply_key(Key::Enter, incremental.document(), false);
        assert!(live.search.active().unwrap().is_pending());

        incremental.push_chunk("needle\nfour\n");
        live.document_changed(incremental.document(), false);
        while live.search.active().unwrap().is_pending() {
            live.advance_search(incremental.document(), false);
        }
        incremental.complete();
        live.document_changed(incremental.document(), true);

        let mut retained = PagerState::new(4, 80);
        retained.apply_key(Key::Text('/'), &complete, true);
        retained.apply_key(Key::Text('x'), &complete, true);
        for character in "needle".chars() {
            retained.apply_key(Key::Text(character), &complete, true);
        }
        retained.apply_key(Key::Enter, &complete, true);
        while retained.search.active().unwrap().is_pending() {
            retained.advance_search(&complete, true);
        }

        assert_eq!(live.top, retained.top);
        assert_eq!(
            live.search.active().unwrap().selected(),
            retained.search.active().unwrap().selected()
        );
        for line in 0..complete.line_count() {
            assert_eq!(
                live.search.active().unwrap().ranges(line),
                retained.search.active().unwrap().ranges(line)
            );
        }
    }
}
