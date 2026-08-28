use crate::error::{Result, StoneError};
use crate::logger;
use crate::record::Record;

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayStats {
    pub records_replayed: usize,
    pub bytes_replayed: u64,
    pub truncated_tail_bytes: u64,
}

pub struct Wal {
    file: File,
    path: PathBuf,
}

impl Wal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn append(&mut self, record: &Record) -> Result<()> {
        let encoded = record.encode()?;

        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&encoded)?;
        self.file.sync_all()?;

        Ok(())
    }

    pub fn replay<F>(path: &Path, mut apply: F) -> Result<ReplayStats>
    where
        F: FnMut(&Record),
    {
        if !path.exists() {
            return Ok(ReplayStats::default());
        }

        let bytes = fs::read(path)?;

        if bytes.is_empty() {
            return Ok(ReplayStats::default());
        }

        let mut offset = 0usize;
        let mut records_replayed = 0usize;
        let mut bytes_replayed = 0u64;

        while offset < bytes.len() {
            match Record::decode(&bytes[offset..]) {
                Ok((record, consumed)) => {
                    if consumed == 0 {
                        return Err(StoneError::CorruptRecord {
                            reason: "record decoder consumed zero bytes".to_string(),
                        });
                    }

                    apply(&record);

                    offset =
                        offset
                            .checked_add(consumed)
                            .ok_or_else(|| StoneError::CorruptRecord {
                                reason: "WAL replay offset overflow".to_string(),
                            })?;

                    records_replayed += 1;
                    bytes_replayed = offset as u64;
                }

                Err(StoneError::TruncatedRecord { .. }) => {
                    let original_len = bytes.len() as u64;
                    let last_good_offset = offset as u64;

                    let truncated_tail_bytes = original_len.saturating_sub(last_good_offset);

                    logger::warn(&format!(
                        "truncated WAL tail detected: removing {} invalid byte(s)",
                        truncated_tail_bytes
                    ));

                    truncate_file_to(path, last_good_offset)?;

                    return Ok(ReplayStats {
                        records_replayed,
                        bytes_replayed,
                        truncated_tail_bytes,
                    });
                }

                Err(StoneError::ChecksumMismatch { expected, actual }) => {
                    return Err(StoneError::ChecksumMismatch { expected, actual });
                }

                Err(error) => {
                    return Err(error);
                }
            }
        }

        Ok(ReplayStats {
            records_replayed,
            bytes_replayed,
            truncated_tail_bytes: 0,
        })
    }

    pub fn truncate(&mut self) -> Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.sync_all()?;

        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn truncate_file_to(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;

    file.set_len(len)?;
    file.sync_all()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Op, Record};

    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!(
            "stone_wal_test_{}_{}_{}.log",
            std::process::id(),
            id,
            name
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_nonexistent_wal_returns_zero_stats() {
        let path = temp_path("nonexistent");
        cleanup(&path);

        let stats = Wal::replay(&path, |_| {}).unwrap();

        assert_eq!(stats.records_replayed, 0);
        assert_eq!(stats.bytes_replayed, 0);
        assert_eq!(stats.truncated_tail_bytes, 0);
    }

    #[test]
    fn replay_empty_wal_returns_zero_stats() {
        let path = temp_path("empty");

        {
            let _wal = Wal::open(&path).unwrap();
        }

        let stats = Wal::replay(&path, |_| {}).unwrap();

        assert_eq!(stats.records_replayed, 0);
        assert_eq!(stats.bytes_replayed, 0);
        assert_eq!(stats.truncated_tail_bytes, 0);

        cleanup(&path);
    }

    #[test]
    fn append_and_replay_multiple_records() {
        let path = temp_path("multiple");

        let first = Record::new_set(b"name".to_vec(), b"Abhishek".to_vec());

        let second = Record::new_set(b"project".to_vec(), b"StoneKV".to_vec());

        let third = Record::new_delete(b"name".to_vec());

        {
            let mut wal = Wal::open(&path).unwrap();

            wal.append(&first).unwrap();
            wal.append(&second).unwrap();
            wal.append(&third).unwrap();
        }

        let mut replayed = Vec::new();

        let stats = Wal::replay(&path, |record| {
            replayed.push(record.clone());
        })
        .unwrap();

        assert_eq!(stats.records_replayed, 3);
        assert_eq!(stats.truncated_tail_bytes, 0);

        assert_eq!(replayed.len(), 3);
        assert_eq!(replayed[0], first);
        assert_eq!(replayed[1], second);
        assert_eq!(replayed[2], third);

        cleanup(&path);
    }

    #[test]
    fn truncated_final_record_recovers_valid_prefix() {
        let path = temp_path("truncated");

        let first = Record::new_set(b"key1".to_vec(), b"value1".to_vec());

        let second = Record::new_set(b"key2".to_vec(), b"value2".to_vec());

        let first_bytes = first.encode().unwrap();
        let second_bytes = second.encode().unwrap();

        let partial_second_len = second_bytes.len() / 2;

        let mut damaged = Vec::new();
        damaged.extend_from_slice(&first_bytes);
        damaged.extend_from_slice(&second_bytes[..partial_second_len]);

        fs::write(&path, &damaged).unwrap();

        let mut replayed = Vec::new();

        let stats = Wal::replay(&path, |record| {
            replayed.push(record.clone());
        })
        .unwrap();

        assert_eq!(replayed, vec![first]);
        assert_eq!(stats.records_replayed, 1);

        assert_eq!(stats.truncated_tail_bytes, partial_second_len as u64);

        let recovered_len = fs::metadata(&path).unwrap().len();

        assert_eq!(recovered_len, first_bytes.len() as u64);

        cleanup(&path);
    }

    #[test]
    fn recovered_wal_accepts_new_appends() {
        let path = temp_path("recover_append");

        let first = Record::new_set(b"key1".to_vec(), b"value1".to_vec());

        let damaged = Record::new_set(b"key2".to_vec(), b"value2".to_vec());

        let first_bytes = first.encode().unwrap();
        let damaged_bytes = damaged.encode().unwrap();

        let mut contents = Vec::new();

        contents.extend_from_slice(&first_bytes);
        contents.extend_from_slice(&damaged_bytes[..5]);

        fs::write(&path, contents).unwrap();

        Wal::replay(&path, |_| {}).unwrap();

        let third = Record::new_set(b"key3".to_vec(), b"value3".to_vec());

        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(&third).unwrap();
        }

        let mut replayed = Vec::new();

        let stats = Wal::replay(&path, |record| {
            replayed.push(record.clone());
        })
        .unwrap();

        assert_eq!(stats.records_replayed, 2);
        assert_eq!(replayed, vec![first, third]);

        cleanup(&path);
    }

    #[test]
    fn checksum_corruption_returns_error() {
        let path = temp_path("checksum");

        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());

        let mut encoded = record.encode().unwrap();

        let value_start = 1 + 4 + record.key.len() + 4;

        encoded[value_start] ^= 0x01;

        fs::write(&path, encoded).unwrap();

        let result = Wal::replay(&path, |_| {});

        assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));

        cleanup(&path);
    }

    #[test]
    fn checksum_corruption_is_not_truncated_away() {
        let path = temp_path("checksum_preserved");

        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());

        let mut encoded = record.encode().unwrap();

        let original_len = encoded.len() as u64;

        let value_start = 1 + 4 + record.key.len() + 4;

        encoded[value_start] ^= 0x01;

        fs::write(&path, encoded).unwrap();

        let result = Wal::replay(&path, |_| {});

        assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));

        assert_eq!(fs::metadata(&path).unwrap().len(), original_len);

        cleanup(&path);
    }

    #[test]
    fn truncate_clears_wal() {
        let path = temp_path("truncate");

        let record = Record::new_set(b"hello".to_vec(), b"world".to_vec());

        {
            let mut wal = Wal::open(&path).unwrap();

            wal.append(&record).unwrap();

            assert!(fs::metadata(&path).unwrap().len() > 0);

            wal.truncate().unwrap();

            assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        }

        cleanup(&path);
    }

    #[test]
    fn wal_path_returns_original_path() {
        let path = temp_path("path");

        {
            let wal = Wal::open(&path).unwrap();

            assert_eq!(wal.path(), path.as_path());
        }

        cleanup(&path);
    }

    #[test]
    fn delete_record_replays_correctly() {
        let path = temp_path("delete");

        let record = Record::new_delete(b"user:1".to_vec());

        {
            let mut wal = Wal::open(&path).unwrap();

            wal.append(&record).unwrap();
        }

        let mut replayed = Vec::new();

        Wal::replay(&path, |record| {
            replayed.push(record.clone());
        })
        .unwrap();

        assert_eq!(replayed.len(), 1);

        assert_eq!(replayed[0].op, Op::Delete);
        assert_eq!(replayed[0].key, b"user:1");
        assert!(replayed[0].val.is_empty());

        cleanup(&path);
    }
}
