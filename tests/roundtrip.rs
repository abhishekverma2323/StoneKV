use stone::Store;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "stone_roundtrip_{}_{}_{}",
        std::process::id(),
        id,
        name
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn brand_new_store_returns_not_found() {
    let dir = temp_dir("new");

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"missing").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn set_get_roundtrip() {
    let dir = temp_dir("set_get");

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"user:1", b"Abhishek").unwrap();

        assert_eq!(store.get(b"user:1").unwrap(), Some(b"Abhishek".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn overwrite_returns_latest_value() {
    let dir = temp_dir("overwrite");

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"key", b"old").unwrap();
        store.set(b"key", b"new").unwrap();

        assert_eq!(store.get(b"key").unwrap(), Some(b"new".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn delete_hides_value() {
    let dir = temp_dir("delete");

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"key", b"value").unwrap();
        store.delete(b"key").unwrap();

        assert_eq!(store.get(b"key").unwrap(), None);
    }

    cleanup(&dir);
}

#[test]
fn data_survives_reopen() {
    let dir = temp_dir("reopen");

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"a", b"1").unwrap();
        store.set(b"b", b"2").unwrap();
        store.set(b"c", b"3").unwrap();

        store.delete(b"b").unwrap();
    }

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"a").unwrap(), Some(b"1".to_vec()));

        assert_eq!(store.get(b"b").unwrap(), None);

        assert_eq!(store.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    cleanup(&dir);
}

#[test]
fn empty_key_and_empty_value_roundtrip() {
    let dir = temp_dir("empty");

    {
        let mut store = Store::open(&dir).unwrap();

        store.set(b"", b"").unwrap();

        assert_eq!(store.get(b"").unwrap(), Some(Vec::new()));
    }

    {
        let mut store = Store::open(&dir).unwrap();

        assert_eq!(store.get(b"").unwrap(), Some(Vec::new()));
    }

    cleanup(&dir);
}
