//! Diff-specific source integration for the generic scrl runner.

use std::io::{self, BufRead, IsTerminal, Write};

pub use scrl::PagingMode;
use scrl::{ChunkSource, ExitReason, RunOptions, SessionOptions};

pub fn should_use_pager(mode: PagingMode) -> bool {
    matches!(mode, PagingMode::Always | PagingMode::Auto) && io::stdout().is_terminal()
}

pub fn emit_reader<R: BufRead + Send + 'static>(input: R, mode: PagingMode) -> io::Result<()> {
    if !should_use_pager(mode) {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        crate::render::render_reader_to(input, &mut output)?;
        output.flush()?;
        return Ok(());
    }
    let options = RunOptions {
        paging: mode,
        session: SessionOptions {
            title: "diff-pretty".into(),
            search_history: Vec::new(),
            wrap: false,
        },
    };
    scrl::run_source(DiffSource { input }, options).map(|_: ExitReason| ())
}

struct DiffSource<R> {
    input: R,
}

impl<R: BufRead + Send + 'static> ChunkSource for DiffSource<R> {
    fn produce(self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        let mut input = self.input;
        crate::render::for_each_render_chunk(&mut input, emit)
    }
}
