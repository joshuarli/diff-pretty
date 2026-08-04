//! Minimal pager handling, mirroring the relevant part of delta's `OutputType`.
//!
//! The renderer (`render()`) is pure and never pages; the decision to page and
//! the pager child-process plumbing live here so the benchmark and the
//! byte-for-byte oracle tests are never affected by paging.

use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagingMode {
    /// Page only when stdout is a terminal (delta's `auto`).
    Auto,
    /// Always page (delta's `always`).
    Always,
    /// Never page; write to stdout (delta's `never`).
    Never,
}

/// Whether a pager will be used for this invocation.
pub fn should_use_pager(mode: PagingMode) -> bool {
    match mode {
        PagingMode::Always => true,
        PagingMode::Never => false,
        PagingMode::Auto => std::io::stdout().is_terminal(),
    }
}

/// Emit `output`, optionally through a pager child process. Falls back to
/// writing to stdout if paging was requested but the pager cannot be run.
pub fn emit(output: &str, mode: PagingMode) -> std::io::Result<()> {
    if !should_use_pager(mode) {
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        h.write_all(output.as_bytes())?;
        h.flush()?;
        return Ok(());
    }
    // In `auto` mode, a diff that fits on one screen is written straight to
    // stdout (so it stays in the terminal like delta; we don't page it or wrap
    // it in the alternate screen, which would make it vanish on `less -F`
    // quit-if-one-screen). Only multi-screen output gets the alternate-screen
    // pager.
    if mode == PagingMode::Auto && fits_on_one_screen(output) {
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        h.write_all(output.as_bytes())?;
        h.flush()?;
        return Ok(());
    }
    match run_pager(output) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Fall back to plain stdout (mirrors delta's pager fallback).
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            h.write_all(output.as_bytes())?;
            h.flush()
        }
    }
}

/// Whether the rendered output is short enough to display without paging. We
/// estimate the display height as the number of lines and compare it with the
/// terminal's row count.
fn fits_on_one_screen(output: &str) -> bool {
    let Some(rows) = terminal_rows() else {
        // Can't determine terminal size: conservatively page.
        return false;
    };
    if rows == 0 {
        return false;
    }
    let lines = output.bytes().filter(|&b| b == b'\n').count() + 1;
    lines <= rows as usize
}

/// Number of rows in the terminal (from the `TIOCGWINSZ` ioctl on stdout).
#[cfg(unix)]
fn terminal_rows() -> Option<u16> {
    use std::os::fd::AsRawFd;

    #[repr(C)]
    struct WinSize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }
    #[cfg(target_os = "macos")]
    const TIOCGWINSZ: u64 = 0x40087468; // _IOR('t', 104, struct winsize)
    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: u64 = 0x5413;

    unsafe extern "C" {
        #[link_name = "ioctl"]
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }

    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let fd = std::io::stdout().as_raw_fd();
    let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut WinSize) };
    if rc == 0 && ws.ws_row > 0 {
        Some(ws.ws_row)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn terminal_rows() -> Option<u16> {
    std::env::var("LINES").ok().and_then(|s| s.parse().ok())
}

/// Enter / leave the alternate screen buffer, so the pager's content never
/// pollutes the terminal scrollback and nothing lingers after quitting.
pub const ACS_ENTER: &str = "\x1b[?1049h";
pub const ACS_EXIT: &str = "\x1b[?1049l";

/// Spawn the pager (env `PAGER`, default `less -R`), feed it the output, wait.
/// When stdout is a terminal we wrap the whole session in the alternate screen
/// buffer so the paged content is discarded on exit.
fn run_pager(output: &str) -> std::io::Result<()> {
    let use_acs = std::io::stdout().is_terminal();
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());

    let mut parts = pager.split_whitespace();
    let prog = parts.next().unwrap_or("less").to_string();
    let mut args: Vec<String> = parts.map(str::to_string).collect();
    let prog_is_less = prog.rsplit('/').next().unwrap_or(&prog) == "less";

    if prog_is_less {
        // `less` needs -R to interpret the ANSI color codes we emit.
        if !args.iter().any(|a| a.starts_with("-R") || a == "-r") {
            args.insert(0, "-R".into());
        }
        // We own the alternate screen: tell less not to run its own termcap
        // init/deinit so it does not fight us over screen control.
        if use_acs && !args.iter().any(|a| a == "-X" || a == "--no-init") {
            args.push("-X".into());
        }
    }

    let mut stdout = std::io::stdout();
    if use_acs {
        stdout.write_all(ACS_ENTER.as_bytes())?;
        stdout.flush()?;
    }

    let result = (|| -> std::io::Result<()> {
        let mut child = match Command::new(&prog)
            .args(&args)
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            // Only a failure to *spawn* the pager (e.g. binary not found) is a
            // real error that should fall back to writing to stdout.
            Err(e) => return Err(e),
        };
        // The user may quit the pager partway through the write: `less` exits,
        // its stdin pipe breaks, and `write_all` returns a broken-pipe error.
        // delta treats that as a clean stop (`BrokenPipe => return Ok(0)`),
        // NOT as a reason to dump the remaining output to stdout.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(output.as_bytes());
            // Dropping stdin closes the pipe so less sees end of input.
        }
        let _ = child.wait();
        Ok(())
    })();

    if use_acs {
        // Always leave the alternate screen, even if the pager failed partway.
        let _ = stdout.write_all(ACS_EXIT.as_bytes());
        let _ = stdout.flush();
    }
    result
}
