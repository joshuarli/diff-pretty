use std::io::BufReader;

use scrl::{PagingMode, RunOptions, SessionOptions};

fn parse_paging() -> Result<PagingMode, String> {
    let mut mode = PagingMode::Auto;
    for argument in std::env::args().skip(1) {
        if argument == "--no-pager" {
            mode = PagingMode::Never;
        } else if let Some(value) = argument.strip_prefix("--paging=") {
            mode = match value {
                "auto" => PagingMode::Auto,
                "always" => PagingMode::Always,
                "never" => PagingMode::Never,
                _ => return Err(format!("invalid paging mode: {value}")),
            };
        } else {
            return Err(format!("unexpected argument: {argument}"));
        }
    }
    Ok(mode)
}

fn main() {
    let mode = match parse_paging() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("scrl: {error}");
            std::process::exit(2);
        }
    };
    let options = RunOptions {
        paging: mode,
        session: SessionOptions {
            title: "scrl".into(),
        },
    };
    if let Err(error) = scrl::run_reader(BufReader::new(std::io::stdin()), options) {
        eprintln!("scrl: {error}");
        std::process::exit(1);
    }
}
