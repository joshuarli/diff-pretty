//! Minimal native pager handling.
//!
//! The renderer is pure and never pages; the decision to page and the terminal
//! session live here so the benchmark and byte-for-byte oracle tests remain
//! pager-free. The native pager consumes `RenderedDocument` directly. It keeps
//! fixed terminal dimensions, vertical navigation, lazy regex search, and an
//! alternate screen. It does not handle resize signals or horizontal scrolling.

use std::fmt::Write as _;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
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

/// Exercise the pager's lazy search and highlighted viewport path for the
/// benchmark suite without involving a terminal session.
#[doc(hidden)]
pub fn benchmark_search_viewport<W: Write>(
    document: &RenderedDocument,
    query: &str,
    top: usize,
    rows: usize,
    columns: usize,
    draws: usize,
    output: &mut W,
) -> io::Result<()> {
    let mut state = PagerState::new(rows, columns);
    state.search.begin();
    if let Some(input) = state.search.input_mut() {
        input.push('\0');
        for character in query.chars() {
            input.push(character);
        }
    }
    if let Some(selected_top) = state
        .search
        .submit(document, top, state.content_rows(), true)
    {
        state.top = selected_top;
    }
    state.top = top.min(state.max_top(document));
    let height = state.content_rows();
    if let Some(session) = state.search.active_mut() {
        session.ensure_window(document, state.top, height);
    }
    for _ in 0..draws {
        document.write_pager_viewport(output, state.top, rows, columns, false, &state.search)?;
        state.apply_key(Key::Text('j'), document, true);
    }
    Ok(())
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
    let mut viewer = Viewer::new(document, rows, columns);
    let mut input = &tty;
    let result = viewer.run(&mut screen, &mut input);
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
            Err(_) => return Ok(()),
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
    receiver: Receiver<Key>,
    thread: thread::JoinHandle<()>,
}

#[cfg(unix)]
fn spawn_key_reader(tty: File, cancelled: Arc<AtomicBool>) -> KeyReader {
    let (sender, receiver) = mpsc::channel();
    let thread = thread::spawn(move || {
        let mut input = tty;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            match read_key(&mut input) {
                Ok(key) => {
                    if sender.send(key).is_err() || key == Key::Interrupt {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(_) => break,
            }
        }
    });
    KeyReader { receiver, thread }
}

#[cfg(unix)]
fn run_live_viewer(
    screen: &mut Screen<'_>,
    rows: usize,
    columns: usize,
    mut renderer: IncrementalDocumentRenderer,
    load: Receiver<LoadEvent>,
    keys: Receiver<Key>,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    const FRAME_INTERVAL: Duration = Duration::from_millis(16);

    let mut viewer = LiveViewer::new(rows, columns);
    let mut finished = false;
    viewer.draw(screen, renderer.document(), false)?;
    let mut last_draw = Instant::now();

    while !finished {
        let mut key_changed = false;
        while let Ok(key) = keys.try_recv() {
            if viewer.apply_key(key, renderer.document(), false) {
                cancelled.store(true, Ordering::Relaxed);
                return Ok(());
            }
            key_changed = true;
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
        let Ok(key) = keys.recv() else {
            return Ok(());
        };
        if viewer.apply_key(key, renderer.document(), true) {
            cancelled.store(true, Ordering::Relaxed);
            return Ok(());
        }
        viewer.draw(screen, renderer.document(), true)?;
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
        raw.special_codes[termios::SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[termios::SpecialCodeIndex::VTIME] = 1;
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

    fn apply_key(&mut self, key: Key, document: &RenderedDocument, finished: bool) -> bool {
        let height = self.content_rows();
        if let Some(input) = self.search.input_mut() {
            match key {
                Key::Interrupt => return true,
                Key::Escape => self.search.cancel(),
                Key::Enter => {
                    if let Some(top) = self.search.submit(document, self.top, height, finished) {
                        self.top = top;
                    }
                }
                Key::Backspace => input.backspace(),
                Key::CtrlU => input.clear(),
                Key::Text(character) => input.push(character),
                _ => {}
            }
            return false;
        }

        let old_top = self.top;
        match key {
            Key::Interrupt | Key::Text('q' | 'Q') => return true,
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
            _ => {}
        }
        self.top = self.top.min(self.max_top(document));
        if self.top != old_top
            && let Some(session) = self.search.active_mut()
        {
            session.ensure_window(document, self.top, height);
        }
        false
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

    fn apply_key(&mut self, key: Key, document: &RenderedDocument, finished: bool) -> bool {
        self.state.apply_key(key, document, finished)
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

    fn run<R: Read>(&mut self, screen: &mut Screen<'_>, input: &mut R) -> io::Result<()> {
        self.draw(screen)?;
        loop {
            let key = match read_key(input) {
                Ok(key) => key,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(error) => return Err(error),
            };
            if self.state.apply_key(key, self.document, true) {
                return Ok(());
            }
            self.draw(screen)?;
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
        let _ = write!(status, " /{}  Enter search  Esc cancel", input.query());
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
            if character.is_control() || character == '\u{1b}' {
                character = '�';
            }
            let width = if character.is_ascii() { 1 } else { 2 };
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

fn read_key<R: Read>(input: &mut R) -> io::Result<Key> {
    let byte = read_byte(input)?;
    match byte {
        3 => Ok(Key::Interrupt),
        b'\r' | b'\n' => Ok(Key::Enter),
        0x08 | 0x7f => Ok(Key::Backspace),
        0x15 => Ok(Key::CtrlU),
        0x1b => read_escape_key(input),
        0x20..=0x7e => Ok(Key::Text(byte as char)),
        0x80..=0xff => read_utf8_key(input, byte),
        _ => Ok(Key::Unknown),
    }
}

fn read_escape_key<R: Read>(input: &mut R) -> io::Result<Key> {
    let byte = match read_byte(input) {
        Ok(byte) => byte,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(Key::Escape),
        Err(error) => return Err(error),
    };
    match byte {
        b'[' => {
            let byte = read_byte(input)?;
            match byte {
                b'A' => Ok(Key::Up),
                b'B' => Ok(Key::Down),
                b'H' => Ok(Key::Home),
                b'F' => Ok(Key::End),
                b'1' | b'3' | b'4' | b'5' | b'6' => read_csi_tilde_key(input, byte),
                _ => Ok(Key::Unknown),
            }
        }
        b'O' => match read_byte(input)? {
            b'A' => Ok(Key::Up),
            b'B' => Ok(Key::Down),
            b'H' => Ok(Key::Home),
            b'F' => Ok(Key::End),
            _ => Ok(Key::Unknown),
        },
        _ => Ok(Key::Unknown),
    }
}

fn read_csi_tilde_key<R: Read>(input: &mut R, first: u8) -> io::Result<Key> {
    let mut byte = read_byte(input)?;
    while byte != b'~' {
        byte = read_byte(input)?;
    }
    match first {
        b'1' => Ok(Key::Home),
        b'3' => Ok(Key::Backspace),
        b'4' => Ok(Key::End),
        b'5' => Ok(Key::PageUp),
        b'6' => Ok(Key::PageDown),
        _ => Ok(Key::Unknown),
    }
}

fn read_utf8_key<R: Read>(input: &mut R, first: u8) -> io::Result<Key> {
    let length = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Ok(Key::Unknown),
    };
    let mut bytes = [0; 4];
    bytes[0] = first;
    if input.read_exact(&mut bytes[1..length]).is_err() {
        return Ok(Key::Unknown);
    }
    let Ok(text) = std::str::from_utf8(&bytes[..length]) else {
        return Ok(Key::Unknown);
    };
    Ok(text.chars().next().map_or(Key::Unknown, Key::Text))
}

fn read_byte<R: Read>(input: &mut R) -> io::Result<u8> {
    let mut byte = [0; 1];
    input.read_exact(&mut byte)?;
    Ok(byte[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
        let mut input = Cursor::new(b"kj bgGq".to_vec());

        assert_eq!(read_key(&mut input).unwrap(), Key::Text('k'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('j'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text(' '));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('b'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('g'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('G'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('q'));
    }

    #[test]
    fn reads_arrow_and_page_escape_sequences() {
        let mut input = Cursor::new(b"\x1b[A\x1b[B\x1b[5~\x1b[6~\x1b[H\x1b[F".to_vec());

        assert_eq!(read_key(&mut input).unwrap(), Key::Up);
        assert_eq!(read_key(&mut input).unwrap(), Key::Down);
        assert_eq!(read_key(&mut input).unwrap(), Key::PageUp);
        assert_eq!(read_key(&mut input).unwrap(), Key::PageDown);
        assert_eq!(read_key(&mut input).unwrap(), Key::Home);
        assert_eq!(read_key(&mut input).unwrap(), Key::End);
    }

    #[test]
    fn reads_search_controls_and_unicode_text() {
        let mut input = Cursor::new("/\r\u{8}\u{15}λ".as_bytes());

        assert_eq!(read_key(&mut input).unwrap(), Key::Text('/'));
        assert_eq!(read_key(&mut input).unwrap(), Key::Enter);
        assert_eq!(read_key(&mut input).unwrap(), Key::Backspace);
        assert_eq!(read_key(&mut input).unwrap(), Key::CtrlU);
        assert_eq!(read_key(&mut input).unwrap(), Key::Text('λ'));
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
        assert!(!state.apply_key(Key::Text('q'), &document, true));
        assert_eq!(state.search.input().unwrap().query(), "j/qq");
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
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("\x1b[31mred \x1b[7mtext\x1b[0m\x1b[7m pla"));
        assert!(output.contains("\x1b[0m\x1b[31m\x1b[0min"));
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

        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("\x1b[1;7;31m\x1b[48;5;240mchanged")
        );
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
    }
}
