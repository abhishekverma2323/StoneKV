use crate::compaction::compact_all;
use crate::error::{Result, StoneError};
use crate::logger;
use crate::memtable::Memtable;
use crate::record::{Op, Record};
use crate::segment::{SegmentReader, SegmentWriter};
use crate::wal::Wal;

use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_FLUSH_THRESHOLD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub segment_count: usize,
    pub total_segment_bytes: u64,
    pub wal_bytes: u64,
    pub memtable_entries: usize,
    pub memtable_size_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionStats {
    pub segments_merged: usize,
    pub records_before: usize,
    pub live_records_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyStats {
    pub wal_records: usize,
    pub segments_checked: usize,
    pub records_checked: usize,
}

pub struct Store {
    dir: PathBuf,
    wal: Wal,
    memtable: Memtable,
    segments: Vec<SegmentReader>,
    next_segment_generation: u64,
    flush_threshold_bytes: usize,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with_threshold(dir, DEFAULT_FLUSH_THRESHOLD_BYTES)
    }

    #[doc(hidden)]
    pub fn open_with_threshold(dir: &Path, flush_threshold_bytes: usize) -> Result<Self> {
        fs::create_dir_all(dir)?;

        let segments_dir = dir.join("segments");
        fs::create_dir_all(&segments_dir)?;

        remove_stale_temp_files(&segments_dir)?;

        let (segments, next_segment_generation) = load_segments(&segments_dir)?;

        let wal_path = dir.join("wal.log");

        let mut memtable = Memtable::new();

        Wal::replay(&wal_path, |record| {
            apply_record(&mut memtable, record);
        })?;

        let wal = Wal::open(&wal_path)?;

        Ok(Self {
            dir: dir.to_path_buf(),
            wal,
            memtable,
            segments,
            next_segment_generation,
            flush_threshold_bytes,
        })
    }

    pub fn set(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        let record = Record::new_set(key.to_vec(), val.to_vec());

        // WAL becomes durable first.
        self.wal.append(&record)?;

        self.memtable.set(key.to_vec(), val.to_vec());

        if self.memtable.is_over_threshold(self.flush_threshold_bytes) {
            self.flush_memtable()?;
        }

        Ok(())
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        let record = Record::new_delete(key.to_vec());

        // WAL first.
        self.wal.append(&record)?;

        self.memtable.delete(key.to_vec());

        if self.memtable.is_over_threshold(self.flush_threshold_bytes) {
            self.flush_memtable()?;
        }

        Ok(())
    }

    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.memtable.get(key) {
            Some(Some(value)) => {
                return Ok(Some(value.clone()));
            }

            Some(None) => {
                return Ok(None);
            }

            None => {}
        }

        // Newest segment first.
        for segment in &mut self.segments {
            match segment.get(key)? {
                Some(Some(value)) => {
                    return Ok(Some(value));
                }

                Some(None) => {
                    return Ok(None);
                }

                None => {}
            }
        }

        Ok(None)
    }

    pub fn compact(&mut self) -> Result<CompactionStats> {
        /*
         * Full compaction should include current unflushed state.
         *
         * Flush first so all logical data exists in immutable
         * segments before compaction begins.
         */
        if !self.memtable.is_empty() {
            self.flush_memtable()?;
        }

        /*
         * V2 intentionally performs no compaction when fewer than
         * two segments exist.
         */
        if self.segments.len() < 2 {
            return Ok(CompactionStats::default());
        }

        let generation = self.next_segment_generation;

        let segments_dir = self.dir.join("segments");

        let temp_path = segments_dir.join(segment_temp_filename(generation));

        let final_path = segments_dir.join(segment_filename(generation));

        if temp_path.exists() {
            fs::remove_file(&temp_path)?;
        }

        /*
         * self.segments is newest -> oldest.
         *
         * compact_all requires:
         *
         * oldest -> newest
         */
        let input_paths: Vec<PathBuf> = self
            .segments
            .iter()
            .rev()
            .map(|segment| segment.path().to_path_buf())
            .collect();

        let build_stats = compact_all(&input_paths, &temp_path)?;

        /*
         * compact_all() already flushed + sync_all()'d
         * the temporary segment.
         */
        fs::rename(&temp_path, &final_path)?;

        /*
         * Never remove old segments until the new segment
         * has successfully opened and validated.
         */
        let new_reader = match SegmentReader::open(&final_path, generation) {
            Ok(reader) => reader,

            Err(error) => {
                logger::error(&format!(
                    "failed to validate compacted segment '{}': {}",
                    final_path.display(),
                    error
                ));

                let _ = fs::remove_file(&final_path);

                return Err(error);
            }
        };

        self.next_segment_generation = generation
            .checked_add(1)
            .ok_or_else(|| StoneError::Other("segment generation exhausted".to_string()))?;

        /*
         * Save old filenames before dropping readers.
         *
         * This is important on Windows because an open File
         * handle may prevent deletion.
         */
        let old_paths: Vec<PathBuf> = self
            .segments
            .iter()
            .map(|segment| segment.path().to_path_buf())
            .collect();

        /*
         * Install the new compacted segment in-memory.
         */
        let old_segments = std::mem::replace(&mut self.segments, vec![new_reader]);

        /*
         * Explicitly close old segment file handles before
         * deleting their files.
         */
        drop(old_segments);

        /*
         * New segment is already installed and valid.
         *
         * Old files can now be removed.
         */
        for path in old_paths {
            fs::remove_file(path)?;
        }

        logger::info(&format!(
            "compacted {} segments into generation {}",
            build_stats.segments_read, generation
        ));

        Ok(CompactionStats {
            segments_merged: build_stats.segments_read,

            records_before: build_stats.records_before,

            live_records_after: build_stats.live_records_after,

            bytes_before: build_stats.bytes_before,

            bytes_after: build_stats.bytes_after,
        })
    }

    pub fn stats(&self) -> StoreStats {
        let total_segment_bytes = self
            .segments
            .iter()
            .filter_map(|segment| {
                fs::metadata(segment.path())
                    .ok()
                    .map(|metadata| metadata.len())
            })
            .sum();

        let wal_bytes = fs::metadata(self.wal.path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);

        StoreStats {
            segment_count: self.segments.len(),

            total_segment_bytes,

            wal_bytes,

            memtable_entries: self.memtable.len(),

            memtable_size_bytes: self.memtable.approx_size_bytes(),
        }
    }

    pub fn verify(&mut self) -> Result<VerifyStats> {
        let wal_stats = Wal::replay(self.wal.path(), |_| {})?;

        let mut records_checked = 0usize;
        let mut previous_generation: Option<u64> = None;

        for segment in &mut self.segments {
            let generation = segment.generation();

            // Segments must always be newest -> oldest.
            if let Some(previous) = previous_generation {
                if generation >= previous {
                    return Err(StoneError::InvalidSegmentFile {
                        path: segment.path().display().to_string(),

                        reason: format!(
                            "invalid segment generation order: \
                             generation {} appears after {}",
                            generation, previous
                        ),
                    });
                }
            }

            previous_generation = Some(generation);

            let records = segment.iter_all()?;

            records_checked = records_checked
                .checked_add(records.len())
                .ok_or_else(|| StoneError::Other("verify record count overflow".to_string()))?;
        }

        Ok(VerifyStats {
            wal_records: wal_stats.records_replayed,
            segments_checked: self.segments.len(),
            records_checked,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn flush_threshold_bytes(&self) -> usize {
        self.flush_threshold_bytes
    }

    pub fn memtable_entries(&self) -> usize {
        self.memtable.len()
    }

    pub fn memtable_size_bytes(&self) -> usize {
        self.memtable.approx_size_bytes()
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    fn flush_memtable(&mut self) -> Result<()> {
        if self.memtable.is_empty() {
            return Ok(());
        }

        let generation = self.next_segment_generation;

        let segments_dir = self.dir.join("segments");

        let final_path = segments_dir.join(segment_filename(generation));

        let temp_path = segments_dir.join(segment_temp_filename(generation));

        if temp_path.exists() {
            fs::remove_file(&temp_path)?;
        }

        /*
         * Write segment to temporary filename.
         */
        let mut writer = SegmentWriter::create(&temp_path)?;

        writer.write_all(self.memtable.iter())?;

        /*
         * finish() flushes BufWriter and calls sync_all().
         */
        writer.finish()?;

        /*
         * Install final immutable segment.
         */
        fs::rename(&temp_path, &final_path)?;

        /*
         * Validate before WAL cleanup.
         */
        let reader = match SegmentReader::open(&final_path, generation) {
            Ok(reader) => reader,

            Err(error) => {
                logger::error(&format!(
                    "failed to validate installed segment '{}': {}",
                    final_path.display(),
                    error
                ));

                let _ = fs::remove_file(&final_path);

                return Err(error);
            }
        };

        /*
         * New segment becomes newest.
         */
        self.segments.insert(0, reader);

        self.next_segment_generation = generation
            .checked_add(1)
            .ok_or_else(|| StoneError::Other("segment generation exhausted".to_string()))?;

        /*
         * Only clear WAL AFTER the segment is durable,
         * renamed and successfully validated.
         */
        self.wal.truncate()?;

        self.memtable.clear();

        logger::info(&format!(
            "flushed memtable to segment generation {}",
            generation
        ));

        Ok(())
    }
}

fn apply_record(memtable: &mut Memtable, record: &Record) {
    match record.op {
        Op::Set => {
            memtable.set(record.key.clone(), record.val.clone());
        }

        Op::Delete => {
            memtable.delete(record.key.clone());
        }
    }
}

fn load_segments(segments_dir: &Path) -> Result<(Vec<SegmentReader>, u64)> {
    let mut discovered: Vec<(u64, PathBuf)> = Vec::new();

    for entry in fs::read_dir(segments_dir)? {
        let entry = entry?;

        if !entry.file_type()?.is_file() {
            continue;
        }

        let path = entry.path();

        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };

        if file_name.starts_with("segment_") && file_name.ends_with(".seg") {
            let generation = parse_segment_generation(&file_name).ok_or_else(|| {
                StoneError::InvalidSegmentFile {
                    path: path.display().to_string(),

                    reason: "invalid segment filename".to_string(),
                }
            })?;

            discovered.push((generation, path));
        }
    }

    /*
     * Highest generation first.
     */
    discovered.sort_by(|a, b| b.0.cmp(&a.0));

    /*
     * Duplicate generations are unsafe.
     */
    for pair in discovered.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(StoneError::InvalidSegmentFile {
                path: segments_dir.display().to_string(),

                reason: format!("duplicate segment generation {}", pair[0].0),
            });
        }
    }

    let next_generation = match discovered.first() {
        Some((generation, _)) => generation
            .checked_add(1)
            .ok_or_else(|| StoneError::Other("segment generation exhausted".to_string()))?,

        None => 1,
    };

    let mut readers = Vec::with_capacity(discovered.len());

    for (generation, path) in discovered {
        readers.push(SegmentReader::open(&path, generation)?);
    }

    Ok((readers, next_generation))
}

fn remove_stale_temp_files(segments_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(segments_dir)? {
        let entry = entry?;

        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name();

        let Some(file_name) = file_name.to_str() else {
            continue;
        };

        if parse_temp_segment_generation(file_name).is_some() {
            let path = entry.path();

            logger::warn(&format!(
                "removing stale temporary segment '{}'",
                path.display()
            ));

            fs::remove_file(path)?;
        }
    }

    Ok(())
}

fn segment_filename(generation: u64) -> String {
    format!("segment_{:020}.seg", generation)
}

fn segment_temp_filename(generation: u64) -> String {
    format!("segment_{:020}.seg.tmp", generation)
}

fn parse_segment_generation(file_name: &str) -> Option<u64> {
    parse_generation_with_suffix(file_name, ".seg")
}

fn parse_temp_segment_generation(file_name: &str) -> Option<u64> {
    parse_generation_with_suffix(file_name, ".seg.tmp")
}

fn parse_generation_with_suffix(file_name: &str, suffix: &str) -> Option<u64> {
    let generation = file_name.strip_prefix("segment_")?.strip_suffix(suffix)?;

    if generation.is_empty() || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    generation.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!(
            "stone_store_v3_test_{}_{}_{}",
            std::process::id(),
            id,
            name
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn opens_new_store() {
        let dir = temp_dir("open");

        {
            let store = Store::open(&dir).unwrap();

            assert_eq!(store.dir(), dir.as_path());

            assert_eq!(store.segment_count(), 0);

            assert_eq!(store.memtable_entries(), 0);
        }

        assert!(dir.join("wal.log").exists());

        assert!(dir.join("segments").is_dir());

        cleanup(&dir);
    }

    #[test]
    fn set_and_get() {
        let dir = temp_dir("set_get");

        {
            let mut store = Store::open(&dir).unwrap();

            store.set(b"user:1", b"Abhishek").unwrap();

            assert_eq!(store.get(b"user:1").unwrap(), Some(b"Abhishek".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn automatic_flush_creates_segment() {
        let dir = temp_dir("auto_flush");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"key", b"value").unwrap();

            assert_eq!(store.segment_count(), 1);

            assert_eq!(store.memtable_entries(), 0);

            assert_eq!(store.get(b"key").unwrap(), Some(b"value".to_vec()));

            assert_eq!(fs::metadata(dir.join("wal.log"),).unwrap().len(), 0);
        }

        cleanup(&dir);
    }

    #[test]
    fn flushed_value_survives_reopen() {
        let dir = temp_dir("flush_reopen");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"user", b"value").unwrap();
        }

        {
            let mut store = Store::open(&dir).unwrap();

            assert_eq!(store.get(b"user").unwrap(), Some(b"value".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn newest_segment_wins() {
        let dir = temp_dir("newest");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"key", b"old").unwrap();

            store.set(b"key", b"new").unwrap();

            assert_eq!(store.segment_count(), 2);

            assert_eq!(store.get(b"key").unwrap(), Some(b"new".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn newer_tombstone_hides_old_value() {
        let dir = temp_dir("tombstone");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"key", b"value").unwrap();

            store.delete(b"key").unwrap();

            assert_eq!(store.get(b"key").unwrap(), None);
        }

        cleanup(&dir);
    }

    #[test]
    fn compaction_merges_all_segments() {
        let dir = temp_dir("compact");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"a", b"1").unwrap();

            store.set(b"b", b"2").unwrap();

            store.set(b"a", b"3").unwrap();

            assert_eq!(store.segment_count(), 3);

            let stats = store.compact().unwrap();

            assert_eq!(stats.segments_merged, 3);

            assert_eq!(stats.records_before, 3);

            assert_eq!(stats.live_records_after, 2);

            assert_eq!(store.segment_count(), 1);

            assert_eq!(store.get(b"a").unwrap(), Some(b"3".to_vec()));

            assert_eq!(store.get(b"b").unwrap(), Some(b"2".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn compacted_store_survives_reopen() {
        let dir = temp_dir("compact_reopen");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"a", b"1").unwrap();

            store.set(b"b", b"2").unwrap();

            store.set(b"a", b"updated").unwrap();

            store.compact().unwrap();

            assert_eq!(store.segment_count(), 1);
        }

        {
            let mut store = Store::open(&dir).unwrap();

            assert_eq!(store.segment_count(), 1);

            assert_eq!(store.get(b"a").unwrap(), Some(b"updated".to_vec()));

            assert_eq!(store.get(b"b").unwrap(), Some(b"2".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn compaction_removes_final_tombstone() {
        let dir = temp_dir("compact_delete");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"deleted", b"value").unwrap();

            store.delete(b"deleted").unwrap();

            assert_eq!(store.segment_count(), 2);

            let stats = store.compact().unwrap();

            assert_eq!(stats.live_records_after, 0);

            assert_eq!(store.segment_count(), 1);

            assert_eq!(store.get(b"deleted").unwrap(), None);
        }

        {
            let mut reopened = Store::open(&dir).unwrap();

            assert_eq!(reopened.get(b"deleted").unwrap(), None);
        }

        cleanup(&dir);
    }

    #[test]
    fn one_segment_compaction_is_noop() {
        let dir = temp_dir("one_segment");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"key", b"value").unwrap();

            assert_eq!(store.segment_count(), 1);

            let stats = store.compact().unwrap();

            assert_eq!(stats, CompactionStats::default());

            assert_eq!(store.segment_count(), 1);
        }

        cleanup(&dir);
    }

    #[test]
    fn compact_flushes_pending_memtable() {
        let dir = temp_dir("pending");

        {
            let mut store = Store::open_with_threshold(&dir, 1024 * 1024).unwrap();

            store.set(b"pending", b"value").unwrap();

            assert_eq!(store.segment_count(), 0);

            assert_eq!(store.memtable_entries(), 1);

            let stats = store.compact().unwrap();

            /*
             * Flushing created only one segment,
             * so full compaction itself is a no-op.
             */
            assert_eq!(stats, CompactionStats::default());

            assert_eq!(store.segment_count(), 1);

            assert_eq!(store.memtable_entries(), 0);

            assert_eq!(store.get(b"pending").unwrap(), Some(b"value".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn stats_report_unflushed_data() {
        let dir = temp_dir("stats_wal");

        {
            let mut store = Store::open(&dir).unwrap();

            store.set(b"hello", b"world").unwrap();

            let stats = store.stats();

            assert_eq!(stats.segment_count, 0);

            assert_eq!(stats.memtable_entries, 1);

            assert!(stats.memtable_size_bytes > 0);

            assert!(stats.wal_bytes > 0);

            assert_eq!(stats.total_segment_bytes, 0);
        }

        cleanup(&dir);
    }

    #[test]
    fn stats_report_flushed_segment() {
        let dir = temp_dir("stats_segment");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"hello", b"world").unwrap();

            let stats = store.stats();

            assert_eq!(stats.segment_count, 1);

            assert!(stats.total_segment_bytes > 0);

            assert_eq!(stats.wal_bytes, 0);

            assert_eq!(stats.memtable_entries, 0);

            assert_eq!(stats.memtable_size_bytes, 0);
        }

        cleanup(&dir);
    }

    #[test]
    fn compaction_reduces_dead_versions() {
        let dir = temp_dir("dead_versions");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            for i in 0..10 {
                store
                    .set(b"key", format!("value-{}", i).as_bytes())
                    .unwrap();
            }

            assert_eq!(store.segment_count(), 10);

            let before = store.stats();

            let compact = store.compact().unwrap();

            let after = store.stats();

            assert_eq!(compact.records_before, 10);

            assert_eq!(compact.live_records_after, 1);

            assert_eq!(after.segment_count, 1);

            assert!(after.total_segment_bytes < before.total_segment_bytes);

            assert_eq!(store.get(b"key").unwrap(), Some(b"value-9".to_vec()));
        }

        cleanup(&dir);
    }

    #[test]
    fn generation_continues_after_compaction() {
        let dir = temp_dir("generation");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"a", b"1").unwrap();

            store.set(b"b", b"2").unwrap();

            store.compact().unwrap();

            /*
             * Generations:
             *
             * 1 = a
             * 2 = b
             * 3 = compacted
             */
            assert!(dir
                .join("segments")
                .join("segment_00000000000000000003.seg")
                .exists());

            store.set(b"c", b"3").unwrap();

            assert!(dir
                .join("segments")
                .join("segment_00000000000000000004.seg")
                .exists());
        }

        cleanup(&dir);
    }

    #[test]
    fn stale_temp_segment_removed_on_open() {
        let dir = temp_dir("stale_tmp");

        let segments = dir.join("segments");

        fs::create_dir_all(&segments).unwrap();

        let temp = segments.join("segment_00000000000000000001.seg.tmp");

        fs::write(&temp, b"incomplete").unwrap();

        {
            let _store = Store::open(&dir).unwrap();
        }

        assert!(!temp.exists());

        cleanup(&dir);
    }

    #[test]
    fn segment_filename_parsing() {
        assert_eq!(
            parse_segment_generation("segment_00000000000000000042.seg"),
            Some(42)
        );

        assert_eq!(parse_segment_generation("segment_invalid.seg"), None);

        assert_eq!(parse_segment_generation("random.txt"), None);
    }

    #[test]
    fn temp_segment_filename_parsing() {
        assert_eq!(
            parse_temp_segment_generation("segment_00000000000000000042.seg.tmp"),
            Some(42)
        );

        assert_eq!(
            parse_temp_segment_generation("segment_invalid.seg.tmp"),
            None
        );
    }
}
