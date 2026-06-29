use anyhow::{Context, Result};
use bzip2::read::MultiBzDecoder;
use flate2::read::GzDecoder;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

/// Magic bytes for gzip files
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Magic bytes for bzip2 files (`BZh`, followed by a `1`–`9` block-size digit)
const BZIP2_MAGIC: [u8; 3] = [0x42, 0x5a, 0x68];

/// Reads the first `N` bytes of a file for magic-byte sniffing. Returns the
/// bytes actually read (a short/empty file yields fewer than `N`).
async fn read_magic<P: AsRef<Path>, const N: usize>(path: P) -> Result<([u8; N], usize)> {
    let mut file = File::open(path.as_ref())
        .await
        .context("Failed to open file for compression detection")?;
    let mut magic = [0u8; N];
    let bytes_read = file
        .read(&mut magic)
        .await
        .context("Failed to read magic bytes")?;
    Ok((magic, bytes_read))
}

/// Detects if a file is gzip-compressed by checking magic bytes
pub async fn is_gzip_file<P: AsRef<Path>>(path: P) -> Result<bool> {
    let (magic, n) = read_magic::<_, 2>(path).await?;
    Ok(n == 2 && magic == GZIP_MAGIC)
}

/// Detects if a file is bzip2-compressed by checking magic bytes
pub async fn is_bz2_file<P: AsRef<Path>>(path: P) -> Result<bool> {
    let (magic, n) = read_magic::<_, 3>(path).await?;
    Ok(n == 3 && magic == BZIP2_MAGIC)
}

/// Active reader variant — plain text, gzip-, or bzip2-compressed.
/// `Bz2` uses `MultiBzDecoder` so concatenated bzip2 streams (e.g. files
/// produced by `pbzip2`/`lbzip2`) are fully decoded, not just the first stream.
/// Decode is single-threaded per file; concurrency across multiple files comes
/// from `--parallel`.
enum LineReader {
    Plain(BufReader<std::fs::File>),
    Gzip(BufReader<GzDecoder<std::fs::File>>),
    Bz2(BufReader<MultiBzDecoder<std::fs::File>>),
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

        let reader = if is_gzip_file(path).await? {
            let file = std::fs::File::open(path).context("Failed to open gzip file")?;
            LineReader::Gzip(BufReader::new(GzDecoder::new(file)))
        } else if is_bz2_file(path).await? {
            let file = std::fs::File::open(path).context("Failed to open bzip2 file")?;
            LineReader::Bz2(BufReader::new(MultiBzDecoder::new(file)))
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
                LineReader::Bz2(r) => r.read_line(&mut buf),
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
    async fn test_read_bz2_file() {
        use bzip2::Compression;
        use bzip2::write::BzEncoder;

        let mut temp_file = NamedTempFile::new().unwrap();

        // Write bzip2-compressed content
        {
            let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
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
    async fn test_read_bz2_concatenated_streams() {
        // Files produced by pbzip2/lbzip2 are multiple independent bzip2 streams
        // concatenated. MultiBzDecoder must decode ALL of them; a plain
        // single-stream decoder would silently stop after the first.
        use bzip2::Compression;
        use bzip2::write::BzEncoder;

        let stream = |lines: &[&str]| {
            let mut enc = BzEncoder::new(Vec::new(), Compression::default());
            for l in lines {
                writeln!(enc, "{l}").unwrap();
            }
            enc.finish().unwrap()
        };

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&stream(&["a1", "a2"])).unwrap();
        temp_file.write_all(&stream(&["b1", "b2"])).unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.next_line().unwrap().unwrap(), "a1");
        assert_eq!(reader.next_line().unwrap().unwrap(), "a2");
        assert_eq!(reader.next_line().unwrap().unwrap(), "b1");
        assert_eq!(reader.next_line().unwrap().unwrap(), "b2");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 4);
    }

    #[tokio::test]
    async fn test_is_bz2_file_detection() {
        use bzip2::Compression;
        use bzip2::write::BzEncoder;

        // Plain text is not bz2
        let mut plain_file = NamedTempFile::new().unwrap();
        writeln!(plain_file, "plain text").unwrap();
        plain_file.flush().unwrap();
        assert!(!is_bz2_file(plain_file.path()).await.unwrap());

        // bzip2 file is detected, and is not mistaken for gzip
        let mut bz2_file = NamedTempFile::new().unwrap();
        let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
        writeln!(encoder, "compressed").unwrap();
        let compressed = encoder.finish().unwrap();
        bz2_file.write_all(&compressed).unwrap();
        bz2_file.flush().unwrap();

        assert!(is_bz2_file(bz2_file.path()).await.unwrap());
        assert!(!is_gzip_file(bz2_file.path()).await.unwrap());
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
