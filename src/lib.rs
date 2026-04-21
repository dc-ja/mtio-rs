//! Safe Rust bindings for the Linux SCSI tape driver (`/dev/st*`, `/dev/nst*`).
//!
//! # Background: how tape differs from disk
//!
//! Magnetic tape is a *sequential-access* medium. Unlike a disk, you cannot
//! seek to an arbitrary byte position in O(1); you can only move forward or
//! backward by whole *records* or *filemarks*. Writing at any position
//! implicitly discards everything recorded after that point.
//!
//! ## Records and tape files
//!
//! Data is written in *records* (sometimes called *blocks*). Each `write(2)`
//! call to a tape device produces exactly one record on tape; each `read(2)`
//! call reads at most one record. Records are grouped into *tape files*
//! separated by *filemarks* (also called end-of-file marks, or EOF marks).
//! A filemark is a special magnetic pattern that acts as a delimiter; it is
//! written by [`Tape::write_filemarks`] and signals `read(2)` to return zero
//! bytes (just like a regular file EOF).
//!
//! ## Positioning
//!
//! The tape driver tracks position as a `(file_number, block_number)` pair.
//! You can jump by filemarks with [`Tape::space_filemarks`] or by records
//! with [`Tape::space_records`]. For coarse positioning, [`Tape::rewind`]
//! goes to the absolute beginning (BOT — beginning of tape) and
//! [`Tape::seek_to_eod`] goes to the end of all recorded data (EOD).
//!
//! ## Logical end of archive
//!
//! The POSIX and GNU `tar` convention is to write *two* consecutive filemarks
//! to signal the logical end of an archive. A reader that encounters two
//! consecutive filemarks knows there is no more archive data. This crate does
//! not enforce this convention but exposes [`Tape::write_filemarks`] with a
//! `count` parameter so callers can write the double filemark easily.
//!
//! ## Non-rewinding device nodes
//!
//! Linux exposes each tape drive as a group of device nodes, most notably:
//!
//! - `/dev/st0`, `/dev/st1`, … — *rewinding*: the drive rewinds to BOT
//!   when the file descriptor is closed.
//! - `/dev/nst0`, `/dev/nst1`, … — *non-rewinding*: the drive stays at its
//!   current position when the file descriptor is closed.
//!
//! Prefer using the non-rewinding node in multi-file sessions. Using the 
//! rewinding node in a multi-file session causes the drive to rewind on the 
//! first `close(2)`, silently destroying everything written after the first 
//! tape file.
//!
//! # Structure
//!
//! - [`Tape`] — trait implemented by both [`TapeDevice`] and `MockTape`,
//!   allowing all higher-level logic to be tested without hardware.
//! - [`TapeDevice`] — wraps a real tape device file; Linux only.
//! - `MockTape` — in-memory tape simulation for unit tests; available under
//!   the `mock` feature or in `#[cfg(test)]` contexts.
//!
//! # Non-rewinding devices
//!
//! `TapeDevice` does not implicitly rewind on open or drop; positioning is
//! entirely caller-controlled.

pub mod error;
pub mod status;

#[cfg(target_os = "linux")]
mod ioctl;

#[cfg(target_os = "linux")]
pub use ioctl::{
    // Tape motion and control operation codes (mt_op field of MTIOCTOP):
    MTBSF, MTBSFM, MTBSR, MTBSS, MTCOMPRESSION, MTEOM, MTERASE, MTFSF, MTFSFM, MTFSR, MTFSS,
    MTLOAD, MTLOCK, MTMKPART, MTNOP, MTOFFL, MTRETEN, MTREW, MTSEEK, MTSETBLK, MTSETDENSITY,
    MTSETDRVBUFFER, MTSETPART, MTUNLOAD, MTUNLOCK, MTWEOF, MTWEOFI, MTWSM,
    // MTSETDRVBUFFER option group selectors:
    MT_ST_BOOLEANS, MT_ST_CLEARBOOLEANS, MT_ST_DEF_BLKSIZE, MT_ST_DEF_OPTIONS, MT_ST_OPTIONS,
    MT_ST_SET_CLN, MT_ST_SET_LONG_TIMEOUT, MT_ST_SET_TIMEOUT, MT_ST_SETBOOLEANS,
    MT_ST_WRITE_THRESHOLD,
    // MTSETDRVBUFFER boolean flags:
    MT_ST_ASYNC_WRITES, MT_ST_AUTO_LOCK, MT_ST_BUFFER_WRITES, MT_ST_CAN_BSR,
    MT_ST_CAN_PARTITIONS, MT_ST_DEBUGGING, MT_ST_DEF_WRITES, MT_ST_FAST_MTEOM, MT_ST_NO_BLKLIMS,
    MT_ST_NOWAIT, MT_ST_NOWAIT_EOF, MT_ST_READ_AHEAD, MT_ST_SCSI2LOGICAL, MT_ST_SILI,
    MT_ST_SYSV, MT_ST_TWO_FM,
};

#[cfg(target_os = "linux")]
pub mod device;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use error::TapeError;
pub use status::{DriveType, StatusFlags, TapeStatus};

#[cfg(target_os = "linux")]
pub use device::TapeDevice;

#[cfg(any(test, feature = "mock"))]
pub use mock::MockTape;

use std::io::{Read, Write};

/// Operations common to all tape devices.
///
/// ## Reading across filemarks
///
/// `Read::read` returns `Ok(0)` at a filemark boundary — identical to EOF on
/// a regular file. The Linux `st` driver automatically advances past the
/// filemark at that point, so the next `read` call returns data from the
/// following tape file without any `space_filemarks` call.
///
/// A double filemark marks the logical end of archive (EOA). Reading through
/// it produces two `Ok(0)` returns (one per filemark), after which the head
/// is on blank tape immediately following a filemark. In that state the driver
/// returns `Ok(0)` twice more before returning
/// `Err(`[`TapeError::Io`]`)` (`EIO`) to signal true EOD. On a completely
/// blank tape (never written), where no filemark precedes the blank region,
/// `EIO` is returned immediately without any `Ok(0)` first.
///
/// ```text
/// [record][record][FM][record][record][FM][FM][blank tape ...]
///                  ^                   ^    ^   ^    ^    ^
///              read → 0            read → 0 |   0    0   EIO
///         (next read starts             read → 0        (EOD)
///          at following file)       (EOA convention)
/// ```
///
/// ## Writing across filemarks
///
/// Write data with `Write::write`, then call
/// [`write_filemarks(1)`](Tape::write_filemarks) to close the tape file and
/// begin the next. Write a double filemark
/// ([`write_filemarks(2)`](Tape::write_filemarks)) to signal the logical end
/// of the archive.
///
/// On real tape, any write implicitly discards everything recorded after the
/// current position. Rewind before writing to overwrite the tape from the
/// start.
pub trait Tape: Read + Write {
    /// Rewind to the physical beginning of the tape (BOT — beginning of tape).
    ///
    /// Equivalent to the `mt rewind` shell command. After this call, the next
    /// read or write starts at the very first record.
    fn rewind(&mut self) -> Result<(), TapeError>;

    /// Seek forward to the end of all recorded data (EOD — end of data).
    ///
    /// Positions the tape just past the last written filemark, ready to
    /// append new data. Equivalent to `mt eom`.
    fn seek_to_eod(&mut self) -> Result<(), TapeError>;

    /// Space over `count` filemarks, forward (positive) or backward (negative).
    ///
    /// After this call the tape is positioned at the start of the tape file
    /// that immediately follows the last filemark traversed.
    ///
    /// Example: to skip the first tape file and land at the start of the
    /// second, call `space_filemarks(1)` after rewinding.
    ///
    /// Equivalent to `mt fsf N` (forward) or `mt bsf N` (backward).
    fn space_filemarks(&mut self, count: i32) -> Result<(), TapeError>;

    /// Space over `count` records (individual write blocks), forward or backward.
    ///
    /// Records are the granularity below tape files. Most backup operations
    /// work at the tape-file level; record-level spacing is mainly useful for
    /// low-level recovery or diagnostics.
    ///
    /// Equivalent to `mt fsr N` / `mt bsr N`.
    fn space_records(&mut self, count: i32) -> Result<(), TapeError>;

    /// Write `count` filemarks at the current position.
    ///
    /// A single filemark closes the current tape file; subsequent writes begin
    /// a new tape file. Two consecutive filemarks (`count = 2`) signal the
    /// logical end of the archive — the POSIX and GNU `tar` convention.
    ///
    /// Equivalent to `mt weof N`.
    fn write_filemarks(&mut self, count: u32) -> Result<(), TapeError>;

    /// Seek to a specific logical block number.
    ///
    /// This uses the SCSI `MTSEEK` operation, which accepts a 32-bit block
    /// count. Block numbers larger than [`i32::MAX`] (≈ 2.1 billion) return
    /// [`TapeError::BlockNumberTooLarge`].
    fn seek_block(&mut self, block: u64) -> Result<(), TapeError>;

    /// Set the fixed block (record) size in bytes.
    ///
    /// Pass `0` to switch to variable-length block mode, where each `write`
    /// call produces a record of exactly the size written. Most modern
    /// deployments use variable-length mode. Fixed-size mode can improve
    /// performance on some drives but requires every write to be exactly
    /// `bytes` long.
    fn set_block_size(&mut self, bytes: u32) -> Result<(), TapeError>;

    /// Execute a SCSI LOAD command.
    ///
    /// Instructs the drive to load the cartridge into the read/write
    /// mechanism. Usually called automatically by the drive on insertion;
    /// explicit use is needed with tape libraries or after an `unload`.
    fn load(&mut self) -> Result<(), TapeError>;

    /// Execute a SCSI UNLOAD command (eject the cartridge).
    ///
    /// The drive rewinds and then ejects the tape. Equivalent to `mt eject`.
    fn unload(&mut self) -> Result<(), TapeError>;

    /// Lock the drive door, preventing ejection.
    ///
    /// Call this at the start of a write session to prevent accidental
    /// ejection. Always pair with [`unlock`](Tape::unlock).
    fn lock(&mut self) -> Result<(), TapeError>;

    /// Unlock the drive door.
    fn unlock(&mut self) -> Result<(), TapeError>;

    /// Query the drive for its current status via `MTIOCGET`.
    ///
    /// Returns a [`TapeStatus`] containing the current position, drive type,
    /// and a set of [`StatusFlags`] (online, BOT, EOT, write-protected, …).
    /// This is the primary way to check whether a tape is write-protected
    /// before starting a write session.
    fn status(&mut self) -> Result<TapeStatus, TapeError>;

    /// Return the current logical block position via `MTIOCPOS`.
    ///
    /// The returned value is an opaque block number that can be saved and
    /// later passed to [`seek_block`](Tape::seek_block) to return to this
    /// exact position. Subject to the 32-bit limit documented on `seek_block`.
    fn position(&mut self) -> Result<u64, TapeError>;

    /// Physically erase the tape from the current position to EOT.
    ///
    /// This is a **destructive, time-consuming, high-wear operation**. The
    /// erase head traverses the full remaining tape, which takes minutes to
    /// hours depending on tape length. All data from the current position
    /// onwards is permanently destroyed.
    ///
    /// This is **not** a cryptographic erase; it is a magnetic erase that
    /// renders data unreadable by normal means. For security-sensitive data,
    /// consult the drive and media manufacturer's guidance on secure erase.
    ///
    /// To erase only from a specific file, seek to that position first using
    /// [`rewind`](Tape::rewind) and [`space_filemarks`](Tape::space_filemarks)
    /// before calling this method.
    ///
    /// Equivalent to `mt erase`.
    fn erase(&mut self) -> Result<(), TapeError>;
}
