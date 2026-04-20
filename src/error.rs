use thiserror::Error;

/// Errors that can be returned by tape device operations.
///
/// Most callers will want to match on the specific variant to decide whether
/// to abort, retry, or surface a user-facing message:
///
/// ```no_run
/// # use mtio::{TapeDevice, Tape, TapeError};
/// # use std::path::Path;
/// # fn run() -> Result<(), TapeError> {
/// let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;
/// match drive.rewind() {
///     Ok(()) => {}
///     Err(TapeError::NotOnline) => eprintln!("no tape loaded"),
///     Err(TapeError::DoorOpen)  => eprintln!("drive door is open"),
///     Err(e) => return Err(e),
/// }
/// # Ok(()) }
/// ```
#[derive(Debug, Error)]
pub enum TapeError {
    /// A `read(2)` or `write(2)` system call failed.
    ///
    /// The inner [`std::io::Error`] carries the OS error code. Common causes:
    /// `ENOSPC` at physical end of tape, `EIO` for a hardware error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An `ioctl(2)` call to the tape driver failed.
    ///
    /// The inner `nix::Error` carries the OS `errno`. Common causes:
    /// `EACCES` for write-protect violations at the ioctl level, `EIO` for
    /// hardware errors, `ENODEV` if the drive is not present.
    #[cfg(target_os = "linux")]
    #[error("ioctl error: {0}")]
    Ioctl(#[from] nix::Error),

    /// The operation was rejected because the loaded cartridge is
    /// write-protected (physical write-protect tab set, or WORM media).
    ///
    /// Check the cartridge before retrying; write-protection cannot be
    /// overridden in software.
    #[error("tape is write-protected")]
    WriteProtected,

    /// The drive door is open — no cartridge is loaded.
    ///
    /// Load a cartridge and wait for the drive to come online before
    /// retrying.
    #[error("tape door is open — no media loaded")]
    DoorOpen,

    /// The drive is not online (no cartridge loaded, or drive powered off).
    ///
    /// Check physical drive status before retrying.
    #[error("drive is not online")]
    NotOnline,

    /// The tape has reached the physical end of medium.
    ///
    /// No more data can be written. If reading, all recorded data has been
    /// consumed.
    #[error("end of tape")]
    EndOfTape,

    /// The requested block number exceeds [`i32::MAX`], which is the maximum
    /// value accepted by the `MTSEEK` ioctl's 32-bit `mt_count` field.
    ///
    /// For the small number of tape files written by this application this
    /// limit is never reached in practice.
    #[error("block number {0} exceeds the 32-bit limit of the MTSEEK operation")]
    BlockNumberTooLarge(u64),

    /// An unrecognised operation code was passed to
    /// [`TapeDevice::raw_op`](crate::TapeDevice::raw_op).
    ///
    /// Use one of the `MT*` constants exported from this crate.
    #[error("unknown tape operation code {0}")]
    UnknownOperation(i16),
}
