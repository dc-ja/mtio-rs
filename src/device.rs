//! [`TapeDevice`] — a safe wrapper around a Linux tape device file.
//!
//! # Device nodes
//!
//! Linux exposes each physical tape drive as a group of character devices,
//! most notably:
//!
//! - `/dev/st0`, `/dev/st1`, … — *rewinding*: the kernel rewinds the tape
//!   to BOT when the file descriptor is closed.
//! - `/dev/nst0`, `/dev/nst1`, … — *non-rewinding*: the tape stays at its
//!   current position when the file descriptor is closed.
//!
//! In multi-file use-cases, non-rewinding devices should be preferred over 
//! their rewinding counterparts.
//! Writing multiple tape files in one session requires opening the device,
//! writing file 0, writing a filemark, writing file 1, writing a filemark,
//! etc. — all without closing in between. If the rewinding node is used, the
//! implicit rewind on the first `close(2)` would destroy everything after
//! the first tape file.
//!
//! # I/O model
//!
//! Data is transferred by ordinary `read(2)` and `write(2)` system calls on
//! the device file descriptor. Each `write` call produces exactly one tape
//! *record*; each `read` call reads at most one record. When a `read` reaches
//! a filemark, the kernel returns 0 bytes and advances the internal eof state
//! from `ST_FM_HIT` to `ST_FM`. The **next** `read` issues a real SCSI READ
//! from the drive, which is now positioned at the start of the following tape
//! file, so data flows normally — no explicit
//! [`space_filemarks`](crate::Tape::space_filemarks) call is required between
//! consecutive tape files.
//!
//! # ioctl operations
//!
//! Tape-specific operations (rewind, filemark write, positioning) are
//! performed via `ioctl(2)` using the constants and structs in the private
//! `ioctl` module. All ioctl calls are wrapped by the private `do_op`
//! method, which constructs an `MtOp` struct and translates `nix::Error`
//! into [`TapeError`].

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::error::TapeError;
use crate::ioctl::{self, MtGet, MtOp, MtPos};
use crate::status::{DriveType, StatusFlags, TapeStatus};
use crate::Tape;

/// A handle to a Linux tape device (typically `/dev/nst0`, `/dev/nst1`, …).
///
/// In most cases, specifically multi-file sessions, `/dev/nst*` should be
/// preferred over `/dev/st*` since the latter automatically rewinds the tape
/// whenever the file descriptor is closed. Writing multiple files would 
/// cause each new file to be written at the beginning of the tape, thus
/// overwriting all previous data on the tape; you would end up with only
/// the most recently written file on tape (and the wear on both tape and
/// drive caused by repeated writing and rewinding).
///
/// `TapeDevice` does not rewind on [`Drop`]; all positioning is explicit.
pub struct TapeDevice {
    file: File,
}

impl TapeDevice {
    /// Open a tape device for reading and writing.
    pub fn open(path: &Path) -> Result<Self, TapeError> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    /// Issue a tape operation via `MTIOCTOP`.
    fn do_op(&self, mt_op: i16, mt_count: i32) -> Result<(), TapeError> {
        let op = MtOp::new(mt_op, mt_count);
        unsafe { ioctl::mtioctop_raw(self.file.as_raw_fd(), &op) }?;
        Ok(())
    }

    /// Issue a raw `MTIOCTOP` ioctl with the given operation code and count.
    ///
    /// This is an escape hatch for tape operations not covered by the [`Tape`]
    /// trait. Use the `MT*` constants re-exported from the crate root for `op`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mtio::{TapeDevice, MTRETEN};
    /// use std::path::Path;
    ///
    /// let mut drive = TapeDevice::open(Path::new("/dev/nst0")).unwrap();
    /// drive.raw_op(MTRETEN, 1).unwrap(); // re-tension the tape
    /// ```
    pub fn raw_op(&self, op: i16, count: i32) -> Result<(), TapeError> {
        if !ioctl::is_known_op(op) {
            return Err(TapeError::UnknownOperation(op));
        }
        self.do_op(op, count)
    }
}

impl Read for TapeDevice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for TapeDevice {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Tape for TapeDevice {
    fn rewind(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTREW, 1)
    }

    fn seek_to_eod(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTEOM, 1)
    }

    fn space_filemarks(&mut self, count: i32) -> Result<(), TapeError> {
        if count >= 0 {
            self.do_op(ioctl::MTFSF, count)
        } else {
            self.do_op(ioctl::MTBSF, -count)
        }
    }

    fn space_records(&mut self, count: i32) -> Result<(), TapeError> {
        if count >= 0 {
            self.do_op(ioctl::MTFSR, count)
        } else {
            self.do_op(ioctl::MTBSR, -count)
        }
    }

    fn write_filemarks(&mut self, count: u32) -> Result<(), TapeError> {
        // MTWEOF accepts an i32; saturate silently — writing 2^31 filemarks
        // is not a realistic scenario.
        self.do_op(ioctl::MTWEOF, count.min(i32::MAX as u32) as i32)
    }

    fn seek_block(&mut self, block: u64) -> Result<(), TapeError> {
        if block > i32::MAX as u64 {
            return Err(TapeError::BlockNumberTooLarge(block));
        }
        self.do_op(ioctl::MTSEEK, block as i32)
    }

    fn set_block_size(&mut self, bytes: u32) -> Result<(), TapeError> {
        self.do_op(ioctl::MTSETBLK, bytes as i32)
    }

    fn load(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTLOAD, 1)
    }

    fn unload(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTUNLOAD, 1)
    }

    fn lock(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTLOCK, 1)
    }

    fn unlock(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTUNLOCK, 1)
    }

    fn status(&mut self) -> Result<TapeStatus, TapeError> {
        let mut raw = MtGet {
            mt_type: 0,
            mt_resid: 0,
            mt_dsreg: 0,
            mt_gstat: 0,
            mt_erreg: 0,
            mt_fileno: 0,
            mt_blkno: 0,
        };
        unsafe { ioctl::mtiocget_raw(self.file.as_raw_fd(), &mut raw) }?;
        Ok(TapeStatus {
            drive_type: DriveType(raw.mt_type),
            file_number: raw.mt_fileno,
            block_number: raw.mt_blkno,
            // mt_dsreg encodes the density code in bits 24–31 and the block
            // size in bits 0–23 (MT_ST_BLKSIZE_MASK = 0x00ff_ffff).
            block_size: (raw.mt_dsreg & 0x00ff_ffff) as u32,
            flags: StatusFlags(raw.mt_gstat),
        })
    }

    fn position(&mut self) -> Result<u64, TapeError> {
        let mut raw = MtPos { mt_blkno: 0 };
        unsafe { ioctl::mtiocpos_raw(self.file.as_raw_fd(), &mut raw) }?;
        Ok(raw.mt_blkno as u64)
    }

    fn erase(&mut self, long_erase: bool) -> Result<(), TapeError> {
        self.do_op(ioctl::MTERASE, long_erase as i32)
    }
}
