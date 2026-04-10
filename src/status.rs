/// Tape drive status returned by the `MTIOCGET` ioctl.
///
/// Obtained via [`Tape::status`](crate::Tape::status). The most useful field
/// for callers is [`flags`](TapeStatus::flags), which reports whether the tape
/// is write-protected, online, at BOT, etc.
#[derive(Debug, Clone)]
pub struct TapeStatus {
    /// Manufacturer-specific drive type code from `mt_type`. Opaque; use
    /// [`DriveType`] for any type-based branching.
    pub drive_type: DriveType,
    /// Current tape file number (0-based). Increments by one each time the
    /// tape crosses a filemark. Resets to 0 on rewind.
    pub file_number: i32,
    /// Current record number within the current tape file. Resets to 0 at
    /// each filemark boundary.
    pub block_number: i32,
    /// Generic device-independent status flags decoded from `mt_gstat`.
    pub flags: StatusFlags,
}

/// Opaque drive-type identifier from the `mt_type` field of `struct mtget`.
///
/// The numeric value is manufacturer-specific. Stored as-is for logging and
/// diagnostics; no semantic meaning is ascribed to specific values here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveType(pub i64);

/// Bitmask of generic, device-independent status flags from the `mt_gstat`
/// field of `struct mtget`.
///
/// Bit positions match the `GMT_*` macros in `linux/mtio.h`. Not all drives
/// report all flags; unrecognised bits are silently ignored.
///
/// # Example
///
/// ```no_run
/// # use mtio::{TapeDevice, Tape};
/// # use std::path::Path;
/// # fn main() -> Result<(), mtio::TapeError> {
/// let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;
/// let status = drive.status()?;
/// if status.flags.is_write_protected() {
///     eprintln!("tape is write-protected — aborting");
///     return Ok(());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusFlags(pub i64);

impl StatusFlags {
    /// A filemark was encountered during the last spacing or read operation.
    /// This is set transiently; check it immediately after the operation.
    pub const EOF: i64 = 0x8000_0000;
    /// The tape is positioned at the physical beginning of the medium (BOT).
    /// Set after a rewind or when the tape is first loaded.
    pub const BOT: i64 = 0x4000_0000;
    /// The tape is at or past the physical end of the medium (EOT — the
    /// early-warning region near the end of writable tape). This is *not* the
    /// same as [`EOD`](Self::EOD): EOT is physical; EOD is logical.
    pub const EOT: i64 = 0x2000_0000;
    /// A setmark was encountered (SCSI-2 feature, rarely used).
    pub const SM: i64 = 0x1000_0000;
    /// End of recorded data (EOD): no further data has been written past the
    /// current position. Appending new data starts here.
    pub const EOD: i64 = 0x0800_0000;
    /// The loaded cartridge is write-protected (physical write-protect tab or
    /// WORM media). Writing will fail with a permission error.
    pub const WR_PROT: i64 = 0x0400_0000;
    /// The drive is online (a cartridge is loaded and the drive is ready).
    pub const ONLINE: i64 = 0x0100_0000;
    /// The drive door is open — no cartridge is loaded.
    pub const DR_OPEN: i64 = 0x0004_0000;
    /// Immediate report mode is enabled (drive returns status before completing
    /// long operations). Not commonly used in backup applications.
    pub const IM_REP_EN: i64 = 0x0001_0000;
    /// The drive is requesting a cleaning cartridge. Back up and replace with
    /// a cleaning tape before continuing.
    pub const CLN: i64 = 0x0000_8000;

    fn has(&self, flag: i64) -> bool {
        self.0 & flag != 0
    }

    /// Returns `true` if a filemark was encountered during the last operation.
    ///
    /// This flag is transient — it reflects the outcome of the most recent
    /// spacing or read call and should be checked immediately after that call.
    pub fn is_eof(&self) -> bool {
        self.has(Self::EOF)
    }

    /// Returns `true` if the tape is positioned at the physical beginning
    /// of the medium (BOT).
    pub fn is_bot(&self) -> bool {
        self.has(Self::BOT)
    }

    /// Returns `true` if the tape is at or past the physical end of the
    /// medium (EOT — early-warning zone before the actual end of tape).
    ///
    /// EOT is a *physical* marker. It does not mean there is no more recorded
    /// data; see [`is_eod`](Self::is_eod) for the logical end of data.
    pub fn is_eot(&self) -> bool {
        self.has(Self::EOT)
    }

    /// Returns `true` if the tape is at the logical end of recorded data
    /// (EOD): no data has been written past this point.
    ///
    /// Appending new data is only valid at or before the EOD position.
    pub fn is_eod(&self) -> bool {
        self.has(Self::EOD)
    }

    /// Returns `true` if the loaded cartridge is write-protected.
    ///
    /// Any write or filemark-write operation will fail with
    /// [`TapeError::WriteProtected`](crate::TapeError::WriteProtected).
    /// Check this flag before starting a write session.
    pub fn is_write_protected(&self) -> bool {
        self.has(Self::WR_PROT)
    }

    /// Returns `true` if the drive is online (a cartridge is loaded and the
    /// drive is ready to accept commands).
    pub fn is_online(&self) -> bool {
        self.has(Self::ONLINE)
    }

    /// Returns `true` if the drive door is open (no cartridge loaded).
    pub fn is_door_open(&self) -> bool {
        self.has(Self::DR_OPEN)
    }

    /// Returns `true` if the drive is requesting a cleaning cartridge.
    ///
    /// Drives accumulate debris on the read/write heads over time. When this
    /// flag is set, insert a cleaning cartridge before the next backup session
    /// to avoid read/write errors.
    pub fn is_cleaning_requested(&self) -> bool {
        self.has(Self::CLN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_flags_set() {
        let f = StatusFlags::default();
        assert!(!f.is_eof());
        assert!(!f.is_bot());
        assert!(!f.is_eot());
        assert!(!f.is_eod());
        assert!(!f.is_write_protected());
        assert!(!f.is_online());
        assert!(!f.is_door_open());
        assert!(!f.is_cleaning_requested());
    }

    #[test]
    fn individual_flags_are_independently_testable() {
        // Each constant must affect exactly the method that names it and no other.
        let cases: &[(i64, fn(&StatusFlags) -> bool)] = &[
            (StatusFlags::EOF,    StatusFlags::is_eof),
            (StatusFlags::BOT,    StatusFlags::is_bot),
            (StatusFlags::EOT,    StatusFlags::is_eot),
            (StatusFlags::EOD,    StatusFlags::is_eod),
            (StatusFlags::WR_PROT, StatusFlags::is_write_protected),
            (StatusFlags::ONLINE, StatusFlags::is_online),
            (StatusFlags::DR_OPEN, StatusFlags::is_door_open),
            (StatusFlags::CLN,    StatusFlags::is_cleaning_requested),
        ];

        for &(bit, check) in cases {
            let f = StatusFlags(bit);
            assert!(check(&f), "flag 0x{bit:08x} not detected by its own method");

            // All other methods must return false.
            for &(other_bit, other_check) in cases {
                if other_bit != bit {
                    assert!(
                        !other_check(&f),
                        "flag 0x{bit:08x} spuriously triggered method for 0x{other_bit:08x}"
                    );
                }
            }
        }
    }

    #[test]
    fn multiple_flags_set_simultaneously() {
        let f = StatusFlags(StatusFlags::BOT | StatusFlags::ONLINE);
        assert!(f.is_bot());
        assert!(f.is_online());
        assert!(!f.is_eod());
        assert!(!f.is_write_protected());
    }

    #[test]
    fn flag_constants_match_expected_bit_positions() {
        // Verify against the GMT_* macro values in linux/mtio.h.
        assert_eq!(StatusFlags::EOF,     0x8000_0000);
        assert_eq!(StatusFlags::BOT,     0x4000_0000);
        assert_eq!(StatusFlags::EOT,     0x2000_0000);
        assert_eq!(StatusFlags::SM,      0x1000_0000);
        assert_eq!(StatusFlags::EOD,     0x0800_0000);
        assert_eq!(StatusFlags::WR_PROT, 0x0400_0000);
        assert_eq!(StatusFlags::ONLINE,  0x0100_0000);
        assert_eq!(StatusFlags::DR_OPEN, 0x0004_0000);
        assert_eq!(StatusFlags::IM_REP_EN, 0x0001_0000);
        assert_eq!(StatusFlags::CLN,     0x0000_8000);
    }
}
