//! Minimal native pager handling.
//!
//! The renderer is pure and never pages; the decision to page and the terminal
//! session live here so the benchmark and byte-for-byte oracle tests remain
//! pager-free. The native pager consumes `RenderedDocument` directly. It keeps
//! a narrow feature set: fixed terminal dimensions, vertical navigation, and an
//! alternate screen. It does not handle resize signals, search, or horizontal
//! scrolling.

use std::io::{self, IsTerminal, Read, Write};

use crate::render::RenderedDocument;

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
    let mut viewer = Viewer::new(document, rows);
    let mut input = &tty;
    let result = viewer.run(&mut screen, &mut input);
    let leave_result = screen.leave();
    result.and(leave_result)
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
        output.write_all(ANSI_WRAP_DISABLE)?;
        output.write_all(ANSI_CURSOR_HIDE)?;
        output.write_all(ANSI_CURSOR_HOME)?;
        output.flush()?;
        Ok(Self {
            output,
            active: true,
        })
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

struct Viewer<'a> {
    document: &'a RenderedDocument,
    rows: usize,
    top: usize,
}

impl<'a> Viewer<'a> {
    fn new(document: &'a RenderedDocument, rows: usize) -> Self {
        Self {
            document,
            rows,
            top: 0,
        }
    }

    fn run<R: Read>(&mut self, screen: &mut Screen<'_>, input: &mut R) -> io::Result<()> {
        self.draw(screen)?;
        loop {
            match read_key(input)? {
                Key::Quit => return Ok(()),
                Key::Up => self.scroll_up(1),
                Key::Down => self.scroll_down(1),
                Key::PageUp => self.scroll_up(self.content_rows()),
                Key::PageDown => self.scroll_down(self.content_rows()),
                Key::Home => self.top = 0,
                Key::End => self.top = self.max_top(),
                Key::Unknown => continue,
            }
            self.draw(screen)?;
        }
    }

    fn content_rows(&self) -> usize {
        if self.rows > 1 {
            self.rows - 1
        } else {
            1
        }
    }

    fn max_top(&self) -> usize {
        self.document
            .line_count()
            .saturating_sub(self.content_rows())
    }

    fn scroll_up(&mut self, amount: usize) {
        self.top = self.top.saturating_sub(amount);
    }

    fn scroll_down(&mut self, amount: usize) {
        self.top = self.top.saturating_add(amount).min(self.max_top());
    }

    fn draw(&self, screen: &mut Screen<'_>) -> io::Result<()> {
        self.document
            .write_viewport(screen.output, self.top, self.rows)?;
        screen.flush()
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
                " diff-pretty  {first}-{last}/{}  ↑/↓ scroll  PgUp/PgDn page  q quit",
                self.line_count()
            )?;
            output.write_all(ANSI_RESET)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Unknown,
}

fn read_key<R: Read>(input: &mut R) -> io::Result<Key> {
    let byte = read_byte(input)?;
    match byte {
        b'q' | b'Q' | 3 => Ok(Key::Quit),
        b'k' => Ok(Key::Up),
        b'j' => Ok(Key::Down),
        b'b' => Ok(Key::PageUp),
        b' ' => Ok(Key::PageDown),
        b'g' => Ok(Key::Home),
        b'G' => Ok(Key::End),
        0x1b => read_escape_key(input),
        _ => Ok(Key::Unknown),
    }
}

fn read_escape_key<R: Read>(input: &mut R) -> io::Result<Key> {
    match read_byte(input)? {
        b'[' => {
            let byte = read_byte(input)?;
            match byte {
                b'A' => Ok(Key::Up),
                b'B' => Ok(Key::Down),
                b'H' => Ok(Key::Home),
                b'F' => Ok(Key::End),
                b'1' | b'4' | b'5' | b'6' => read_csi_tilde_key(input, byte),
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
        b'4' => Ok(Key::End),
        b'5' => Ok(Key::PageUp),
        b'6' => Ok(Key::PageDown),
        _ => Ok(Key::Unknown),
    }
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
    fn reads_navigation_keys() {
        let mut input = Cursor::new(b"kj bgGq".to_vec());

        assert_eq!(read_key(&mut input).unwrap(), Key::Up);
        assert_eq!(read_key(&mut input).unwrap(), Key::Down);
        assert_eq!(read_key(&mut input).unwrap(), Key::PageDown);
        assert_eq!(read_key(&mut input).unwrap(), Key::PageUp);
        assert_eq!(read_key(&mut input).unwrap(), Key::Home);
        assert_eq!(read_key(&mut input).unwrap(), Key::End);
        assert_eq!(read_key(&mut input).unwrap(), Key::Quit);
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
}
