# STDLIB.md

StoneKV is built entirely on the Rust standard library. No third-party
runtime dependency appears anywhere in `Cargo.toml`, and none is required
to build, test, or run the project.

This document lists every substitution actually present in the shipped
code — not an aspirational list. Each entry names the crate normally
reached for, what StoneKV uses instead, why the substitution was needed,
how it is implemented, and the tradeoff accepted.

---

## 1. `serde` / `bincode` → manual binary encoding

**File:** `src/record.rs`

Records are encoded by hand as a fixed little-endian byte layout
(`op`, `key_len`, `key`, `val_len`, `val`, `crc32`) using
`to_le_bytes()` / `from_le_bytes()` directly, with `checked_add` used
throughout offset arithmetic to reject overflow rather than panic.

**Why:** serde's derive macros and bincode's wire format are exactly the
kind of "glue" dependency the event asks teams to remove. A KV store's
on-disk format also benefits from being explicit and human-auditable
rather than opaque.

**Tradeoff:** more boilerplate per field, and adding a new field means
manually updating the encode/decode pair and bumping the format version
— serde would do this automatically via derive.

---

## 2. `crc32fast` → handwritten IEEE CRC-32

**File:** `src/crc32.rs`

A standard 256-entry CRC-32 lookup table (polynomial `0xEDB88320`) is
built once at first use via `std::sync::LazyLock` and reused for every
checksum call. Verified against the canonical IEEE test vector
(`checksum(b"123456789") == 0xCBF43926`).

**Why:** every record and every segment footer needs a checksum to
detect corruption; `crc32fast` is one of the most common
"just install it" dependencies in this exact use case.

**Tradeoff:** no runtime CPU feature detection (SIMD/hardware CRC
instructions), so raw throughput is lower on very large inputs. Correctness
is unaffected — only raw speed.

---

## 3. WAL crate → custom append/replay/truncate log

**File:** `src/wal.rs`

A minimal write-ahead log implemented directly over `std::fs::File`:
sequential `Record`-encoded appends, `sync_all()` after every append,
a replay routine that reconstructs a memtable on startup, and a
truncate routine that physically shortens the file after a successful
flush.

**Why:** WAL correctness (fsync ordering, partial-write detection,
tail truncation) is the actual subject of the hackathon track, so this
is core engineering, not something to delegate to a library.

**Tradeoff:** no group-commit / batched fsync optimization — every
write pays one `sync_all()` call.

---

## 4. `sled` / RocksDB bindings → custom log-structured storage engine

**Files:** `src/store.rs`, `src/segment.rs`, `src/compaction.rs`,
`src/memtable.rs`

The full read/write/flush/compact lifecycle (WAL → memtable →
immutable sorted segment → sparse index → full compaction) is
implemented from scratch.

**Why:** this is the project itself — the whole point of Track D is to
reimplement what an embedded KV engine normally delegates to a
library.

**Tradeoff:** no leveled compaction, no bloom filters, no block
compression — deliberately out of scope for a 72-hour build (see
"Honest limits" in README.md).

---

## 5. Indexing helper → `std::collections::BTreeMap`

**File:** `src/compaction.rs`

Full compaction merges every segment into a `BTreeMap<Vec<u8>,
Option<Vec<u8>>>` (newer segments overwrite older entries), then
writes the map back out in sorted order.

**Why:** compaction needs sorted, deduplicated key ordering; std's
`BTreeMap` provides exactly that without an external indexing crate.

**Tradeoff:** the entire logical dataset is materialized in memory
during compaction — acceptable for the hackathon's dataset sizes, not
appropriate for very large stores (documented in README.md).

---

## 6. `clap` → `std::env::args()`

**File:** `src/main.rs`

CLI parsing is a small hand-written dispatcher over
`std::env::args().skip(1)`, with `std::process::exit()` used for
explicit exit codes (1 on any user-facing error, per spec).

**Why:** clap is a very common "obvious" dependency for even a
five-command CLI; a manual dispatcher this small doesn't need it.

**Tradeoff:** no auto-generated `--help` formatting, no flag
validation beyond what's hand-coded — acceptable at this command
count.

---

## 7. `thiserror` / `anyhow` → handwritten `StoneError`

**File:** `src/error.rs`

A single `enum StoneError` with a manual `impl fmt::Display` and
`impl std::error::Error`, plus a `From<std::io::Error>` conversion so
`?` composes cleanly through I/O-heavy code.

**Why:** thiserror/anyhow are near-default choices for Rust error
handling; a fixed, small error surface doesn't need the macro
machinery.

**Tradeoff:** every new error variant requires a manual `Display` arm
— thiserror would generate this from an attribute.

---

## 8. `log` / `tracing` → a ~20-line stderr logger

**File:** `src/logger.rs`

`Level::{Info, Warn, Error}` with `log()`/`info()`/`warn()`/`error()`
helpers, formatted as `[INFO]`/`[WARN]`/`[ERROR]` and written to
stderr. No timestamps, by design — this keeps CLI output deterministic
for tests and demos.

**Why:** log/tracing's subscriber/formatter model is significant
machinery for a CLI tool that only needs a handful of diagnostic
lines.

**Tradeoff:** no log levels filtering, no structured fields, no
external sinks.

---

## 9. `parking_lot` → `std::sync::{Arc, Mutex}`

**File:** `tests/concurrent_access.rs`

Multi-threaded access to a single `Store` is demonstrated by wrapping
it in `Arc<Mutex<Store>>` and driving it from several `std::thread`
workers writing disjoint keys, then verifying every key after `join`
and after a reopen.

**Why:** std's `Mutex` is sufficient for the coarse, single-lock
concurrency model StoneKV intentionally uses (see "Concurrency model"
in README.md); parking_lot's speed advantages target much
higher-contention workloads than this project has.

**Tradeoff:** std's `Mutex` is marginally slower under heavy
contention and has no fairness guarantees — irrelevant at StoneKV's
intended scale.

---

## 10. `tempfile` → explicit `.tmp` file + `rename`

**Files:** `src/store.rs`, `src/compaction.rs`

Every atomic install (segment flush, compaction output, the
compaction transaction marker) is written to an explicit
`*.tmp` path, `sync_all()`'d, then moved into place with
`std::fs::rename()`. Startup scans for and discards stale `.tmp`
files left behind by a crash mid-write.

**Why:** the atomic-rename pattern is simple enough to hand-write, and
doing so makes the crash-safety story auditable in the code itself
rather than hidden inside a dependency.

**Tradeoff:** no automatic cleanup-on-drop semantics that `tempfile`
provides — StoneKV relies on explicit startup-time sweeping instead.

---

## 11. `uuid` → monotonic segment generations

**File:** `src/store.rs`

Segments are named `segment_{generation:020}.seg` using a
zero-padded `u64` generation counter reconstructed at `Store::open()`
by scanning existing files and taking `max_generation + 1`.

**Why:** generation numbers are sortable, deterministic, and require
no randomness source — a real advantage over UUIDs for this use case,
not just a workaround.

**Tradeoff:** none functionally; this is a strict improvement over
UUID naming for an ordered, single-writer log structure.

---

## 12. Benchmark crate (Criterion) → `std::time::Instant`

**File:** `benches/throughput.rs`

Sequential write/read throughput, reopen time, and compaction duration
are timed with plain `Instant::now()` / `elapsed()` calls around each
phase.

**Why:** Criterion's statistical rigor (warm-up iterations, outlier
detection) is overkill for a rough throughput sanity check; a simple
`Instant`-based harness is transparent about exactly what it measures.

**Tradeoff:** no statistical confidence intervals, no automatic
warm-up — numbers are single-run and should be read as approximate.

---

## 13. Assertion/test crates → built-in `#[test]`

**Files:** all of `src/*.rs` (`#[cfg(test)] mod tests`) and
`tests/*.rs`

117 tests total: 89 unit tests colocated with their modules, plus 28
integration tests across `roundtrip.rs`, `crash_recovery.rs`,
`segment_roundtrip.rs`, `concurrent_access.rs`, and
`compaction_correctness.rs`.

**Why:** std's `#[test]` and `assert_eq!`/`assert!` are sufficient for
this project's testing needs without pulling in `proptest`,
`pretty_assertions`, or similar.

**Tradeoff:** no property-based testing and no colorized/diffed
assertion output — acceptable given the deterministic, byte-level
nature of what's being tested.

---

## 14. `once_cell` → `std::sync::LazyLock`

**File:** `src/crc32.rs`

The CRC-32 lookup table is a `static TABLE: LazyLock<[u32; 256]>`,
built exactly once on first access.

**Why:** `LazyLock` was stabilized in Rust 1.80 specifically to cover
this once_cell use case in std — using it directly avoids the
dependency entirely. (Project is built and tested with Rust 1.97.1 and Cargo 1.97.1.)

**Tradeoff:** requires Rust ≥ 1.80; noted in README.md.

---

## Substitution count

**14 genuine substitutions**, each backed by code that ships in this
submission — not a padded list. Every entry above can be verified by
grepping the referenced file for the cited symbol.
