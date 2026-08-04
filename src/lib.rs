//! diff-pretty: a from-scratch, bone-stripped reimplementation of a strict
//! subset of `delta` (https://github.com/dandavison/delta).
//!
//! The target contract is byte-for-byte equality with the oracle delta
//! (`/opt/homebrew/bin/delta`) for the input produced by `git show`, under
//! exactly this configuration (hardcoded here as the default behavior):
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
//! delta reads its configuration only from a git repo's config, so the test
//! harness renders the oracle from inside a synthetic repo carrying the above
//! config. When stdout is a pipe (not a tty) delta fixes terminal width at 80,
//! which we hardcode as `WIDTH`.

pub mod align;
pub mod config;
pub mod edits;
pub mod pager;
pub mod render;

pub use render::render;

/// Fixed terminal width delta uses when stdout is not a tty (verified: COLUMNS
/// is ignored and the default equals `--width 80`).
pub const WIDTH: usize = 80;
