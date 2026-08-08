//! A small ANSI-aware terminal pager.
//!
//! The reusable part of scrl is terminal-independent. It accepts UTF-8 text
//! with CSI SGR styling, searches visible text, and exposes a session that a
//! caller may drive with its own terminal adapter. The command-line runner is
//! intentionally stdin-only and never consults an external pager.

mod document;
mod search;
mod session;
mod source;
#[cfg(feature = "terminal")]
mod terminal;

pub use document::{Document, DocumentBuilder};
pub use session::{Action, Event, Session, SessionOptions, Size};
pub use source::{ChunkSource, FileSource, FilesSource, ReaderSource};

use std::io::{self, BufRead, IsTerminal, Write};
#[cfg(unix)]
use std::os::fd::{BorrowedFd, RawFd};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingMode {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub paging: PagingMode,
    pub session: SessionOptions,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            paging: PagingMode::Auto,
            session: SessionOptions::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Quit,
    EndOfInput,
}

/// Run a buffered reader. Non-terminal output is copied through the source
/// boundary, preserving every input byte including SGR sequences.
pub fn run_reader<R: BufRead + Send + 'static>(
    input: R,
    options: RunOptions,
) -> io::Result<ExitReason> {
    run_source(ReaderSource::new(input), options)
}

/// Run a chunk producer. A non-terminal has no interactive pager, regardless
/// of `always`; this is what keeps redirected output free of screen controls.
pub fn run_source<S: ChunkSource>(source: S, options: RunOptions) -> io::Result<ExitReason> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if options.paging == PagingMode::Never || !output.is_terminal() {
        let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
        source.produce(&mut emit)?;
        output.flush()?;
        return Ok(ExitReason::EndOfInput);
    }

    // The terminal adapter is deliberately kept behind the feature boundary.
    // The state machine remains usable on all targets; unsupported terminal
    // environments fall back to direct output rather than failing a render.
    #[cfg(feature = "terminal")]
    {
        crate::session::run_terminal(source, options, &mut output)
    }
    #[cfg(not(feature = "terminal"))]
    {
        let mut emit = |chunk: &str| output.write_all(chunk.as_bytes());
        source.produce(&mut emit)?;
        output.flush()?;
        Ok(ExitReason::EndOfInput)
    }
}

/// Run an already-retained document. This is the retained integration seam;
/// callers do not need to serialize a document merely to hand it to scrl.
pub fn run_document(document: &Document, options: RunOptions) -> io::Result<ExitReason> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if options.paging == PagingMode::Never || !output.is_terminal() {
        document.write_to(&mut output)?;
        output.flush()?;
        return Ok(ExitReason::EndOfInput);
    }
    #[cfg(all(feature = "terminal", unix))]
    {
        session::run_retained_terminal(document.clone(), options, &mut output)
    }
    #[cfg(not(all(feature = "terminal", unix)))]
    {
        document.write_to(&mut output)?;
        output.flush()?;
        Ok(ExitReason::EndOfInput)
    }
}

/// Run a retained document using descriptors owned by the caller.
///
/// The descriptors are borrowed for the duration of the call and are never
/// closed by scrl. This is the native Git integration seam: Git keeps control
/// of its process descriptors while scrl owns terminal mode, input, and
/// alternate-screen cleanup.
#[cfg(unix)]
pub fn run_document_with_fds(
    document: &Document,
    options: RunOptions,
    output_fd: RawFd,
    tty_fd: RawFd,
) -> io::Result<ExitReason> {
    if output_fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output descriptor must be non-negative",
        ));
    }
    // Duplicate caller-owned descriptors so scrl can use ordinary `File`
    // values without ever taking ownership of Git's descriptors.
    let output = unsafe { BorrowedFd::borrow_raw(output_fd) }
        .try_clone_to_owned()
        .map(std::fs::File::from)?;
    let mut output = output;
    if options.paging == PagingMode::Never || !output.is_terminal() {
        document
            .write_to(&mut output)
            .and_then(|()| output.flush())
            .map(|()| ExitReason::EndOfInput)
    } else if tty_fd >= 0 {
        let tty = unsafe { BorrowedFd::borrow_raw(tty_fd) }
            .try_clone_to_owned()
            .map(std::fs::File::from)?;
        session::run_retained_terminal_with_tty(document.clone(), options, &mut output, &tty)
    } else {
        session::run_retained_terminal(document.clone(), options, &mut output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn document_round_trips_plain_and_sgr_input() {
        let input = "one\n\x1b[31mred\x1b[0m\n";
        let mut builder = DocumentBuilder::new();
        builder.push_str(input);
        let document = builder.finish();
        let mut output = Vec::new();
        document.write_to(&mut output).unwrap();
        assert_eq!(output, input.as_bytes());
        assert_eq!(document.line_text(1), Some("red"));
        assert_eq!(document.line_count(), 3);
    }

    #[test]
    fn reader_runner_is_direct_for_pipe_even_when_always() {
        let source = ReaderSource::new(Cursor::new("one\ntwo\n"));
        let mut output = Vec::new();
        let mut emit = |chunk: &str| {
            output.extend_from_slice(chunk.as_bytes());
            Ok(())
        };
        source.produce(&mut emit).unwrap();
        assert_eq!(output, b"one\ntwo\n");
        assert!(!output.windows(6).any(|window| window == b"\x1b[?1049h"));
    }

    #[test]
    fn viewport_strips_non_sgr_controls_but_keeps_visible_text() {
        let mut builder = DocumentBuilder::new();
        builder.push_str("\x1b[2Jsafe\x1b]0;unsafe title\x07\n");
        let document = builder.finish();
        assert_eq!(document.line_text(0), Some("safe"));
        let mut output = Vec::new();
        document.write_viewport(&mut output, 0, 1).unwrap();
        assert!(output.windows(4).any(|window| window == b"safe"));
        assert!(!output.windows(4).any(|window| window == b"2Jsa"));
        assert!(!output.windows(2).any(|window| window == b"\x1b]"));
    }
}
