use crate::error::{Result, StoneError};
use crate::record::{Op, Record};

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const SPARSE_INDEX_INTERVAL: usize = 16;

pub const SEGMENT_MAGIC: &[u8; 4] = b"STON";
pub const SEGMENT_VERSION: u8 = 1;
pub const SEGMENT_HEADER_SIZE: u64 = 5;
pub const SEGMENT_FOOTER_SIZE: u64 = 12;

#[derive(Debug, Clone)]
struct SparseIndexEntry {
    key: Vec<u8>,
    file_offset: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentWriteStats {
    pub records_written: usize,
    pub file_bytes: u64,
    pub index_entries: usize,
}

pub struct SegmentWriter {
    writer: BufWriter<File>,
    path: PathBuf,
    current_offset: u64,
    sparse_index: Vec<SparseIndexEntry>,
    record_count: usize,
    last_key: Option<Vec<u8>>,
}

impl SegmentWriter {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(path)?;

        let mut writer = BufWriter::new(file);

        writer.write_all(SEGMENT_MAGIC)?;
        writer.write_all(&[SEGMENT_VERSION])?;

        Ok(Self {
            writer,
            path: path.to_path_buf(),
            current_offset: SEGMENT_HEADER_SIZE,
            sparse_index: Vec::new(),
            record_count: 0,
            last_key: None,
        })
    }

    pub fn write_all<'a, I>(&mut self, entries: I) -> Result<()>
    where
        I: IntoIterator<Item = (&'a Vec<u8>, &'a Option<Vec<u8>>)>,
    {
        for (key, value) in entries {
            if let Some(previous) = &self.last_key {
                if key <= previous {
                    return Err(StoneError::Other(
                        "segment entries must be strictly increasing".to_string(),
                    ));
                }
            }

            let record = match value {
                Some(value) => Record::new_set(key.clone(), value.clone()),
                None => Record::new_delete(key.clone()),
            };

            let encoded = record.encode()?;
            let record_offset = self.current_offset;

            if self.record_count % SPARSE_INDEX_INTERVAL == 0 {
                self.sparse_index.push(SparseIndexEntry {
                    key: key.clone(),
                    file_offset: record_offset,
                });
            }

            self.writer.write_all(&encoded)?;

            self.current_offset = self
                .current_offset
                .checked_add(encoded.len() as u64)
                .ok_or_else(|| StoneError::Other("segment file offset overflow".to_string()))?;

            self.record_count += 1;
            self.last_key = Some(key.clone());
        }

        Ok(())
    }

    pub fn finish(mut self) -> Result<SegmentWriteStats> {
        let index_offset = self.current_offset;

        for entry in &self.sparse_index {
            let key_len =
                u32::try_from(entry.key.len()).map_err(|_| StoneError::RecordTooLarge {
                    field: "segment index key",
                    len: entry.key.len(),
                })?;

            self.writer.write_all(&key_len.to_le_bytes())?;
            self.writer.write_all(&entry.key)?;
            self.writer.write_all(&entry.file_offset.to_le_bytes())?;

            self.current_offset = self
                .current_offset
                .checked_add(4)
                .and_then(|offset| offset.checked_add(entry.key.len() as u64))
                .and_then(|offset| offset.checked_add(8))
                .ok_or_else(|| StoneError::Other("segment index size overflow".to_string()))?;
        }

        self.writer.write_all(&index_offset.to_le_bytes())?;
        self.writer.write_all(SEGMENT_MAGIC)?;

        self.current_offset = self
            .current_offset
            .checked_add(SEGMENT_FOOTER_SIZE)
            .ok_or_else(|| StoneError::Other("segment footer size overflow".to_string()))?;

        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;

        let file_bytes = self.writer.get_ref().metadata()?.len();

        if file_bytes != self.current_offset {
            return Err(StoneError::InvalidSegmentFile {
                path: self.path.display().to_string(),
                reason: format!(
                    "unexpected final size: expected {}, got {}",
                    self.current_offset, file_bytes
                ),
            });
        }

        Ok(SegmentWriteStats {
            records_written: self.record_count,
            file_bytes,
            index_entries: self.sparse_index.len(),
        })
    }
}

pub struct SegmentReader {
    file: File,
    path: PathBuf,
    generation: u64,
    sparse_index: Vec<SparseIndexEntry>,
    records_start: u64,
    index_offset: u64,
}

impl SegmentReader {
    pub fn open(path: &Path, generation: u64) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).open(path)?;

        let file_len = file.metadata()?.len();

        let minimum_size = SEGMENT_HEADER_SIZE + SEGMENT_FOOTER_SIZE;

        if file_len < minimum_size {
            return Err(invalid_segment(
                path,
                format!(
                    "file too small: {} bytes, minimum is {}",
                    file_len, minimum_size
                ),
            ));
        }

        file.seek(SeekFrom::Start(0))?;

        let mut header = [0u8; 5];
        file.read_exact(&mut header)?;

        if &header[..4] != SEGMENT_MAGIC {
            return Err(invalid_segment(path, "invalid header magic".to_string()));
        }

        if header[4] != SEGMENT_VERSION {
            return Err(invalid_segment(
                path,
                format!("unsupported segment version: {}", header[4]),
            ));
        }

        let footer_start = file_len
            .checked_sub(SEGMENT_FOOTER_SIZE)
            .ok_or_else(|| invalid_segment(path, "invalid footer position".to_string()))?;

        file.seek(SeekFrom::Start(footer_start))?;

        let mut index_offset_bytes = [0u8; 8];
        file.read_exact(&mut index_offset_bytes)?;

        let index_offset = u64::from_le_bytes(index_offset_bytes);

        let mut footer_magic = [0u8; 4];
        file.read_exact(&mut footer_magic)?;

        if &footer_magic != SEGMENT_MAGIC {
            return Err(invalid_segment(path, "invalid footer magic".to_string()));
        }

        if index_offset < SEGMENT_HEADER_SIZE || index_offset > footer_start {
            return Err(invalid_segment(
                path,
                format!("invalid index offset: {}", index_offset),
            ));
        }

        let sparse_index = read_sparse_index(&mut file, path, index_offset, footer_start)?;

        if index_offset == SEGMENT_HEADER_SIZE {
            if !sparse_index.is_empty() {
                return Err(invalid_segment(
                    path,
                    "empty record area contains sparse index entries".to_string(),
                ));
            }
        } else {
            if sparse_index.is_empty() {
                return Err(invalid_segment(
                    path,
                    "non-empty segment has no sparse index".to_string(),
                ));
            }

            if sparse_index[0].file_offset != SEGMENT_HEADER_SIZE {
                return Err(invalid_segment(
                    path,
                    "first sparse index entry must point to first record".to_string(),
                ));
            }
        }

        Ok(Self {
            file,
            path: path.to_path_buf(),
            generation,
            sparse_index,
            records_start: SEGMENT_HEADER_SIZE,
            index_offset,
        })
    }

    pub fn get(&mut self, key: &[u8]) -> Result<Option<Option<Vec<u8>>>> {
        let mut start_offset = self.records_start;

        for entry in &self.sparse_index {
            if entry.key.as_slice() <= key {
                start_offset = entry.file_offset;
            } else {
                break;
            }
        }

        let mut current_offset = start_offset;

        while current_offset < self.index_offset {
            let (record, consumed) = self.read_record_at(current_offset)?;

            match record.key.as_slice().cmp(key) {
                std::cmp::Ordering::Equal => {
                    return match record.op {
                        Op::Set => Ok(Some(Some(record.val))),
                        Op::Delete => Ok(Some(None)),
                    };
                }

                std::cmp::Ordering::Greater => {
                    return Ok(None);
                }

                std::cmp::Ordering::Less => {}
            }

            current_offset = current_offset.checked_add(consumed).ok_or_else(|| {
                invalid_segment(&self.path, "record scan offset overflow".to_string())
            })?;
        }

        Ok(None)
    }

    pub fn iter_all(&mut self) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        let mut current_offset = self.records_start;

        while current_offset < self.index_offset {
            let (record, consumed) = self.read_record_at(current_offset)?;

            records.push(record);

            current_offset = current_offset.checked_add(consumed).ok_or_else(|| {
                invalid_segment(&self.path, "record iteration offset overflow".to_string())
            })?;
        }

        if current_offset != self.index_offset {
            return Err(invalid_segment(
                &self.path,
                format!(
                    "record area ended at {}, expected {}",
                    current_offset, self.index_offset
                ),
            ));
        }

        Ok(records)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn read_record_at(&mut self, offset: u64) -> Result<(Record, u64)> {
        if offset < self.records_start || offset >= self.index_offset {
            return Err(invalid_segment(
                &self.path,
                format!("invalid record offset: {}", offset),
            ));
        }

        let fixed_key_header_end = offset.checked_add(5).ok_or_else(|| {
            invalid_segment(&self.path, "record header offset overflow".to_string())
        })?;

        if fixed_key_header_end > self.index_offset {
            return Err(invalid_segment(
                &self.path,
                "record header extends into sparse index".to_string(),
            ));
        }

        self.file.seek(SeekFrom::Start(offset))?;

        let mut first_five = [0u8; 5];
        self.file.read_exact(&mut first_five)?;

        let key_len =
            u32::from_le_bytes([first_five[1], first_five[2], first_five[3], first_five[4]]) as u64;

        let val_len_offset = offset
            .checked_add(5)
            .and_then(|value| value.checked_add(key_len))
            .ok_or_else(|| {
                invalid_segment(&self.path, "key length arithmetic overflow".to_string())
            })?;

        let val_len_end = val_len_offset.checked_add(4).ok_or_else(|| {
            invalid_segment(&self.path, "value length offset overflow".to_string())
        })?;

        if val_len_end > self.index_offset {
            return Err(invalid_segment(
                &self.path,
                "key extends beyond record area".to_string(),
            ));
        }

        self.file.seek(SeekFrom::Start(val_len_offset))?;

        let mut val_len_bytes = [0u8; 4];
        self.file.read_exact(&mut val_len_bytes)?;

        let val_len = u32::from_le_bytes(val_len_bytes) as u64;

        let total_len = 1u64
            .checked_add(4)
            .and_then(|value| value.checked_add(key_len))
            .and_then(|value| value.checked_add(4))
            .and_then(|value| value.checked_add(val_len))
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| invalid_segment(&self.path, "record length overflow".to_string()))?;

        let record_end = offset
            .checked_add(total_len)
            .ok_or_else(|| invalid_segment(&self.path, "record end overflow".to_string()))?;

        if record_end > self.index_offset {
            return Err(invalid_segment(
                &self.path,
                format!("record at offset {} extends beyond index offset", offset),
            ));
        }

        let total_len_usize = usize::try_from(total_len).map_err(|_| {
            invalid_segment(&self.path, "record too large for platform".to_string())
        })?;

        let mut encoded = vec![0u8; total_len_usize];

        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut encoded)?;

        let (record, consumed) = Record::decode(&encoded)?;

        if consumed != encoded.len() {
            return Err(invalid_segment(
                &self.path,
                "record decoder consumed unexpected number of bytes".to_string(),
            ));
        }

        Ok((record, total_len))
    }
}

fn read_sparse_index(
    file: &mut File,
    path: &Path,
    index_offset: u64,
    footer_start: u64,
) -> Result<Vec<SparseIndexEntry>> {
    let mut entries = Vec::new();
    let mut current = index_offset;

    file.seek(SeekFrom::Start(index_offset))?;

    let mut previous_key: Option<Vec<u8>> = None;
    let mut previous_offset: Option<u64> = None;

    while current < footer_start {
        let key_len_end = current
            .checked_add(4)
            .ok_or_else(|| invalid_segment(path, "index key length offset overflow".to_string()))?;

        if key_len_end > footer_start {
            return Err(invalid_segment(
                path,
                "truncated sparse index key length".to_string(),
            ));
        }

        let mut key_len_bytes = [0u8; 4];
        file.read_exact(&mut key_len_bytes)?;

        let key_len = u32::from_le_bytes(key_len_bytes) as u64;

        current = key_len_end;

        let entry_end = current
            .checked_add(key_len)
            .and_then(|value| value.checked_add(8))
            .ok_or_else(|| invalid_segment(path, "sparse index entry size overflow".to_string()))?;

        if entry_end > footer_start {
            return Err(invalid_segment(
                path,
                "truncated sparse index entry".to_string(),
            ));
        }

        let key_len_usize = usize::try_from(key_len)
            .map_err(|_| invalid_segment(path, "sparse index key too large".to_string()))?;

        let mut key = vec![0u8; key_len_usize];
        file.read_exact(&mut key)?;

        let mut offset_bytes = [0u8; 8];
        file.read_exact(&mut offset_bytes)?;

        let record_offset = u64::from_le_bytes(offset_bytes);

        if record_offset < SEGMENT_HEADER_SIZE || record_offset >= index_offset {
            return Err(invalid_segment(
                path,
                format!("invalid sparse index record offset: {}", record_offset),
            ));
        }

        if let Some(previous) = &previous_key {
            if key <= *previous {
                return Err(invalid_segment(
                    path,
                    "sparse index keys are not strictly increasing".to_string(),
                ));
            }
        }

        if let Some(previous) = previous_offset {
            if record_offset <= previous {
                return Err(invalid_segment(
                    path,
                    "sparse index offsets are not strictly increasing".to_string(),
                ));
            }
        }

        previous_key = Some(key.clone());
        previous_offset = Some(record_offset);

        entries.push(SparseIndexEntry {
            key,
            file_offset: record_offset,
        });

        current = entry_end;
    }

    if current != footer_start {
        return Err(invalid_segment(
            path,
            "sparse index did not terminate at footer".to_string(),
        ));
    }

    Ok(entries)
}

fn invalid_segment(path: &Path, reason: String) -> StoneError {
    StoneError::InvalidSegmentFile {
        path: path.display().to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!(
            "stone_segment_test_{}_{}_{}.seg",
            std::process::id(),
            id,
            name
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    fn write_segment(
        path: &Path,
        entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    ) -> SegmentWriteStats {
        let mut writer = SegmentWriter::create(path).unwrap();

        writer.write_all(entries.iter()).unwrap();

        writer.finish().unwrap()
    }

    #[test]
    fn empty_segment_is_valid() {
        let path = temp_path("empty");

        let entries = BTreeMap::new();

        let stats = write_segment(&path, &entries);

        assert_eq!(stats.records_written, 0);
        assert_eq!(stats.index_entries, 0);
        assert_eq!(stats.file_bytes, SEGMENT_HEADER_SIZE + SEGMENT_FOOTER_SIZE);

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        assert!(reader.iter_all().unwrap().is_empty());
        assert_eq!(reader.get(b"anything").unwrap(), None);

        cleanup(&path);
    }

    #[test]
    fn single_record_roundtrip() {
        let path = temp_path("single");

        let mut entries = BTreeMap::new();

        entries.insert(b"user:1".to_vec(), Some(b"Abhishek".to_vec()));

        write_segment(&path, &entries);

        let mut reader = SegmentReader::open(&path, 7).unwrap();

        assert_eq!(reader.generation(), 7);

        assert_eq!(
            reader.get(b"user:1").unwrap(),
            Some(Some(b"Abhishek".to_vec()))
        );

        cleanup(&path);
    }

    #[test]
    fn hundred_records_roundtrip() {
        let path = temp_path("hundred");

        let mut entries = BTreeMap::new();

        for i in 0..100 {
            entries.insert(
                format!("key:{:03}", i).into_bytes(),
                Some(format!("value:{:03}", i).into_bytes()),
            );
        }

        let stats = write_segment(&path, &entries);

        assert_eq!(stats.records_written, 100);
        assert_eq!(
            stats.index_entries,
            100usize.div_ceil(SPARSE_INDEX_INTERVAL)
        );

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        for i in 0..100 {
            let key = format!("key:{:03}", i).into_bytes();

            let expected = format!("value:{:03}", i).into_bytes();

            assert_eq!(reader.get(&key).unwrap(), Some(Some(expected)));
        }

        assert_eq!(reader.iter_all().unwrap().len(), 100);

        cleanup(&path);
    }

    #[test]
    fn missing_keys_return_none() {
        let path = temp_path("missing");

        let mut entries = BTreeMap::new();

        entries.insert(b"b".to_vec(), Some(b"2".to_vec()));

        entries.insert(b"d".to_vec(), Some(b"4".to_vec()));

        entries.insert(b"f".to_vec(), Some(b"6".to_vec()));

        write_segment(&path, &entries);

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        assert_eq!(reader.get(b"a").unwrap(), None);
        assert_eq!(reader.get(b"c").unwrap(), None);
        assert_eq!(reader.get(b"z").unwrap(), None);

        cleanup(&path);
    }

    #[test]
    fn tombstone_roundtrip() {
        let path = temp_path("tombstone");

        let mut entries = BTreeMap::new();

        entries.insert(b"deleted".to_vec(), None);

        write_segment(&path, &entries);

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        assert_eq!(reader.get(b"deleted").unwrap(), Some(None));

        let records = reader.iter_all().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].op, Op::Delete);

        cleanup(&path);
    }

    #[test]
    fn bad_header_is_rejected() {
        let path = temp_path("bad_header");

        let entries = BTreeMap::new();
        write_segment(&path, &entries);

        {
            let mut file = OpenOptions::new().write(true).open(&path).unwrap();

            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"FAIL").unwrap();
            file.sync_all().unwrap();
        }

        let result = SegmentReader::open(&path, 1);

        assert!(matches!(result, Err(StoneError::InvalidSegmentFile { .. })));

        cleanup(&path);
    }

    #[test]
    fn bad_footer_is_rejected() {
        let path = temp_path("bad_footer");

        let entries = BTreeMap::new();
        write_segment(&path, &entries);

        {
            let len = fs::metadata(&path).unwrap().len();

            let mut file = OpenOptions::new().write(true).open(&path).unwrap();

            file.seek(SeekFrom::Start(len - 4)).unwrap();

            file.write_all(b"FAIL").unwrap();
            file.sync_all().unwrap();
        }

        let result = SegmentReader::open(&path, 1);

        assert!(matches!(result, Err(StoneError::InvalidSegmentFile { .. })));

        cleanup(&path);
    }

    #[test]
    fn invalid_index_offset_is_rejected() {
        let path = temp_path("bad_index_offset");

        let mut entries = BTreeMap::new();

        entries.insert(b"a".to_vec(), Some(b"1".to_vec()));

        write_segment(&path, &entries);

        {
            let len = fs::metadata(&path).unwrap().len();

            let mut file = OpenOptions::new().write(true).open(&path).unwrap();

            file.seek(SeekFrom::Start(len - SEGMENT_FOOTER_SIZE))
                .unwrap();

            file.write_all(&0u64.to_le_bytes()).unwrap();

            file.sync_all().unwrap();
        }

        let result = SegmentReader::open(&path, 1);

        assert!(matches!(result, Err(StoneError::InvalidSegmentFile { .. })));

        cleanup(&path);
    }

    #[test]
    fn corrupted_record_crc_is_detected() {
        let path = temp_path("bad_crc");

        let mut entries = BTreeMap::new();

        entries.insert(b"a".to_vec(), Some(b"value".to_vec()));

        write_segment(&path, &entries);

        {
            let value_offset = SEGMENT_HEADER_SIZE + 1 + 4 + 1 + 4;

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

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        let result = reader.get(b"a");

        assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));

        cleanup(&path);
    }

    #[test]
    fn sparse_index_boundary_lookups_work() {
        let path = temp_path("index_boundary");

        let mut entries = BTreeMap::new();

        for i in 0..40 {
            entries.insert(
                format!("key:{:03}", i).into_bytes(),
                Some(format!("value:{:03}", i).into_bytes()),
            );
        }

        write_segment(&path, &entries);

        let mut reader = SegmentReader::open(&path, 1).unwrap();

        for i in [0, 15, 16, 17, 31, 32, 33, 39] {
            let key = format!("key:{:03}", i).into_bytes();

            let expected = format!("value:{:03}", i).into_bytes();

            assert_eq!(reader.get(&key).unwrap(), Some(Some(expected)));
        }

        cleanup(&path);
    }

    #[test]
    fn reader_reports_original_path() {
        let path = temp_path("path");

        let entries = BTreeMap::new();
        write_segment(&path, &entries);

        let reader = SegmentReader::open(&path, 1).unwrap();

        assert_eq!(reader.path(), path.as_path());

        cleanup(&path);
    }
}
