//! Byte-oriented readers for Pi's native JSONL session history.
//!
//! The reader deliberately searches for LF delimiters in bytes and only
//! decodes a complete record after its boundaries are known. This keeps UTF-8
//! characters from being split by a chunk boundary and lets older pages be
//! read from a cursor without scanning the file from byte zero.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

const DEFAULT_CHUNK_BYTES: u64 = 64 * 1024;

/// One complete JSONL record read in reverse. Offsets are byte offsets in the
/// source file; `end_offset` is exclusive and includes the record delimiter
/// when one was present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseRecord {
    pub start_offset: u64,
    pub end_offset: u64,
    pub bytes: Vec<u8>,
}

/// Reads complete JSONL records backwards from an exclusive upper bound.
///
/// The upper bound must be a committed record boundary supplied by the
/// history index. The reader never treats a partial trailing record as part of
/// the historical view.
pub struct ReverseJsonlReader {
    file: File,
    cursor: u64,
    chunk_bytes: u64,
    max_record_bytes: usize,
    bytes_read: u64,
    records_read: u64,
}

impl ReverseJsonlReader {
    /// Creates a reader whose first read is bounded by `upper_bound`.
    #[must_use]
    pub fn new(file: File, upper_bound: u64, max_record_bytes: usize) -> Self {
        Self {
            file,
            cursor: upper_bound,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            max_record_bytes,
            bytes_read: 0,
            records_read: 0,
        }
    }

    /// Creates a reader with a smaller chunk size for boundary-focused tests.
    #[cfg(test)]
    fn with_chunk_bytes(
        file: File,
        upper_bound: u64,
        max_record_bytes: usize,
        chunk_bytes: u64,
    ) -> Self {
        Self {
            file,
            cursor: upper_bound,
            chunk_bytes: chunk_bytes.max(1),
            max_record_bytes,
            bytes_read: 0,
            records_read: 0,
        }
    }

    /// Returns the next complete record in reverse chronological order.
    ///
    /// Blank records are ignored. The returned record's `start_offset` is the
    /// exact line-start offset needed for the next page cursor.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a range cannot be read or a complete record
    /// exceeds `max_record_bytes`.
    pub fn next_record(&mut self) -> io::Result<Option<ReverseRecord>> {
        loop {
            let Some(record) = self.read_previous_record()? else {
                return Ok(None);
            };
            if !record.bytes.is_empty() {
                self.records_read = self.records_read.saturating_add(1);
                return Ok(Some(record));
            }
        }
    }

    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    #[must_use]
    pub fn records_read(&self) -> u64 {
        self.records_read
    }

    fn read_previous_record(&mut self) -> io::Result<Option<ReverseRecord>> {
        if self.cursor == 0 {
            return Ok(None);
        }

        let original_end = self.cursor;
        let mut end = original_end;
        // A committed JSONL boundary normally points just after LF. Strip the
        // delimiter before searching for the preceding record. Also tolerate
        // CRLF files without allowing CR to become part of the JSON payload.
        while end > 0 {
            let byte = self.read_byte(end - 1)?;
            if byte == b'\n' || byte == b'\r' {
                end -= 1;
            } else {
                break;
            }
        }
        if end == 0 {
            self.cursor = 0;
            return Ok(None);
        }

        let mut scan_end = end;
        let mut chunks_read = 0_u64;
        loop {
            let chunk_start = scan_end.saturating_sub(self.chunk_bytes);
            let chunk = self.read_range(chunk_start, scan_end)?;
            let mut newline = None;
            for index in (0..chunk.len()).rev() {
                if chunk[index] == b'\n' {
                    newline = Some(chunk_start + u64::try_from(index).unwrap_or(u64::MAX));
                    break;
                }
            }
            if let Some(newline_offset) = newline {
                let start = newline_offset.saturating_add(1);
                let bytes = self.read_range(start, end)?;
                self.ensure_record_size(bytes.len())?;
                self.cursor = start;
                return Ok(Some(ReverseRecord {
                    start_offset: start,
                    end_offset: original_end,
                    bytes,
                }));
            }
            if chunk_start == 0 {
                let bytes = self.read_range(0, end)?;
                self.ensure_record_size(bytes.len())?;
                self.cursor = 0;
                return Ok(Some(ReverseRecord {
                    start_offset: 0,
                    end_offset: original_end,
                    bytes,
                }));
            }
            scan_end = chunk_start;
            chunks_read = chunks_read.saturating_add(1);
            // Keep the variable meaningful in debug builds and make the
            // potentially long-record path explicit. The size check below is
            // authoritative; this counter documents that chunk boundaries do
            // not imply record boundaries.
            let _ = chunks_read;
        }
    }

    fn read_byte(&mut self, offset: u64) -> io::Result<u8> {
        let bytes = self.read_range(offset, offset.saturating_add(1))?;
        bytes.first().copied().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "missing JSONL boundary byte")
        })
    }

    fn read_range(&mut self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        if end <= start {
            return Ok(Vec::new());
        }
        let length = end.saturating_sub(start);
        let length_usize = usize::try_from(length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL range exceeds addressable memory",
            )
        })?;
        let mut bytes = vec![0_u8; length_usize];
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut bytes)?;
        self.bytes_read = self
            .bytes_read
            .saturating_add(u64::try_from(length_usize).unwrap_or(u64::MAX));
        Ok(bytes)
    }

    fn ensure_record_size(&self, size: usize) -> io::Result<()> {
        if size > self.max_record_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Pi session entry exceeds the supported line limit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::ReverseJsonlReader;

    #[test]
    fn reads_records_backwards_across_small_chunks() {
        let mut file = NamedTempFile::new().expect("temp file");
        write!(file, "first\n第二条\nthird").expect("write JSONL-like records");
        file.as_file_mut().flush().expect("flush file");
        let size = file.as_file().metadata().expect("metadata").len();
        let mut reader = ReverseJsonlReader::with_chunk_bytes(
            File::open(file.path()).expect("open file"),
            size,
            1024,
            3,
        );
        let third = reader.next_record().expect("third read").expect("third");
        assert_eq!(third.start_offset, 16);
        assert_eq!(third.bytes, b"third");
        let second = reader.next_record().expect("second read").expect("second");
        assert_eq!(second.bytes, "第二条".as_bytes());
        let first = reader.next_record().expect("first read").expect("first");
        assert_eq!(first.start_offset, 0);
        assert_eq!(first.bytes, b"first");
        assert!(reader.next_record().expect("end read").is_none());
    }

    #[test]
    fn ignores_trailing_newlines_and_blank_records() {
        let mut file = NamedTempFile::new().expect("temp file");
        write!(file, "first\n\n").expect("write records");
        file.as_file_mut().flush().expect("flush file");
        let size = file.as_file().metadata().expect("metadata").len();
        let mut reader = ReverseJsonlReader::with_chunk_bytes(
            File::open(file.path()).expect("open file"),
            size,
            1024,
            2,
        );
        let first = reader.next_record().expect("first read").expect("first");
        assert_eq!(first.bytes, b"first");
        assert!(reader.next_record().expect("end read").is_none());
    }

    #[test]
    fn reads_a_complete_final_record_without_a_newline() {
        let mut file = NamedTempFile::new().expect("temp file");
        write!(file, "first\nfinal").expect("write records");
        file.as_file_mut().flush().expect("flush file");
        let size = file.as_file().metadata().expect("metadata").len();
        let mut reader = ReverseJsonlReader::with_chunk_bytes(
            File::open(file.path()).expect("open file"),
            size,
            1024,
            2,
        );
        assert_eq!(
            reader
                .next_record()
                .expect("final read")
                .expect("final")
                .bytes,
            b"final"
        );
        assert_eq!(
            reader
                .next_record()
                .expect("first read")
                .expect("first")
                .bytes,
            b"first"
        );
        assert!(reader.next_record().expect("end read").is_none());
    }

    #[test]
    fn rejects_a_record_larger_than_the_configured_limit() {
        let mut file = NamedTempFile::new().expect("temp file");
        writeln!(file, "123456789").expect("write record");
        file.as_file_mut().flush().expect("flush file");
        let size = file.as_file().metadata().expect("metadata").len();
        let mut reader = ReverseJsonlReader::with_chunk_bytes(
            File::open(file.path()).expect("open file"),
            size,
            4,
            2,
        );
        let error = reader.next_record().expect_err("oversized record");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
