# mtio

Safe Rust bindings for the Linux SCSI tape driver — `ioctl(2)` interface to
`/dev/nst*` tape devices, with an in-memory mock for unit testing.

## Features

- **Full `MTIOCTOP` coverage** — rewind, seek to EOD, forward/backward space
  over filemarks and records, write filemarks, set block size, load/unload,
  lock/unlock door, physical erase from current position to EOT.
- **Drive status and position** — `MTIOCGET` (flags, file number, block
  number, block size, drive type) and `MTIOCPOS` (absolute logical block
  position).
- **`Tape` trait** — a single trait implemented by both `TapeDevice` and
  `MockTape`, so all higher-level logic can be written and tested against the
  mock without any hardware present.
- **`MockTape`** — in-memory tape simulation backed by `Vec<Vec<u8>>`, with
  correct filemark, overwrite, and write-protection semantics. Available via
  the `mock` feature flag or automatically in `#[cfg(test)]` contexts.
  Targets variable-length block mode (the Linux `st` driver default); see
  [block mode notes](#block-mode) below.
- **`StatusFlags`** — typed bitmask for `mt_gstat`, with named constants and
  predicate methods (`is_bot()`, `is_write_protected()`, `is_eod()`, …)
  matching the `GMT_*` macros in `linux/mtio.h`.
- **Linux only** — `TapeDevice` and the raw ioctl bindings are compiled only
  on `target_os = "linux"`. The `Tape` trait and `MockTape` compile on all
  platforms, so the rest of the workspace can be developed on macOS or
  Windows.

## Installation

```toml
[dependencies]
mtio = { git = "https://github.com/dc-ja/mtio-rs" }

# To use MockTape in tests of a downstream crate:
[dev-dependencies]
mtio = { git = "https://github.com/dc-ja/mtio-rs", features = ["mock"] }
```

Requires Rust 1.74+ and Linux.

## Usage

### Writing to tape

```rust
use mtio::{TapeDevice, Tape};
use std::io::Write;
use std::path::Path;

fn main() -> Result<(), mtio::TapeError> {
    let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;

    // Abort if the cartridge is write-protected.
    if drive.status()?.flags.is_write_protected() {
        eprintln!("tape is write-protected");
        return Ok(());
    }

    drive.rewind()?;
    drive.lock()?;

    // Write two tape files separated by a filemark, then a double filemark
    // to mark the logical end of the archive.
    drive.write_all(b"file 0 contents")?;
    drive.write_filemarks(1)?;
    drive.write_all(b"file 1 contents")?;
    drive.write_filemarks(2)?; // double filemark = logical EOA

    drive.unlock()?;
    drive.rewind()?;
    Ok(())
}
```

### Reading from tape

```rust
use mtio::{TapeDevice, Tape};
use std::io::Read;
use std::path::Path;

fn main() -> Result<(), mtio::TapeError> {
    let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;
    drive.rewind()?;

    // Read tape file 0 — read() returns Ok(0) at a filemark boundary.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = drive.read(&mut tmp)?;
        if n == 0 { break; }         // filemark reached; driver auto-advances
        buf.extend_from_slice(&tmp[..n]);
    }
    println!("file 0: {} bytes", buf.len());

    // The driver has already advanced past the filemark — the next read
    // starts at file 1 immediately. Do NOT call space_filemarks(1) here;
    // that would skip file 1 entirely.
    // ... repeat read loop ...
    Ok(())
}
```

### Querying drive status

```rust
use mtio::{TapeDevice, Tape};
use std::path::Path;

fn main() -> Result<(), mtio::TapeError> {
    let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;
    let status = drive.status()?;

    println!("file number : {}", status.file_number);
    println!("block number: {}", status.block_number);
    println!("online      : {}", status.flags.is_online());
    println!("write-prot  : {}", status.flags.is_write_protected());
    println!("at BOT      : {}", status.flags.is_bot());
    println!("at EOD      : {}", status.flags.is_eod());
    Ok(())
}
```

### Erasing tape

Two levels of erasure are available:

**Truncation** — every write (data record or filemark) causes the drive
firmware to update its End-of-Data (EOD) marker to immediately follow the
last written item. Everything beyond that point becomes inaccessible: normal
I/O stops at EOD regardless of what is magnetically recorded past it. Writing
a filemark at a chosen position is therefore the conventional way to mark
the end of an archive at that position; the firmware EOD update is what makes
prior data unreachable, not the filemark count itself. The magnetically
recorded data past the new EOD is not destroyed — it simply cannot be reached
by normal means. Fast, minimal wear.

**Physical erase** — `erase(long_erase)` issues `MTERASE`. With
`long_erase = true`, the drive's erase head traverses the full remaining tape,
permanently destroying all data from the current position to EOT (slow,
high-wear). With `long_erase = false`, only an EOD marker is written at the
current position; prior data is left magnetically intact but unreachable (fast,
low-wear). Both forms are irreversible. Neither is a cryptographic secure erase.

```rust
use mtio::{TapeDevice, Tape};
use std::path::Path;

fn main() -> Result<(), mtio::TapeError> {
    let mut drive = TapeDevice::open(Path::new("/dev/nst0"))?;

    // Remove the last file: seek to EOD, space back one filemark,
    // and write a double filemark (the POSIX tar EOA convention).
    // The drive updates its EOD marker after the write, making the
    // removed file unreachable without physically destroying it.
    drive.seek_to_eod()?;
    drive.space_filemarks(-1)?;
    drive.write_filemarks(2)?;

    // Physical erase from the start of file 2 to EOT.
    drive.rewind()?;
    drive.space_filemarks(2)?;
    drive.erase(true)?; // WARNING: long erase — slow, destructive, high-wear

    Ok(())
}
```

### Testing with MockTape
> [!IMPORTANT]
> `MockTape` does not emulate each and every aspect of a tape drive. Apart
> from the obvious (instant operation rather than delay due to tape travel),
> there are differences like the assumption that a tape is always present,
> a focus on variable [block mode](#block-mode) with only 
> [partial support](#mocktape) for fixed block mode, and no auto-rewind after
> writing operations.

```rust
use mtio::{MockTape, Tape};
use std::io::{Read, Write};

#[test]
fn my_backup_logic_round_trip() {
    let mut tape = MockTape::new();

    // Exercise your write logic.
    tape.write_all(b"header").unwrap();
    tape.write_filemarks(1).unwrap();
    tape.write_all(b"payload").unwrap();
    tape.write_filemarks(2).unwrap();

    // Verify with your read logic.
    tape.rewind().unwrap();
    let mut buf = [0u8; 6];
    tape.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"header");
}
```

Any function that accepts `&mut impl Tape` works with both `TapeDevice` and
`MockTape` without modification.

## API overview

All methods below are part of the [`Tape`] trait and are available on both
`TapeDevice` and `MockTape`.

| Method                   | ioctl / operation | Description                          |
| ------------------------ | ----------------- | ------------------------------------ |
| `rewind()`               | `MTREW`           | Seek to BOT                          |
| `seek_to_eod()`          | `MTEOM`           | Seek to end of recorded data         |
| `space_filemarks(n)`     | `MTFSF` / `MTBSF` | Space ±n filemarks                   |
| `space_records(n)`       | `MTFSR` / `MTBSR` | Space ±n records                     |
| `write_filemarks(n)`     | `MTWEOF`          | Write n filemarks                    |
| `seek_block(n)`          | `MTSEEK`          | Seek to logical block n (≤ i32::MAX) |
| `set_block_size(n)`      | `MTSETBLK`        | Set fixed block size (0 = variable)  |
| `load()`                 | `MTLOAD`          | SCSI LOAD                            |
| `unload()`               | `MTUNLOAD`        | SCSI UNLOAD / eject                  |
| `lock()`                 | `MTLOCK`          | Lock drive door                      |
| `unlock()`               | `MTUNLOCK`        | Unlock drive door                    |
| `status()`               | `MTIOCGET`        | Read drive status and flags          |
| `position()`             | `MTIOCPOS`        | Read absolute logical block position |
| `erase(long_erase)`      | `MTERASE`         | Physically erase from current position to EOT |

### TapeDevice-only

The following method is only available on `TapeDevice`, not through the `Tape`
trait, and has no `MockTape` equivalent.

| Method              | ioctl / operation | Description                                       |
| ------------------- | ----------------- | ------------------------------------------------- |
| `raw_op(op, count)` | `MTIOCTOP`        | Issue any `MTIOCTOP` operation by code directly. Use the `MT*` constants exported from this crate. |

## Block mode

The Linux `st` driver supports two block modes, selected via `set_block_size`
(`MTSETBLK`):

**Variable-length mode** (`block_size = 0`, the default) — each `write(2)`
call produces one tape record of whatever size is passed. On `read(2)`, the
drive returns exactly one record; the read buffer must be at least as large as
the record or the read fails with `ENOMEM`. This is the Linux `st` driver
default and is used unless an explicit `set_block_size` call overrides it.

**Fixed block mode** (`block_size > 0`) — every record on tape is exactly
`block_size` bytes. All `read(2)` and `write(2)` buffers must be multiples of
`block_size`; misaligned I/O fails with `EINVAL`. The block size is physically
encoded in the tape format, so a tape written in fixed mode must be read with a
matching block size.

### TapeDevice

`TapeDevice` passes `set_block_size` directly to the drive via `MTSETBLK`. The
current block size is available as `TapeStatus::block_size` after a `status()`
call (decoded from the `mt_dsreg` field of `struct mtget`).

### MockTape

`MockTape` targets variable-length mode as its primary use case, matching the
`st` driver default. Fixed block mode is partially supported:

| Behaviour                                                                               | Supported                                            |
| -----------------------------------------------------------------------------------------| ------------------------------------------------------|
| `set_block_size` stored and reported via `status()`                                     | Yes                                                  |
| Write alignment enforced (`EINVAL` for non-multiples)                                   | Yes                                                  |
| Read alignment enforced (`EINVAL` for non-multiples)                                    | Yes                                                  |
| `space_records` steps by `block_size` bytes                                             | Yes                                                  |
| Per-record read boundary enforcement (`ENOMEM` for undersized buffers in variable mode) | No — `MockTape` always does a byte-stream short read |

## Development notes

### Running tests

```sh
cargo test
```

### AI assistance

The initial API design, ioctl constant verification, and documentation were
developed with assistance from Claude (Anthropic). All code was reviewed and
the final implementation decisions were made by the project author.

## License

MIT OR Apache-2.0
