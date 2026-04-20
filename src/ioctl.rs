//! Raw `ioctl(2)` bindings for the Linux SCSI tape driver (`st`).
//!
//! # Overview
//!
//! The Linux `st` tape driver exposes three `ioctl` requests for controlling
//! a tape drive from userspace. This module provides safe-ish wrappers around
//! those ioctls via the `nix` crate's macro system.
//!
//! The three ioctls are:
//!
//! | Constant   | Value        | Direction | Size (B) | Type | Nr | Purpose             |
//! |------------|--------------|-----------|----------|------|----|---------------------|
//! | `MTIOCTOP` | `0x40086d01` | WRITE     | 8        | `m`  | 1  | Issue a tape op     |
//! | `MTIOCGET` | `0x80306d02` | READ      | 48       | `m`  | 2  | Read drive status   |
//! | `MTIOCPOS` | `0x80086d03` | READ      | 8        | `m`  | 3  | Read block position |
//!
//! WRITE means the kernel reads a struct that userspace provides; READ means
//! the kernel writes a struct into userspace memory.
//!
//! # ioctl number derivation
//!
//! Linux encodes the ioctl number as a 32-bit integer:
//!
//! ```text
//! [31:30] direction  (NONE=0, WRITE=1, READ=2, RW=3)
//! [29:16] size       (sizeof the data struct, in bytes)
//! [15: 8] type       (magic byte identifying the subsystem; 'm' = 0x6d here)
//! [ 7: 0] number     (sequence number within the subsystem)
//! ```
//!
//! The `nix` `ioctl_write_ptr!` and `ioctl_read!` macros compute this value
//! automatically from the type byte, sequence number, and `size_of::<T>()`.
//! The sizes must match the kernel structs exactly, which is enforced by
//! `#[repr(C)]` and verified in the table above.
//!
//! # Struct layouts
//!
//! All structs use `#[repr(C)]` and map directly to their counterparts in
//! `linux/mtio.h`. On a 64-bit Linux target, `long` is 8 bytes and `int` is
//! 4 bytes.

use nix::{ioctl_read, ioctl_write_ptr};

// ── Tape operation codes (mt_op field of MtOp) ────────────────────────────
//
// These are the values placed in `MtOp::mt_op` when issuing `MTIOCTOP`.
// They correspond to the `MT*` constants in `linux/mtio.h`.

/// Forward space over `mt_count` filemarks (positions after the FM).
pub const MTFSF: i16 = 1;
/// Backward space over `mt_count` filemarks (positions before the FM).
pub const MTBSF: i16 = 2;
/// Forward space over `mt_count` records.
pub const MTFSR: i16 = 3;
/// Backward space over `mt_count` records.
pub const MTBSR: i16 = 4;
/// Write `mt_count` end-of-file marks (filemarks) at the current position.
pub const MTWEOF: i16 = 5;
/// Rewind to beginning of tape. `mt_count` is ignored.
pub const MTREW: i16 = 6;
/// Rewind and take the drive offline (eject). `mt_count` is ignored.
pub const MTOFFL: i16 = 7;
/// No-op: update the status registers only. Useful to refresh `MTIOCGET`
/// without moving the tape.
pub const MTNOP: i16 = 8;
/// Re-tension the tape (wind to EOT and back to BOT to reduce wear).
pub const MTRETEN: i16 = 9;
/// Backward space `mt_count` filemarks, leave tape positioned at the FM itself
/// (not after it, unlike `MTBSF`).
pub const MTBSFM: i16 = 10;
/// Forward space `mt_count` filemarks, leave tape positioned at the FM itself.
pub const MTFSFM: i16 = 11;
/// Seek to the end of recorded data (EOD). `mt_count` is ignored.
pub const MTEOM: i16 = 12;
/// Erase the tape from the current position to EOT.
pub const MTERASE: i16 = 13;
/// Set the fixed block (record) size. `mt_count` is the size in bytes;
/// pass `0` for variable-length mode.
pub const MTSETBLK: i16 = 20;
/// Set the tape density code. Consult the drive manual for valid values.
pub const MTSETDENSITY: i16 = 21;
/// Seek to the logical block number given in `mt_count`.
pub const MTSEEK: i16 = 22;
/// Lock the drive door to prevent accidental ejection.
pub const MTLOCK: i16 = 28;
/// Unlock the drive door.
pub const MTUNLOCK: i16 = 29;
/// Issue a SCSI LOAD command (instruct the drive to load the cartridge).
pub const MTLOAD: i16 = 30;
/// Issue a SCSI UNLOAD command (rewind and eject the cartridge).
pub const MTUNLOAD: i16 = 31;
/// Enable or disable hardware data compression. `mt_count = 1` enables,
/// `mt_count = 0` disables.
pub const MTCOMPRESSION: i16 = 32;
/// Select the active tape partition. `mt_count` is the partition number.
/// Requires the drive to have been formatted with partitions via `MTMKPART`.
pub const MTSETPART: i16 = 33;
/// Format the tape with partitions. `mt_count` encodes the partition sizes;
/// see the drive manual and `linux/mtio.h` for the encoding. Destructive.
pub const MTMKPART: i16 = 34;
/// Write `mt_count` filemarks immediately (without waiting for drive
/// confirmation). Faster than `MTWEOF` but the filemark may not be committed
/// if the system crashes before the drive finishes. Requires drive support.
pub const MTWEOFI: i16 = 35;

// ── Setmark operations ────────────────────────────────────────────────────
//
// Setmarks are a SCSI-2 positioning feature primarily found on DDS/DAT drives.
// A setmark is a special magnetic pattern that groups multiple tape files into
// a higher-level unit, allowing fast coarse positioning over many filemarks at
// once. Rarely supported on modern drives (LTO drives typically reject them).

/// Forward space over `mt_count` setmarks.
pub const MTFSS: i16 = 25;
/// Backward space over `mt_count` setmarks.
pub const MTBSS: i16 = 26;
/// Write `mt_count` setmarks at the current position.
pub const MTWSM: i16 = 27;

// ── Driver buffer / mode configuration ───────────────────────────────────
//
// MTSETDRVBUFFER configures st driver options and SCSI mode parameters.
// It requires root privileges. The `mt_count` argument encodes both the
// option group (upper 4 bits) and the value (lower bits) via the bitmasks
// below. The general form is:
//
//   mt_count = MT_ST_<GROUP> | <value>
//
// For example, to enable two-filemark mode on close:
//   mt_count = MT_ST_SETBOOLEANS | MT_ST_TWO_FM
//
// See `linux/mtio.h` and the st(4) man page for the full encoding.

/// Configure st driver options and SCSI mode parameters. Requires root.
/// `mt_count` must be composed from the `MT_ST_*` bitmask constants below.
///
/// There is no ioctl to read back the current settings; they are exposed
/// as individual files under `/sys/class/scsi_tape/<device>/` (e.g.
/// `/sys/class/scsi_tape/nst0/`). Sysfs read support is not yet implemented
/// in this crate.
pub const MTSETDRVBUFFER: i16 = 24;

// Option group selectors (upper bits of mt_count for MTSETDRVBUFFER):
/// Mask covering the option group bits.
pub const MT_ST_OPTIONS: u32 = 0xf000_0000;
/// Set all boolean flags to the values specified in the lower bits (flags not
/// included in the mask are cleared).
pub const MT_ST_BOOLEANS: u32 = 0x1000_0000;
/// Set the write-buffer threshold (lower bits = threshold in kB).
pub const MT_ST_WRITE_THRESHOLD: u32 = 0x2000_0000;
/// Set only the boolean flags included in the lower bits (others unchanged).
pub const MT_ST_SETBOOLEANS: u32 = 0x3000_0000;
/// Clear only the boolean flags included in the lower bits (others unchanged).
pub const MT_ST_CLEARBOOLEANS: u32 = 0x4000_0000;
/// Set the default fixed block size (lower bits = size in bytes; 0 = variable).
pub const MT_ST_DEF_BLKSIZE: u32 = 0x5000_0000;
/// Set miscellaneous default options (density, compression, block size together).
pub const MT_ST_DEF_OPTIONS: u32 = 0x6000_0000;
/// Set the normal SCSI command timeout (lower bits = seconds).
pub const MT_ST_SET_TIMEOUT: u32 = 0x7000_0000;
/// Set the long SCSI command timeout used for rewind/erase (lower bits = seconds).
pub const MT_ST_SET_LONG_TIMEOUT: u32 = 0x7010_0000;
/// Configure the cleaning request threshold reported via status flags.
pub const MT_ST_SET_CLN: u32 = 0x8000_0000;

// Boolean flags (OR into MT_ST_BOOLEANS / MT_ST_SETBOOLEANS / MT_ST_CLEARBOOLEANS):
/// Buffer write operations in the driver (write returns before data reaches tape).
pub const MT_ST_BUFFER_WRITES: u32 = 0x1;
/// Use asynchronous writes (driver returns immediately; errors reported later).
pub const MT_ST_ASYNC_WRITES: u32 = 0x2;
/// Enable read-ahead buffering.
pub const MT_ST_READ_AHEAD: u32 = 0x4;
/// Enable driver debug output (kernel must be compiled with `ST_DEBUGGING`).
pub const MT_ST_DEBUGGING: u32 = 0x8;
/// Write two filemarks when the device is closed after a write, rather than one.
pub const MT_ST_TWO_FM: u32 = 0x10;
/// Use the fast seek-to-EOD path (`MTEOM` ioctl) rather than spacing filemarks.
pub const MT_ST_FAST_MTEOM: u32 = 0x20;
/// Automatically lock the drive door when the device is opened.
pub const MT_ST_AUTO_LOCK: u32 = 0x40;
/// Interpret `O_WRONLY` opens as a request to write from the current position
/// (default writes mode for the device).
pub const MT_ST_DEF_WRITES: u32 = 0x80;
/// Drive can backspace over records (enables `MTBSR`).
pub const MT_ST_CAN_BSR: u32 = 0x100;
/// Drive does not report block size limits (skip `READ BLOCK LIMITS` command).
pub const MT_ST_NO_BLKLIMS: u32 = 0x200;
/// Drive supports tape partitions (required before using `MTSETPART`/`MTMKPART`).
pub const MT_ST_CAN_PARTITIONS: u32 = 0x400;
/// Use SCSI-2 logical block addressing for seek/tell operations.
pub const MT_ST_SCSI2LOGICAL: u32 = 0x800;
/// Use System V semantics: rewind on the last `close` of a non-rewinding node.
pub const MT_ST_SYSV: u32 = 0x1000;
/// Return immediately (rather than waiting) when the drive is not ready.
pub const MT_ST_NOWAIT: u32 = 0x2000;
/// Suppress the Incorrect Length Indicator (SILI) for variable-block reads.
pub const MT_ST_SILI: u32 = 0x4000;
/// Do not wait for the drive to report EOF; return immediately at filemark.
pub const MT_ST_NOWAIT_EOF: u32 = 0x8000;

/// Returns `true` if `op` is one of the `MT*` constants defined in this module.
pub fn is_known_op(op: i16) -> bool {
    matches!(
        op,
        MTFSF | MTBSF | MTFSR | MTBSR | MTWEOF | MTREW | MTOFFL | MTNOP | MTRETEN | MTBSFM
            | MTFSFM | MTEOM | MTERASE | MTSETBLK | MTSETDENSITY | MTSEEK | MTLOCK | MTUNLOCK
            | MTLOAD | MTUNLOAD | MTCOMPRESSION | MTSETPART | MTMKPART | MTWEOFI | MTFSS | MTBSS
            | MTWSM | MTSETDRVBUFFER
    )
}

// ── C structs ─────────────────────────────────────────────────────────────

/// Argument passed to the `MTIOCTOP` ioctl.
///
/// Corresponds to `struct mtop` in `linux/mtio.h`:
///
/// ```c
/// struct mtop {
///     short mt_op;    /* operation code (MT* constants above) */
///     int   mt_count; /* repeat count or parameter */
/// };
/// ```
#[repr(C)]
pub struct MtOp {
    /// The operation to perform (one of the `MT*` constants in this module).
    pub mt_op: i16,
    _pad: i16,
    /// Operation parameter — meaning depends on `mt_op`.
    /// For spacing operations: number of filemarks/records to traverse.
    /// For `MTSETBLK`: block size in bytes.
    /// For `MTSEEK`: target logical block number.
    pub mt_count: i32,
}

impl MtOp {
    pub fn new(mt_op: i16, mt_count: i32) -> Self {
        Self { mt_op, _pad: 0, mt_count }
    }
}

/// Drive status returned by the `MTIOCGET` ioctl.
///
/// Corresponds to `struct mtget` in `linux/mtio.h`:
///
/// ```c
/// struct mtget {
///     long mt_type;    /* drive type identifier */
///     long mt_resid;   /* residual count after last I/O */
///     long mt_dsreg;   /* drive-specific status register */
///     long mt_gstat;   /* generic (device-independent) status flags */
///     long mt_erreg;   /* error register */
///     int  mt_fileno;  /* current tape file number (0-based) */
///     int  mt_blkno;   /* current block number within the tape file */
/// };
/// ```
///
/// The most useful field for callers is `mt_gstat`, whose bits are decoded by
/// [`StatusFlags`](crate::status::StatusFlags).
#[repr(C)]
pub struct MtGet {
    /// Drive type (manufacturer-specific; useful for distinguishing LTO
    /// generations or other drive families).
    pub mt_type: i64,
    /// Residual byte count from the last read or write that did not transfer
    /// a complete record. Non-zero values indicate partial I/O.
    pub mt_resid: i64,
    /// Drive-specific status register. Contents are hardware-dependent.
    pub mt_dsreg: i64,
    /// Generic status flags. Decode with
    /// [`StatusFlags`](crate::status::StatusFlags).
    pub mt_gstat: i64,
    /// Drive-specific error register. Consult the drive manual.
    pub mt_erreg: i64,
    /// Current tape file number (0-based). Increments each time a filemark is
    /// crossed. Resets to 0 on rewind.
    pub mt_fileno: i32,
    /// Current record (block) number within the current tape file.
    pub mt_blkno: i32,
}

/// Logical block position returned by the `MTIOCPOS` ioctl.
///
/// Corresponds to `struct mtpos` in `linux/mtio.h`:
///
/// ```c
/// struct mtpos {
///     long mt_blkno; /* absolute logical block number */
/// };
/// ```
///
/// Unlike `mt_blkno` in `MtGet` (which resets per tape file), `mt_blkno`
/// here is an *absolute* logical block number that increases monotonically
/// from BOT. It can be passed to `MTSEEK` to return to the same position
/// later.
#[repr(C)]
pub struct MtPos {
    /// Absolute logical block number from the beginning of the tape.
    pub mt_blkno: i64,
}

// ── ioctl bindings ────────────────────────────────────────────────────────
//
// `nix::ioctl_write_ptr!` generates:
//   unsafe fn <name>(fd: RawFd, data: *const T) -> nix::Result<i32>
//
// `nix::ioctl_read!` generates:
//   unsafe fn <name>(fd: RawFd, data: *mut T) -> nix::Result<i32>
//
// The macro derives the ioctl number as _IOW / _IOR(type, nr, size_of::<T>()).
// Sizes must match the kernel structs exactly to produce the right numbers:
//
//   MtOp  →  8 B  →  _IOW('m', 1,  8) = 0x40086d01 = MTIOCTOP  ✓
//   MtGet → 48 B  →  _IOR('m', 2, 48) = 0x80306d02 = MTIOCGET  ✓
//   MtPos →  8 B  →  _IOR('m', 3,  8) = 0x80086d03 = MTIOCPOS  ✓

// Issue a tape operation (MTIOCTOP).
// Safety: fd must be a valid tape device fd; data must point to a valid MtOp.
ioctl_write_ptr!(mtioctop_raw, b'm', 1, MtOp);

// Read drive status (MTIOCGET).
// Safety: fd must be a valid tape device fd; data must point to a valid MtGet.
ioctl_read!(mtiocget_raw, b'm', 2, MtGet);

// Read current logical block position (MTIOCPOS).
// Safety: fd must be a valid tape device fd; data must point to a valid MtPos.
ioctl_read!(mtiocpos_raw, b'm', 3, MtPos);
