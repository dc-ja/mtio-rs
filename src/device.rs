//! [`TapeDevice`] — a safe wrapper around a Linux tape device file.
//!
//! # Device nodes
//!
//! Linux exposes each physical tape drive as a pair of character devices:
//!
//! - `/dev/st0`, `/dev/st1`, … — *rewinding*: the kernel rewinds the tape
//!   to BOT when the file descriptor is closed.
//! - `/dev/nst0`, `/dev/nst1`, … — *non-rewinding*: the tape stays at its
//!   current position when the file descriptor is closed.
//!
//! **Always open the non-rewinding node (`/dev/nst*`).**
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
//! a filemark, the kernel returns 0 bytes. The next `read` would still return
//! 0; the caller must issue [`Tape::space_filemarks(1)`](crate::Tape) to
//! step past the filemark.
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
/// Always use the **non-rewinding** device node (`/dev/nst*`). The rewinding
/// node (`/dev/st*`) rewinds on close, which will silently destroy data
/// written in a multi-file session.
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
        let op = MtOp {
            mt_op,
            _pad: 0,
            mt_count,
        };
        unsafe { ioctl::mtioctop_raw(self.file.as_raw_fd(), &op) }?;
        Ok(())
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

    fn erase(&mut self) -> Result<(), TapeError> {
        self.do_op(ioctl::MTERASE, 1)
    }
}
