use std::io::{self, Read};

use crate::Event;

pub(crate) fn read_event<R: Read>(input: &mut R) -> io::Result<Option<Event>> {
    let mut byte = [0; 1];
    if input.read(&mut byte)? == 0 {
        return Ok(None);
    }
    let first = match byte[0] {
        3 => Event::Interrupt,
        b'\n' | b'\r' => Event::Enter,
        8 | 127 => Event::Backspace,
        0x15 => Event::CtrlU,
        0x20..=0x7e => Event::Text(byte[0] as char),
        _ => Event::Escape,
    };
    if first != Event::Escape {
        return Ok(Some(first));
    }
    let mut sequence = [0; 2];
    if input.read_exact(&mut sequence).is_err() {
        return Ok(Some(Event::Escape));
    }
    Ok(Some(match sequence {
        [b'[', b'A'] => Event::Up,
        [b'[', b'B'] => Event::Down,
        [b'[', b'C'] => Event::Right,
        [b'[', b'D'] => Event::Left,
        _ => Event::Escape,
    }))
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
        raw.special_codes[termios::SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[termios::SpecialCodeIndex::VTIME] = 0;
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
}
