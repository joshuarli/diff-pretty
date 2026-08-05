use std::io::{self, Read};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::Event;

#[cfg(unix)]
static TERMINATED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signal: std::ffi::c_int, handler: usize) -> usize;
}

#[cfg(unix)]
unsafe extern "C" fn handle_term(_signal: std::ffi::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
pub(crate) struct SignalGuard {
    previous: usize,
}

#[cfg(unix)]
impl SignalGuard {
    pub(crate) fn install() -> Self {
        TERMINATED.store(false, Ordering::Relaxed);
        // SIGTERM is delivered asynchronously; the handler only flips an
        // atomic. The owning loop observes it and performs normal cleanup.
        let previous = unsafe { signal(15, handle_term as *const () as usize) };
        Self { previous }
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        unsafe {
            signal(15, self.previous);
        }
    }
}

#[cfg(unix)]
pub(crate) fn terminated() -> bool {
    TERMINATED.load(Ordering::Relaxed)
}

pub(crate) fn read_event<R: Read>(input: &mut R) -> io::Result<Option<Event>> {
    let mut decoder = KeyDecoder::new();
    let mut byte = [0; 1];
    loop {
        let count = input.read(&mut byte)?;
        if count == 0 {
            if let Some(event) = decoder.timed_out(Instant::now()) {
                return Ok(Some(event));
            }
            continue;
        }
        decoder.push(byte[0]);
        if let Some(event) = decoder.next() {
            return Ok(Some(event));
        }
    }
}

/// Decodes terminal bytes without assuming that an escape sequence or UTF-8
/// scalar arrives in one read. The fixed storage bounds malformed input and
/// lets the terminal adapter retain a decoder across reads.
pub(crate) struct KeyDecoder {
    bytes: [u8; 64],
    len: usize,
}

impl KeyDecoder {
    pub(crate) fn new() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, byte: u8) {
        if self.len == self.bytes.len() {
            self.consume(1);
        }
        self.bytes[self.len] = byte;
        self.len += 1;
    }

    pub(crate) fn next(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        match self.bytes[0] {
            3 => {
                self.consume(1);
                Some(Event::Interrupt)
            }
            b'\n' | b'\r' => {
                self.consume(1);
                Some(Event::Enter)
            }
            8 | 127 => {
                self.consume(1);
                Some(Event::Backspace)
            }
            0x15 => {
                self.consume(1);
                Some(Event::CtrlU)
            }
            0x20..=0x7e => {
                let character = self.bytes[0] as char;
                self.consume(1);
                Some(Event::Text(character))
            }
            0x1b => self.escape_event(),
            _ => self.utf8_event(),
        }
    }

    pub(crate) fn finish(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        self.consume(1);
        Some(Event::Escape)
    }

    #[allow(dead_code)]
    pub(crate) fn timed_out(&mut self, _at: Instant) -> Option<Event> {
        // The raw tty uses VTIME below, so a zero-byte read is the deadline.
        // Keeping the clock at this seam makes the policy explicit and keeps
        // the decoder usable with adapters that implement a finer deadline.
        self.finish()
    }

    fn escape_event(&mut self) -> Option<Event> {
        if self.len == 1 {
            return None;
        }
        if self.bytes[1] != b'[' {
            self.consume(1);
            return Some(Event::Escape);
        }
        let final_index = (2..self.len).find(|&index| (0x40..=0x7e).contains(&self.bytes[index]));
        let Some(final_index) = final_index else {
            return None;
        };
        let sequence = &self.bytes[2..final_index];
        let event = match self.bytes[final_index] {
            b'A' if sequence.is_empty() => Event::Up,
            b'B' if sequence.is_empty() => Event::Down,
            b'C' if sequence.is_empty() => Event::Right,
            b'D' if sequence.is_empty() => Event::Left,
            b'H' if sequence.is_empty() => Event::Home,
            b'F' if sequence.is_empty() => Event::End,
            b'~' => match sequence {
                b"1" | b"7" => Event::Home,
                b"4" | b"8" => Event::End,
                b"3" => Event::Delete,
                b"5" => Event::PageUp,
                b"6" => Event::PageDown,
                _ => Event::Escape,
            },
            _ => Event::Escape,
        };
        self.consume(final_index + 1);
        Some(event)
    }

    fn utf8_event(&mut self) -> Option<Event> {
        match std::str::from_utf8(&self.bytes[..self.len]) {
            Ok(text) => {
                let character = text.chars().next()?;
                self.consume(character.len_utf8());
                Some(Event::Text(character))
            }
            Err(error) if error.error_len().is_none() => None,
            Err(_) => {
                self.consume(1);
                Some(Event::Escape)
            }
        }
    }

    fn consume(&mut self, count: usize) {
        self.bytes.copy_within(count..self.len, 0);
        self.len -= count;
    }
}

#[cfg(unix)]
pub(crate) struct RawMode<'a> {
    tty: &'a std::fs::File,
    original: rustix::termios::Termios,
}

#[cfg(unix)]
impl<'a> RawMode<'a> {
    pub(crate) fn enter(tty: &'a std::fs::File) -> io::Result<Self> {
        use rustix::termios::{self, OptionalActions};
        let original = termios::tcgetattr(tty)?;
        let mut raw = original.clone();
        raw.make_raw();
        raw.special_codes[termios::SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[termios::SpecialCodeIndex::VTIME] = 1;
        termios::tcsetattr(tty, OptionalActions::Now, &raw)?;
        Ok(Self { tty, original })
    }
}

#[cfg(unix)]
impl Drop for RawMode<'_> {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            self.tty,
            rustix::termios::OptionalActions::Now,
            &self.original,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_control_and_arrow_events() {
        assert_eq!(read_event(&mut &b"\x1b[A"[..]).unwrap(), Some(Event::Up));
        assert_eq!(
            read_event(&mut &b"\x03"[..]).unwrap(),
            Some(Event::Interrupt)
        );
        assert_eq!(read_event(&mut &b"q"[..]).unwrap(), Some(Event::Text('q')));
    }

    #[test]
    fn decoder_handles_split_sequences_and_navigation() {
        let mut decoder = KeyDecoder::new();
        decoder.push(0x1b);
        assert_eq!(decoder.next(), None);
        decoder.push(b'[');
        decoder.push(b'6');
        decoder.push(b'~');
        assert_eq!(decoder.next(), Some(Event::PageDown));

        for byte in "é".as_bytes().iter().copied().take(1) {
            decoder.push(byte);
        }
        assert_eq!(decoder.next(), None);
        decoder.push("é".as_bytes()[1]);
        assert_eq!(decoder.next(), Some(Event::Text('é')));
    }

    #[test]
    fn decoder_recognizes_home_end_delete_and_bare_escape() {
        let mut decoder = KeyDecoder::new();
        for byte in b"\x1b[H\x1b[F\x1b[3~" {
            decoder.push(*byte);
            if let Some(event) = decoder.next() {
                assert!(matches!(event, Event::Home | Event::End | Event::Delete));
            }
        }
        decoder.push(0x1b);
        assert_eq!(decoder.next(), None);
        assert_eq!(decoder.timed_out(Instant::now()), Some(Event::Escape));
    }
}
