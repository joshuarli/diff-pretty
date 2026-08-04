use std::io::Read;

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
    let out = diff_pretty::render(&input);
    // Paging only happens through `emit` (and only when a terminal is attached
    // for `auto`); the render itself is pure and never enters terminal mode.
    let _ = pager::emit(&out, mode);
}
