//! diff-pretty: a from-scratch, bone-stripped reimplementation of a strict
//! subset of `delta` (https://github.com/dandavison/delta).
//!
//! The starting contract was byte-for-byte equality with delta for git-named
//! input (`git show`/`git log`/`git diff`); that behavior is frozen in the
//! golden baselines under `fixtures/oracle/` (see `tests/golden.rs`), and is
//! now independent of delta. The configuration (hardcoded as the default
//! behavior) is:
//!
//! ```text
//! [delta]
//!     features = line-numbers
//!     syntax-theme = none
//!     minus-style = red
//!     plus-style  = green
//!     minus-emph-style = "red bold reverse"
//!     plus-emph-style  = "green bold reverse"
//!     line-numbers-left-format  = {nm:^4}
//!     line-numbers-right-format = {np:^4}
//!     navigate = true
//!     word-diff-regex = \w+
//! ```
//!
//! The goldens were originally produced by running delta inside a synthetic git
//! repo carrying the above config. When stdout is a pipe (not a tty) delta (and
//! we) use terminal width 80, hardcoded as `WIDTH`.

pub mod align;
pub mod config;
pub mod edits;
pub mod render;
pub mod source;

pub use render::{render, render_document, render_reader_document, render_reader_to, render_to};

/// Fixed terminal width delta uses when stdout is not a tty (verified: COLUMNS
/// is ignored and the default equals `--width 80`).
pub const WIDTH: usize = 80;
