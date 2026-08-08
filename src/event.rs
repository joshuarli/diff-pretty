//! Git diff-symbol events consumed by the native integration.
//!
//! The numeric values are intentionally explicit.  The C adapter maps Git's
//! private `enum diff_symbol` into this model, so a Git source update that
//! changes the mapping must fail in the adapter rather than silently changing
//! presentation.

use std::io;

pub const ABI_VERSION: u32 = 1;

pub const BINARY_DIFF_HEADER: u32 = 0;
pub const BINARY_DIFF_HEADER_DELTA: u32 = 1;
pub const BINARY_DIFF_HEADER_LITERAL: u32 = 2;
pub const BINARY_DIFF_BODY: u32 = 3;
pub const BINARY_DIFF_FOOTER: u32 = 4;
pub const STATS_SUMMARY_NO_FILES: u32 = 5;
pub const STATS_SUMMARY_ABBREV: u32 = 6;
pub const STATS_SUMMARY_INSERTS_DELETES: u32 = 7;
pub const STATS_LINE: u32 = 8;
pub const WORD_DIFF: u32 = 9;
pub const STAT_SEP: u32 = 10;
pub const SUMMARY: u32 = 11;
pub const SUBMODULE_ADD: u32 = 12;
pub const SUBMODULE_DEL: u32 = 13;
pub const SUBMODULE_UNTRACKED: u32 = 14;
pub const SUBMODULE_MODIFIED: u32 = 15;
pub const SUBMODULE_HEADER: u32 = 16;
pub const SUBMODULE_ERROR: u32 = 17;
pub const SUBMODULE_PIPETHROUGH: u32 = 18;
pub const REWRITE_DIFF: u32 = 19;
pub const BINARY_FILES: u32 = 20;
pub const HEADER: u32 = 21;
pub const FILEPAIR_PLUS: u32 = 22;
pub const FILEPAIR_MINUS: u32 = 23;
pub const WORDS_PORCELAIN: u32 = 24;
pub const WORDS: u32 = 25;
pub const CONTEXT: u32 = 26;
pub const CONTEXT_INCOMPLETE: u32 = 27;
pub const PLUS: u32 = 28;
pub const MINUS: u32 = 29;
pub const CONTEXT_FRAGINFO: u32 = 30;
pub const CONTEXT_MARKER: u32 = 31;
pub const SEPARATOR: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffEvent<'a> {
    pub kind: u32,
    pub flags: u32,
    pub data: &'a [u8],
}

impl<'a> DiffEvent<'a> {
    pub const fn new(kind: u32, flags: u32, data: &'a [u8]) -> Self {
        Self { kind, flags, data }
    }
}

fn append_data(output: &mut String, data: &[u8]) -> io::Result<()> {
    let text = std::str::from_utf8(data)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Git diff event is not UTF-8"))?;
    output.push_str(text);
    Ok(())
}

fn append_line(output: &mut String, data: &[u8]) -> io::Result<()> {
    append_data(output, data)?;
    if !data.ends_with(b"\n") {
        output.push('\n');
    }
    Ok(())
}

/// Convert one Git semantic event into the smallest compatible patch fragment
/// accepted by the existing renderer.  This keeps the frozen renderer and its
/// golden contract intact while moving the Git process boundary below it.
pub fn append_patch_fragment(event: DiffEvent<'_>, output: &mut String) -> io::Result<()> {
    match event.kind {
        BINARY_DIFF_HEADER => output.push_str("GIT binary patch\n"),
        BINARY_DIFF_HEADER_DELTA => {
            output.push_str("delta ");
            append_line(output, event.data)?;
        }
        BINARY_DIFF_HEADER_LITERAL => {
            output.push_str("literal ");
            append_line(output, event.data)?;
        }
        BINARY_DIFF_BODY
        | HEADER
        | BINARY_FILES
        | SUMMARY
        | STATS_LINE
        | SUBMODULE_HEADER
        | SUBMODULE_ERROR
        | SUBMODULE_PIPETHROUGH
        | CONTEXT_FRAGINFO
        | CONTEXT_MARKER
        | REWRITE_DIFF
        | CONTEXT_INCOMPLETE => append_data(output, event.data)?,
        BINARY_DIFF_FOOTER => output.push('\n'),
        STATS_SUMMARY_NO_FILES => output.push_str(" 0 files changed\n"),
        STATS_SUMMARY_ABBREV => output.push_str(" ...\n"),
        STATS_SUMMARY_INSERTS_DELETES => append_line(output, event.data)?,
        STAT_SEP => append_data(output, event.data)?,
        SUBMODULE_ADD | SUBMODULE_DEL => append_line(output, event.data)?,
        SUBMODULE_UNTRACKED => {
            output.push_str("Submodule ");
            append_data(output, event.data)?;
            output.push_str(" contains untracked content\n");
        }
        SUBMODULE_MODIFIED => {
            output.push_str("Submodule ");
            append_data(output, event.data)?;
            output.push_str(" contains modified content\n");
        }
        FILEPAIR_PLUS | FILEPAIR_MINUS => {
            output.push_str(if event.kind == FILEPAIR_PLUS {
                "+++ "
            } else {
                "--- "
            });
            append_data(output, event.data)?;
            if event.data.contains(&b' ') {
                output.push('\t');
            }
            output.push('\n');
        }
        WORDS_PORCELAIN => {
            append_line(output, event.data)?;
            output.push_str("~\n");
        }
        WORDS => {
            let content = event.data.get(1..).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "word event has no prefix")
            })?;
            append_line(output, content)?;
        }
        CONTEXT => {
            output.push(' ');
            append_data(output, event.data)?;
        }
        PLUS => {
            output.push('+');
            append_data(output, event.data)?;
        }
        MINUS => {
            output.push('-');
            append_data(output, event.data)?;
        }
        SEPARATOR => output.push('\n'),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown Git diff event kind {}", event.kind),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_content_events_form_a_patch_fragment() {
        let mut patch = String::new();
        append_patch_fragment(
            DiffEvent::new(HEADER, 0, b"diff --git a/a.txt b/a.txt\n"),
            &mut patch,
        )
        .unwrap();
        append_patch_fragment(DiffEvent::new(FILEPAIR_MINUS, 0, b"a/a.txt"), &mut patch).unwrap();
        append_patch_fragment(DiffEvent::new(FILEPAIR_PLUS, 0, b"b/a.txt"), &mut patch).unwrap();
        append_patch_fragment(
            DiffEvent::new(CONTEXT_FRAGINFO, 0, b"@@ -1 +1 @@\n"),
            &mut patch,
        )
        .unwrap();
        append_patch_fragment(DiffEvent::new(MINUS, 0, b"old\n"), &mut patch).unwrap();
        append_patch_fragment(DiffEvent::new(PLUS, 0, b"new\n"), &mut patch).unwrap();
        assert_eq!(
            patch,
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n"
        );
    }
}
