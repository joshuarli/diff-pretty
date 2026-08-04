//! Configuration hardcoded from the target `[delta]` contract.
//!
//! All values are literal translations of the delta config that the test
//! harness pins on the oracle. See the crate docs for the full config.

/// ANSI SGR sequences used by the renderer. These mirror the exact bytes delta
/// emits for each style; an `ansi_term`-style writer coalesces adjacent
/// segments (emit prefix only on style change, reset on transition to plain,
/// one reset at line end when the last style is non-plain).
pub mod sgr {
    pub const RESET: &str = "\x1b[0m";
    /// Blue: file decoration, hunk box, line-number cell borders.
    pub const BLUE: &str = "\x1b[34m";
    /// Gray: context ("zero") line numbers -> rgb(68,68,68).
    pub const ZERO_NUM: &str = "\x1b[38;2;68;68;68m";
    /// Dark red: minus-line number (color index 88).
    pub const MINUS_NUM: &str = "\x1b[38;5;88m";
    /// Dark green: plus-line number (color index 28).
    pub const PLUS_NUM: &str = "\x1b[38;5;28m";
    /// Red: minus content (`minus-style = red`).
    pub const MINUS: &str = "\x1b[31m";
    /// Green: plus content (`plus-style = green`).
    pub const PLUS: &str = "\x1b[32m";
    /// Emph (bold + reverse) for changed words; color is inherited from the
    /// surrounding minus/plus base, so only the added attributes are emitted.
    pub const EMPH: &str = "\x1b[1;7m";

    pub const HORIZONTAL: &str = "\u{2500}"; // ─
    pub const DOWN_LEFT: &str = "\u{2510}"; // ┐
    pub const UP_LEFT: &str = "\u{2518}"; // ┘
    pub const VERTICAL: &str = "\u{2502}"; // │
    pub const BULLET: &str = "\u{2022}"; // •
    pub const RIGHT_ARROW: &str = "\u{27f6}  "; // ⟶  (arrow + two spaces)
}

/// The line-numbers format `{nm:^4}` / `{np:^4}`: a centered field of width 4,
/// with delta's special "center-right" shift for single-digit numbers. A
/// missing number produces `width` spaces.
pub fn pad_number(line_number: Option<usize>, width: usize) -> String {
    match line_number {
        None => " ".repeat(width),
        Some(n) => {
            let n_width = num_digits(n);
            let space = if width > n_width && (width % 2 != n_width % 2) {
                " "
            } else {
                ""
            };
            let centered = format!("{n:^width$}");
            let mut s = format!("{space}{centered}");
            if space == " " {
                s.pop();
            }
            s
        }
    }
}

/// floor(log10(n)) + 1, with 0 treated as 1 digit.
pub fn num_digits(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut len = 0;
    while n >= 10_000 {
        len += 4;
        n /= 10_000;
    }
    if n >= 1000 {
        len + 4
    } else if n >= 100 {
        len + 3
    } else if n >= 10 {
        len + 2
    } else {
        len + 1
    }
}
