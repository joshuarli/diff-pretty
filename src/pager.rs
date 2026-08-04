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
        let mut child = Command::new(&prog)
            .args(&args)
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(output.as_bytes())?;
            // Dropping stdin closes the pipe so less sees end of input.
        }
        child.wait()?;
        Ok(())
    })();

    if use_acs {
        // Always leave the alternate screen, even if the pager failed partway.
        let _ = stdout.write_all(ACS_EXIT.as_bytes());
        let _ = stdout.flush();
    }
    result
}
