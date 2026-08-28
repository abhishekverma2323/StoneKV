use stone::Store;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "stone_compaction_correctness_{}_{}_{}",
        std::process::id(),
        id,
        name
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn snapshot(store: &mut Store, keys: &[&[u8]]) -> BTreeMap<Vec<u8>, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();

    for key in keys {
        result.insert(key.to_vec(), store.get(key).unwrap());
    }

    result
}

#[test]
fn overwrite_merge_preserves_latest_values() {
    let dir = temp_dir("overwrite");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"1", b"a").unwrap();
        store.set(b"2", b"b").unwrap();

        store.set(b"2", b"c").unwrap();
        store.set(b"3", b"d").unwrap();

        let before = snapshot(&mut store, &[b"1", b"2", b"3"]);

        assert_eq!(store.stats().segment_count, 4);

        let stats = store.compact().unwrap();

        assert_eq!(stats.segments_merged, 4);

        assert_eq!(stats.live_records_after, 3);

        assert_eq!(store.stats().segment_count, 1);

        let after = snapshot(&mut store, &[b"1", b"2", b"3"]);

        assert_eq!(before, after);

        assert_eq!(store.get(b"1").unwrap(), Some(b"a".to_vec()));

        assert_eq!(store.get(b"2").unwrap(), Some(b"c".to_vec()));

        assert_eq!(store.get(b"3").unwrap(), Some(b"d".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn deleted_keys_do_not_resurrect() {
    let dir = temp_dir("delete");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"user", b"first").unwrap();

        store.set(b"user", b"second").unwrap();

        store.delete(b"user").unwrap();

        assert_eq!(store.get(b"user").unwrap(), None);

        let stats = store.compact().unwrap();

        assert_eq!(stats.live_records_after, 0);

        assert_eq!(store.get(b"user").unwrap(), None);
    }

    {
        let mut reopened = Store::open(&dir).unwrap();

        assert_eq!(reopened.get(b"user").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn logical_state_is_identical_before_and_after() {
    let dir = temp_dir("logical_state");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"a", b"1").unwrap();
        store.set(b"b", b"2").unwrap();
        store.set(b"c", b"3").unwrap();

        store.set(b"a", b"10").unwrap();
        store.delete(b"b").unwrap();
        store.set(b"d", b"4").unwrap();

        store.set(b"c", b"30").unwrap();
        store.set(b"a", b"100").unwrap();

        let keys: [&[u8]; 5] = [b"a", b"b", b"c", b"d", b"missing"];

        let before = snapshot(&mut store, &keys);

        store.compact().unwrap();

        let after = snapshot(&mut store, &keys);

        assert_eq!(before, after);

        assert_eq!(store.stats().segment_count, 1);
    }

    cleanup(&dir);
}

#[test]
fn compaction_reduces_dead_data() {
    let dir = temp_dir("size");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        for i in 0..25 {
            let value = format!("large-value-version-{}-{}", i, "x".repeat(100));

            store.set(b"same-key", value.as_bytes()).unwrap();
        }

        let before = store.stats();

        assert_eq!(before.segment_count, 25);

        let compact = store.compact().unwrap();

        let after = store.stats();

        assert_eq!(compact.records_before, 25);

        assert_eq!(compact.live_records_after, 1);

        assert_eq!(after.segment_count, 1);

        assert!(after.total_segment_bytes < before.total_segment_bytes);
    }

    cleanup(&dir);
}

#[test]
fn three_generation_overwrite_chain_is_correct() {
    let dir = temp_dir("generations");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"key", b"v1").unwrap();
        store.set(b"key", b"v2").unwrap();
        store.set(b"key", b"v3").unwrap();

        assert_eq!(store.get(b"key").unwrap(), Some(b"v3".to_vec()));

        store.compact().unwrap();

        assert_eq!(store.get(b"key").unwrap(), Some(b"v3".to_vec()));
    }

    {
        let mut reopened = Store::open(&dir).unwrap();

        assert_eq!(reopened.get(b"key").unwrap(), Some(b"v3".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn compact_empty_store_is_safe() {
    let dir = temp_dir("empty");

    {
        let mut store = Store::open(&dir).unwrap();

        let stats = store.compact().unwrap();

        assert_eq!(stats.segments_merged, 0);

        assert_eq!(store.stats().segment_count, 0);
    }

    cleanup(&dir);
}

#[test]
fn compact_one_segment_is_noop() {
    let dir = temp_dir("one");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"a", b"1").unwrap();

        assert_eq!(store.stats().segment_count, 1);

        let stats = store.compact().unwrap();

        assert_eq!(stats.segments_merged, 0);

        assert_eq!(store.stats().segment_count, 1);

        assert_eq!(store.get(b"a").unwrap(), Some(b"1".to_vec()));
    }

    cleanup(&dir);
}
