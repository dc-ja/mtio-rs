use bitflags::bitflags;

/// Tape drive status returned by the `MTIOCGET` ioctl.
///
/// Obtained via [`Tape::status`](crate::Tape::status). The most useful field
/// for callers is [`flags`](TapeStatus::flags), which reports whether the tape
/// is write-protected, online, at BOT, etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TapeStatus {
    /// Manufacturer-specific drive type code from `mt_type`. Opaque; use
    /// [`DriveType`] for any type-based branching.
    pub drive_type: DriveType,
    /// Current tape file number (0-based). Increments by one each time the
    /// tape crosses a filemark. Resets to 0 on rewind. If the file-number
    /// is unknown, the value is -1.
    pub file_number: i32,
    /// Current record number within the current tape file. Resets to 0 at
    /// each filemark boundary. If the record number is unknown, the value
    /// is -1.
    pub block_number: i32,
    /// Current block (record) size in bytes, from the `mt_dsreg` field of
    /// `struct mtget` (bits 0–23, `MT_ST_BLKSIZE_MASK`).
    ///
    /// `0` means variable-length mode — each `write(2)` call determines the
    /// size of that record individually. Any non-zero value is the fixed block
    /// size: every record on tape is exactly this many bytes, and all `read(2)`
    /// and `write(2)` buffers must be multiples of it.
    pub block_size: u32,
    /// Generic device-independent status flags decoded from `mt_gstat`.
    pub flags: StatusFlags,
}

/// Opaque drive-type identifier from the `mt_type` field of `struct mtget`.
///
/// The numeric value is manufacturer-specific. Stored as-is for logging and
/// diagnostics; no semantic meaning is ascribed to specific values here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DriveType(pub i64);

bitflags! {
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct StatusFlags: i64 {
        /// A filemark was encountered during the last spacing or read operation.
        /// This is set transiently; check it immediately after the operation.
        const EOF     = 0x8000_0000;
        /// The tape is positioned at the physical beginning of the medium (BOT).
        /// Set after a rewind or when the tape is first loaded.
        const BOT     = 0x4000_0000;
        /// The tape is at or past the physical end of the medium (EOT — the
        /// early-warning region near the end of writable tape). This is *not* the
        /// same as [`EOD`](Self::EOD): EOT is physical; EOD is logical.
        const EOT     = 0x2000_0000;
        /// A setmark was encountered (SCSI-2 feature, rarely used).
        const SM      = 0x1000_0000;
        /// End of recorded data (EOD): no further data has been written past the
        /// current position. Appending new data starts here.
        const EOD     = 0x0800_0000;
        /// The loaded cartridge is write-protected (physical write-protect tab or
        /// WORM media). Writing will fail with a permission error.
        const WR_PROT = 0x0400_0000;
        /// The drive is online (a cartridge is loaded and the drive is ready).
        const ONLINE  = 0x0100_0000;
        /// The drive door is open — no cartridge is loaded.
        const DR_OPEN = 0x0004_0000;
        /// Immediate report mode is enabled (drive returns status before completing
        /// long operations). Not commonly used in backup applications.
        const IM_REP_EN = 0x0001_0000;
        /// The drive is requesting a cleaning cartridge. Back up and replace with
        /// a cleaning tape before continuing.
        const CLN     = 0x0000_8000;
    }
}

impl Default for StatusFlags {
    fn default() -> Self {
        StatusFlags::empty()
    }
}

impl StatusFlags {
    /// Returns `true` if a filemark was encountered during the last operation.
    ///
    /// This flag is transient — it reflects the outcome of the most recent
    /// spacing or read call and should be checked immediately after that call.
    #[must_use]
    pub fn is_eof(self) -> bool { self.contains(Self::EOF) }

    /// Returns `true` if the tape is positioned at the physical beginning
    /// of the medium (BOT).
    #[must_use]
    pub fn is_bot(self) -> bool { self.contains(Self::BOT) }

    /// Returns `true` if the tape is at or past the physical end of the
    /// medium (EOT — early-warning zone before the actual end of tape).
    #[must_use]
    pub fn is_eot(self) -> bool { self.contains(Self::EOT) }

    /// Returns `true` if the tape is at the logical end of recorded data
    /// (EOD): no data has been written past this point.
    #[must_use]
    pub fn is_eod(self) -> bool { self.contains(Self::EOD) }

    /// Returns `true` if the loaded cartridge is write-protected.
    #[must_use]
    pub fn is_write_protected(self) -> bool { self.contains(Self::WR_PROT) }

    /// Returns `true` if the drive is online (a cartridge is loaded and the
    /// drive is ready to accept commands).
    #[must_use]
    pub fn is_online(self) -> bool { self.contains(Self::ONLINE) }

    /// Returns `true` if the drive door is open (no cartridge loaded).
    #[must_use]
    pub fn is_door_open(self) -> bool { self.contains(Self::DR_OPEN) }

    /// Returns `true` if the drive is requesting a cleaning cartridge.
    #[must_use]
    pub fn is_cleaning_requested(self) -> bool { self.contains(Self::CLN) }

    /// Returns `true` if a setmark was encountered during the last operation.
    #[must_use]
    pub fn is_setmark(self) -> bool { self.contains(Self::SM) }

    /// Returns `true` if immediate report mode is enabled.
    #[must_use]
    pub fn is_immediate_report(self) -> bool { self.contains(Self::IM_REP_EN) }
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
        assert!(!f.is_setmark());
        assert!(!f.is_immediate_report());
    }

    #[test]
    fn individual_flags_are_independently_testable() {
        // Each constant must affect exactly the method that names it and no other.
        let cases: &[(StatusFlags, fn(StatusFlags) -> bool)] = &[
            (StatusFlags::EOF,    StatusFlags::is_eof),
            (StatusFlags::BOT,    StatusFlags::is_bot),
            (StatusFlags::EOT,    StatusFlags::is_eot),
            (StatusFlags::EOD,    StatusFlags::is_eod),
            (StatusFlags::WR_PROT, StatusFlags::is_write_protected),
            (StatusFlags::ONLINE, StatusFlags::is_online),
            (StatusFlags::DR_OPEN,   StatusFlags::is_door_open),
            (StatusFlags::CLN,       StatusFlags::is_cleaning_requested),
            (StatusFlags::SM,        StatusFlags::is_setmark),
            (StatusFlags::IM_REP_EN, StatusFlags::is_immediate_report),
        ];

        for &(flag, check) in cases {
            assert!(check(flag), "flag {flag:?} not detected by its own method");

            // All other methods must return false.
            for &(other_flag, other_check) in cases {
                if other_flag != flag {
                    assert!(
                        !other_check(flag),
                        "flag {flag:?} spuriously triggered method for {other_flag:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn multiple_flags_set_simultaneously() {
        let f = StatusFlags::BOT | StatusFlags::ONLINE;
        assert!(f.is_bot());
        assert!(f.is_online());
        assert!(!f.is_eod());
        assert!(!f.is_write_protected());
    }

    #[test]
    fn flag_constants_match_expected_bit_positions() {
        // Verify against the GMT_* macro values in linux/mtio.h.
        assert_eq!(StatusFlags::EOF.bits(),     0x8000_0000);
        assert_eq!(StatusFlags::BOT.bits(),     0x4000_0000);
        assert_eq!(StatusFlags::EOT.bits(),     0x2000_0000);
        assert_eq!(StatusFlags::SM.bits(),      0x1000_0000);
        assert_eq!(StatusFlags::EOD.bits(),     0x0800_0000);
        assert_eq!(StatusFlags::WR_PROT.bits(), 0x0400_0000);
        assert_eq!(StatusFlags::ONLINE.bits(),  0x0100_0000);
        assert_eq!(StatusFlags::DR_OPEN.bits(), 0x0004_0000);
        assert_eq!(StatusFlags::IM_REP_EN.bits(), 0x0001_0000);
        assert_eq!(StatusFlags::CLN.bits(),     0x0000_8000);
    }
}
