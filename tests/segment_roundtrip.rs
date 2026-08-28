use stone::{StoneError, Store};

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "stone_segment_roundtrip_{}_{}_{}",
        std::process::id(),
        id,
        name
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn first_segment_path(dir: &Path) -> PathBuf {
    let segments_dir = dir.join("segments");

    fs::read_dir(segments_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("seg"))
        .expect("segment file should exist")
}

#[test]
fn zero_records_store_is_valid() {
    let dir = temp_dir("zero");

    {
        let store = Store::open(&dir).unwrap();

        assert_eq!(store.stats().segment_count, 0);
    }

    cleanup(&dir);
}

#[test]
fn one_record_segment_survives_reopen() {
    let dir = temp_dir("one");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"user", b"Abhishek").unwrap();

        assert_eq!(store.stats().segment_count, 1);
    }

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"user").unwrap(), Some(b"Abhishek".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn hundred_sorted_records_roundtrip() {
    let dir = temp_dir("hundred");

    {
        let mut store = Store::open_with_threshold(&dir, usize::MAX).unwrap();

        for i in 0..100 {
            store
                .set(
                    format!("key:{:03}", i).as_bytes(),
                    format!("value:{:03}", i).as_bytes(),
                )
                .unwrap();
        }

        /*
         * compact() flushes pending memtable.
         * Since it creates only one segment,
         * compaction itself becomes a no-op.
         */
        store.compact().unwrap();

        assert_eq!(store.stats().segment_count, 1);
    }

    {
        let mut store = Store::open(&dir).unwrap();

        for i in 0..100 {
            let key = format!("key:{:03}", i);

            let expected = format!("value:{:03}", i).into_bytes();

            assert_eq!(store.get(key.as_bytes()).unwrap(), Some(expected));
        }
    }

    cleanup(&dir);
}

#[test]
fn missing_keys_at_multiple_positions_return_none() {
    let dir = temp_dir("missing");

    {
        let mut store = Store::open_with_threshold(&dir, usize::MAX).unwrap();

        store.set(b"b", b"2").unwrap();
        store.set(b"d", b"4").unwrap();
        store.set(b"f", b"6").unwrap();

        store.compact().unwrap();
    }

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"a").unwrap(), None);

        assert_eq!(store.get(b"c").unwrap(), None);

        assert_eq!(store.get(b"z").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn tombstone_segment_prevents_resurrection() {
    let dir = temp_dir("tombstone");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"key", b"value").unwrap();
        store.delete(b"key").unwrap();

        assert_eq!(store.get(b"key").unwrap(), None);
    }

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"key").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn corrupted_segment_header_is_rejected() {
    let dir = temp_dir("bad_header");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"key", b"value").unwrap();
    }

    let path = first_segment_path(&dir);

    {
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();

        file.seek(SeekFrom::Start(0)).unwrap();

        file.write_all(b"FAIL").unwrap();

        file.sync_all().unwrap();
    }

    let result = Store::open(&dir);

    assert!(matches!(result, Err(StoneError::InvalidSegmentFile { .. })));

    cleanup(&dir);
}

#[test]
fn corrupted_segment_record_crc_is_detected() {
    let dir = temp_dir("bad_crc");

    {
        let mut store = Store::open_with_threshold(&dir, 1).unwrap();

        store.set(b"k", b"value").unwrap();
    }

    let path = first_segment_path(&dir);

    /*
     * Segment header = 5 bytes.
     *
     * Record:
     * op        1
     * key_len   4
     * key       1
     * val_len   4
     *
     * Value begins at:
     * 5 + 1 + 4 + 1 + 4 = 15
     */
    let value_offset = 15u64;

    {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        file.seek(SeekFrom::Start(value_offset)).unwrap();

        let mut byte = [0u8; 1];

        file.read_exact(&mut byte).unwrap();

        byte[0] ^= 0x01;

        file.seek(SeekFrom::Start(value_offset)).unwrap();

        file.write_all(&byte).unwrap();

        file.sync_all().unwrap();
    }

    let mut store = Store::open(&dir).unwrap();

    let result = store.get(b"k");

    assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));

    cleanup(&dir);
}

#[test]
fn verify_scans_segment_records() {
    let dir = temp_dir("verify");

    {
        let mut store = Store::open_with_threshold(&dir, usize::MAX).unwrap();

        for i in 0..40 {
            store
                .set(format!("key:{:03}", i).as_bytes(), b"value")
                .unwrap();
        }

        store.compact().unwrap();

        let stats = store.verify().unwrap();

        assert_eq!(stats.segments_checked, 1);

        assert_eq!(stats.records_checked, 40);
    }

    cleanup(&dir);
}
