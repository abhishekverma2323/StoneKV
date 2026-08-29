use crate::compaction::compact_all;
use crate::error::{Result, StoneError};
use crate::logger;
use crate::memtable::Memtable;
use crate::record::{Op, Record};
use crate::segment::{SegmentReader, SegmentWriter};
use crate::wal::Wal;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_FLUSH_THRESHOLD_BYTES: usize = 4 * 1024 * 1024;

const COMPACTION_MARKER_FILE: &str = "compaction.pending";
const COMPACTION_MARKER_TEMP_FILE: &str = "compaction.pending.tmp";
const COMPACTION_MARKER_MAGIC: &str = "STONE_COMPACTION_V1";

#[derive(Debug)]
struct CompactionMarker {
    output_generation: u64,
    old_generations: Vec<u64>,
}

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

        /*
         * Recover a compaction transaction before loading
         * any segment into the normal store view.
         */
        recover_pending_compaction(&segments_dir)?;

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
         * Full compaction includes current unflushed state.
         */
        if !self.memtable.is_empty() {
            self.flush_memtable()?;
        }

        /*
         * No useful merge when fewer than two segments exist.
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
         * self.segments:
         * newest -> oldest
         *
         * compact_all expects:
         * oldest -> newest
         */
        let input_paths: Vec<PathBuf> = self
            .segments
            .iter()
            .rev()
            .map(|segment| segment.path().to_path_buf())
            .collect();

        /*
         * Record which exact generations this compaction
         * transaction is replacing.
         */
        let old_generations: Vec<u64> = self
            .segments
            .iter()
            .map(|segment| segment.generation())
            .collect();

        /*
         * Build + sync the compacted temporary segment.
         */
        let build_stats = compact_all(&input_paths, &temp_path)?;

        /*
         * Write durable transaction marker BEFORE making the
         * compacted final segment visible.
         */
        if let Err(error) = write_compaction_marker(&segments_dir, generation, &old_generations) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        /*
         * Install new compacted segment.
         */
        if let Err(error) = fs::rename(&temp_path, &final_path) {
            /*
             * Old segments are still untouched, so rollback
             * is safe.
             */
            let _ = fs::remove_file(segments_dir.join(COMPACTION_MARKER_FILE));
            let _ = fs::remove_file(&temp_path);

            return Err(error.into());
        }

        /*
         * Validate replacement BEFORE touching old segments.
         */
        let new_reader = match SegmentReader::open(&final_path, generation) {
            Ok(reader) => reader,

            Err(error) => {
                logger::error(&format!(
                    "failed to validate compacted segment '{}': {}",
                    final_path.display(),
                    error
                ));

                /*
                 * Old segments still exist.
                 */
                let _ = fs::remove_file(&final_path);

                let _ = fs::remove_file(segments_dir.join(COMPACTION_MARKER_FILE));

                return Err(error);
            }
        };

        self.next_segment_generation = generation
            .checked_add(1)
            .ok_or_else(|| StoneError::Other("segment generation exhausted".to_string()))?;

        let old_paths: Vec<PathBuf> = self
            .segments
            .iter()
            .map(|segment| segment.path().to_path_buf())
            .collect();

        /*
         * New compacted segment becomes the in-memory
         * authoritative segment.
         */
        let old_segments = std::mem::replace(&mut self.segments, vec![new_reader]);

        /*
         * Important on Windows:
         * close old File handles before deletion.
         */
        drop(old_segments);

        /*
         * Marker stays present during old-file deletion.
         *
         * If Stone crashes here, startup sees:
         *
         *   compaction.pending
         *   new compacted segment
         *   some/all old segments
         *
         * and completes cleanup before normal loading.
         */
        for path in old_paths {
            fs::remove_file(path)?;
        }

        /*
         * Transaction becomes complete only after every
         * replaced old segment has been removed.
         */
        fs::remove_file(segments_dir.join(COMPACTION_MARKER_FILE))?;

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

            /*
             * Segments must always be newest -> oldest.
             */
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

/*
 * -------------------------------------------------------------------------
 * COMPACTION TRANSACTION MARKER
 * -------------------------------------------------------------------------
 */

fn write_compaction_marker(
    segments_dir: &Path,
    output_generation: u64,
    old_generations: &[u64],
) -> Result<()> {
    let marker_path = segments_dir.join(COMPACTION_MARKER_FILE);

    let temp_marker_path = segments_dir.join(COMPACTION_MARKER_TEMP_FILE);

    if marker_path.exists() {
        return Err(StoneError::Other(format!(
            "compaction marker already exists: {}",
            marker_path.display()
        )));
    }

    if temp_marker_path.exists() {
        fs::remove_file(&temp_marker_path)?;
    }

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_marker_path)?;

        writeln!(file, "{}", COMPACTION_MARKER_MAGIC)?;

        writeln!(file, "output={}", output_generation)?;

        for generation in old_generations {
            writeln!(file, "old={}", generation)?;
        }

        file.flush()?;

        file.sync_all()?;

        drop(file);

        fs::rename(&temp_marker_path, &marker_path)?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_marker_path);
    }

    result
}

fn read_compaction_marker(marker_path: &Path) -> Result<CompactionMarker> {
    let contents = fs::read_to_string(marker_path)?;

    let mut lines = contents.lines();

    if lines.next() != Some(COMPACTION_MARKER_MAGIC) {
        return Err(StoneError::Other(format!(
            "invalid compaction marker header: {}",
            marker_path.display()
        )));
    }

    let mut output_generation = None;

    let mut old_generations = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }

        if let Some(value) = line.strip_prefix("output=") {
            if output_generation.is_some() {
                return Err(StoneError::Other(
                    "duplicate output generation in compaction marker".to_string(),
                ));
            }

            let generation = value.parse::<u64>().map_err(|_| {
                StoneError::Other(format!(
                    "invalid output generation in compaction marker: {}",
                    value
                ))
            })?;

            output_generation = Some(generation);

            continue;
        }

        if let Some(value) = line.strip_prefix("old=") {
            let generation = value.parse::<u64>().map_err(|_| {
                StoneError::Other(format!(
                    "invalid old generation in compaction marker: {}",
                    value
                ))
            })?;

            if old_generations.contains(&generation) {
                return Err(StoneError::Other(format!(
                    "duplicate old generation {} in compaction marker",
                    generation
                )));
            }

            old_generations.push(generation);

            continue;
        }

        return Err(StoneError::Other(format!(
            "unknown line in compaction marker: {}",
            line
        )));
    }

    let output_generation = output_generation.ok_or_else(|| {
        StoneError::Other("compaction marker missing output generation".to_string())
    })?;

    if old_generations.len() < 2 {
        return Err(StoneError::Other(
            "compaction marker must contain at least two old segments".to_string(),
        ));
    }

    if old_generations.contains(&output_generation) {
        return Err(StoneError::Other(
            "compaction output generation is also listed as an old generation".to_string(),
        ));
    }

    if old_generations
        .iter()
        .any(|generation| *generation >= output_generation)
    {
        return Err(StoneError::Other(
            "compaction output generation must be newer than every replaced generation".to_string(),
        ));
    }

    Ok(CompactionMarker {
        output_generation,
        old_generations,
    })
}

fn recover_pending_compaction(segments_dir: &Path) -> Result<()> {
    let marker_path = segments_dir.join(COMPACTION_MARKER_FILE);

    let marker_temp_path = segments_dir.join(COMPACTION_MARKER_TEMP_FILE);

    /*
     * A temporary marker without the real marker means Stone
     * crashed before the compaction transaction became active.
     */
    if !marker_path.exists() {
        if marker_temp_path.exists() {
            logger::warn(&format!(
                "removing incomplete compaction marker '{}'",
                marker_temp_path.display()
            ));

            fs::remove_file(marker_temp_path)?;
        }

        return Ok(());
    }

    /*
     * Real marker is authoritative.
     */
    if marker_temp_path.exists() {
        fs::remove_file(&marker_temp_path)?;
    }

    let marker = read_compaction_marker(&marker_path)?;

    let final_path = segments_dir.join(segment_filename(marker.output_generation));

    let temp_path = segments_dir.join(segment_temp_filename(marker.output_generation));

    /*
     * Marker exists but the final compacted segment does not.
     *
     * Stone crashed before the new segment became visible.
     * Old segments must therefore remain authoritative.
     */
    if !final_path.exists() {
        for generation in &marker.old_generations {
            let old_path = segments_dir.join(segment_filename(*generation));

            if !old_path.exists() {
                return Err(StoneError::Other(format!(
                    "cannot roll back interrupted compaction: \
                     old segment {} is missing",
                    generation
                )));
            }
        }

        if temp_path.exists() {
            fs::remove_file(&temp_path)?;
        }

        fs::remove_file(&marker_path)?;

        logger::warn("rolled back interrupted compaction before final segment installation");

        return Ok(());
    }

    /*
     * Final compacted segment exists.
     *
     * Validate it before deleting any remaining old segment.
     */
    {
        let reader = SegmentReader::open(&final_path, marker.output_generation)?;

        drop(reader);
    }

    /*
     * Valid compacted segment is authoritative.
     *
     * Finish removal of all old generations before normal
     * Store loading is allowed.
     */
    for generation in &marker.old_generations {
        let old_path = segments_dir.join(segment_filename(*generation));

        if old_path.exists() {
            fs::remove_file(old_path)?;
        }
    }

    if temp_path.exists() {
        fs::remove_file(temp_path)?;
    }

    fs::remove_file(marker_path)?;

    logger::warn("completed recovery of interrupted compaction");

    Ok(())
}

/*
 * -------------------------------------------------------------------------
 * STORE HELPERS
 * -------------------------------------------------------------------------
 */

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
            "stone_store_v4_test_{}_{}_{}",
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

            assert_eq!(fs::metadata(dir.join("wal.log")).unwrap().len(), 0);
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

    /*
     * ---------------------------------------------------------------------
     * NEW COMPACTION CRASH-SAFETY TEST
     * ---------------------------------------------------------------------
     */

    #[test]
    fn interrupted_compaction_does_not_resurrect_deleted_key() {
        let dir = temp_dir("compaction_crash_delete");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            /*
             * generation 1:
             *
             * key = value
             */
            store.set(b"key", b"value").unwrap();

            /*
             * generation 2:
             *
             * key = tombstone
             */
            store.delete(b"key").unwrap();

            assert_eq!(store.segment_count(), 2);

            assert_eq!(store.get(b"key").unwrap(), None);

            let generation = store.next_segment_generation;

            let segments_dir = dir.join("segments");

            let temp_path = segments_dir.join(segment_temp_filename(generation));

            let final_path = segments_dir.join(segment_filename(generation));

            let input_paths: Vec<PathBuf> = store
                .segments
                .iter()
                .rev()
                .map(|segment| segment.path().to_path_buf())
                .collect();

            let old_generations: Vec<u64> = store
                .segments
                .iter()
                .map(|segment| segment.generation())
                .collect();

            /*
             * Build compacted segment.
             *
             * Final logical state is DELETE, therefore
             * compacted segment contains no live key.
             */
            compact_all(&input_paths, &temp_path).unwrap();

            /*
             * Start compaction transaction.
             */
            write_compaction_marker(&segments_dir, generation, &old_generations).unwrap();

            /*
             * Simulated crash point:
             *
             * New segment is now visible, but old
             * segments have NOT been deleted yet.
             */
            fs::rename(&temp_path, &final_path).unwrap();

            /*
             * Store drops here, simulating process death.
             */
        }

        /*
         * Startup must finish interrupted compaction
         * BEFORE normal segment loading.
         */
        {
            let mut reopened = Store::open(&dir).unwrap();

            assert_eq!(reopened.segment_count(), 1);

            /*
             * Critical assertion:
             *
             * Deleted value must never resurrect from one
             * of the old segments.
             */
            assert_eq!(reopened.get(b"key").unwrap(), None);

            assert!(!dir.join("segments").join(COMPACTION_MARKER_FILE).exists());
        }

        cleanup(&dir);
    }

    #[test]
    fn interrupted_compaction_before_install_rolls_back() {
        let dir = temp_dir("compaction_crash_before_install");

        {
            let mut store = Store::open_with_threshold(&dir, 1).unwrap();

            store.set(b"a", b"1").unwrap();

            store.set(b"b", b"2").unwrap();

            let generation = store.next_segment_generation;

            let segments_dir = dir.join("segments");

            let temp_path = segments_dir.join(segment_temp_filename(generation));

            let input_paths: Vec<PathBuf> = store
                .segments
                .iter()
                .rev()
                .map(|segment| segment.path().to_path_buf())
                .collect();

            let old_generations: Vec<u64> = store
                .segments
                .iter()
                .map(|segment| segment.generation())
                .collect();

            compact_all(&input_paths, &temp_path).unwrap();

            write_compaction_marker(&segments_dir, generation, &old_generations).unwrap();

            /*
             * Simulated crash BEFORE:
             *
             * temp -> final rename
             */
        }

        {
            let mut reopened = Store::open(&dir).unwrap();

            /*
             * Recovery must roll back and retain both
             * original segments.
             */
            assert_eq!(reopened.segment_count(), 2);

            assert_eq!(reopened.get(b"a").unwrap(), Some(b"1".to_vec()));

            assert_eq!(reopened.get(b"b").unwrap(), Some(b"2".to_vec()));

            assert!(!dir.join("segments").join(COMPACTION_MARKER_FILE).exists());
        }

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
