# Standard-Library Substitution Log

StoneKV is built entirely with the Rust standard library and project-specific code.

**Third-party Rust dependencies: 0**

This document lists the substitutions that are actually present in the submitted implementation. It is not an aspirational list.

For each substitution, this log identifies:

* what third-party crate or external facility would commonly be used
* what StoneKV uses instead
* where the implementation lives
* why the substitution was made
* the tradeoff accepted

**Minimum Rust version:** Rust 1.80+
**Validated environment:** `rustc 1.97.1` and `cargo 1.97.1`

---

# At a Glance

|  # | Common external choice         | StoneKV replacement                         |
| -: | ------------------------------ | ------------------------------------------- |
|  1 | `serde` / `bincode`            | Manual binary record encoding               |
|  2 | `crc32fast`                    | Hand-written IEEE CRC32                     |
|  3 | WAL library                    | Custom append/replay/truncate WAL           |
|  4 | `sled` / `rocksdb`             | Custom log-structured storage engine        |
|  5 | Ordered-map/index helper       | `std::collections::BTreeMap`                |
|  6 | `clap`                         | `std::env::args()`                          |
|  7 | `thiserror` / `anyhow`         | Hand-written `StoneError`                   |
|  8 | `log` / `tracing`              | Small stderr logger                         |
|  9 | `parking_lot`                  | `std::sync::{Arc, Mutex}`                   |
| 10 | `tempfile`                     | Explicit `.tmp` files + `std::fs::rename()` |
| 11 | `uuid`                         | Monotonic segment generations               |
| 12 | Criterion                      | `std::time::Instant` benchmark harness      |
| 13 | Assertion/property-test crates | Built-in `#[test]` and assertions           |
| 14 | `once_cell`                    | `std::sync::LazyLock`                       |

**Total: 14 genuine substitutions.**

---

# 1. `serde` / `bincode` → Manual Binary Encoding

**File:** `src/record.rs`

StoneKV manually encodes every database record into a documented little-endian binary layout:

```text
[op: u8]
[key_len: u32 LE]
[key bytes]
[val_len: u32 LE]
[value bytes]
[crc32: u32 LE]
```

Encoding and decoding use standard-library operations such as:

```rust
u32::to_le_bytes()
u32::from_le_bytes()
```

Offset and length calculations use checked arithmetic before slicing input data.

StoneKV also enforces the same defensive field-size ceiling during both encoding and decoding:

```text
MAX_FIELD_LEN = 64 MiB
```

This ensures the encoder does not create a field that the decoder would reject solely because of its size.

**Why**

`serde` and `bincode` would normally remove much of the serialization boilerplate, but the on-disk representation is a core part of a storage engine.

Implementing it directly makes the format:

* explicit
* deterministic
* auditable
* versionable
* independent of an external serialization format

**Tradeoff**

Adding or changing record fields requires manually updating both encoding and decoding logic and may require an on-disk format-version change.

---

# 2. `crc32fast` → Hand-Written IEEE CRC32

**File:** `src/crc32.rs`

StoneKV implements reflected IEEE CRC32 directly.

```text
Polynomial: 0xEDB88320
Initial:    0xFFFFFFFF
Final XOR:  0xFFFFFFFF
```

A 256-entry lookup table is generated once and stored using:

```rust
std::sync::LazyLock
```

The implementation is checked against the standard test vector:

```text
CRC32("123456789") = 0xCBF43926
```

**Why**

Every encoded record needs an integrity check. A crate such as `crc32fast` would normally provide this functionality immediately, but checksum generation is small enough to implement directly and is central to StoneKV's corruption-detection story.

**Tradeoff**

The implementation does not use runtime CPU-feature detection or specialized SIMD/hardware acceleration.

Correctness is preserved, but raw checksum throughput may be lower than a highly optimized CRC crate.

---

# 3. WAL Library → Custom Append / Replay / Recovery Log

**File:** `src/wal.rs`

StoneKV implements its write-ahead log directly on top of:

```rust
std::fs::File
```

A successful append performs:

```text
encode record
     |
     v
write_all()
     |
     v
sync_all()
```

The WAL implementation also handles:

* sequential appends
* startup replay
* memtable reconstruction
* incomplete final-record detection
* physical crash-tail truncation
* WAL truncation after successful segment installation
* continued writes after recovery

**Why**

WAL behavior is one of the central engineering problems of a durable storage engine.

Delegating append ordering, synchronization, replay, and crash-tail handling to an external log library would hide much of the work StoneKV is intended to demonstrate.

**Tradeoff**

StoneKV does not implement group commit or batched durability.

Every acknowledged write currently pays for its own `sync_all()` operation.

---

# 4. `sled` / `rocksdb` → Custom Log-Structured Storage Engine

**Files:**

```text
src/store.rs
src/segment.rs
src/compaction.rs
src/memtable.rs
src/wal.rs
```

StoneKV implements its complete storage lifecycle itself:

```text
WAL
 |
 v
BTreeMap memtable
 |
 v
immutable sorted segments
 |
 v
sparse indexes
 |
 v
full compaction
```

The project implements:

* write durability
* restart recovery
* in-memory state
* immutable segment creation
* sparse lookup indexes
* generation ordering
* tombstones
* newest-value resolution
* full compaction
* compaction crash recovery

**Why**

Using an existing embedded database would remove the central engineering challenge of Track D.

The storage engine itself is the project.

**Tradeoff**

StoneKV intentionally does not implement advanced database features such as:

* leveled compaction
* bloom filters
* block compression
* snapshots
* transactions
* replication

These limitations are documented in `README.md`.

---

# 5. Ordered-Map / Index Helper → `std::collections::BTreeMap`

**Files:**

```text
src/memtable.rs
src/compaction.rs
```

StoneKV uses:

```rust
std::collections::BTreeMap
```

for sorted in-memory state and compaction merging.

The memtable representation is:

```rust
BTreeMap<Vec<u8>, Option<Vec<u8>>>
```

where:

```text
Some(value) -> live value
None        -> tombstone
```

During full compaction, older segment entries are applied before newer entries so later versions replace earlier ones.

The final map can then be written directly in sorted key order.

**Why**

StoneKV needs deterministic ordered keys for segment creation and compaction.

`BTreeMap` already provides exactly that functionality in the standard library.

**Tradeoff**

Full compaction materializes the logical dataset in memory.

That approach is appropriate for the hackathon scope but is not intended for arbitrarily large production databases.

---

# 6. `clap` → `std::env::args()`

**File:** `src/main.rs`

StoneKV's CLI parser is a small hand-written dispatcher built on:

```rust
std::env::args()
```

Supported commands are:

```text
stone set <key> <value> [--dir PATH]
stone get <key> [--dir PATH]
stone del <key> [--dir PATH]
stone compact [--dir PATH]
stone stats [--dir PATH]
stone verify [--dir PATH]
stone help
```

User-facing failures return an explicit non-zero process exit status.

**Why**

`clap` is a common default for Rust CLI applications, but StoneKV has a small command surface that does not require a large parsing framework.

**Tradeoff**

StoneKV does not receive:

* generated CLI schemas
* derive-based command definitions
* automatic completion generation
* sophisticated flag validation

The necessary validation is implemented manually.

---

# 7. `thiserror` / `anyhow` → Hand-Written `StoneError`

**File:** `src/error.rs`

StoneKV defines its own error type:

```rust
enum StoneError
```

with manual implementations of:

```rust
std::fmt::Display
std::error::Error
From<std::io::Error>
```

The conversion from `std::io::Error` allows idiomatic propagation with:

```rust
?
```

through I/O-heavy code.

**Why**

`thiserror` and `anyhow` are common choices for ergonomic Rust error handling, but StoneKV has a relatively small and controlled error surface.

**Tradeoff**

Every new error variant requires explicitly updating its display behavior and any relevant conversions.

No derive macros generate that code automatically.

---

# 8. `log` / `tracing` → Small stderr Logger

**File:** `src/logger.rs`

StoneKV implements a minimal logger with levels such as:

```text
INFO
WARN
ERROR
```

and output such as:

```text
[INFO] flushed memtable to segment generation 2
[WARN] truncated WAL tail detected: removing 11 invalid byte(s)
```

Diagnostics are written to stderr.

Timestamps are intentionally omitted.

**Why**

A complete `log` or `tracing` ecosystem would add substantial machinery for a command-line storage engine that only needs a small number of deterministic diagnostic messages.

**Tradeoff**

The logger does not provide:

* dynamic level filtering
* structured fields
* spans
* subscriber configuration
* external log sinks
* timestamp formatting

---

# 9. `parking_lot` → `std::sync::{Arc, Mutex}`

**File:** `tests/concurrent_access.rs`

StoneKV intentionally uses a coarse single-process synchronization model.

Multiple threads can share one `Store` using:

```rust
Arc<Mutex<Store>>
```

The integration test starts multiple `std::thread` workers, writes disjoint keys, joins them, verifies the values, reopens the database, and verifies persistence again.

**Why**

The standard-library mutex is sufficient for the concurrency guarantees StoneKV intentionally provides.

An external synchronization crate is unnecessary for this scale and design.

**Tradeoff**

The design uses coarse locking rather than fine-grained internal database concurrency.

StoneKV also intentionally does not support multiple independent processes writing to the same store directory concurrently.

---

# 10. `tempfile` → Explicit `.tmp` Files + `rename`

**Files:**

```text
src/store.rs
src/compaction.rs
```

Crash-sensitive file installation uses explicit temporary paths.

Examples include:

```text
segment_00000000000000000004.seg.tmp
compaction.pending.tmp
```

The basic installation pattern is:

```text
write temporary file
        |
        v
flush
        |
        v
sync_all()
        |
        v
rename into final location
```

Startup and recovery logic handle temporary files that can remain after interrupted operations.

**Why**

The temporary-file + atomic-rename pattern is small enough to implement directly.

Doing so also makes StoneKV's crash-safety ordering visible and auditable rather than hiding it behind a helper crate.

**Tradeoff**

StoneKV does not receive automatic cleanup-on-drop behavior.

Temporary-file lifecycle and crash cleanup must therefore be implemented explicitly.

---

# 11. `uuid` → Monotonic Segment Generations

**File:** `src/store.rs`

Segments use deterministic generation-based names:

```text
segment_00000000000000000001.seg
segment_00000000000000000002.seg
segment_00000000000000000003.seg
```

Generation numbers are represented as `u64` values and formatted with zero padding.

At startup StoneKV scans existing segment filenames and determines the next generation from the highest generation already present.

**Why**

A segment identifier must primarily provide ordering, not global uniqueness.

Monotonic generations are:

* sortable
* deterministic
* easy to inspect
* easy to compare
* available without randomness

**Tradeoff**

Generation numbers are local to a StoneKV store directory rather than globally unique.

That is appropriate for StoneKV's documented single-writer storage model.

---

# 12. Criterion → `std::time::Instant`

**File:** `benches/throughput.rs`

StoneKV includes a zero-dependency benchmark harness implemented with:

```rust
std::time::Instant
```

and:

```rust
Instant::now()
elapsed()
```

The harness measures operations such as:

* durable sequential writes
* reads
* reopen time
* verification time
* full compaction

**Why**

Criterion provides statistical benchmarking features that are useful for serious performance analysis, but StoneKV only needs a transparent sanity-check benchmark for the hackathon.

**Tradeoff**

The benchmark does not provide:

* confidence intervals
* automatic warm-up
* statistical outlier detection
* regression analysis

Results should therefore be treated as approximate and environment-dependent.

---

# 13. Assertion / Test Crates → Built-In `#[test]`

**Files:**

```text
src/*.rs
tests/*.rs
```

StoneKV uses Rust's built-in testing facilities:

```rust
#[test]
assert!()
assert_eq!()
```

The current validated test suite contains:

```text
89 unit tests
28 integration tests
--------------------
117 total tests
```

Integration coverage is distributed across:

```text
tests/roundtrip.rs
tests/crash_recovery.rs
tests/segment_roundtrip.rs
tests/concurrent_access.rs
tests/compaction_correctness.rs
```

Coverage includes:

* CRC32
* record encoding and decoding
* field-size boundaries
* truncation handling
* checksum corruption
* WAL replay
* physical crash-tail recovery
* segment structure
* sparse indexing
* restart persistence
* automatic flush
* tombstones
* compaction
* compaction crash recovery
* deleted-key resurrection prevention
* threaded single-process access

**Why**

Rust's built-in test framework is sufficient for the deterministic byte-level and state-transition tests used by StoneKV.

**Tradeoff**

StoneKV does not currently use features from libraries such as:

* `proptest`
* `quickcheck`
* `pretty_assertions`

so there is no automatic property-based generation or enhanced assertion diff formatting.

---

# 14. `once_cell` → `std::sync::LazyLock`

**File:** `src/crc32.rs`

The CRC32 lookup table is declared using:

```rust
static TABLE: LazyLock<[u32; 256]>
```

and initialized once on first access.

**Why**

`once_cell` historically provided this convenient lazy-initialization functionality.

Rust 1.80 stabilized:

```rust
std::sync::LazyLock
```

which directly covers StoneKV's requirement without adding a crate.

**Tradeoff**

This establishes StoneKV's minimum Rust version at:

```text
Rust 1.80+
```

The submitted implementation has been built and validated using:

```text
rustc 1.97.1
cargo 1.97.1
```

---

# Zero-Dependency Verification

The normal dependency tree can be inspected with:

```bash
cargo tree -e normal
```

Expected structure:

```text
stone v0.1.0 (...)
```

with no third-party crates underneath it.

Cargo metadata can also be inspected using:

```bash
cargo metadata --format-version 1 --no-deps
```

The repository includes a generated copy:

```text
deps-proof.txt
```

`Cargo.toml` intentionally contains:

```toml
[dependencies]
# intentionally empty - zero third-party runtime dependencies
```

---

# Substitution Count

StoneKV contains:

```text
14 genuine standard-library substitutions
```

Every substitution above points to code that ships with the submission.

The list is intentionally limited to functionality that is actually implemented rather than padded with hypothetical dependencies.

A reviewer can verify each entry directly through the referenced source files.

---

# Design Philosophy

The objective was not merely to make `cargo tree` small.

The objective was to expose the pieces normally hidden behind dependencies:

```text
serialization
     |
     v
checksums
     |
     v
WAL durability
     |
     v
recovery
     |
     v
sorted storage
     |
     v
atomic installation
     |
     v
compaction
     |
     v
verification
```

For StoneKV, **zero dependency means implementing and understanding the critical storage path rather than outsourcing it.**
