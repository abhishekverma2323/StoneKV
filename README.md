# stone

**A crash-safe embedded key-value store written in Rust with zero third-party dependencies.**

`stone` is a small log-structured storage engine built using only the Rust standard library. It provides persistent `set`, `get`, and `delete` operations through both a command-line interface and an embeddable Rust API.

The project focuses on durability, crash recovery, binary storage formats, sparse indexing, immutable segments, and full-store compaction without depending on external crates.

## Features

* Persistent `SET`, `GET`, and `DELETE`
* Write-ahead log (WAL)
* `File::sync_all()` durability before acknowledging writes
* Recovery from incomplete WAL crash tails
* Physical removal of invalid WAL tails after recovery
* Manual IEEE CRC32 integrity checks
* Sorted in-memory memtable using `BTreeMap`
* Automatic memtable flushing
* Immutable sorted segment files
* Sparse segment indexes
* Newest-to-oldest reads
* Tombstone-based deletes
* Full-store compaction
* Atomic temporary-file-to-segment installation
* Monotonic segment generations
* Store statistics
* Integrity verification command
* CLI and embeddable Rust API
* Single-process threaded access through `Arc<Mutex<Store>>`
* Zero third-party runtime dependencies

## Requirements

* Rust 1.98+ recommended
* Cargo
* No third-party Rust crates

## Build

```bash
cargo build --release
```

The release binary is produced at:

### Linux / macOS

```text
target/release/stone
```

### Windows

```text
target\release\stone.exe
```

## Quick Start

### Set a value

```bash
stone set hello world
```

Output:

```text
OK
```

### Get a value

```bash
stone get hello
```

Output:

```text
world
```

### Delete a value

```bash
stone del hello
```

Output:

```text
OK
```

### Get a missing value

```bash
stone get hello
```

Output on stderr:

```text
not found
```

### Use a custom data directory

```bash
stone set user:1 Abhishek --dir ./my-data
stone get user:1 --dir ./my-data
```

## Commands

```text
stone set <key> <value> [--dir PATH]
stone get <key> [--dir PATH]
stone del <key> [--dir PATH]
stone compact [--dir PATH]
stone stats [--dir PATH]
stone verify [--dir PATH]
stone help
```

The default storage directory is:

```text
./stone-data
```

## Architecture

```text
                 CLI / Rust Library
                         |
                         v
                     +-------+
                     | Store |
                     +---+---+
                         |
               +---------+---------+
               |                   |
               v                   v
            +------+          +----------+
            | WAL  |          | Memtable |
            +------+          | BTreeMap |
               |              +-----+----+
           sync_all()               |
                                    | threshold
                                    v
                           +----------------+
                           | segment.tmp    |
                           | sorted records |
                           +-------+--------+
                                   |
                              flush + sync
                                   |
                                  rename
                                   v
                           +----------------+
                           | segment.seg    |
                           | sparse index   |
                           +-------+--------+
                                   |
                             full compaction
                                   v
                           +----------------+
                           | merged segment |
                           +----------------+
```

## Write Path

Every `set` and `delete` follows:

```text
Record
   |
   v
append WAL
   |
   v
File::sync_all()
   |
   v
update memtable
   |
   v
flush when threshold is reached
```

Stone does not mutate the memtable until the WAL append and `sync_all()` succeed.

Therefore, if:

```rust
store.set(key, value)?;
```

returns `Ok(())`, Stone has completed the WAL durability step for that operation.

## Read Path

```text
GET
 |
 +--> memtable
 |
 |    live value -> return
 |    tombstone  -> not found
 |    absent     -> continue
 |
 +--> segments newest to oldest
      |
      live value -> return
      tombstone  -> not found
      absent     -> continue
 |
 +--> not found
```

The first value or tombstone found is authoritative.

## Record Format

Records use a manually encoded binary representation:

```text
[op: u8]
[key_len: u32 LE]
[key bytes]
[val_len: u32 LE]
[value bytes]
[crc32: u32 LE]
```

Operations:

```text
0 = SET
1 = DELETE
```

A delete record contains:

```text
val_len = 0
```

CRC32 covers every byte from `op` through the final value byte. The CRC field itself is not included.

All record length calculations use checked arithmetic before slicing data.

## CRC32

Stone implements reflected IEEE CRC32 manually using:

```text
Polynomial: 0xEDB88320
Initial:    0xFFFFFFFF
Final XOR:  0xFFFFFFFF
```

The lookup table is generated once using:

```rust
std::sync::LazyLock
```

No CRC or lazy-initialization crate is required.

The standard test vector is verified:

```text
CRC32("123456789") = 0xCBF43926
```

## Write-Ahead Log

The WAL is stored at:

```text
<store>/wal.log
```

A successful WAL append performs:

```text
encode
  ->
write_all
  ->
sync_all
```

During startup Stone replays complete WAL records into the memtable.

### Crash-tail recovery

A process may terminate halfway through its final WAL write:

```text
valid record
valid record
partial record
```

Stone distinguishes this from checksum corruption.

For an incomplete final record Stone:

1. Replays the valid prefix.
2. Stops at the incomplete record.
3. Physically truncates the WAL to the last valid byte.
4. Calls `sync_all()`.
5. Continues startup.

Physical truncation is important because otherwise future valid appends could remain unreachable behind the damaged bytes.

A complete record with an invalid checksum is treated as corruption and returns an error instead of being silently discarded.

## Memtable

Unflushed state is stored in:

```rust
BTreeMap<Vec<u8>, Option<Vec<u8>>>
```

Representation:

```text
Some(value) -> live value
None        -> tombstone
```

`BTreeMap` keeps keys sorted automatically, which makes segment generation straightforward.

The default approximate flush threshold is:

```text
4 MiB
```

## Segment Files

Segments are immutable and generation ordered.

Example:

```text
segments/
  segment_00000000000000000001.seg
  segment_00000000000000000002.seg
  segment_00000000000000000003.seg
```

Temporary installation files use:

```text
segment_00000000000000000004.seg.tmp
```

Wall-clock timestamps and UUIDs are not required.

## Segment Format

```text
+-------------------------------+
| MAGIC "STON"       4 bytes    |
| VERSION            1 byte     |
+-------------------------------+
| Record 1                       |
| Record 2                       |
| ...                            |
| Record N                       |
+-------------------------------+  <- index_offset
| sparse index entries          |
+-------------------------------+
| index_offset        u64 LE     |
| MAGIC "STON"        4 bytes    |
+-------------------------------+
```

Header size:

```text
5 bytes
```

Footer size:

```text
12 bytes
```

Segment readers scan records only between byte `5` and `index_offset`.

Sparse-index data is never interpreted as record data.

## Sparse Index

Every 16th record is indexed:

```text
0, 16, 32, 48, ...
```

An index entry contains:

```text
[key_len: u32 LE]
[key]
[file_offset: u64 LE]
```

The offset points to the beginning of an encoded record.

The first record in every non-empty segment is always indexed.

## Atomic Segment Installation

Memtable data is never written directly to its final segment filename.

Stone performs:

```text
write segment.seg.tmp
        |
        v
flush + sync_all
        |
        v
rename to segment.seg
        |
        v
open + validate final segment
        |
        v
truncate WAL
        |
        v
clear memtable
```

This ordering protects acknowledged WAL-backed writes from being lost during segment installation.

If the process terminates before rename, the old WAL still contains the data.

If the process terminates after rename but before WAL truncation, both the segment and WAL may contain the newest state. Replaying that state again is logically harmless.

## Compaction

Stone intentionally implements **full compaction only**.

All segments are read:

```text
oldest -> newest
```

and merged into a `BTreeMap`.

Newer entries overwrite older versions.

Because every old segment participates in full compaction, final tombstones can safely be removed rather than copied into the new segment.

Compaction performs:

```text
all old segments
       |
       v
logical merge
       |
       v
new segment.tmp
       |
       v
sync_all
       |
       v
rename
       |
       v
validate new segment
       |
       v
close old readers
       |
       v
delete old segments
```

Old segments are never deleted before the replacement segment is written, synced, renamed, and successfully opened.

## Statistics

```bash
stone stats
```

Reports:

```text
segments
segment_bytes
wal_bytes
memtable_entries
memtable_bytes
```

Example:

```text
segments: 2
segment_bytes: 82310
wal_bytes: 0
memtable_entries: 0
memtable_bytes: 0
```

## Verification

```bash
stone verify
```

Stone validates the current WAL and scans all segment records.

Successful output resembles:

```text
OK
wal_records: 0
segments_checked: 2
records_checked: 500
```

Validation includes record CRC checks and segment structural checks.

## Rust Library API

```rust
use stone::Store;
use std::path::Path;

fn main() -> stone::Result<()> {
    let mut store =
        Store::open(Path::new("./stone-data"))?;

    store.set(b"user:1", b"Abhishek")?;

    let value =
        store.get(b"user:1")?;

    assert_eq!(
        value,
        Some(b"Abhishek".to_vec())
    );

    store.delete(b"user:1")?;

    assert_eq!(
        store.get(b"user:1")?,
        None
    );

    Ok(())
}
```

Public API includes:

```text
Store
StoreStats
CompactionStats
VerifyStats
StoneError
Result
```

## Concurrency Model

`Store` does not perform internal concurrent mutation.

Multiple threads inside one process can coordinate access using:

```rust
Arc<Mutex<Store>>
```

with standard-library synchronization primitives.

Stone does **not** implement multi-process file locking.

Two independent processes writing to the same store directory concurrently are unsupported.

## Durability Claim

Stone's durability claim is intentionally narrow:

> After `Store::set()` or `Store::delete()` returns `Ok`, the operation has been appended to the WAL and `File::sync_all()` has completed. On restart, Stone replays valid WAL records. If the process terminated during the final record append, Stone discards and physically truncates only that incomplete tail. Segment installation uses a synced temporary file followed by rename before the WAL is discarded.

Stone relies on Rust `File::sync_all()` and same-filesystem rename semantics.

## Tests

Run:

```bash
cargo test
```

The current implementation includes unit and integration coverage for:

* CRC32
* binary record encoding
* truncation handling
* checksum corruption
* WAL append/replay
* physical crash-tail truncation
* memtable behavior
* segment headers and footers
* sparse-index lookups
* segment CRC corruption
* store reopen/recovery
* tombstones
* generation ordering
* automatic flush
* full compaction
* deleted-key resurrection prevention
* threaded single-process access

The current validated suite contains more than one hundred passing tests.

## Benchmark

Stone contains a zero-dependency benchmark harness:

```bash
cargo bench
```

It uses only:

```rust
std::time::Instant
```

The harness measures:

* durable sequential writes
* reads
* reopen time
* verification time
* full compaction

Write throughput should be interpreted carefully because every acknowledged write performs `File::sync_all()`.

Do not compare these numbers directly with databases configured for asynchronous or batched durability.

Benchmark results are intentionally not hard-coded in this README because performance varies significantly by operating system, filesystem, storage hardware, and durability semantics.

## Zero Dependency Proof

`Cargo.toml` intentionally contains:

```toml
[dependencies]
# intentionally empty — zero third-party runtime dependencies
```

Verify it with:

```bash
cargo tree -e normal
```

Generate metadata proof using:

```bash
cargo metadata --format-version 1 --no-deps > deps-proof.txt
```

See [`STDLIB.md`](STDLIB.md) for the standard-library substitutions used by Stone.

## Honest Limitations

Stone currently does not provide:

* networking
* SQL
* transactions
* replication
* distributed consensus
* TTL
* snapshots
* leveled compaction
* bloom filters
* compression
* asynchronous I/O
* multi-process writer locking
* internal sharding
* GUI or TUI
* protection against malicious file modification
* guarantees under arbitrary disk corruption
* tested guarantees for every network filesystem
* explicit disk-full handling
* power-controller fault testing

Stone prioritizes a small, explainable, durable core rather than feature breadth.

## Project Structure

```text
StoneKV/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── STDLIB.md
├── LICENSE
├── deps-proof.txt
├── .zero-dep.toml
├── .gitignore
│
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── error.rs
│   ├── logger.rs
│   ├── crc32.rs
│   ├── record.rs
│   ├── wal.rs
│   ├── memtable.rs
│   ├── segment.rs
│   ├── compaction.rs
│   └── store.rs
│
├── tests/
│   ├── roundtrip.rs
│   ├── crash_recovery.rs
│   ├── segment_roundtrip.rs
│   ├── concurrent_access.rs
│   └── compaction_correctness.rs
│
├── benches/
│   └── throughput.rs
│
├── scripts/
│   ├── build_repro.sh
│   ├── demo_crash.sh
│   └── deps_proof.sh
│
└── demo/
    └── link.txt
```

## Design Principle

```text
write acknowledged
      |
      v
WAL synced
      |
      v
restart works
      |
      v
segments remain valid
      |
      v
overwrites/deletes never resurrect
      |
      v
compaction preserves logical state
      |
      v
zero third-party dependencies
```

Correctness, durability, and explainability are the product.
