use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

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

/// A lazily opened seekable file source. The file itself is not opened until
/// the runner starts consuming it, so argument parsing remains side-effect
/// free and a source can be queued behind another source.
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_owned(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ChunkSource for FileSource {
    fn produce(self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        let file = std::fs::File::open(&self.path)?;
        ReaderSource::new(BufReader::new(file)).produce(emit)
    }
}

/// Concatenates lazily opened file operands using the same bounded reader as
/// stdin. It deliberately does not insert separators, matching `cat` and
/// preserving the exact bytes supplied by each operand.
pub struct FilesSource {
    paths: Vec<PathBuf>,
}

impl FilesSource {
    pub fn new(paths: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        Self {
            paths: paths
                .into_iter()
                .map(|path| path.as_ref().to_owned())
                .collect(),
        }
    }
}

impl ChunkSource for FilesSource {
    fn produce(self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        for path in self.paths {
            FileSource::new(path).produce(emit)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ChunkSource, FileSource, FilesSource};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path(name: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("scrl-{name}-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn file_sources_stream_operands_without_materializing_them() {
        let first = temporary_path("first");
        let second = temporary_path("second");
        fs::write(&first, "one\né\n").unwrap();
        fs::write(&second, "two\n").unwrap();
        let mut output = String::new();
        FilesSource::new([&first, &second])
            .produce(&mut |chunk| {
                output.push_str(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(output, "one\né\ntwo\n");
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
    }

    #[test]
    fn missing_file_is_reported_when_source_starts() {
        let path = temporary_path("missing");
        let error = FileSource::new(&path).produce(&mut |_| Ok(())).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
