use std::io::BufReader;

use scrl::{PagingMode, RunOptions, SessionOptions};

fn parse_args() -> Result<(PagingMode, Vec<String>), String> {
    let mut mode = PagingMode::Auto;
    let mut paths = Vec::new();
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
        } else if argument == "--help" || argument == "-h" {
            println!("usage: scrl [--paging=auto|always|never] [--no-pager] [FILE ...]");
            std::process::exit(0);
        } else {
            paths.push(argument);
        }
    }
    Ok((mode, paths))
}

fn main() {
    let (mode, paths) = match parse_args() {
        Ok(arguments) => arguments,
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
    let result = if paths.is_empty() {
        scrl::run_reader(BufReader::new(std::io::stdin()), options)
    } else if paths.len() == 1 {
        scrl::run_source(scrl::FileSource::new(&paths[0]), options)
    } else {
        scrl::run_source(scrl::FilesSource::new(&paths), options)
    };
    if let Err(error) = result {
        eprintln!("scrl: {error}");
        std::process::exit(1);
    }
}
