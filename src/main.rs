use std::io::{Read, Write};

use diff_pretty::pager::{self, PagingMode};

/// Parse `--paging=auto|always|never` (or `--no-pager`). Default `auto`.
fn parse_paging() -> PagingMode {
    for arg in std::env::args() {
        if let Some(v) = arg.strip_prefix("--paging=") {
            return match v {
                "always" => PagingMode::Always,
                "never" => PagingMode::Never,
                _ => PagingMode::Auto,
            };
        }
        if arg == "--no-pager" {
            return PagingMode::Never;
        }
    }
    PagingMode::Auto
}

fn main() {
    let mode = parse_paging();
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("failed to read stdin");
        std::process::exit(1);
    }
    let result = if pager::should_use_pager(mode) {
        let document = diff_pretty::render_document(&input);
        pager::emit(&document, mode)
    } else {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        diff_pretty::render_to(&input, &mut output).and_then(|()| output.flush())
    };
    if let Err(error) = result {
        eprintln!("failed to write output: {error}");
        std::process::exit(1);
    }
}
