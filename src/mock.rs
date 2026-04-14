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
//! of the current tape file, mirroring the Linux `st` driver. The filemark is
//! **auto-consumed**: `file_idx` advances automatically, so the next `read`
//! call immediately returns data from the following tape file. Do **not** call
//! [`Tape::space_filemarks`]`(1)` between consecutive tape-file reads; that
//! would skip one additional filemark.
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
    /// Whether the file number is known and can be reported in [`status`].
    ///
    /// The Linux `st` driver sets `drv_file = -1` (unknown) after `MTSEEK`
    /// because an absolute block seek does not convey which logical tape file
    /// the head lands in. All other positioning operations update `drv_file`
    /// directly, so `file_known` returns to `true` after any of them.
    file_known: bool,
    /// Whether the block-within-file number is known and can be reported in
    /// [`status`].
    ///
    /// The driver sets `drv_block = -1` after `MTBSF` (backward space
    /// filemarks) because the head is positioned before a filemark with no
    /// record count available, and after `MTSEEK` for the same reason as
    /// `file_known`. Forward operations — `MTFSF`, `MTWEOF`, rewind, and any
    /// read or write that moves `byte_idx` — restore the known state.
    block_known: bool,
    /// Number of consecutive zero-byte `read()` calls (filemark crossings or
    /// reads past EOD) since the last call that returned actual data or the
    /// last positioning operation.
    ///
    /// The Linux `st` driver documents: *"End of data is signified by
    /// returning zero bytes for two consecutive reads."* After two such reads
    /// any further read returns `ENOSPC` (modelled here as an
    /// `io::ErrorKind::Other` error), matching driver behaviour where the
    /// EOF state machine transitions from `ST_EOD_2` into `ST_EOD`.
    ///
    /// Reset to 0 by any operation that changes the tape position or returns
    /// actual data.
    consecutive_zero_reads: u8,
}

impl MockTape {
    /// Create an empty, writable mock tape positioned at BOT.
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            file_idx: 0,
            byte_idx: 0,
            write_protected: false,
            file_known: true,
            block_known: true,
            consecutive_zero_reads: 0,
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
    /// Returns `Ok(0)` at a filemark boundary, mirroring the Linux `st`
    /// driver: when the last byte of a tape file is consumed, `file_idx` is
    /// automatically advanced past the filemark so the next `read` begins at
    /// the start of the following tape file. This means callers should NOT
    /// call [`Tape::space_filemarks`]`(1)` between consecutive tape-file reads;
    /// doing so would skip an additional filemark.
    ///
    /// Returns `Ok(0)` without advancing when already past all written files
    /// (end of data).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.file_idx >= self.files.len() {
            // Past all written files — end-of-data.
            //
            // The Linux st driver documents: "End of data is signified by
            // returning zero bytes for two consecutive reads." After two such
            // reads, the driver's EOF state machine enters ST_EOD and returns
            // ENOSPC on any further read. We mirror that here.
            self.consecutive_zero_reads = self.consecutive_zero_reads.saturating_add(1);
            if self.consecutive_zero_reads > 2 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "ENOSPC: end of data (st driver ST_EOD state)",
                ));
            }
            return Ok(0);
        }
        let file = &self.files[self.file_idx];
        if self.byte_idx >= file.len() {
            // Filemark boundary: auto-advance past it, matching the Linux st
            // driver's behaviour. The next read will start at the following
            // tape file (or return Ok(0) again if that is also empty / EOD).
            // After crossing a filemark forward, file and block numbers are
            // known again (driver resets drv_block = 0, drv_file += 1).
            self.file_idx += 1;
            self.byte_idx = 0;
            self.file_known = true;
            self.block_known = true;
            self.consecutive_zero_reads = self.consecutive_zero_reads.saturating_add(1);
            if self.consecutive_zero_reads > 2 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "ENOSPC: end of data (st driver ST_EOD state)",
                ));
            }
            return Ok(0);
        }
        let available = file.len() - self.byte_idx;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&file[self.byte_idx..self.byte_idx + n]);
        self.byte_idx += n;
        // Any non-zero read resets the consecutive-zero counter and restores
        // known position — we know exactly where we are within the file.
        if n > 0 {
            self.file_known = true;
            self.block_known = true;
            self.consecutive_zero_reads = 0;
        }
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
        self.file_known = true;
        self.block_known = true;
        self.consecutive_zero_reads = 0;
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
        self.file_known = true;
        self.block_known = true;
        self.consecutive_zero_reads = 0;
        Ok(())
    }

    fn seek_to_eod(&mut self) -> Result<(), TapeError> {
        // Position one past the last written tape file, analogous to a real
        // drive sitting just after the last filemark before blank tape.
        self.file_idx = self.files.len();
        self.byte_idx = 0;
        self.file_known = true;
        self.block_known = true;
        self.consecutive_zero_reads = 0;
        Ok(())
    }

    fn space_filemarks(&mut self, count: i32) -> Result<(), TapeError> {
        if count >= 0 {
            // MTFSF: file number advances, block number resets to 0 (known).
            self.file_idx = self.file_idx.saturating_add(count as usize);
            self.file_known = true;
            self.block_known = true;
        } else {
            // MTBSF: file number decrements, block number becomes unknown
            // (driver sets drv_block = -1 after backward filemark space).
            self.file_idx = self.file_idx.saturating_sub((-count) as usize);
            self.file_known = true;
            self.block_known = false;
        }
        self.byte_idx = 0;
        self.consecutive_zero_reads = 0;
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
    /// (overwrite semantics). The current file is truncated at `byte_idx`
    /// first — if that leaves it empty (i.e. `byte_idx == 0`) the slot is
    /// removed entirely rather than kept as a zero-length placeholder. This
    /// matches real tape behaviour: writing a filemark at the start of a file
    /// replaces that file's slot, not appends an empty one before it.
    fn write_filemarks(&mut self, count: u32) -> Result<(), TapeError> {
        self.check_write_protected()?;
        for _ in 0..count {
            if self.file_idx < self.files.len() {
                // Truncate the current file at the write head position.
                self.files[self.file_idx].truncate(self.byte_idx);
                if self.files[self.file_idx].is_empty() {
                    // No data was written to this slot; remove it so that no
                    // empty placeholder is left for scan_files to count.
                    self.files.truncate(self.file_idx);
                } else {
                    // Keep the partial file; discard everything after it.
                    self.files.truncate(self.file_idx + 1);
                }
            }
            // Advance past the filemark to the next file slot.
            // MTWEOF restores known position: drv_file += 1, drv_block = 0.
            self.file_idx += 1;
            self.byte_idx = 0;
            self.file_known = true;
            self.block_known = true;
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
        // MTSEEK: both file and block number become unknown — the driver sets
        // drv_file = drv_block = -1 after an absolute block seek.
        self.file_known = false;
        self.block_known = false;
        self.consecutive_zero_reads = 0;
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
        // The mock always has a "tape" loaded and the drive ready.
        bits |= StatusFlags::ONLINE;
        // BOT: both file number and block number are zero (driver source:
        // `if (mt_fileno == 0 && mt_blkno == 0) gstat |= GMT_BOT`).
        if self.file_idx == 0 && self.byte_idx == 0 {
            bits |= StatusFlags::BOT;
        }
        // EOF: block number is zero but file number is non-zero — i.e. the
        // head is positioned at the start of any file after the first
        // (driver source: `else if (mt_blkno == 0) gstat |= GMT_EOF`).
        // Mutually exclusive with BOT because the driver uses else-if.
        if self.file_idx > 0 && self.byte_idx == 0 {
            bits |= StatusFlags::EOF;
        }
        if self.file_idx >= self.files.len() {
            bits |= StatusFlags::EOD;
        }
        if self.write_protected {
            bits |= StatusFlags::WR_PROT;
        }
        Ok(TapeStatus {
            drive_type: DriveType(0),
            file_number: if self.file_known { self.file_idx as i32 } else { -1 },
            block_number: if self.block_known { self.byte_idx as i32 } else { -1 },
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

    /// Erase from the current position to EOD.
    ///
    /// Truncates the current tape file at the current byte offset and removes
    /// all subsequent files, mirroring the effect of a physical erase. Returns
    /// [`TapeError::WriteProtected`] if the tape is write-protected.
    fn erase(&mut self) -> Result<(), TapeError> {
        self.check_write_protected()?;
        if self.file_idx < self.files.len() {
            self.files[self.file_idx].truncate(self.byte_idx);
            if self.files[self.file_idx].is_empty() {
                self.files.truncate(self.file_idx);
            } else {
                self.files.truncate(self.file_idx + 1);
            }
        }
        Ok(())
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

        // The filemark after "file0" is auto-consumed when read_file returns
        // Ok(0), so the next read immediately starts at "file1".
        assert_eq!(read_file(&mut tape), b"file0");
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

        // The filemark after "new0" is auto-consumed. The next read is at EOD
        // because file 1 no longer exists.
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

        // The first filemark was auto-consumed by the read above. A second
        // consecutive read returning Ok(0) confirms the double filemark: there
        // is no data between the two filemarks.
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

    // ── Unknown position (MTBSF / MTSEEK) ────────────────────────────────

    #[test]
    fn space_filemarks_backward_sets_block_unknown() {
        // MTBSF sets drv_block = -1 in the driver: the head is before a
        // filemark and the record count within the prior file is not known.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        // Positioned at EOD (file_idx 2). Space back one filemark.
        tape.space_filemarks(-1).unwrap();
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, 1, "file number should be known after MTBSF");
        assert_eq!(st.block_number, -1, "block number should be -1 after MTBSF");
    }

    #[test]
    fn space_filemarks_forward_restores_known_position() {
        // After MTBSF the block is unknown; a subsequent MTFSF restores it.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.space_filemarks(-1).unwrap(); // block unknown
        tape.space_filemarks(1).unwrap();  // MTFSF restores
        let st = tape.status().unwrap();
        assert_eq!(st.block_number, 0, "block number should be 0 after MTFSF");
    }

    #[test]
    fn seek_block_sets_both_file_and_block_unknown() {
        // MTSEEK: the driver sets drv_file = drv_block = -1 because an
        // absolute block address does not map to a logical file number.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        let pos = tape.position().unwrap();
        tape.seek_block(pos).unwrap();
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, -1, "file number should be -1 after MTSEEK");
        assert_eq!(st.block_number, -1, "block number should be -1 after MTSEEK");
    }

    #[test]
    fn read_after_seek_block_restores_known_position() {
        // Reading data after an MTSEEK fixes the position tracking.
        let mut tape = MockTape::new();
        tape.write_all(b"hello").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        let pos = tape.position().unwrap();
        tape.seek_block(pos).unwrap();
        // Both unknown at this point.
        let mut buf = [0u8; 5];
        tape.read_exact(&mut buf).unwrap();
        let st = tape.status().unwrap();
        assert_ne!(st.file_number, -1, "file number should be known after read");
        assert_ne!(st.block_number, -1, "block number should be known after read");
    }

    #[test]
    fn write_after_seek_block_restores_known_position() {
        // Writing after MTSEEK also restores known position.
        let mut tape = MockTape::new();
        tape.write_all(b"old").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        let pos = tape.position().unwrap();
        tape.seek_block(pos).unwrap();
        tape.write_all(b"new").unwrap();
        let st = tape.status().unwrap();
        assert_ne!(st.file_number, -1);
        assert_ne!(st.block_number, -1);
    }

    #[test]
    fn rewind_after_seek_block_restores_known_position() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        tape.seek_block(0).unwrap();
        // Both unknown.
        tape.rewind().unwrap();
        let st = tape.status().unwrap();
        assert_eq!(st.file_number, 0);
        assert_eq!(st.block_number, 0);
    }

    #[test]
    fn write_filemark_after_bsf_restores_block_known() {
        // MTWEOF sets drv_file += 1, drv_block = 0 — restores known state.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.space_filemarks(-1).unwrap(); // block unknown
        tape.write_filemarks(1).unwrap();  // MTWEOF restores
        let st = tape.status().unwrap();
        assert_ne!(st.block_number, -1, "block number should be known after MTWEOF");
    }

    // ── EOD state machine ─────────────────────────────────────────────────

    #[test]
    fn read_at_eod_first_two_calls_return_zero() {
        // The first two reads past all recorded data must return Ok(0), not an
        // error — the double-zero is the signal that EOD has been reached.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        assert_eq!(tape.read(&mut [0u8; 4]).unwrap(), 0, "first EOD read");
        assert_eq!(tape.read(&mut [0u8; 4]).unwrap(), 0, "second EOD read");
    }

    #[test]
    fn read_past_double_zero_returns_enospc() {
        // After two consecutive zero-byte reads at EOD, further reads return
        // an error (driver ST_EOD state → ENOSPC).
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        tape.read(&mut [0u8; 4]).unwrap(); // 1st zero
        tape.read(&mut [0u8; 4]).unwrap(); // 2nd zero
        assert!(tape.read(&mut [0u8; 4]).is_err(), "3rd read should error");
    }

    #[test]
    fn double_filemark_triggers_enospc_on_third_zero_read() {
        // A double filemark is the conventional EOA marker. Reading through
        // both filemarks consumes two zero-byte reads; the third read errors.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(2).unwrap(); // double filemark = EOA
        tape.rewind().unwrap();

        // Drain the data file.
        read_file(&mut tape); // consumes data, returns Ok(0) at FM #1 (zero #1)
        // Second zero-byte read: crossing FM #2 (or blank past it).
        assert_eq!(tape.read(&mut [0u8; 4]).unwrap(), 0, "second zero read");
        // Third read must error.
        assert!(tape.read(&mut [0u8; 4]).is_err(), "third read should error");
    }

    #[test]
    fn rewind_resets_eod_state_machine() {
        // After hitting the ENOSPC state, a rewind must clear the counter so
        // normal reads work again from the start.
        let mut tape = MockTape::new();
        tape.write_all(b"hello").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        tape.read(&mut [0u8; 4]).unwrap();
        tape.read(&mut [0u8; 4]).unwrap();
        assert!(tape.read(&mut [0u8; 4]).is_err());

        tape.rewind().unwrap();
        assert_eq!(read_file(&mut tape), b"hello");
    }

    #[test]
    fn space_filemarks_resets_eod_state_machine() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        tape.read(&mut [0u8; 4]).unwrap();
        tape.read(&mut [0u8; 4]).unwrap();
        assert!(tape.read(&mut [0u8; 4]).is_err());

        // Space backward to file 0 and verify the error state is gone.
        tape.space_filemarks(-1).unwrap();
        assert_eq!(read_file(&mut tape), b"data");
    }

    #[test]
    fn data_read_resets_eod_state_machine() {
        // Reading actual data must reset the consecutive-zero counter so
        // interleaved reads (data, filemark, data, filemark) do not
        // accidentally accumulate towards ENOSPC.
        let mut tape = MockTape::new();
        tape.write_all(b"a").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"b").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        read_file(&mut tape); // zero #1 (filemark after "a")
        read_file(&mut tape); // data resets counter, then zero #1 again (filemark after "b")
        // We've only seen one zero in a row since the last data read.
        // One more Ok(0) is still allowed before ENOSPC.
        assert_eq!(tape.read(&mut [0u8; 4]).unwrap(), 0);
        assert!(tape.read(&mut [0u8; 4]).is_err());
    }

    // ── ONLINE flag ───────────────────────────────────────────────────────

    #[test]
    fn status_online_always_set() {
        // The mock always has a tape loaded; ONLINE should be set in every
        // position: BOT, mid-tape, and EOD.
        let mut tape = MockTape::new();
        assert!(tape.status().unwrap().flags.is_online(), "not online at BOT/EOD");

        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        assert!(tape.status().unwrap().flags.is_online(), "not online at BOT");

        tape.space_filemarks(1).unwrap();
        assert!(tape.status().unwrap().flags.is_online(), "not online at EOD");
    }

    #[test]
    fn status_online_set_on_write_protected_tape() {
        let mut tape = MockTape::new().write_protected();
        assert!(tape.status().unwrap().flags.is_online());
    }

    // ── EOF flag ──────────────────────────────────────────────────────────

    #[test]
    fn status_eof_not_set_at_bot() {
        // At BOT (file 0, byte 0), EOF must not be set — the driver uses
        // else-if, so BOT and EOF are mutually exclusive.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();
        let st = tape.status().unwrap();
        assert!(st.flags.is_bot());
        assert!(!st.flags.is_eof());
    }

    #[test]
    fn status_eof_set_after_forward_space_to_non_first_file() {
        // After space_filemarks(1) from BOT, the head is at the start of
        // file 1 (byte 0). The driver sets GMT_EOF in this position.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(1).unwrap();
        let st = tape.status().unwrap();
        assert!(st.flags.is_eof(), "EOF not set at start of file 1");
        assert!(!st.flags.is_bot());
    }

    #[test]
    fn status_eof_set_after_auto_advance_past_filemark() {
        // read() returning Ok(0) at a filemark auto-advances to the next
        // file. At that point (file > 0, byte 0), EOF should be set.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        // Drain file 0 — the Ok(0) at the end auto-advances.
        read_file(&mut tape);
        let st = tape.status().unwrap();
        assert!(st.flags.is_eof(), "EOF not set after auto-advance to file 1");
    }

    #[test]
    fn status_eof_cleared_after_reading_into_file() {
        // Once we've read at least one byte into a file, byte offset > 0
        // and EOF should be cleared.
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(1).unwrap();
        let st = tape.status().unwrap();
        assert!(st.flags.is_eof(), "precondition: EOF set at start of file 1");

        let mut buf = [0u8; 1];
        tape.read_exact(&mut buf).unwrap();
        let st = tape.status().unwrap();
        assert!(!st.flags.is_eof(), "EOF still set after reading into file 1");
    }

    #[test]
    fn status_eof_set_at_eod_after_last_filemark() {
        // After space_filemarks(1) past the last filemark, we're at EOD with
        // file_idx > 0 and byte_idx == 0: both EOF and EOD should be set.
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.space_filemarks(1).unwrap();
        let st = tape.status().unwrap();
        assert!(st.flags.is_eof(), "EOF not set at EOD");
        assert!(st.flags.is_eod(), "EOD not set");
    }

    // ── erase ─────────────────────────────────────────────────────────────

    #[test]
    fn erase_from_bot_removes_all_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"file0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"file1").unwrap();
        tape.write_filemarks(1).unwrap();

        tape.rewind().unwrap();
        tape.erase().unwrap();

        assert_eq!(tape.file_count(), 0);
    }

    #[test]
    fn erase_from_mid_tape_removes_remaining_files() {
        let mut tape = MockTape::new();
        tape.write_all(b"keep").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"gone0").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.write_all(b"gone1").unwrap();
        tape.write_filemarks(1).unwrap();

        // Position at the start of file 1 and erase from there.
        tape.rewind().unwrap();
        tape.space_filemarks(1).unwrap();
        tape.erase().unwrap();

        assert_eq!(tape.file_count(), 1);
        assert_eq!(tape.files()[0], b"keep");
    }

    #[test]
    fn erase_from_mid_file_truncates_partial_data() {
        let mut tape = MockTape::new();
        tape.write_all(b"abcdef").unwrap();
        tape.write_filemarks(1).unwrap();

        // Position after the first 3 bytes of file 0 and erase.
        tape.rewind().unwrap();
        let mut tmp = [0u8; 3];
        tape.read_exact(&mut tmp).unwrap();
        tape.erase().unwrap();

        // The first 3 bytes should be preserved; the rest gone.
        assert_eq!(tape.file_count(), 1);
        assert_eq!(tape.files()[0], b"abc");
    }

    #[test]
    fn erase_at_eod_is_noop() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.seek_to_eod().unwrap();

        let count_before = tape.file_count();
        tape.erase().unwrap();
        assert_eq!(tape.file_count(), count_before);
    }

    #[test]
    fn erase_leaves_tape_at_eod() {
        let mut tape = MockTape::new();
        tape.write_all(b"data").unwrap();
        tape.write_filemarks(1).unwrap();
        tape.rewind().unwrap();

        tape.erase().unwrap();
        assert!(tape.status().unwrap().flags.is_eod());
    }

    #[test]
    fn erase_on_write_protected_tape_errors() {
        let mut tape = MockTape::new().write_protected();
        assert!(tape.erase().is_err());
    }
}
