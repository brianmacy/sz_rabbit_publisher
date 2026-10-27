use anyhow::{Context, Result};
use bzip2::read::MultiBzDecoder;
use flate2::read::MultiGzDecoder;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use zstd::stream::read::Decoder as ZstdDecoder;

/// Magic bytes for gzip files
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Magic bytes for bzip2 files (`BZh`, followed by a `1`–`9` block-size digit)
const BZIP2_MAGIC: [u8; 3] = [0x42, 0x5a, 0x68];

/// Magic bytes for a zstd frame (`0xFD2FB528`, little-endian on disk)
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// A zstd *skippable* frame starts with `0x184D2A5?` (little-endian `5? 2A 4D 18`).
/// Some encoders (e.g. `zstd --rsyncable` wrappers, seekable-format writers) emit one
/// before the first data frame; the decoder skips it transparently.
const ZSTD_SKIPPABLE_MAGIC_TAIL: [u8; 3] = [0x2a, 0x4d, 0x18];

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

/// Detects if a file is zstd-compressed by checking magic bytes (a data frame, or a
/// skippable frame preceding one)
pub async fn is_zstd_file<P: AsRef<Path>>(path: P) -> Result<bool> {
    let (magic, n) = read_magic::<_, 4>(path).await?;
    Ok(n == 4
        && (magic == ZSTD_MAGIC
            || (magic[0] & 0xf0 == 0x50 && magic[1..] == ZSTD_SKIPPABLE_MAGIC_TAIL)))
}

/// Active reader variant — plain text, gzip-, bzip2-, or zstd-compressed.
/// `Gzip` uses `MultiGzDecoder` so multi-member gzip files (`cat a.gz b.gz`,
/// `pigz`, `bgzip`) are fully decoded; a single-member decoder silently stops
/// at the end of the first member, dropping the rest of the file.
/// `Bz2` uses `MultiBzDecoder` so concatenated bzip2 streams (e.g. files
/// produced by `pbzip2`/`lbzip2`) are fully decoded, not just the first stream.
/// `Zstd` uses the `zstd` crate's streaming decoder, which decodes concatenated
/// frames back to back (e.g. `cat a.zst b.zst`, `pzstd` output) and skips skippable
/// frames, so multi-frame files are fully decoded.
/// Decode is single-threaded per file; concurrency across multiple files comes
/// from `--parallel`.
enum LineReader {
    Plain(BufReader<std::fs::File>),
    Gzip(BufReader<MultiGzDecoder<std::fs::File>>),
    Bz2(BufReader<MultiBzDecoder<std::fs::File>>),
    Zstd(BufReader<ZstdDecoder<'static, BufReader<std::fs::File>>>),
}

/// Streams lines lazily from a file, automatically handling gzip, bzip2 and zstd compression
pub struct FileReader {
    reader: LineReader,
    lines_read: u64,
}

impl FileReader {
    /// Opens a file for lazy line-by-line reading.
    /// Automatically detects (by magic bytes) and decompresses gzip, bzip2 and zstd.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        let reader = if is_gzip_file(path).await? {
            let file = std::fs::File::open(path).context("Failed to open gzip file")?;
            LineReader::Gzip(BufReader::new(MultiGzDecoder::new(file)))
        } else if is_bz2_file(path).await? {
            let file = std::fs::File::open(path).context("Failed to open bzip2 file")?;
            LineReader::Bz2(BufReader::new(MultiBzDecoder::new(file)))
        } else if is_zstd_file(path).await? {
            let file = std::fs::File::open(path).context("Failed to open zstd file")?;
            let decoder = ZstdDecoder::new(file).context("Failed to initialise zstd decoder")?;
            LineReader::Zstd(BufReader::new(decoder))
        } else {
            let file = std::fs::File::open(path).context("Failed to open plain text file")?;
            LineReader::Plain(BufReader::new(file))
        };

        Ok(Self {
            reader,
            lines_read: 0,
        })
    }

    /// Reads one raw line (including its terminator) into `buf`. Returns the
    /// byte count; `0` means EOF.
    fn read_raw_line(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        buf.clear();
        match &mut self.reader {
            LineReader::Plain(r) => r.read_until(b'\n', buf),
            LineReader::Gzip(r) => r.read_until(b'\n', buf),
            LineReader::Bz2(r) => r.read_until(b'\n', buf),
            LineReader::Zstd(r) => r.read_until(b'\n', buf),
        }
    }

    /// Returns the next non-empty line from the file as raw bytes, or `None` at
    /// EOF. Lines are NOT required to be UTF-8: the publisher is a transport and
    /// must never drop a record, so a line with invalid UTF-8 is published
    /// verbatim (the consumer decides whether to reject it). Only genuine I/O or
    /// decompression errors (e.g. a truncated gzip) are returned as `Err`; the
    /// caller must treat those as fatal, because every record after the error is
    /// unreadable.
    pub fn next_line(&mut self) -> Option<Result<Vec<u8>>> {
        let mut buf = Vec::new();
        loop {
            match self.read_raw_line(&mut buf) {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let len = trimmed_len(&buf);
                    if len == 0 {
                        continue; // skip empty lines
                    }
                    buf.truncate(len);
                    self.lines_read += 1;
                    return Some(Ok(buf));
                }
                Err(e) => {
                    return Some(Err(anyhow::Error::new(e).context(format!(
                        "Failed to read record {} from file",
                        self.lines_read + 1
                    ))));
                }
            }
        }
    }

    /// Skips (reads and discards) the first `n` non-empty records, for resuming
    /// an interrupted publish. Empty lines are consumed but not counted, exactly
    /// as `next_line` filters them, so `skip(n)` lines up with the `total` a prior
    /// run reported. Compressed inputs have no seek, so this decodes through the
    /// stream. Returns the number actually skipped (fewer than `n` only if EOF is
    /// reached first). Does not affect `lines_read` (which counts this run's reads).
    ///
    /// Skipped records are NOT published. Over-skipping loses records; only
    /// under-skipping is safe (re-sent records are absorbed by the engine's
    /// idempotent `add_record`).
    pub fn skip(&mut self, n: u64) -> Result<u64> {
        let mut skipped = 0u64;
        let mut buf = Vec::new();
        while skipped < n {
            match self.read_raw_line(&mut buf) {
                Ok(0) => break, // EOF before N — caller sees fewer skipped
                Ok(_) => {
                    if trimmed_len(&buf) == 0 {
                        continue; // empty lines are not records (mirrors next_line)
                    }
                    skipped += 1;
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context("Failed to read line during skip"));
                }
            }
        }
        Ok(skipped)
    }

    /// Returns the number of lines read so far
    pub fn lines_read(&self) -> u64 {
        self.lines_read
    }
}

/// Length of `line` without its trailing `\n` and ALL trailing `\r`s
/// (same as the original `trim_end_matches('\n').trim_end_matches('\r')`).
fn trimmed_len(line: &[u8]) -> usize {
    let without_lf = line.len() - line.iter().rev().take_while(|b| **b == b'\n').count();
    without_lf
        - line[..without_lf]
            .iter()
            .rev()
            .take_while(|b| **b == b'\r')
            .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_trimmed_len_strips_all_trailing_cr() {
        assert_eq!(trimmed_len(b"abc\n"), 3);
        assert_eq!(trimmed_len(b"abc\r\n"), 3);
        assert_eq!(trimmed_len(b"abc\r\r\n"), 3);
        assert_eq!(trimmed_len(b"abc\r\r"), 3);
        assert_eq!(trimmed_len(b"a\rc\n"), 3);
        assert_eq!(trimmed_len(b"\r\r\n"), 0);
        assert_eq!(trimmed_len(b""), 0);
    }

    #[tokio::test]
    async fn test_read_plain_text_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "line1").unwrap();
        writeln!(temp_file, "line2").unwrap();
        writeln!(temp_file, "line3").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();

        assert_eq!(reader.next_line().unwrap().unwrap(), b"line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line3");
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

        assert_eq!(reader.next_line().unwrap().unwrap(), b"line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line2");
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

        assert_eq!(reader.next_line().unwrap().unwrap(), b"line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line3");
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

        assert_eq!(reader.next_line().unwrap().unwrap(), b"line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line3");
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
        assert_eq!(reader.next_line().unwrap().unwrap(), b"a1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"a2");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"b1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"b2");
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

    #[tokio::test]
    async fn test_skip_lines_resume() {
        let mut temp_file = NamedTempFile::new().unwrap();
        for i in 1..=5 {
            writeln!(temp_file, "line{i}").unwrap();
        }
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.skip(2).unwrap(), 2);
        // First record after skipping the first two is line3.
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line3");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line4");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line5");
        assert!(reader.next_line().is_none());
        // lines_read counts only this run's reads (post-skip), not skipped records.
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_skip_lines_counts_only_non_empty() {
        // Empty lines are consumed but not counted, mirroring next_line's filter,
        // so skip(2) lands past the second *record*, not the second physical line.
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "rec1").unwrap();
        temp_file.write_all(b"\n").unwrap(); // blank line between records
        writeln!(temp_file, "rec2").unwrap();
        writeln!(temp_file, "rec3").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.skip(2).unwrap(), 2);
        assert_eq!(reader.next_line().unwrap().unwrap(), b"rec3");
    }

    #[tokio::test]
    async fn test_skip_past_eof_returns_fewer() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "only1").unwrap();
        writeln!(temp_file, "only2").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        // Asking to skip more than exist stops at EOF and reports the real count.
        assert_eq!(reader.skip(10).unwrap(), 2);
        assert!(reader.next_line().is_none());
    }

    #[tokio::test]
    async fn test_invalid_utf8_line_is_returned_verbatim() {
        // A non-UTF-8 line must not end the read (that would silently drop every
        // record after it). It is returned byte-for-byte and reading continues.
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "before").unwrap();
        temp_file.write_all(b"{\"bad\":\"\xff\xfe\"}\r\n").unwrap();
        writeln!(temp_file, "after").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.next_line().unwrap().unwrap(), b"before");
        assert_eq!(
            reader.next_line().unwrap().unwrap(),
            b"{\"bad\":\"\xff\xfe\"}".to_vec()
        );
        assert_eq!(reader.next_line().unwrap().unwrap(), b"after");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_skip_over_invalid_utf8() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"\xff\n").unwrap();
        writeln!(temp_file, "next").unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.skip(1).unwrap(), 1);
        assert_eq!(reader.next_line().unwrap().unwrap(), b"next");
    }

    #[tokio::test]
    async fn test_truncated_gzip_is_an_error() {
        // A truncated/corrupt compressed file must surface as Err (never a quiet
        // EOF), because every record after the corruption is unreadable.
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        for i in 0..5000 {
            writeln!(encoder, "{{\"id\":{i},\"pad\":\"{i:0>64}\"}}").unwrap();
        }
        let compressed = encoder.finish().unwrap();
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file
            .write_all(&compressed[..compressed.len() / 2])
            .unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        let mut ok = 0u64;
        let err = loop {
            match reader.next_line() {
                Some(Ok(_)) => ok += 1,
                Some(Err(e)) => break e,
                None => panic!("truncated gzip reached a clean EOF after {ok} records"),
            }
        };
        assert!(ok > 0 && ok < 5000, "read {ok} records before the error");
        assert!(
            format!("{err:#}").contains(&format!("record {}", ok + 1)),
            "error must name the failing record: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_read_gzip_multi_member() {
        // Concatenated gzip members (cat a.gz b.gz, pigz, bgzip) must ALL be read.
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let member = |lines: &[&str]| {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            for l in lines {
                writeln!(enc, "{l}").unwrap();
            }
            enc.finish().unwrap()
        };
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&member(&["a1", "a2"])).unwrap();
        temp_file.write_all(&member(&["b1", "b2"])).unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        for want in ["a1", "a2", "b1", "b2"] {
            assert_eq!(reader.next_line().unwrap().unwrap(), want.as_bytes());
        }
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 4);
    }

    /// Builds a zstd frame holding `lines` (one per line).
    fn zstd_frame(lines: &[String]) -> Vec<u8> {
        let mut text = Vec::new();
        for l in lines {
            writeln!(text, "{l}").unwrap();
        }
        zstd::stream::encode_all(&text[..], 3).unwrap()
    }

    /// Drains a reader into a Vec, failing the test on any read error.
    fn drain(reader: &mut FileReader) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(line) = reader.next_line() {
            out.push(line.unwrap());
        }
        out
    }

    fn sample_records(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                format!("{{\"DATA_SOURCE\":\"T\",\"RECORD_ID\":\"{i}\",\"pad\":\"{i:0>96}\"}}")
            })
            .collect()
    }

    #[tokio::test]
    async fn test_read_zstd_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file
            .write_all(&zstd_frame(&[
                "line1".into(),
                "line2".into(),
                "line3".into(),
            ]))
            .unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line1");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line2");
        assert_eq!(reader.next_line().unwrap().unwrap(), b"line3");
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 3);
    }

    #[tokio::test]
    async fn test_zstd_matches_plain_next_line_and_skip() {
        // Large enough to span many decoder buffer refills (~1.4 MB decoded).
        let records = sample_records(10_000);
        let mut plain = NamedTempFile::new().unwrap();
        for r in &records {
            writeln!(plain, "{r}").unwrap();
        }
        plain.flush().unwrap();
        let mut zst = NamedTempFile::with_suffix(".jsonl.zst").unwrap();
        zst.write_all(&zstd_frame(&records)).unwrap();
        zst.flush().unwrap();

        // next_line path
        let want = drain(&mut FileReader::open(plain.path()).await.unwrap());
        let got = drain(&mut FileReader::open(zst.path()).await.unwrap());
        assert_eq!(want.len(), 10_000);
        assert_eq!(got, want);

        // skip path (resume): skip then drain must equal the plain tail
        let mut p = FileReader::open(plain.path()).await.unwrap();
        let mut z = FileReader::open(zst.path()).await.unwrap();
        assert_eq!(p.skip(7_321).unwrap(), 7_321);
        assert_eq!(z.skip(7_321).unwrap(), 7_321);
        assert_eq!(drain(&mut z), drain(&mut p));
        assert_eq!(z.lines_read(), 10_000 - 7_321);

        // skip past EOF reports the real count
        let mut z = FileReader::open(zst.path()).await.unwrap();
        assert_eq!(z.skip(20_000).unwrap(), 10_000);
        assert!(z.next_line().is_none());
    }

    #[tokio::test]
    async fn test_read_zstd_multi_frame() {
        // Concatenated frames (cat a.zst b.zst, pzstd) must ALL be read.
        let a: Vec<String> = vec!["a1".into(), "a2".into()];
        let b: Vec<String> = vec!["b1".into(), "b2".into()];
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&zstd_frame(&a)).unwrap();
        temp_file.write_all(&zstd_frame(&b)).unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        for want in ["a1", "a2", "b1", "b2"] {
            assert_eq!(reader.next_line().unwrap().unwrap(), want.as_bytes());
        }
        assert!(reader.next_line().is_none());
        assert_eq!(reader.lines_read(), 4);
    }

    #[tokio::test]
    async fn test_zstd_record_split_across_frames() {
        // Frame boundaries need not align to line boundaries.
        let mut full = Vec::new();
        full.extend_from_slice(&zstd::stream::encode_all(&b"rec1\nre"[..], 3).unwrap());
        full.extend_from_slice(&zstd::stream::encode_all(&b"c2\nrec3\n"[..], 3).unwrap());
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&full).unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(
            drain(&mut reader),
            vec![b"rec1".to_vec(), b"rec2".to_vec(), b"rec3".to_vec()]
        );
    }

    #[tokio::test]
    async fn test_read_zstd_with_skippable_frame_prefix() {
        let mut full = Vec::new();
        full.extend_from_slice(&[0x5b, 0x2a, 0x4d, 0x18]); // skippable frame magic 0x184D2A5B
        full.extend_from_slice(&4u32.to_le_bytes());
        full.extend_from_slice(b"meta");
        full.extend_from_slice(&zstd_frame(&["x1".into(), "x2".into()]));
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&full).unwrap();
        temp_file.flush().unwrap();

        assert!(is_zstd_file(temp_file.path()).await.unwrap());
        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        assert_eq!(drain(&mut reader), vec![b"x1".to_vec(), b"x2".to_vec()]);
    }

    #[tokio::test]
    async fn test_is_zstd_file_detection() {
        let mut plain_file = NamedTempFile::new().unwrap();
        writeln!(plain_file, "plain text").unwrap();
        plain_file.flush().unwrap();
        assert!(!is_zstd_file(plain_file.path()).await.unwrap());

        let mut short_file = NamedTempFile::new().unwrap();
        short_file.write_all(&ZSTD_MAGIC[..3]).unwrap();
        short_file.flush().unwrap();
        assert!(!is_zstd_file(short_file.path()).await.unwrap());

        let mut zst_file = NamedTempFile::new().unwrap();
        zst_file
            .write_all(&zstd_frame(&["compressed".into()]))
            .unwrap();
        zst_file.flush().unwrap();
        assert!(is_zstd_file(zst_file.path()).await.unwrap());
        assert!(!is_gzip_file(zst_file.path()).await.unwrap());
        assert!(!is_bz2_file(zst_file.path()).await.unwrap());
    }

    #[tokio::test]
    async fn test_truncated_zstd_is_an_error() {
        let records = sample_records(5_000);
        let compressed = zstd_frame(&records);
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file
            .write_all(&compressed[..compressed.len() / 2])
            .unwrap();
        temp_file.flush().unwrap();

        let mut reader = FileReader::open(temp_file.path()).await.unwrap();
        let mut ok = 0u64;
        let err = loop {
            match reader.next_line() {
                Some(Ok(_)) => ok += 1,
                Some(Err(e)) => break e,
                None => panic!("truncated zstd reached a clean EOF after {ok} records"),
            }
        };
        assert!(ok > 0 && ok < 5000, "read {ok} records before the error");
        assert!(
            format!("{err:#}").contains(&format!("record {}", ok + 1)),
            "error must name the failing record: {err:#}"
        );
    }
}
