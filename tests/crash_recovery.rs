use stone::{StoneError, Store};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const POLY: u32 = 0xEDB88320;

fn temp_dir(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "stone_crash_recovery_{}_{}_{}",
        std::process::id(),
        id,
        name
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;

    for &byte in data {
        crc ^= byte as u32;

        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ POLY;
            } else {
                crc >>= 1;
            }
        }
    }

    crc ^ 0xFFFF_FFFF
}

fn encode_set(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();

    bytes.push(0);

    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());

    bytes.extend_from_slice(key);

    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());

    bytes.extend_from_slice(value);

    let checksum = crc32(&bytes);

    bytes.extend_from_slice(&checksum.to_le_bytes());

    bytes
}

#[test]
fn nonexistent_wal_is_valid() {
    let dir = temp_dir("nonexistent");

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"missing").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn empty_wal_is_valid() {
    let dir = temp_dir("empty");

    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("wal.log"), b"").unwrap();

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"missing").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn complete_wal_records_are_replayed() {
    let dir = temp_dir("replay");

    fs::create_dir_all(&dir).unwrap();

    let first = encode_set(b"a", b"1");
    let second = encode_set(b"b", b"2");
    let third = encode_set(b"c", b"3");

    let mut wal = Vec::new();

    wal.extend_from_slice(&first);
    wal.extend_from_slice(&second);
    wal.extend_from_slice(&third);

    fs::write(dir.join("wal.log"), &wal).unwrap();

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"a").unwrap(), Some(b"1".to_vec()));

        assert_eq!(store.get(b"b").unwrap(), Some(b"2".to_vec()));

        assert_eq!(store.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn truncated_final_record_recovers_valid_prefix() {
    let dir = temp_dir("truncated");

    fs::create_dir_all(&dir).unwrap();

    let first = encode_set(b"a", b"1");
    let second = encode_set(b"b", b"2");

    let partial_len = second.len() / 2;

    let mut wal = Vec::new();

    wal.extend_from_slice(&first);
    wal.extend_from_slice(&second[..partial_len]);

    let wal_path = dir.join("wal.log");

    fs::write(&wal_path, &wal).unwrap();

    let damaged_size = fs::metadata(&wal_path).unwrap().len();

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"a").unwrap(), Some(b"1".to_vec()));

        assert_eq!(store.get(b"b").unwrap(), None);
    }

    let recovered_size = fs::metadata(&wal_path).unwrap().len();

    assert_eq!(recovered_size, first.len() as u64);

    assert!(recovered_size < damaged_size);

    cleanup(&dir);
}

#[test]
fn new_write_after_recovery_remains_reachable() {
    let dir = temp_dir("append_after_recovery");

    fs::create_dir_all(&dir).unwrap();

    let first = encode_set(b"a", b"1");
    let damaged = encode_set(b"b", b"2");

    let mut wal = Vec::new();

    wal.extend_from_slice(&first);
    wal.extend_from_slice(&damaged[..5]);

    fs::write(dir.join("wal.log"), wal).unwrap();

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"c", b"3").unwrap();
    }

    {
        let mut reopened = Store::open(&dir).unwrap();

        assert_eq!(reopened.get(b"a").unwrap(), Some(b"1".to_vec()));

        assert_eq!(reopened.get(b"b").unwrap(), None);

        assert_eq!(reopened.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn complete_checksum_corruption_fails_loudly() {
    let dir = temp_dir("checksum");

    fs::create_dir_all(&dir).unwrap();

    let mut encoded = encode_set(b"key", b"value");

    let value_start = 1 + 4 + b"key".len() + 4;

    encoded[value_start] ^= 0x01;

    let wal_path = dir.join("wal.log");

    fs::write(&wal_path, &encoded).unwrap();

    let original_len = fs::metadata(&wal_path).unwrap().len();

    let result = Store::open(&dir);

    assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));

    assert_eq!(fs::metadata(&wal_path).unwrap().len(), original_len);

    cleanup(&dir);
}
