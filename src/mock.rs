//! [`MockTape`] — an in-memory tape simulation for unit testing.
//!
//! Real tape hardware is not available in most CI environments, and even when
//! it is, tests that touch a physical drive are slow and potentially
//! destructive. `MockTape` lets the entire higher-level logic of
//! `tape-backup-lib` be tested without any hardware.
//!
//! # Model
//!
//! A real tape stores data as a sequence of *tape files* separated by
//! *filemarks*. `MockTape` represents this as a `Vec<Vec<u8>>`, where each
//! inner `Vec<u8>` holds the bytes of one tape file. The current position is
//! tracked as a `(file_index, byte_offset)` pair.
//!
//! ## Filemark behaviour
//!
//! Reading returns `Ok(0)` (zero bytes) when the read cursor reaches the end
//! of the current tape file — identical to the behaviour of the Linux `st`
//! driver at a filemark boundary. The caller must call
//! [`Tape::space_filemarks`]`(1)` to advance past the filemark before reading
//! the next file.
//!
//! ## Overwrite semantics
//!
//! Writing at position `(f, b)` truncates the current tape file at offset `b`
//! and removes all tape files that follow — mirroring the behaviour of a real
//! tape, where the write head physically overwrites everything from the
//! current position onwards. This means that rewinding and writing new data
//! replaces everything that was there before.

use std::io::{self, Read, Write};

use crate::error::TapeError;
use crate::status::{DriveType, StatusFlags, TapeStatus};
use crate::Tape;

/// In-memory tape simulation. See the [module documentation](self) for the
/// model and behavioural contract.
pub struct MockTape {
    /// The tape contents, one entry per tape file (data between filemarks).
    files: Vec<Vec<u8>>,
    /// Index into `files` of the tape file currently being read or written.
    file_idx: usize,
    /// Byte offset within `files[file_idx]`.
    byte_idx: usize,
    /// When `true`, [`Write`] and [`Tape::write_filemarks`] return an error.
    write_protected: bool,
}

impl MockTape {
    /// Create an empty, writable mock tape positioned at BOT.
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            file_idx: 0,
            byte_idx: 0,
            write_protected: false,
        }
    }

    /// Mark the tape as write-protected. Builder-style.
    pub fn write_protected(mut self) -> Self {
        self.write_protected = true;
        self
    }

    /// Access the raw tape-file data for assertions in tests.
    ///
    /// `files()[i]` contains all bytes written to tape file `i`.
    pub fn files(&self) -> &[Vec<u8>] {
        &self.files
    }

    /// Number of complete tape files written so far.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    fn check_write_protected(&self) -> Result<(), TapeError> {
        if self.write_protected {
            Err(TapeError::WriteProtected)
        } else {
            Ok(())
        }
    }
}

impl Default for MockTape {
    fn default() -> Self {
        Self::new()
    }
}

// ── std::io::Read ─────────────────────────────────────────────────────────

impl Read for MockTape {
    /// Read bytes from the current tape file.
    ///
    /// Returns `Ok(0)` when the read cursor reaches the end of the current
    /// tape file (filemark boundary) or when the cursor is past all written
    /// files (end of data). The caller advances past a filemark with
    /// [`Tape::space_filemarks`]`(1)`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.file_idx >= self.files.len() {
            // Past all written files — simulate end-of-data.
            return Ok(0);
        }
        let file = &self.files[self.file_idx];
        if self.byte_idx >= file.len() {
            // At the end of this tape file: signal filemark boundary.
            return Ok(0);
        }
        let available = file.len() - self.byte_idx;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&file[self.byte_idx..self.byte_idx + n]);
        self.byte_idx += n;
        Ok(n)
    }
}

// ── std::io::Write ────────────────────────────────────────────────────────

impl Write for MockTape {
    /// Write bytes to the current tape file from the current byte offset.
    ///
    /// Any bytes that existed in the current tape file after `byte_idx` are
    /// discarded, and all tape files after the current one are removed. This
    /// models real tape overwrite behaviour: once you start writing at a
    /// position, everything recorded past that point is gone.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.write_protected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "tape is write-protected",
            ));
        }
        // Create the current file slot if it does not yet exist.
        while self.files.len() <= self.file_idx {
            self.files.push(Vec::new());
        }
        // Overwrite from byte_idx: truncate then extend.
        self.files[self.file_idx].truncate(self.byte_idx);
        self.files[self.file_idx].extend_from_slice(buf);
        // Remove all tape files that follow (tape overwrite semantics).
        self.files.truncate(self.file_idx + 1);
        self.byte_idx += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ── Tape ──────────────────────────────────────────────────────────────────

impl Tape for MockTape {
    fn rewind(&mut self) -> Result<(), TapeError> {
        self.file_idx = 0;
        self.byte_idx = 0;
        Ok(())
    }

    fn seek_to_eod(&mut self) -> Result<(), TapeError> {
        // Position one past the last written tape file, analogous to a real
        // drive sitting just after the last filemark before blank tape.
        self.file_idx = self.files.len();
        self.byte_idx = 0;
        Ok(())
    }

    fn space_filemarks(&mut self, count: i32) -> Result<(), TapeError> {
        if count >= 0 {
            self.file_idx = self.file_idx.saturating_add(count as usize);
        } else {
            self.file_idx = self.file_idx.saturating_sub((-count) as usize);
        }
        self.byte_idx = 0;
        Ok(())
    }

    fn space_records(&mut self, _count: i32) -> Result<(), TapeError> {
        // Individual records within a tape file are not separately tracked by
        // this mock. This is a no-op; add tracking if tests require it.
        Ok(())
    }

    /// Write `count` filemarks.
    ///
    /// Each call to this method advances `file_idx` by one per filemark and
    /// removes any tape files that existed after the current position
    /// (overwrite semantics). After this call, `file_idx` points one past the
    /// last filemark written, ready for the next tape file's data.
    fn write_filemarks(&mut self, count: u32) -> Result<(), TapeError> {
        self.check_write_protected()?;
        for _ in 0..count {
            // Discard everything after the current file (tape overwrite).
            self.files.truncate(self.file_idx + 1);
            // Advance past the filemark to the next file slot.
            self.file_idx += 1;
            self.byte_idx = 0;
        }
        Ok(())
    }

    /// Seek to a logical block number.
    ///
    /// In this mock, the "block number" is the tape-file index, which matches
    /// what [`MockTape::position`] returns. This approximation is sufficient
    /// for testing round-trip seek behaviour; it does not model sub-file block
    /// addressing.
    fn seek_block(&mut self, block: u64) -> Result<(), TapeError> {
        self.file_idx = block as usize;
        self.byte_idx = 0;
        Ok(())
    }

    /// No-op: the mock operates in variable-length mode only.
    ///
    /// On a real drive this changes the physical block size; the mock ignores
    /// it because all reads and writes are already byte-granular.
    fn set_block_size(&mut self, _bytes: u32) -> Result<(), TapeError> {
        Ok(())
    }

    /// No-op: the mock has no physical cartridge mechanism to load.
    fn load(&mut self) -> Result<(), TapeError> {
        Ok(())
    }

    /// No-op: the mock has no physical cartridge mechanism to eject.
    fn unload(&mut self) -> Result<(), TapeError> {
        Ok(())
    }

    /// No-op: the mock has no door to lock.
    fn lock(&mut self) -> Result<(), TapeError> {
        Ok(())
    }

    /// No-op: the mock has no door to unlock.
    fn unlock(&mut self) -> Result<(), TapeError> {
        Ok(())
    }

    fn status(&mut self) -> Result<TapeStatus, TapeError> {
        let mut bits: i64 = 0;
        if self.file_idx == 0 && self.byte_idx == 0 {
            bits |= StatusFlags::BOT;
        }
        if self.file_idx >= self.files.len() {
            bits |= StatusFlags::EOD;
        }
        if self.write_protected {
            bits |= StatusFlags::WR_PROT;
        }
        Ok(TapeStatus {
            drive_type: DriveType(0),
            file_number: self.file_idx as i32,
            block_number: self.byte_idx as i32,
            flags: StatusFlags(bits),
        })
    }

    /// Return the current tape-file index as a position token.
    ///
    /// The returned value can be passed back to [`seek_block`](Self::seek_block)
    /// to return to this position. Sub-file byte offsets are not encoded.
    fn position(&mut self) -> Result<u64, TapeError> {
        Ok(self.file_idx as u64)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    // Helper: read all bytes from the current tape file, stopping at the
    // filemark boundary (Ok(0)).
    fn read_file(tape: &mut MockTape) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 256];
        loop {
            let n = tape.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        buf
    }

    #[test]
    fn write_and_read_single_file() {
        let mut tape = MockTape::new();
        tape.write_all(b"hello world").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        assert_eq!(read_file(&mut tape), b"hello world");
    }

    #[test]
    fn write_and_read_multiple_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        assert_eq!(read_file(&mut tape), b"file0");
        tape.space_filemarks(1).unwrap(); // step past the filemark
        assert_eq!(read_file(&mut tape), b"file1");
    }

    #[test]
    fn rewind_repositions_to_start() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();

        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"data");

        // Second rewind, same result.
        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"data");
    }

    #[test]
    fn space_filemarks_forward_skips_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"skip").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"target").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(1).unwrap(); // skip over "skip" file
        assert_eq!(read_file(&mut tape), b"target");
    }

    #[test]
    fn space_filemarks_backward_revisits_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"first").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"second").unwrap();
        tape.write_filemarks(1).unwrap();

        // Already at file 2 (past the last filemark). Go back one.
        tape.space_filemarks(-1).unwrap();
        assert_eq!(read_file(&mut tape), b"second");
    }

    #[test]
    fn write_protected_rejects_data_write() {
        let mut tape = MockTape::new().write_protected();
        assert!(tape.write_all(b"data").is_err());
    }

    #[test]
    fn write_protected_rejects_filemarks() {
        let mut tape = MockTape::new().write_protected();
        assert!(tape.write_filemarks(1).is_err());
    }

    #[test]
    fn write_protected_allows_reads() {
        // Pre-populate by writing without protection, then flip.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        tape.write_protected = true;

        assert_eq!(read_file(&mut tape), b"data");
    }

    #[test]
    fn overwrite_truncates_subsequent_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"original0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"original1").unwrap();
        tape.write_filemarks(1).unwrap();

        // Rewind and replace only file 0.
        tape.rewind().unwrap();
        tape.write_all(b"new0").unwrap();
        tape.write_filemarks(1).unwrap();

        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"new0");

        // File 1 must no longer exist.
        tape.space_filemarks(1).unwrap();
        assert_eq!(read_file(&mut tape), b""); // at EOD
        assert_eq!(tape.file_count(), 1);
    }

    #[test]
    fn seek_to_eod_positions_past_all_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.seek_to_eod().unwrap();
        assert_eq!(read_file(&mut tape), b""); // nothing past EOD
    }

    #[test]
    fn status_bot_at_start() {
        let mut tape = MockTape::new();
        let st = tape.status().unwrap();
        // A freshly created, never-written tape is at BOT and also at EOD
        // (no data has been recorded yet), so both flags are set.
        assert!(st.flags.is_bot());
        assert!(st.flags.is_eod());
    }

    #[test]
    fn status_eod_after_seek_to_eod() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        let st = tape.status().unwrap();
        assert!(st.flags.is_eod());
    }

    #[test]
    fn status_write_protected() {
        let mut tape = MockTape::new().write_protected();
        assert!(tape.status().unwrap().flags.is_write_protected());
    }

    #[test]
    fn position_and_seek_block_round_trip() {
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();

        // Capture position at file 1.
        tape.rewind().unwrap();
        tape.space_filemarks(1).unwrap();
        let pos = tape.position().unwrap();

        // Advance past file 1, then seek back.
        tape.space_filemarks(1).unwrap();
        tape.seek_block(pos).unwrap();

        assert_eq!(read_file(&mut tape), b"file1");
    }

    #[test]
    fn double_filemark_written_correctly() {
        // The logical end of a tape archive is signalled by two consecutive
        // filemarks. Verify that write_filemarks(2) produces two file slots.
        let mut tape = MockTape::new();
        tape.write_all(b"archive").unwrap();
        tape.write_filemarks(2).unwrap();

        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"archive");

        // First filemark crossed.
        tape.space_filemarks(1).unwrap();
        assert_eq!(read_file(&mut tape), b""); // second filemark / EOD
    }

    #[test]
    fn space_filemarks_zero_is_noop() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(0).unwrap();
        assert_eq!(read_file(&mut tape), b"data");
    }

    #[test]
    fn space_filemarks_backward_saturates_at_bot() {
        // Going backward past BOT must not underflow the usize file index.
        let mut tape = MockTape::new();
        tape.space_filemarks(-100).unwrap();
        let st = tape.status().unwrap();
        assert!(st.flags.is_bot());
        assert_eq!(st.file_number, 0);
    }

    #[test]
    fn write_filemarks_zero_is_noop() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(0).unwrap(); // should not advance or truncate
        tape.write_filemarks(1).unwrap(); // close the file normally

        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"data");
        assert_eq!(tape.file_count(), 1);
    }

    #[test]
    fn partial_reads_reassemble_full_content() {
        // Use a 1-byte buffer to exercise the partial-read path in Read::read.
        let mut tape = MockTape::new();
        tape.write_all(b"abcdef").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        let mut result = Vec::new();
        let mut buf = [0u8; 1];
        loop {
            let n = tape.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            result.push(buf[0]);
        }
        assert_eq!(result, b"abcdef");
    }

    #[test]
    fn read_into_empty_buffer_returns_zero_without_advancing() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        // A zero-length read must not consume any bytes or trigger a filemark.
        assert_eq!(tape.read(&mut []).unwrap(), 0);
        // The full content is still readable afterwards.
        assert_eq!(read_file(&mut tape), b"data");
    }

    #[test]
    fn file_count_tracks_written_files() {
        let mut tape = MockTape::new();
        assert_eq!(tape.file_count(), 0);

        tape.write_all(b"a").unwrap();
        tape.write_filemarks(1).unwrap();
        assert_eq!(tape.file_count(), 1);

        tape.write_all(b"b").unwrap();
        tape.write_filemarks(1).unwrap();
        assert_eq!(tape.file_count(), 2);
    }

    #[test]
    fn files_accessor_returns_raw_content() {
        let mut tape = MockTape::new();
        tape.write_all(b"hello").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"world").unwrap();
        tape.write_filemarks(1).unwrap();

        let files = tape.files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], b"hello");
        assert_eq!(files[1], b"world");
    }

    #[test]
    fn default_is_equivalent_to_new() {
        let mut a = MockTape::new();
        let mut b = MockTape::default();

        // Both start empty, writable, at BOT/EOD.
        let sa = a.status().unwrap();
        let sb = b.status().unwrap();
        assert_eq!(sa.flags, sb.flags);
        assert_eq!(sa.file_number, sb.file_number);
        assert_eq!(a.file_count(), b.file_count());
    }

    #[test]
    fn status_file_number_and_block_number_reflect_position() {
        let mut tape = MockTape::new();
        tape.write_all(b"file0data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        // At file 0.
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, 0);
        assert_eq!(st.block_number, 0);

        // Read 4 bytes; block_number should advance to 4.
        let mut buf = [0u8; 4];
        tape.read_exact(&mut buf).unwrap();
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, 0);
        assert_eq!(st.block_number, 4);

        // Space to file 1; file_number should become 1.
        tape.space_filemarks(1).unwrap();
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, 1);
        assert_eq!(st.block_number, 0);
    }

    #[test]
    fn status_not_bot_after_advancing() {
        let mut tape = MockTape::new();
        tape.write_all(b"x").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(1).unwrap();
        assert!(!tape.status().unwrap().flags.is_bot());
    }
}
