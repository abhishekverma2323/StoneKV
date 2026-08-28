use stone::Store;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "stone_concurrent_{}_{}_{}",
        std::process::id(),
        id,
        name
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn multiple_threads_can_share_one_store() {
    let dir = temp_dir("shared");

    let store = Store::open(&dir).unwrap();

    let store = Arc::new(Mutex::new(store));

    let thread_count = 8usize;
    let keys_per_thread = 25usize;

    let mut handles = Vec::new();

    for thread_id in 0..thread_count {
        let store = Arc::clone(&store);

        handles.push(thread::spawn(move || {
            for index in 0..keys_per_thread {
                let key = format!("thread:{}:key:{}", thread_id, index);

                let value = format!("value:{}:{}", thread_id, index);

                let mut guard = store.lock().unwrap();

                guard.set(key.as_bytes(), value.as_bytes()).unwrap();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    {
        let mut guard = store.lock().unwrap();

        for thread_id in 0..thread_count {
            for index in 0..keys_per_thread {
                let key = format!("thread:{}:key:{}", thread_id, index);

                let expected = format!("value:{}:{}", thread_id, index).into_bytes();

                assert_eq!(guard.get(key.as_bytes()).unwrap(), Some(expected));
            }
        }
    }

    drop(store);

    {
        let mut reopened = Store::open(&dir).unwrap();

        for thread_id in 0..thread_count {
            for index in 0..keys_per_thread {
                let key = format!("thread:{}:key:{}", thread_id, index);

                let expected = format!("value:{}:{}", thread_id, index).into_bytes();

                assert_eq!(reopened.get(key.as_bytes()).unwrap(), Some(expected));
            }
        }
    }

    cleanup(&dir);
}
