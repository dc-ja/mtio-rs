# mtio

Safe Rust bindings for the Linux SCSI tape driver — `ioctl(2)` interface to
`/dev/nst*` tape devices, with an in-memory mock for unit testing.

> [!CAUTION]
> Not all AI output has been verified yet.

> [!CAUTION]
> This crate has not yet been tested with an actual tape drive -- yet.

> [!NOTE]
> AI was used in the creation of this crate. 
> [See below for details](#ai-assistance).

## Features

- **Full `MTIOCTOP` coverage** — rewind, seek to EOD, forward/backward space
  over filemarks and records, write filemarks, set block size, load/unload,
  lock/unlock door.
- **Drive status and position** — `MTIOCGET` (flags, file number, block
  number, drive type) and `MTIOCPOS` (absolute logical block position).
- **`Tape` trait** — a single trait implemented by both `TapeDevice` and
  `MockTape`, so all higher-level logic can be written and tested against the
  mock without any hardware present.
- **`MockTape`** — in-memory tape simulation backed by `Vec<Vec<u8>>`, with
  correct filemark, overwrite, and write-protection semantics. Available via
  the `mock` feature flag or automatically in `#[cfg(test)]` contexts.
- **`StatusFlags`** — typed bitmask for `mt_gstat`, with named constants and
  predicate methods (`is_bot()`, `is_write_protected()`, `is_eod()`, …)
  matching the `GMT_*` macros in `linux/mtio.h`.
- **Linux only** — `TapeDevice` and the raw ioctl bindings are compiled only
  on `target_os = "linux"`. The `Tape` trait and `MockTape` compile on all
  platforms, so the rest of the workspace can be developed on macOS or
  Windows.

## Background: tape vs. disk

Tape is a *sequential-access* medium. You cannot seek to an arbitrary byte
position; you move forward or backward by whole *records* (blocks) or
*filemarks*. Writing at any position discards everything recorded after it.

Data is grouped into *tape files* separated by filemarks. A `read(2)` at a
filemark boundary returns 0 bytes (like a regular EOF); the caller must call
`space_filemarks(1)` to step past it. Two consecutive filemarks signal the
logical end of an archive (the POSIX/GNU `tar` convention).

Always open the **non-rewinding** device node (`/dev/nst0`, `/dev/nst1`, …).
The rewinding node (`/dev/st0`) rewinds to BOT on `close(2)`, which silently
destroys data in a multi-file session.

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
        if n == 0 { break; }         // filemark reached
        buf.extend_from_slice(&tmp[..n]);
    }
    println!("file 0: {} bytes", buf.len());

    // Step past the filemark and read file 1.
    drive.space_filemarks(1)?;
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

### Testing with MockTape

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

| Method | ioctl / operation | Description |
|---|---|---|
| `rewind()` | `MTREW` | Seek to BOT |
| `seek_to_eod()` | `MTEOM` | Seek to end of recorded data |
| `space_filemarks(n)` | `MTFSF` / `MTBSF` | Space ±n filemarks |
| `space_records(n)` | `MTFSR` / `MTBSR` | Space ±n records |
| `write_filemarks(n)` | `MTWEOF` | Write n filemarks |
| `seek_block(n)` | `MTSEEK` | Seek to logical block n (≤ i32::MAX) |
| `set_block_size(n)` | `MTSETBLK` | Set fixed block size (0 = variable) |
| `load()` | `MTLOAD` | SCSI LOAD |
| `unload()` | `MTUNLOAD` | SCSI UNLOAD / eject |
| `lock()` | `MTLOCK` | Lock drive door |
| `unlock()` | `MTUNLOCK` | Unlock drive door |
| `status()` | `MTIOCGET` | Read drive status and flags |
| `position()` | `MTIOCPOS` | Read absolute logical block position |

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
