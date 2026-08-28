use crate::error::Result;
use crate::record::Op;
use crate::segment::{SegmentReader, SegmentWriter};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionBuildStats {
    pub segments_read: usize,
    pub records_before: usize,
    pub live_records_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

pub fn compact_all(
    input_paths_oldest_to_newest: &[PathBuf],
    output_tmp_path: &Path,
) -> Result<CompactionBuildStats> {
    let mut merged: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();

    let mut records_before = 0usize;
    let mut bytes_before = 0u64;

    /*
        Input order matters.

        Oldest segment is read first.
        Newer segments overwrite older values in the map.

        Therefore the final BTreeMap contains the latest logical
        state of every key.
    */
    for (index, path) in input_paths_oldest_to_newest.iter().enumerate() {
        bytes_before = bytes_before.saturating_add(fs::metadata(path)?.len());

        let generation = (index as u64) + 1;

        let mut reader = SegmentReader::open(path, generation)?;

        let records = reader.iter_all()?;

        records_before = records_before.saturating_add(records.len());

        for record in records {
            match record.op {
                Op::Set => {
                    merged.insert(record.key, Some(record.val));
                }

                Op::Delete => {
                    merged.insert(record.key, None);
                }
            }
        }
    }

    /*
        Because this is FULL compaction, every older segment
        participates.

        Therefore a final tombstone does not need to survive:
        there is no older segment left from which the key could
        resurrect.
    */
    merged.retain(|_, value| value.is_some());

    let live_records_after = merged.len();

    if output_tmp_path.exists() {
        fs::remove_file(output_tmp_path)?;
    }

    let mut writer = SegmentWriter::create(output_tmp_path)?;

    writer.write_all(merged.iter())?;

    let write_stats = writer.finish()?;

    Ok(CompactionBuildStats {
        segments_read: input_paths_oldest_to_newest.len(),

        records_before,

        live_records_after,

        bytes_before,

        bytes_after: write_stats.file_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::record::Record;
    use crate::segment::{SegmentReader, SegmentWriter};

    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!(
            "stone_compaction_test_{}_{}_{}",
            std::process::id(),
            id,
            name
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn write_segment(path: &Path, entries: Vec<(&[u8], Option<&[u8]>)>) {
        let mut map: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();

        for (key, value) in entries {
            map.insert(key.to_vec(), value.map(|v| v.to_vec()));
        }

        let mut writer = SegmentWriter::create(path).unwrap();

        writer.write_all(map.iter()).unwrap();

        writer.finish().unwrap();
    }

    fn read_logical_map(path: &Path) -> BTreeMap<Vec<u8>, Option<Vec<u8>>> {
        let mut reader = SegmentReader::open(path, 999).unwrap();

        let records = reader.iter_all().unwrap();

        let mut map = BTreeMap::new();

        for record in records {
            match record.op {
                Op::Set => {
                    map.insert(record.key, Some(record.val));
                }

                Op::Delete => {
                    map.insert(record.key, None);
                }
            }
        }

        map
    }

    #[test]
    fn merges_two_segments_with_newest_value_winning() {
        let dir = temp_dir("two_segments");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"1", Some(b"a")), (b"2", Some(b"b"))]);

        write_segment(&second, vec![(b"2", Some(b"c")), (b"3", Some(b"d"))]);

        let stats = compact_all(&[first, second], &output).unwrap();

        assert_eq!(stats.segments_read, 2);

        assert_eq!(stats.records_before, 4);

        assert_eq!(stats.live_records_after, 3);

        let map = read_logical_map(&output);

        assert_eq!(map.get(b"1".as_slice()), Some(&Some(b"a".to_vec())));

        assert_eq!(map.get(b"2".as_slice()), Some(&Some(b"c".to_vec())));

        assert_eq!(map.get(b"3".as_slice()), Some(&Some(b"d".to_vec())));

        cleanup(&dir);
    }

    #[test]
    fn final_tombstone_is_removed() {
        let dir = temp_dir("tombstone");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"user", Some(b"Abhishek"))]);

        write_segment(&second, vec![(b"user", None)]);

        let stats = compact_all(&[first, second], &output).unwrap();

        assert_eq!(stats.records_before, 2);

        assert_eq!(stats.live_records_after, 0);

        let map = read_logical_map(&output);

        assert!(map.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn newer_value_after_delete_survives() {
        let dir = temp_dir("delete_then_set");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let third = dir.join("segment_3.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"key", Some(b"old"))]);

        write_segment(&second, vec![(b"key", None)]);

        write_segment(&third, vec![(b"key", Some(b"new"))]);

        compact_all(&[first, second, third], &output).unwrap();

        let map = read_logical_map(&output);

        assert_eq!(map.get(b"key".as_slice()), Some(&Some(b"new".to_vec())));

        cleanup(&dir);
    }

    #[test]
    fn overwrite_chain_across_many_segments() {
        let dir = temp_dir("overwrite_chain");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let third = dir.join("segment_3.seg");

        let fourth = dir.join("segment_4.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"key", Some(b"v1"))]);

        write_segment(&second, vec![(b"key", Some(b"v2"))]);

        write_segment(&third, vec![(b"key", Some(b"v3"))]);

        write_segment(&fourth, vec![(b"key", Some(b"v4"))]);

        compact_all(&[first, second, third, fourth], &output).unwrap();

        let map = read_logical_map(&output);

        assert_eq!(map.get(b"key".as_slice()), Some(&Some(b"v4".to_vec())));

        cleanup(&dir);
    }

    #[test]
    fn compaction_preserves_multiple_latest_values() {
        let dir = temp_dir("logical_state");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let third = dir.join("segment_3.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(
            &first,
            vec![(b"a", Some(b"1")), (b"b", Some(b"2")), (b"c", Some(b"3"))],
        );

        write_segment(&second, vec![(b"b", Some(b"20")), (b"c", None)]);

        write_segment(&third, vec![(b"d", Some(b"4"))]);

        compact_all(&[first, second, third], &output).unwrap();

        let map = read_logical_map(&output);

        assert_eq!(map.len(), 3);

        assert_eq!(map.get(b"a".as_slice()), Some(&Some(b"1".to_vec())));

        assert_eq!(map.get(b"b".as_slice()), Some(&Some(b"20".to_vec())));

        assert_eq!(map.get(b"c".as_slice()), None);

        assert_eq!(map.get(b"d".as_slice()), Some(&Some(b"4".to_vec())));

        cleanup(&dir);
    }

    #[test]
    fn empty_input_creates_valid_empty_segment() {
        let dir = temp_dir("empty");

        fs::create_dir_all(&dir).unwrap();

        let output = dir.join("output.seg.tmp");

        let stats = compact_all(&[], &output).unwrap();

        assert_eq!(stats.segments_read, 0);

        assert_eq!(stats.records_before, 0);

        assert_eq!(stats.live_records_after, 0);

        let mut reader = SegmentReader::open(&output, 1).unwrap();

        assert!(reader.iter_all().unwrap().is_empty());

        cleanup(&dir);
    }

    #[test]
    fn bytes_before_and_after_are_reported() {
        let dir = temp_dir("stats");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"key", Some(b"very-old-value"))]);

        write_segment(&second, vec![(b"key", Some(b"new"))]);

        let expected_before =
            fs::metadata(&first).unwrap().len() + fs::metadata(&second).unwrap().len();

        let stats = compact_all(&[first, second], &output).unwrap();

        assert_eq!(stats.bytes_before, expected_before);

        assert_eq!(stats.bytes_after, fs::metadata(&output).unwrap().len());

        assert!(stats.bytes_after > 0);

        cleanup(&dir);
    }

    #[test]
    fn latest_tombstone_does_not_resurrect_old_value() {
        let dir = temp_dir("no_resurrection");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let third = dir.join("segment_3.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"account", Some(b"first"))]);

        write_segment(&second, vec![(b"account", Some(b"second"))]);

        write_segment(&third, vec![(b"account", None)]);

        compact_all(&[first, second, third], &output).unwrap();

        let map = read_logical_map(&output);

        assert!(!map.contains_key(b"account".as_slice()));

        cleanup(&dir);
    }

    #[test]
    fn output_contains_only_set_records() {
        let dir = temp_dir("sets_only");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"a", Some(b"1")), (b"b", Some(b"2"))]);

        write_segment(&second, vec![(b"a", None), (b"c", Some(b"3"))]);

        compact_all(&[first, second], &output).unwrap();

        let mut reader = SegmentReader::open(&output, 1).unwrap();

        let records = reader.iter_all().unwrap();

        for record in records {
            assert_eq!(record.op, Op::Set);
        }

        cleanup(&dir);
    }

    #[test]
    fn compacted_segment_records_are_sorted() {
        let dir = temp_dir("sorted");

        fs::create_dir_all(&dir).unwrap();

        let first = dir.join("segment_1.seg");

        let second = dir.join("segment_2.seg");

        let output = dir.join("output.seg.tmp");

        write_segment(&first, vec![(b"a", Some(b"1")), (b"z", Some(b"26"))]);

        write_segment(&second, vec![(b"m", Some(b"13"))]);

        compact_all(&[first, second], &output).unwrap();

        let mut reader = SegmentReader::open(&output, 1).unwrap();

        let records = reader.iter_all().unwrap();

        let keys: Vec<Vec<u8>> = records
            .into_iter()
            .map(|record: Record| record.key)
            .collect();

        assert_eq!(keys, vec![b"a".to_vec(), b"m".to_vec(), b"z".to_vec(),]);

        cleanup(&dir);
    }
}
