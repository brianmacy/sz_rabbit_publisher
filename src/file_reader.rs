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

/// Reads lines from a file, automatically handling gzip compression
pub struct FileReader {
    lines: Vec<String>,
    current_index: usize,
}

impl FileReader {
    /// Opens a file and reads all lines into memory
    /// Automatically detects and handles gzip compression
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let is_gzip = is_gzip_file(path).await?;

        let lines = if is_gzip {
            Self::read_gzip_lines(path)?
        } else {
            Self::read_plain_lines(path).await?
        };

        Ok(Self {
            lines,
            current_index: 0,
        })
    }

    /// Reads lines from a plain text file
    async fn read_plain_lines<P: AsRef<Path>>(path: P) -> Result<Vec<String>> {
        let content = tokio::fs::read_to_string(path.as_ref())
            .await
            .context("Failed to read plain text file")?;

        Ok(content
            .lines()
            .filter(|line| !line.is_empty())
            .map(|s| s.to_string())
            .collect())
    }

    /// Reads lines from a gzip-compressed file
    fn read_gzip_lines<P: AsRef<Path>>(path: P) -> Result<Vec<String>> {
        let file = std::fs::File::open(path.as_ref()).context("Failed to open gzip file")?;
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);

        let mut lines = Vec::new();
        for line in reader.lines() {
            let line = line.context("Failed to read line from gzip file")?;
            if !line.is_empty() {
                lines.push(line);
            }
        }

        Ok(lines)
    }

    /// Returns the next line from the file, or None if EOF
    pub fn next_line(&mut self) -> Option<String> {
        if self.current_index < self.lines.len() {
            let line = self.lines[self.current_index].clone();
            self.current_index += 1;
            Some(line)
        } else {
            None
        }
    }

    /// Returns the total number of lines in the file
    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    /// Returns the number of lines remaining to read
    pub fn remaining_lines(&self) -> usize {
        self.lines.len().saturating_sub(self.current_index)
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

        assert_eq!(reader.total_lines(), 3);
        assert_eq!(reader.next_line(), Some("line1".to_string()));
        assert_eq!(reader.next_line(), Some("line2".to_string()));
        assert_eq!(reader.next_line(), Some("line3".to_string()));
        assert_eq!(reader.next_line(), None);
    }

    #[tokio::test]
    async fn test_read_empty_lines_filtered() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        temp_file.write_all(b"\n").unwrap();
        writeln!(temp_file, "line2").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.total_lines(), 2);
        assert_eq!(reader.next_line(), Some("line1".to_string()));
        assert_eq!(reader.next_line(), Some("line2".to_string()));
        assert_eq!(reader.next_line(), None);
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

        assert_eq!(reader.total_lines(), 3);
        assert_eq!(reader.next_line(), Some("line1".to_string()));
        assert_eq!(reader.next_line(), Some("line2".to_string()));
        assert_eq!(reader.next_line(), Some("line3".to_string()));
        assert_eq!(reader.next_line(), None);
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
    async fn test_remaining_lines() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        writeln!(temp_file, "line2").unwrap();
        writeln!(temp_file, "line3").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.remaining_lines(), 3);
        reader.next_line();
        assert_eq!(reader.remaining_lines(), 2);
        reader.next_line();
        assert_eq!(reader.remaining_lines(), 1);
        reader.next_line();
        assert_eq!(reader.remaining_lines(), 0);
    }

    #[tokio::test]
    async fn test_file_not_found() {
        let result = FileReader::open("/nonexistent/file.txt").await;
        assert!(result.is_err());
    }
}
