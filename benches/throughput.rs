use stone::Store;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const WRITE_COUNT: usize = 10_000;
const READ_COUNT: usize = 10_000;

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark failed: {}", error);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let dir = benchmark_dir();

    cleanup(&dir);

    println!("stone throughput benchmark");
    println!("directory: {}", dir.display());
    println!();

    let mut store = Store::open_with_threshold(&dir, 64 * 1024)?;

    let write_start = Instant::now();

    for i in 0..WRITE_COUNT {
        let key = format!("key:{:08}", i);
        let value = format!("value:{:08}", i);

        store.set(key.as_bytes(), value.as_bytes())?;
    }

    let write_elapsed = write_start.elapsed();

    let writes_per_second = WRITE_COUNT as f64 / write_elapsed.as_secs_f64();

    println!("writes: {} in {:?}", WRITE_COUNT, write_elapsed);

    println!("write throughput: {:.2} ops/sec", writes_per_second);

    let read_start = Instant::now();

    for i in 0..READ_COUNT {
        let key = format!("key:{:08}", i);

        let value = store.get(key.as_bytes())?;

        if value.is_none() {
            return Err(format!("missing benchmark key {}", key).into());
        }
    }

    let read_elapsed = read_start.elapsed();

    let reads_per_second = READ_COUNT as f64 / read_elapsed.as_secs_f64();

    println!();

    println!("reads: {} in {:?}", READ_COUNT, read_elapsed);

    println!("read throughput: {:.2} ops/sec", reads_per_second);

    drop(store);

    let reopen_start = Instant::now();

    let mut reopened = Store::open_with_threshold(&dir, 64 * 1024)?;

    let reopen_elapsed = reopen_start.elapsed();

    println!();

    println!("reopen time: {:?}", reopen_elapsed);

    let verify_start = Instant::now();

    let verify = reopened.verify()?;

    let verify_elapsed = verify_start.elapsed();

    println!("verify time: {:?}", verify_elapsed);

    println!("verified WAL records: {}", verify.wal_records);

    println!("verified segments: {}", verify.segments_checked);

    println!("verified segment records: {}", verify.records_checked);

    let stats_before = reopened.stats();

    println!();

    println!("segments before compaction: {}", stats_before.segment_count);

    println!(
        "segment bytes before compaction: {}",
        stats_before.total_segment_bytes
    );

    let compact_start = Instant::now();

    let compact_stats = reopened.compact()?;

    let compact_elapsed = compact_start.elapsed();

    println!("compaction time: {:?}", compact_elapsed);

    println!("segments merged: {}", compact_stats.segments_merged);

    println!("records before: {}", compact_stats.records_before);

    println!("live records after: {}", compact_stats.live_records_after);

    let stats_after = reopened.stats();

    println!("segments after compaction: {}", stats_after.segment_count);

    println!(
        "segment bytes after compaction: {}",
        stats_after.total_segment_bytes
    );

    cleanup(&dir);

    Ok(())
}

fn benchmark_dir() -> PathBuf {
    std::env::temp_dir().join(format!("stone_throughput_bench_{}", std::process::id()))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}
