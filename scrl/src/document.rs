use std::io::{self, Write};

use unicode_width::UnicodeWidthChar;

const RESET: &str = "\x1b[0m";
const SEARCH_STYLE: &str = "\x1b[48;5;226;30m";

#[derive(Clone, Debug, Default)]
pub struct Document {
    raw: String,
    visible: String,
    raw_lines: Vec<(usize, usize)>,
    visible_lines: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct DocumentBuilder {
    document: Document,
}

impl DocumentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_str(&mut self, chunk: &str) {
        self.document.append(chunk);
    }

    pub fn finish(self) -> Document {
        self.document
    }
}

impl Document {
    pub(crate) fn append(&mut self, chunk: &str) {
        self.raw.push_str(chunk);
        self.reindex();
    }

    fn reindex(&mut self) {
        self.visible.clear();
        self.raw_lines.clear();
        self.visible_lines.clear();
        let mut raw_start = 0;
        let mut visible_start = 0;
        let bytes = self.raw.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\n' {
                self.raw_lines.push((raw_start, index));
                self.visible_lines.push((visible_start, self.visible.len()));
                index += 1;
                raw_start = index;
                visible_start = self.visible.len();
            } else if let Some((end, _)) = control_end(bytes, index) {
                index = end;
            } else {
                let character = self.raw[index..].chars().next().expect("valid UTF-8 input");
                self.visible.push(character);
                index += character.len_utf8();
            }
        }
        self.raw_lines.push((raw_start, bytes.len()));
        self.visible_lines.push((visible_start, self.visible.len()));
    }

    pub fn line_count(&self) -> usize {
        self.visible_lines.len()
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Returns the visible UTF-8 text of a logical line. SGR bytes and the
    /// line terminator are omitted; tabs remain tabs and are eight-column
    /// stops only when a viewport is drawn.
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let (start, end) = *self.visible_lines.get(line)?;
        Some(&self.visible[start..end])
    }

    pub fn write_to<W: Write + ?Sized>(&self, output: &mut W) -> io::Result<()> {
        output.write_all(self.raw.as_bytes())
    }

    pub fn write_viewport<W: Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
    ) -> io::Result<()> {
        self.write_viewport_search(output, top, rows, rows > 1, 0, None, "scrl", false)
    }

    pub(crate) fn write_viewport_search<W: Write + ?Sized>(
        &self,
        output: &mut W,
        top: usize,
        rows: usize,
        status: bool,
        horizontal_offset: usize,
        ranges: Option<(usize, &[Vec<Range>])>,
        title: &str,
        loading: bool,
    ) -> io::Result<()> {
        output.write_all(b"\x1b[H")?;
        let content_rows = rows.saturating_sub(usize::from(status)).max(1);
        for row in 0..content_rows {
            output.write_all(b"\x1b[2K\r")?;
            let line = top + row;
            if let Some(line_ranges) = ranges.and_then(|(range_top, all)| {
                line.checked_sub(range_top).and_then(|row| all.get(row))
            }) {
                self.write_line_search(output, line, line_ranges, horizontal_offset)?;
            } else {
                self.write_line(output, line, horizontal_offset)?;
            }
            output.write_all(RESET.as_bytes())?;
            if row + 1 < content_rows {
                output.write_all(b"\n")?;
            }
        }
        if status {
            output.write_all(b"\n\x1b[2K\r\x1b[7m")?;
            let first = if self.line_count() == 0 { 0 } else { top + 1 };
            let last = top.saturating_add(content_rows).min(self.line_count());
            let loading = if loading { "  loading" } else { "" };
            let text = format!(
                " {title}  {first}-{last}/{}{loading}  ↑/↓ scroll  ←/→ shift  PgUp/PgDn page  q quit",
                self.line_count()
            );
            write_status(output, &text, usize::MAX)?;
            output.write_all(RESET.as_bytes())?;
        }
        Ok(())
    }

    fn write_line<W: Write + ?Sized>(
        &self,
        output: &mut W,
        line: usize,
        offset: usize,
    ) -> io::Result<()> {
        let Some(&(start, end)) = self.raw_lines.get(line) else {
            return Ok(());
        };
        let clip = self.clip_byte(line, offset);
        let mut index = start;
        while index < end {
            if let Some((end, is_sgr)) = control_end(self.raw.as_bytes(), index) {
                if is_sgr {
                    output.write_all(self.raw[index..end].as_bytes())?;
                }
                index = end;
            } else {
                let character = self.raw[index..].chars().next().unwrap();
                if index >= clip {
                    write!(output, "{character}")?;
                }
                index += character.len_utf8();
            }
        }
        Ok(())
    }

    fn write_line_search<W: Write + ?Sized>(
        &self,
        output: &mut W,
        line: usize,
        ranges: &[Range],
        offset: usize,
    ) -> io::Result<()> {
        let Some(&(start, end)) = self.raw_lines.get(line) else {
            return Ok(());
        };
        let clip = self.clip_byte(line, offset);
        let mut index = start;
        let mut visible = 0;
        let mut style = SgrState::default();
        let mut overlay = false;
        while index < end {
            if let Some((end, is_sgr)) = control_end(self.raw.as_bytes(), index) {
                if overlay {
                    output.write_all(RESET.as_bytes())?;
                    style.write(output)?;
                    overlay = false;
                }
                if is_sgr {
                    output.write_all(self.raw[index..end].as_bytes())?;
                    style.apply(&self.raw[index..end]);
                }
                if is_sgr
                    && ranges.iter().any(|range| {
                        range.start <= visible && visible < range.end && range.start < range.end
                    })
                {
                    output.write_all(RESET.as_bytes())?;
                    output.write_all(SEARCH_STYLE.as_bytes())?;
                    overlay = true;
                }
                index = end;
                continue;
            }
            let character = self.raw[index..].chars().next().unwrap();
            let next = visible + character.len_utf8();
            let highlighted = ranges
                .iter()
                .any(|range| range.start < next && range.end > visible && range.start < range.end);
            if highlighted && !overlay {
                output.write_all(RESET.as_bytes())?;
                output.write_all(SEARCH_STYLE.as_bytes())?;
                overlay = true;
            } else if !highlighted && overlay {
                output.write_all(RESET.as_bytes())?;
                style.write(output)?;
                overlay = false;
            }
            if index >= clip {
                output.write_all(&self.raw.as_bytes()[index..index + character.len_utf8()])?;
            }
            visible = next;
            index += character.len_utf8();
        }
        if overlay {
            output.write_all(RESET.as_bytes())?;
            style.write(output)?;
        }
        Ok(())
    }

    fn clip_byte(&self, line: usize, offset: usize) -> usize {
        let Some(&(start, end)) = self.raw_lines.get(line) else {
            return 0;
        };
        if offset == 0 {
            return start;
        }
        let mut cells = 0;
        let mut index = start;
        while index < end {
            if let Some((next, _)) = control_end(self.raw.as_bytes(), index) {
                index = next;
                continue;
            }
            let character = self.raw[index..].chars().next().unwrap();
            if cells >= offset {
                break;
            }
            cells += if character == '\t' {
                8 - (cells % 8)
            } else {
                cell_width(character)
            };
            index += character.len_utf8();
        }
        index
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Range {
    pub start: usize,
    pub end: usize,
}

/// Return the end and whether an escape sequence is an SGR style change.
/// Unsupported controls stay out of visible text and are not replayed into a
/// viewport, so input cannot move the terminal cursor or rewrite its title.
fn control_end(bytes: &[u8], start: usize) -> Option<(usize, bool)> {
    if bytes.get(start) != Some(&0x1b) {
        return None;
    }
    match bytes.get(start + 1).copied() {
        Some(b'[') => {
            let final_offset = bytes[start + 2..]
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))?;
            let end = start + 3 + final_offset;
            Some((end, bytes[end - 1] == b'm'))
        }
        Some(b']') => {
            let rest = &bytes[start + 2..];
            if let Some(offset) = rest.iter().position(|&byte| byte == 0x07) {
                Some((start + 3 + offset, false))
            } else {
                let offset = rest.windows(2).position(|pair| pair == [0x1b, b'\\'])?;
                Some((start + 2 + offset + 2, false))
            }
        }
        Some(_) => Some((start + 2, false)),
        None => Some((start + 1, false)),
    }
}

fn cell_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

fn write_status<W: Write + ?Sized>(output: &mut W, text: &str, columns: usize) -> io::Result<()> {
    let mut used: usize = 0;
    for character in text.chars() {
        let character = if character.is_control() {
            '�'
        } else {
            character
        };
        let width = cell_width(character);
        if used.saturating_add(width) > columns {
            break;
        }
        write!(output, "{character}")?;
        used += width;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct SgrState {
    bold: bool,
    reverse: bool,
    foreground: Option<u16>,
    background: Option<u16>,
}

impl SgrState {
    fn apply(&mut self, sequence: &str) {
        let Some(parameters) = sequence
            .strip_prefix("\x1b[")
            .and_then(|s| s.strip_suffix('m'))
        else {
            return;
        };
        for parameter in parameters.split(';') {
            match parameter.parse::<u16>().ok() {
                Some(0) => *self = Self::default(),
                Some(1) => self.bold = true,
                Some(22) => self.bold = false,
                Some(7) => self.reverse = true,
                Some(27) => self.reverse = false,
                Some(30..=37 | 90..=97) => self.foreground = parameter.parse().ok(),
                Some(39) => self.foreground = None,
                Some(40..=47 | 100..=107) => self.background = parameter.parse().ok(),
                Some(49) => self.background = None,
                _ => {}
            }
        }
    }

    fn write<W: Write + ?Sized>(self, output: &mut W) -> io::Result<()> {
        if self.bold {
            output.write_all(b"\x1b[1m")?;
        }
        if self.reverse {
            output.write_all(b"\x1b[7m")?;
        }
        if let Some(code) = self.foreground {
            write!(output, "\x1b[{code}m")?;
        }
        if let Some(code) = self.background {
            write!(output, "\x1b[{code}m")?;
        }
        Ok(())
    }
}
