use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

/// Magic bytes for gzip files
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Detects if a file is gzip-compressed by checking magic bytes
pub async fn is_gzip_file<P: AsRef<Path>>(path: P) -> Result<bool> {
    let mut file = File::open(path.as_ref())
        .await
        .context("Failed to open file for gzip detection")?;

    let mut magic = [0u8; 2];
    let bytes_read = file
        .read(&mut magic)
        .await
        .context("Failed to read magic bytes")?;

    Ok(bytes_read == 2 && magic == GZIP_MAGIC)
}

/// Active reader variant — plain text or gzip-compressed
enum LineReader {
    Plain(BufReader<std::fs::File>),
    Gzip(BufReader<GzDecoder<std::fs::File>>),
}

/// Streams lines lazily from a file, automatically handling gzip compression
pub struct FileReader {
    reader: LineReader,
    lines_read: u64,
}

impl FileReader {
    /// Opens a file for lazy line-by-line reading.
    /// Automatically detects and handles gzip compression.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let is_gzip = is_gzip_file(path).await?;

        let reader = if is_gzip {
            let file = std::fs::File::open(path).context("Failed to open gzip file")?;
            let decoder = GzDecoder::new(file);
            LineReader::Gzip(BufReader::new(decoder))
        } else {
            let file = std::fs::File::open(path).context("Failed to open plain text file")?;
            LineReader::Plain(BufReader::new(file))
        };

        Ok(Self {
            reader,
            lines_read: 0,
        })
    }

    /// Returns the next non-empty line from the file, or `None` at EOF.
    /// I/O errors are propagated via `Result`.
    pub fn next_line(&mut self) -> Option<Result<String>> {
        let mut buf = String::new();
        loop {
            buf.clear();
            let bytes = match &mut self.reader {
                LineReader::Plain(r) => r.read_line(&mut buf),
                LineReader::Gzip(r) => r.read_line(&mut buf),
            };

            match bytes {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if trimmed.is_empty() {
                        continue; // skip empty lines
                    }
                    self.lines_read += 1;
                    return Some(Ok(trimmed.to_string()));
                }
                Err(e) => {
                    return Some(Err(
                        anyhow::Error::new(e).context("Failed to read line from file")
                    ));
                }
            }
        }
    }

    /// Returns the number of lines read so far
    pub fn lines_read(&self) -> u64 {
        self.lines_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_read_plain_text_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        writeln!(temp_file, "line2").unwrap();
        writeln!(temp_file, "line3").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.next_line().unwrap().unwrap(), "line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), "line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), "line3");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_read_empty_lines_filtered() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        temp_file.write_all(b"\n").unwrap();
        writeln!(temp_file, "line2").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.next_line().unwrap().unwrap(), "line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), "line2");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 2);
    }

    #[tokio::test]
    async fn test_read_gzip_file() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut temp_file = NamedTempFile::new().unwrap();

        // Write gzip-compressed content
        {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            writeln!(encoder, "line1").unwrap();
            writeln!(encoder, "line2").unwrap();
            writeln!(encoder, "line3").unwrap();
            let compressed = encoder.finish().unwrap();
            temp_file.write_all(&compressed).unwrap();
            temp_file.flush().unwrap();
        }

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.next_line().unwrap().unwrap(), "line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), "line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), "line3");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_is_gzip_file_detection() {
        // Test plain text file
        let mut plain_file = NamedTempFile::new().unwrap();
        writeln!(plain_file, "plain text").unwrap();
        plain_file.flush().unwrap();
        assert!(!is_gzip_file(plain_file.path()).await.unwrap());

        // Test gzip file
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut gzip_file = NamedTempFile::new().unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        writeln!(encoder, "compressed").unwrap();
        let compressed = encoder.finish().unwrap();
        gzip_file.write_all(&compressed).unwrap();
        gzip_file.flush().unwrap();

        assert!(is_gzip_file(gzip_file.path()).await.unwrap());
    }

    #[tokio::test]
    async fn test_lines_read_increments() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        writeln!(temp_file, "line2").unwrap();
        writeln!(temp_file, "line3").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.lines_read(), 0);
        reader.next_line();
        assert_eq!(reader.lines_read(), 1);
        reader.next_line();
        assert_eq!(reader.lines_read(), 2);
        reader.next_line();
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_file_not_found() {
        let result = FileReader::open("/nonexistent/file.txt").await;
        assert!(result.is_err());
    }
}
