use std::io::{self, BufRead};

pub trait ChunkSource: Send + 'static {
    fn produce(self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()>;
}

pub struct ReaderSource<R> {
    reader: R,
    chunk_size: usize,
}

impl<R> ReaderSource<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            chunk_size: 16 * 1024,
        }
    }
}

impl<R: BufRead + Send + 'static> ChunkSource for ReaderSource<R> {
    fn produce(mut self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                return Ok(());
            }
            let mut take = buffer.len().min(self.chunk_size);
            while take > 0 && std::str::from_utf8(&buffer[..take]).is_err() {
                take -= 1;
            }
            if take == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "input is not valid UTF-8",
                ));
            }
            let chunk = std::str::from_utf8(&buffer[..take])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            emit(chunk)?;
            self.reader.consume(take);
        }
    }
}
