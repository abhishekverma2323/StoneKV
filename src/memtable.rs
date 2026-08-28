use std::collections::BTreeMap;

const ENTRY_OVERHEAD_BYTES: usize = 16;

pub struct Memtable {
    map: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    approx_size_bytes: usize,
}

impl Memtable {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            approx_size_bytes: 0,
        }
    }

    pub fn set(&mut self, key: Vec<u8>, val: Vec<u8>) {
        let key_len = key.len();
        let new_size = entry_size(key_len, Some(val.len()));

        if let Some(old_value) = self.map.insert(key, Some(val)) {
            let old_size = entry_size(key_len, old_value.as_ref().map(|value| value.len()));

            self.approx_size_bytes = self.approx_size_bytes.saturating_sub(old_size);
        }

        self.approx_size_bytes = self.approx_size_bytes.saturating_add(new_size);
    }

    pub fn delete(&mut self, key: Vec<u8>) {
        let key_len = key.len();
        let new_size = entry_size(key_len, None);

        if let Some(old_value) = self.map.insert(key, None) {
            let old_size = entry_size(key_len, old_value.as_ref().map(|value| value.len()));

            self.approx_size_bytes = self.approx_size_bytes.saturating_sub(old_size);
        }

        self.approx_size_bytes = self.approx_size_bytes.saturating_add(new_size);
    }

    pub fn get(&self, key: &[u8]) -> Option<Option<&Vec<u8>>> {
        match self.map.get(key) {
            Some(Some(value)) => Some(Some(value)),
            Some(None) => Some(None),
            None => None,
        }
    }

    pub fn approx_size_bytes(&self) -> usize {
        self.approx_size_bytes
    }

    pub fn is_over_threshold(&self, threshold: usize) -> bool {
        self.approx_size_bytes >= threshold
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &Option<Vec<u8>>)> {
        self.map.iter()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.approx_size_bytes = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

impl Default for Memtable {
    fn default() -> Self {
        Self::new()
    }
}

fn entry_size(key_len: usize, value_len: Option<usize>) -> usize {
    key_len
        .saturating_add(value_len.unwrap_or(0))
        .saturating_add(ENTRY_OVERHEAD_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_value() {
        let mut memtable = Memtable::new();

        memtable.set(b"user:1".to_vec(), b"Abhishek".to_vec());

        let result = memtable.get(b"user:1");

        assert_eq!(result, Some(Some(&b"Abhishek".to_vec())));
    }

    #[test]
    fn overwrite_updates_value() {
        let mut memtable = Memtable::new();

        memtable.set(b"key".to_vec(), b"old".to_vec());

        memtable.set(b"key".to_vec(), b"new".to_vec());

        assert_eq!(memtable.get(b"key"), Some(Some(&b"new".to_vec())));

        assert_eq!(memtable.len(), 1);
    }

    #[test]
    fn delete_creates_tombstone() {
        let mut memtable = Memtable::new();

        memtable.set(b"user:1".to_vec(), b"value".to_vec());

        memtable.delete(b"user:1".to_vec());

        assert_eq!(memtable.get(b"user:1"), Some(None));
    }

    #[test]
    fn untouched_key_returns_none() {
        let memtable = Memtable::new();

        assert_eq!(memtable.get(b"missing"), None);
    }

    #[test]
    fn iteration_is_sorted() {
        let mut memtable = Memtable::new();

        memtable.set(b"z".to_vec(), b"3".to_vec());
        memtable.set(b"a".to_vec(), b"1".to_vec());
        memtable.set(b"m".to_vec(), b"2".to_vec());

        let keys: Vec<Vec<u8>> = memtable.iter().map(|(key, _)| key.clone()).collect();

        assert_eq!(keys, vec![b"a".to_vec(), b"m".to_vec(), b"z".to_vec(),]);
    }

    #[test]
    fn threshold_behavior() {
        let mut memtable = Memtable::new();

        assert!(!memtable.is_over_threshold(20));

        memtable.set(b"key".to_vec(), b"value".to_vec());

        assert!(memtable.approx_size_bytes() > 0);

        assert!(memtable.is_over_threshold(memtable.approx_size_bytes()));

        assert!(!memtable.is_over_threshold(memtable.approx_size_bytes() + 1));
    }

    #[test]
    fn replacing_large_value_with_small_value_decreases_size() {
        let mut memtable = Memtable::new();

        memtable.set(b"key".to_vec(), vec![b'x'; 1000]);

        let large_size = memtable.approx_size_bytes();

        memtable.set(b"key".to_vec(), b"x".to_vec());

        let small_size = memtable.approx_size_bytes();

        assert!(small_size < large_size);
    }

    #[test]
    fn replacing_value_with_tombstone_decreases_size() {
        let mut memtable = Memtable::new();

        memtable.set(b"key".to_vec(), vec![b'x'; 500]);

        let before = memtable.approx_size_bytes();

        memtable.delete(b"key".to_vec());

        let after = memtable.approx_size_bytes();

        assert!(after < before);
        assert_eq!(memtable.get(b"key"), Some(None));
    }

    #[test]
    fn clear_resets_memtable() {
        let mut memtable = Memtable::new();

        memtable.set(b"a".to_vec(), b"one".to_vec());

        memtable.set(b"b".to_vec(), b"two".to_vec());

        assert!(!memtable.is_empty());
        assert!(memtable.approx_size_bytes() > 0);

        memtable.clear();

        assert!(memtable.is_empty());
        assert_eq!(memtable.len(), 0);
        assert_eq!(memtable.approx_size_bytes(), 0);
    }

    #[test]
    fn deleting_nonexistent_key_creates_tombstone() {
        let mut memtable = Memtable::new();

        memtable.delete(b"ghost".to_vec());

        assert_eq!(memtable.len(), 1);
        assert_eq!(memtable.get(b"ghost"), Some(None));
    }
}
