# StoneKV

**A crash-safe embedded key-value store built in Rust with zero third-party dependencies.**

**Zero Dependency Hackathon 2026 — Track D: Data & Storage**

StoneKV is a small log-structured storage engine implemented entirely with the Rust standard library. It provides persistent `set`, `get`, and `delete` operations through both a command-line interface and an embeddable Rust API.

Its goal is simple:

> **Build the core durable workflow of a real embedded key-value database without relying on external crates for storage, serialization, checksums, CLI parsing, error handling, or recovery.**

---

## Highlights

* Persistent `SET`, `GET`, and `DELETE`
* Write-ahead log (WAL)
* `File::sync_all()` before acknowledging writes
* Restart recovery from the WAL
* Recovery from incomplete final WAL writes
* Physical truncation of invalid crash tails
* Hand-written IEEE CRC32 integrity checking
* Sorted in-memory memtable using `BTreeMap`
* Automatic memtable flushing
* Immutable sorted segment files
* Sparse segment indexes
* Newest-to-oldest reads
* Tombstone-based deletes
* Full-store compaction
* Crash-safe compaction recovery using `compaction.pending`
* Synced temporary-file-to-segment installation
* Monotonic segment generation numbers
* Store statistics
* Integrity verification command
* CLI and embeddable Rust API
* Single-process threaded access through `Arc<Mutex<Store>>`
* **Zero third-party Rust dependencies**

---

## Requirements

* **Minimum Rust version:** Rust 1.80+
* **Validated environment:** `rustc 1.97.1` and `cargo 1.97.1`
* No third-party Rust crates

Rust 1.80+ is required because StoneKV uses `std::sync::LazyLock`.

---

## Build

StoneKV builds with one command:

```bash
cargo build --release
```

The release binary is produced at:

### Windows

```text
target\release\stone.exe
```

### Linux / macOS

```text
target/release/stone
```

---

# Judge Quick Check

A reviewer can verify the core claims without reading the implementation.

## 1. Verify zero dependencies

```bash
cargo tree -e normal
```

Expected dependency tree:

```text
stone v0.1.0 (...)
```

There should be no third-party crates listed underneath StoneKV.

`Cargo.toml` intentionally contains an empty dependency section:

```toml
[dependencies]
```

A generated Cargo metadata proof is also committed as [`deps-proof.txt`](deps-proof.txt).

---

## 2. Build

```bash
cargo build --release
```

---

## 3. Inspect the CLI

### Windows

```powershell
.\target\release\stone.exe help
```

### Linux / macOS

```bash
./target/release/stone help
```

Available commands:

```text
stone set <key> <value> [--dir PATH]
stone get <key> [--dir PATH]
stone del <key> [--dir PATH]
stone compact [--dir PATH]
stone stats [--dir PATH]
stone verify [--dir PATH]
stone help
```

---

## 4. Store and retrieve arbitrary data

### Windows

```powershell
.\target\release\stone.exe set employee:42 "Rohan Sharma" --dir judge-data
.\target\release\stone.exe get employee:42 --dir judge-data
```

### Linux / macOS

```bash
./target/release/stone set employee:42 "Rohan Sharma" --dir judge-data
./target/release/stone get employee:42 --dir judge-data
```

Expected output:

```text
OK
Rohan Sharma
```

---

## 5. Verify the store

### Windows

```powershell
.\target\release\stone.exe verify --dir judge-data
```

### Linux / macOS

```bash
./target/release/stone verify --dir judge-data
```

Successful verification starts with:

```text
OK
```

Using `--dir` lets reviewers create isolated stores without modifying the default `./stone-data` directory.

---

# Quick Start

## Set a value

### Windows

```powershell
.\target\release\stone.exe set user:1 Abhishek
```

### Linux / macOS

```bash
./target/release/stone set user:1 Abhishek
```

Output:

```text
OK
```

---

## Get a value

### Windows

```powershell
.\target\release\stone.exe get user:1
```

### Linux / macOS

```bash
./target/release/stone get user:1
```

Output:

```text
Abhishek
```

---

## Overwrite a value

```text
SET user:1 = Abhishek
SET user:1 = Rahul
GET user:1
```

The newest value is authoritative:

```text
Rahul
```

---

## Delete a value

### Windows

```powershell
.\target\release\stone.exe del user:1
```

### Linux / macOS

```bash
./target/release/stone del user:1
```

Output:

```text
OK
```

Reading the deleted key returns:

```text
not found
```

A missing key is written to stderr and returns a non-zero exit status.

---

# Architecture

```text
                 CLI / Rust Library
                         |
                         v
                     +-------+
                     | Store |
                     +---+---+
                         |
                +--------+--------+
                |                 |
                v                 v
             +------+        +----------+
             | WAL  |        | Memtable |
             +------+        | BTreeMap |
                |            +-----+----+
            sync_all()             |
                                   | threshold
                                   v
                          +----------------+
                          | segment.seg.tmp|
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

StoneKV uses a log-structured design:

```text
WAL -> Memtable -> Immutable Segments -> Full Compaction
```

---

# Write Path

Every `set` and `delete` follows this ordering:

```text
construct record
      |
      v
append to WAL
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

StoneKV does **not** mutate the memtable until the WAL append and `sync_all()` succeed.

Therefore, if:

```rust
store.set(key, value)?;
```

returns `Ok(())`, StoneKV has completed its WAL durability step for that operation.

---

# Read Path

```text
GET
 |
 +--> memtable
 |      |
 |      +--> live value -> return
 |      +--> tombstone  -> not found
 |      +--> absent     -> continue
 |
 +--> segments newest to oldest
        |
        +--> live value -> return
        +--> tombstone  -> not found
        +--> absent     -> continue
 |
 +--> not found
```

The first live value or tombstone encountered is authoritative.

---

# Record Format

StoneKV manually encodes records using an explicit binary format:

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

CRC32 covers every byte from `op` through the final value byte. The CRC field itself is excluded.

All length arithmetic is checked before slicing input data.

StoneKV also applies a defensive field-size ceiling:

```text
MAX_FIELD_LEN = 64 MiB
```

The same limit is enforced during both encoding and decoding so StoneKV never writes a record that it would later reject solely because of its field size.

---

# CRC32

StoneKV implements reflected IEEE CRC32 manually.

```text
Polynomial: 0xEDB88320
Initial:    0xFFFFFFFF
Final XOR:  0xFFFFFFFF
```

The lookup table is initialized once using:

```rust
std::sync::LazyLock
```

No checksum or lazy-initialization crate is required.

The standard test vector is verified:

```text
CRC32("123456789") = 0xCBF43926
```

---

# Write-Ahead Log

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

During startup StoneKV replays complete WAL records into a fresh memtable.

---

## Crash-Tail Recovery

A process may terminate while the final WAL record is still being written:

```text
valid record
valid record
partial record
```

For an incomplete final record StoneKV:

1. Replays the valid prefix.
2. Stops at the incomplete record.
3. Truncates the WAL back to the last valid byte.
4. Calls `sync_all()`.
5. Continues startup.

Physical truncation is important.

Without it:

```text
valid
valid
broken tail
NEW VALID WRITE
```

would leave the newer valid write unreachable during the next replay.

A complete record with a bad checksum or an invalid operation code is treated as corruption and causes StoneKV to fail loudly rather than silently removing it.

---

# Memtable

Unflushed logical state is stored in:

```rust
BTreeMap<Vec<u8>, Option<Vec<u8>>>
```

Representation:

```text
Some(value) -> live value
None        -> tombstone
```

`BTreeMap` automatically keeps keys sorted, which makes immutable segment generation straightforward.

The default approximate flush threshold is:

```text
4 MiB
```

Once the threshold is crossed, the memtable is written into an immutable segment.

---

# Segment Files

Segments are immutable and ordered using monotonic generation numbers.

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

StoneKV does not require UUIDs or wall-clock timestamps for segment ordering.

---

# Segment Format

```text
+-------------------------------+
| MAGIC "STON"       4 bytes    |
| VERSION            1 byte     |
+-------------------------------+
| Record 1                      |
| Record 2                      |
| ...                           |
| Record N                      |
+-------------------------------+ <- index_offset
| sparse index entries          |
+-------------------------------+
| index_offset        u64 LE    |
| MAGIC "STON"        4 bytes   |
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

Segment readers decode records only between byte `5` and `index_offset`.

Sparse-index bytes are never interpreted as record data.

---

# Sparse Index

Every 16th record is indexed:

```text
0, 16, 32, 48, ...
```

Each sparse-index entry contains:

```text
[key_len: u32 LE]
[key]
[file_offset: u64 LE]
```

The stored offset points to the beginning of an encoded record.

The first record in every non-empty segment is always indexed.

For lookup, StoneKV finds the largest indexed key less than or equal to the target and begins scanning from that record.

---

# Atomic Segment Installation

Memtable contents are never written directly to the final segment filename.

StoneKV performs:

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

This ordering prevents acknowledged WAL-backed writes from being lost during segment installation.

### Crash before rename

```text
WAL remains authoritative
temporary segment is ignored
```

### Crash after rename but before WAL truncation

```text
durable segment exists
WAL still exists
```

On restart the WAL may replay data that is already present in the newest segment. This duplication is logically harmless because the replayed memtable state shadows disk segments.

---

# Compaction

StoneKV intentionally implements **full compaction only**.

All existing segments are processed:

```text
oldest -> newest
```

into a logical:

```rust
BTreeMap<Vec<u8>, Option<Vec<u8>>>
```

Newer entries overwrite older entries.

Because every old segment participates in the operation, final tombstones can safely be removed from the compacted output.

Conceptually:

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
install final segment
       |
       v
validate replacement
       |
       v
remove replaced segments
```

StoneKV never intentionally removes old segment data before a valid replacement exists.

---

# Compaction Crash Recovery

Compaction contains an additional crash-recovery mechanism using:

```text
segments/compaction.pending
```

The marker records:

* the generation being produced
* the old generations being replaced

The simplified transaction is:

```text
build compacted segment.tmp
        |
        v
sync_all
        |
        v
write compaction.pending
        |
        v
rename compacted segment into place
        |
        v
validate final segment
        |
        v
delete replaced segments
        |
        v
remove compaction.pending
```

If StoneKV restarts and discovers `compaction.pending`, it determines whether the interrupted transaction should be rolled back or completed.

Important cases include:

| Crash state                                        | Recovery                                               |
| -------------------------------------------------- | ------------------------------------------------------ |
| Marker was never activated                         | Old segments remain authoritative                      |
| Only temporary marker exists                       | Temporary marker is removed                            |
| Marker exists but final compacted segment does not | Transaction rolls back                                 |
| Marker and final compacted segment exist           | Replacement is revalidated, then cleanup rolls forward |
| Old segments are already gone but marker remains   | Marker is removed                                      |

The key invariant is:

> **Never delete data until its replacement has been written and successfully validated.**

This behavior is tested by the compaction recovery tests, including prevention of deleted-key resurrection.

---

# Statistics

Run:

```bash
stone stats
```

or use the release binary directly.

Example output:

```text
segments: 2
segment_bytes: 82310
wal_bytes: 0
memtable_entries: 0
memtable_bytes: 0
```

Reported fields:

```text
segments
segment_bytes
wal_bytes
memtable_entries
memtable_bytes
```

---

# Verification

Run:

```bash
stone verify
```

Successful output resembles:

```text
OK
wal_records: 0
segments_checked: 2
records_checked: 500
```

Verification checks:

* WAL record decoding
* record CRC32 values
* segment headers
* segment footers
* sparse-index structure
* record boundaries
* segment records

---

# Rust Library API

StoneKV can also be embedded directly into a Rust program.

```rust
use stone::Store;
use std::path::Path;

fn main() -> stone::Result<()> {
    let mut store = Store::open(Path::new("./stone-data"))?;

    store.set(b"user:1", b"Abhishek")?;

    let value = store.get(b"user:1")?;
    assert_eq!(value, Some(b"Abhishek".to_vec()));

    store.delete(b"user:1")?;
    assert_eq!(store.get(b"user:1")?, None);

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

---

# Concurrency Model

`Store` does not perform internal concurrent mutation.

Multiple threads within one process can coordinate access using:

```rust
Arc<Mutex<Store>>
```

with standard-library synchronization primitives.

StoneKV does **not** implement multi-process writer locking.

Two independent processes writing to the same store directory concurrently are unsupported.

---

# Durability Claim

StoneKV's durability claim is intentionally narrow:

> After `Store::set()` or `Store::delete()` returns `Ok(())`, the operation has been appended to the WAL and `File::sync_all()` has completed. On restart, StoneKV replays valid WAL records. If the process terminated during the final record append, StoneKV discards and physically truncates the incomplete tail. Segment installation uses a synced temporary file followed by rename before the WAL is discarded.

StoneKV relies on Rust `File::sync_all()` and same-filesystem rename semantics.

It does not claim protection against every possible filesystem, storage controller, power-loss, disk-full, or malicious-corruption scenario.

---

# Tests

Run:

```bash
cargo test
```

The validated suite contains:

```text
89 unit tests
28 integration tests
--------------------
117 total tests
```

Coverage includes:

* CRC32
* record encoding and decoding
* empty keys and values
* malformed operation bytes
* truncation boundaries
* field-size limits
* checksum corruption
* length-field corruption hardening
* WAL append and replay
* physical WAL crash-tail truncation
* writes after recovery
* memtable replacement and tombstones
* automatic flush
* segment headers and footers
* sparse-index boundaries
* segment CRC corruption
* store reopen and recovery
* generation ordering
* newest-value semantics
* full compaction
* deleted-key resurrection prevention
* interrupted compaction recovery
* threaded single-process access

The current validated result is:

```text
117 passed
0 failed
```

---

# Benchmark

StoneKV includes a zero-dependency benchmark harness:

```bash
cargo bench
```

It uses:

```rust
std::time::Instant
```

rather than an external benchmarking framework.

The harness measures:

* durable sequential writes
* reads
* reopen time
* verification time
* full compaction

Write throughput should be interpreted carefully because each acknowledged write performs `File::sync_all()`.

Numbers are intentionally not hard-coded into this README because results depend heavily on the operating system, filesystem, storage device, and durability semantics.

---

# Zero-Dependency Proof

`Cargo.toml` contains:

```toml
[dependencies]
# intentionally empty - zero third-party runtime dependencies
```

Verify the normal dependency tree:

```bash
cargo tree -e normal
```

Expected:

```text
stone v0.1.0 (...)
```

with no third-party crates underneath.

Generate Cargo metadata using:

```bash
cargo metadata --format-version 1 --no-deps
```

A generated proof is committed as:

[`deps-proof.txt`](deps-proof.txt)

See [`STDLIB.md`](STDLIB.md) for the standard-library substitutions used throughout StoneKV.

Examples include replacements for functionality commonly provided by:

* `serde`
* `bincode`
* `crc32fast`
* `once_cell`
* `clap`
* `thiserror`
* `anyhow`
* `log`
* `tracing`
* `tempfile`
* `uuid`
* external database engines
* external benchmark frameworks

StoneKV implements these needs using Rust's standard library and project-specific code.

---

# Honest Limitations

StoneKV intentionally does not provide:

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
* guarantees for every network filesystem
* explicit disk-full recovery
* storage-controller or power-loss fault testing

StoneKV prioritizes a small, explainable, durable core over feature breadth.

## Length-field corruption

StoneKV rejects implausibly large corrupted `key_len` or `val_len` values using its `MAX_FIELD_LEN` validation.

However, the current record format cannot perfectly distinguish every moderate length-field corruption from a genuinely interrupted final append because the CRC that would disambiguate the record is located at its end.

Completely eliminating that ambiguity would require additional framing or integrity metadata and therefore an on-disk format change.

For the hackathon version this behavior is explicitly documented and tested rather than hidden or overclaimed.

---

# Project Structure

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

---

# Design Principle

StoneKV is intentionally optimized for correctness and explainability rather than feature count.

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

**Correctness, durability, and explainability are the product.**
