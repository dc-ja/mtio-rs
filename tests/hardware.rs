//! Hardware integration tests for `mtio`.
//!
//! These tests require a real tape drive and a scratch tape. They are gated
//! behind the `hardware` feature to prevent accidental runs in CI or during
//! normal development.
//!
//! # Running
//!
//! ```sh
//! cargo test --features hardware -- --test-threads=1
//! ```
//!
//! `--test-threads=1` is mandatory: Rust's test harness runs tests in parallel
//! by default, but there is only one tape head. Concurrent tests on the same
//! device would produce undefined results.
//!
//! # Device path
//!
//! Set the `MTIO_TEST_DEVICE` environment variable to override the default:
//!
//! ```sh
//! MTIO_TEST_DEVICE=/dev/nst1 cargo test --features hardware -- --test-threads=1
//! ```
//!
//! Always use the **non-rewinding** node (`/dev/nst*`).
//!
//! # Tape wear
//!
//! All tests write only a small amount of data near the beginning of tape and
//! rewind at most once each. Operations that traverse the full tape
//! (`seek_to_eod` on a full tape, re-tension) are deliberately omitted.
//! Use a dedicated scratch tape for these tests.

#![cfg(feature = "hardware")]

use mtio::{Tape, TapeDevice, TapeError};
use std::io::{Read, Write};
use std::path::Path;

/// Timeout applied to the initial `open()` call in [`open_drive`].
///
/// Opening a tape device can block indefinitely when no tape is loaded — the
/// kernel waits for the drive to become ready. Ten seconds is generous for a
/// drive with a tape already seated; increase it if your drive is slow to spin
/// up after insertion.
const OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// ── Helpers ───────────────────────────────────────────────────────────────

/// Open the tape device, failing with a clear message if it takes too long.
///
/// `TapeDevice::open` can block indefinitely when no tape is loaded (the
/// kernel waits for the drive to become ready). This wrapper spawns the open
/// on a background thread and panics with a timeout message if it does not
/// complete within [`OPEN_TIMEOUT`].
fn open_drive() -> TapeDevice {
    let path = std::env::var("MTIO_TEST_DEVICE").unwrap_or_else(|_| "/dev/nst0".into());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = TapeDevice::open(Path::new(&path));
        let _ = tx.send(result);
    });
    rx.recv_timeout(OPEN_TIMEOUT)
        .expect("timed out waiting for tape drive — load a tape and retry")
        .expect("failed to open tape device — is the drive attached?")
}

/// Read bytes from the current tape file until a filemark boundary (`Ok(0)`).
fn read_file(drive: &mut TapeDevice) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = drive.read(&mut tmp).expect("read failed");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    buf
}

// ── Tests ─────────────────────────────────────────────────────────────────
//
// Each test rewinding to BOT at the start so they can run in any order
// (given --test-threads=1). Tests write only near BOT and use small payloads.

/// The drive should be online with a tape loaded before these tests run.
#[test]
fn drive_is_online() {
    let mut drive = open_drive();
    let status = drive.status().expect("status() failed");
    assert!(
        status.flags.is_online(),
        "drive is not online — load a tape before running hardware tests"
    );
    assert!(
        !status.flags.is_door_open(),
        "drive door is open — load a tape before running hardware tests"
    );
}

/// `status()` and `position()` return without error and report a sane state.
///
/// `position()` requires the drive to support the SCSI READ POSITION command
/// (MTIOCPOS ioctl). Drives that do not support it return EIO; the test is
/// skipped in that case rather than failed.
#[test]
fn status_and_position_succeed() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    let status = drive.status().expect("status() failed");
    assert!(status.flags.is_bot(), "expected BOT after rewind");

    match drive.position() {
        Ok(pos) => {
            // After a rewind the logical block position should be at or very
            // close to the start. We don't assert == 0 because some drives
            // report 1 here.
            assert!(pos < 16, "unexpected block position after rewind: {pos}");
        }
        Err(TapeError::Ioctl(_)) => {
            eprintln!("note: position() is not supported by this drive (MTIOCPOS returned an ioctl error) — skipping position assertion");
        }
        Err(e) => panic!("position() failed unexpectedly: {e}"),
    }
}

/// Write a single tape file, rewind, read it back, and verify the contents.
#[test]
fn write_and_read_single_file() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    let payload = b"mtio hardware test -- single file";
    drive.write_all(payload).expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");

    drive.rewind().expect("rewind failed");
    let read_back = read_file(&mut drive);
    assert_eq!(read_back, payload);
}

/// Write two tape files, rewind, and verify each can be read back independently.
#[test]
fn write_and_read_two_files() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    let file0 = b"mtio hardware test -- file 0";
    let file1 = b"mtio hardware test -- file 1";

    drive.write_all(file0).expect("write file0 failed");
    drive.write_filemarks(1).expect("write_filemarks(1) failed");
    drive.write_all(file1).expect("write file1 failed");
    drive.write_filemarks(2).expect("write_filemarks(2) failed"); // double FM = logical EOA

    // Verify file 0.
    drive.rewind().expect("rewind failed");
    let got0 = read_file(&mut drive);
    assert_eq!(got0, file0);

    // Verify file 1. We rewind and use space_filemarks from a clean (non-FM)
    // state rather than continuing from where the previous read left off.
    //
    // When read() returns Ok(0) at a filemark, the Linux st driver increments
    // the logical file position past the filemark internally. A subsequent
    // space_filemarks(1) would then skip one *additional* filemark, landing
    // past file 1. Rewinding first avoids this driver-specific behaviour.
    drive.rewind().expect("rewind failed");
    drive.space_filemarks(1).expect("space_filemarks failed");
    let got1 = read_file(&mut drive);
    assert_eq!(got1, file1);
}

/// Reading to a filemark auto-advances to the next tape file.
///
/// When `read()` returns `Ok(0)` at a filemark boundary, the Linux `st` driver
/// automatically advances the logical position past the filemark. The next
/// `read()` call returns data from the following tape file without any
/// `space_filemarks` call.
#[test]
fn read_auto_advances_past_filemark() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    let file0 = b"mtio hardware test -- auto-advance file 0";
    let file1 = b"mtio hardware test -- auto-advance file 1";

    drive.write_all(file0).expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");
    drive.write_all(file1).expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");

    drive.rewind().expect("rewind failed");

    // Read file 0 to its filemark boundary (returns Ok(0)).
    let got0 = read_file(&mut drive);
    assert_eq!(got0, file0);

    // No space_filemarks call: the next read should already be at file 1.
    let got1 = read_file(&mut drive);
    assert_eq!(got1, file1);
}

/// `space_filemarks(1)` advances past a filemark to the next tape file.
#[test]
fn space_filemarks_skips_to_next_file() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    drive.write_all(b"skip me").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");
    drive.write_all(b"read me").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");

    drive.rewind().expect("rewind failed");
    drive.space_filemarks(1).expect("space_filemarks failed"); // skip file 0
    let got = read_file(&mut drive);
    assert_eq!(got, b"read me");
}

/// `position()` returns a block number that `seek_block()` can return to.
///
/// Both `position()` and `seek_block()` require the drive to support absolute
/// block addressing (MTIOCPOS / MTSEEK ioctls, backed by the SCSI READ
/// POSITION and LOCATE commands). The test is skipped on drives that do not
/// support them.
#[test]
fn position_and_seek_block_round_trip() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    drive.write_all(b"file 0").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");
    drive.write_all(b"file 1").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");

    // Capture the block position at the start of file 1.
    drive.rewind().expect("rewind failed");
    drive.space_filemarks(1).expect("space_filemarks failed");
    let pos = match drive.position() {
        Ok(p) => p,
        Err(TapeError::Ioctl(_)) => {
            eprintln!("note: position() is not supported by this drive — skipping seek_block round-trip test");
            return;
        }
        Err(e) => panic!("position() failed unexpectedly: {e}"),
    };

    // Advance further, then seek back.
    drive.space_filemarks(1).expect("space_filemarks failed");
    match drive.seek_block(pos) {
        Ok(()) => {}
        Err(TapeError::Ioctl(_)) => {
            eprintln!("note: seek_block() is not supported by this drive — skipping assertion");
            return;
        }
        Err(e) => panic!("seek_block() failed unexpectedly: {e}"),
    }

    let got = read_file(&mut drive);
    assert_eq!(got, b"file 1");
}

/// `status()` reports the correct file number after positioning.
#[test]
fn status_file_number_tracks_position() {
    let mut drive = open_drive();
    drive.rewind().expect("rewind failed");

    if drive.status().expect("status failed").flags.is_write_protected() {
        panic!("tape is write-protected — use a writable scratch tape");
    }

    drive.write_all(b"f0").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");
    drive.write_all(b"f1").expect("write failed");
    drive.write_filemarks(1).expect("write_filemarks failed");

    drive.rewind().expect("rewind failed");
    assert_eq!(drive.status().expect("status failed").file_number, 0);

    drive.space_filemarks(1).expect("space_filemarks failed");
    assert_eq!(drive.status().expect("status failed").file_number, 1);
}

/// `lock()` and `unlock()` succeed without error.
///
/// We cannot verify the mechanical effect without hardware introspection,
/// but at minimum they must not return an error on a drive that supports them.
#[test]
fn lock_and_unlock_succeed() {
    let mut drive = open_drive();
    drive.lock().expect("lock() failed");
    drive.unlock().expect("unlock() failed");
}

/// `set_block_size(0)` (variable-length mode) succeeds without error.
#[test]
fn set_block_size_variable_succeeds() {
    let mut drive = open_drive();
    drive.set_block_size(0).expect("set_block_size(0) failed");
}
