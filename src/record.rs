use crate::crc32;
use crate::error::{Result, StoneError};

const OP_SET: u8 = 0;
const OP_DELETE: u8 = 1;

const U32_SIZE: usize = 4;
const CRC_SIZE: usize = 4;
const KEY_LEN_OFFSET: usize = 1;
const KEY_START_OFFSET: usize = KEY_LEN_OFFSET + U32_SIZE;

/// Sanity ceiling on a single key/value field length, independent of how
/// many bytes are actually available in the buffer being decoded.
///
/// This exists to catch one specific, narrow failure mode: a corrupted
/// `key_len`/`val_len` field whose value is implausibly large (e.g. a
/// flipped high bit turning it into billions of bytes). Without this
/// bound, such a field would make `decode()` report `TruncatedRecord`
/// purely because the declared length exceeds the bytes remaining in the
/// file — and `Wal::replay` treats `TruncatedRecord` as a forgivable
/// crash tail, silently discarding it.
///
/// This bound is enforced symmetrically in both `encode()` and
/// `decode()`, so Stone can never write a record that it would later
/// refuse to read back as valid.
///
/// IMPORTANT — what this does NOT solve: a length field corrupted to a
/// *moderate* value (e.g. `key_len` flipped from 3 to 1000, still well
/// under this ceiling) that happens to exceed the bytes actually
/// remaining in the file is indistinguishable from a genuine crash tail
/// under this record format. The CRC lives at the end of the record, so
/// there is no way to verify a record's integrity until all of its
/// (possibly corrupted) declared length has already been consumed.
/// Resolving that fully would require additional framing/integrity
/// metadata — an on-disk format change — which is out of scope here.
/// See `record::tests::moderate_length_corruption_is_still_indistinguishable_from_truncation`
/// for a test that documents this honestly rather than claiming it's
/// fixed.
const MAX_FIELD_LEN: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Set,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub op: Op,
    pub key: Vec<u8>,
    pub val: Vec<u8>,
}

impl Record {
    pub fn new_set(key: Vec<u8>, val: Vec<u8>) -> Self {
        Self {
            op: Op::Set,
            key,
            val,
        }
    }

    pub fn new_delete(key: Vec<u8>) -> Self {
        Self {
            op: Op::Delete,
            key,
            val: Vec::new(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.key.len() > MAX_FIELD_LEN {
            return Err(StoneError::RecordTooLarge {
                field: "key",
                len: self.key.len(),
            });
        }

        let key_len = u32::try_from(self.key.len()).map_err(|_| StoneError::RecordTooLarge {
            field: "key",
            len: self.key.len(),
        })?;

        let value: &[u8] = match self.op {
            Op::Set => &self.val,
            Op::Delete => &[],
        };

        if value.len() > MAX_FIELD_LEN {
            return Err(StoneError::RecordTooLarge {
                field: "value",
                len: value.len(),
            });
        }

        let val_len = u32::try_from(value.len()).map_err(|_| StoneError::RecordTooLarge {
            field: "value",
            len: value.len(),
        })?;

        let op_byte = match self.op {
            Op::Set => OP_SET,
            Op::Delete => OP_DELETE,
        };

        let capacity = 1usize
            .checked_add(U32_SIZE)
            .and_then(|n| n.checked_add(self.key.len()))
            .and_then(|n| n.checked_add(U32_SIZE))
            .and_then(|n| n.checked_add(value.len()))
            .and_then(|n| n.checked_add(CRC_SIZE))
            .ok_or_else(|| StoneError::Other("record size overflow".to_string()))?;

        let mut bytes = Vec::with_capacity(capacity);

        bytes.push(op_byte);
        bytes.extend_from_slice(&key_len.to_le_bytes());
        bytes.extend_from_slice(&self.key);
        bytes.extend_from_slice(&val_len.to_le_bytes());
        bytes.extend_from_slice(value);

        let checksum = crc32::checksum(&bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());

        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<(Record, usize)> {
        if bytes.is_empty() {
            return Err(StoneError::TruncatedRecord {
                needed: 1,
                available: 0,
            });
        }

        let op = match bytes[0] {
            OP_SET => Op::Set,
            OP_DELETE => Op::Delete,
            other => {
                return Err(StoneError::CorruptRecord {
                    reason: format!("unknown operation byte: {}", other),
                });
            }
        };

        if bytes.len() < KEY_START_OFFSET {
            return Err(StoneError::TruncatedRecord {
                needed: KEY_START_OFFSET,
                available: bytes.len(),
            });
        }

        let key_len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;

        if key_len > MAX_FIELD_LEN {
            return Err(StoneError::CorruptRecord {
                reason: format!(
                    "declared key length {} exceeds sanity bound {} — treating as corruption, not a crash tail",
                    key_len, MAX_FIELD_LEN
                ),
            });
        }

        let key_end =
            KEY_START_OFFSET
                .checked_add(key_len)
                .ok_or_else(|| StoneError::CorruptRecord {
                    reason: "key length overflow".to_string(),
                })?;

        let val_len_end =
            key_end
                .checked_add(U32_SIZE)
                .ok_or_else(|| StoneError::CorruptRecord {
                    reason: "value length offset overflow".to_string(),
                })?;

        if bytes.len() < val_len_end {
            return Err(StoneError::TruncatedRecord {
                needed: val_len_end,
                available: bytes.len(),
            });
        }

        let val_len = u32::from_le_bytes([
            bytes[key_end],
            bytes[key_end + 1],
            bytes[key_end + 2],
            bytes[key_end + 3],
        ]) as usize;

        if val_len > MAX_FIELD_LEN {
            return Err(StoneError::CorruptRecord {
                reason: format!(
                    "declared value length {} exceeds sanity bound {} — treating as corruption, not a crash tail",
                    val_len, MAX_FIELD_LEN
                ),
            });
        }

        let val_start = val_len_end;

        let val_end = val_start
            .checked_add(val_len)
            .ok_or_else(|| StoneError::CorruptRecord {
                reason: "value length overflow".to_string(),
            })?;

        let record_end =
            val_end
                .checked_add(CRC_SIZE)
                .ok_or_else(|| StoneError::CorruptRecord {
                    reason: "record length overflow".to_string(),
                })?;

        if bytes.len() < record_end {
            return Err(StoneError::TruncatedRecord {
                needed: record_end,
                available: bytes.len(),
            });
        }

        if matches!(op, Op::Delete) && val_len != 0 {
            return Err(StoneError::CorruptRecord {
                reason: "DELETE record must have zero-length value".to_string(),
            });
        }

        let expected_crc = u32::from_le_bytes([
            bytes[val_end],
            bytes[val_end + 1],
            bytes[val_end + 2],
            bytes[val_end + 3],
        ]);

        let actual_crc = crc32::checksum(&bytes[..val_end]);

        if expected_crc != actual_crc {
            return Err(StoneError::ChecksumMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        let record = Record {
            op,
            key: bytes[KEY_START_OFFSET..key_end].to_vec(),
            val: bytes[val_start..val_end].to_vec(),
        };

        Ok((record, record_end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_record_roundtrip() {
        let original = Record::new_set(b"user:1".to_vec(), b"Abhishek".to_vec());

        let encoded = original.encode().unwrap();
        let (decoded, consumed) = Record::decode(&encoded).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn delete_record_roundtrip() {
        let original = Record::new_delete(b"user:1".to_vec());

        let encoded = original.encode().unwrap();
        let (decoded, consumed) = Record::decode(&encoded).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn empty_key_roundtrip() {
        let original = Record::new_set(Vec::new(), b"value".to_vec());

        let encoded = original.encode().unwrap();
        let (decoded, _) = Record::decode(&encoded).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn empty_value_roundtrip() {
        let original = Record::new_set(b"empty".to_vec(), Vec::new());

        let encoded = original.encode().unwrap();
        let (decoded, _) = Record::decode(&encoded).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn truncation_at_multiple_boundaries() {
        let record = Record::new_set(b"hello".to_vec(), b"world".to_vec());

        let encoded = record.encode().unwrap();

        let truncation_points = [0, 1, 3, 5, 7, encoded.len() - 1];

        for end in truncation_points {
            let result = Record::decode(&encoded[..end]);

            assert!(
                matches!(result, Err(StoneError::TruncatedRecord { .. })),
                "expected TruncatedRecord at byte {}, got {:?}",
                end,
                result
            );
        }
    }

    #[test]
    fn flipped_payload_bit_causes_checksum_failure() {
        let record = Record::new_set(b"user".to_vec(), b"Abhishek".to_vec());

        let mut encoded = record.encode().unwrap();

        let payload_position = 1 + 4 + record.key.len() + 4;

        encoded[payload_position] ^= 0x01;

        let result = Record::decode(&encoded);

        assert!(matches!(result, Err(StoneError::ChecksumMismatch { .. })));
    }

    #[test]
    fn unknown_operation_is_rejected() {
        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());

        let mut encoded = record.encode().unwrap();

        encoded[0] = 99;

        let result = Record::decode(&encoded);

        assert!(matches!(result, Err(StoneError::CorruptRecord { .. })));
    }

    #[test]
    fn corrupted_key_len_field_is_rejected_as_corrupt_not_truncated() {
        // Simulates a bit flip in the key_len field of an otherwise
        // complete, well-formed record. Before the MAX_FIELD_LEN sanity
        // bound, this was misclassified as TruncatedRecord (because the
        // bogus declared length exceeds the bytes actually available),
        // which caused Wal::replay to silently discard it as a forgivable
        // crash tail. It must now be reported as CorruptRecord instead.
        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());
        let mut encoded = record.encode().unwrap();

        // key_len occupies bytes[1..5]; smash it to an implausibly large
        // value while leaving every other byte (including the trailing
        // CRC) untouched.
        encoded[1] = 0xFF;
        encoded[2] = 0xFF;
        encoded[3] = 0xFF;
        encoded[4] = 0x7F;

        let result = Record::decode(&encoded);

        assert!(
            matches!(result, Err(StoneError::CorruptRecord { .. })),
            "expected CorruptRecord for a corrupted key_len field, got {:?}",
            result
        );
    }

    #[test]
    fn corrupted_val_len_field_is_rejected_as_corrupt_not_truncated() {
        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());
        let mut encoded = record.encode().unwrap();

        // val_len occupies the 4 bytes immediately after the key.
        let val_len_offset = 1 + 4 + record.key.len();

        encoded[val_len_offset] = 0xFF;
        encoded[val_len_offset + 1] = 0xFF;
        encoded[val_len_offset + 2] = 0xFF;
        encoded[val_len_offset + 3] = 0x7F;

        let result = Record::decode(&encoded);

        assert!(
            matches!(result, Err(StoneError::CorruptRecord { .. })),
            "expected CorruptRecord for a corrupted val_len field, got {:?}",
            result
        );
    }

    #[test]
    fn corrupted_key_len_in_a_record_followed_by_more_data_is_still_rejected() {
        // This specifically exercises an implausibly large corrupted
        // length (well past MAX_FIELD_LEN), which is what makes it
        // reliably classifiable as corruption regardless of what follows
        // it in the buffer. This is NOT a general proof that all
        // non-final-record corruption is detectable — a moderate
        // corrupted length that stays under MAX_FIELD_LEN is not caught
        // by this mechanism; see
        // moderate_length_corruption_is_still_indistinguishable_from_truncation.
        let victim = Record::new_set(b"key".to_vec(), b"value".to_vec());
        let mut encoded = victim.encode().unwrap();

        encoded[1] = 0xFF;
        encoded[2] = 0xFF;
        encoded[3] = 0xFF;
        encoded[4] = 0x7F;

        let following = Record::new_set(b"next".to_vec(), b"record".to_vec());
        let following_bytes = following.encode().unwrap();

        let mut combined = encoded;
        combined.extend_from_slice(&following_bytes);

        let result = Record::decode(&combined);

        assert!(
            matches!(result, Err(StoneError::CorruptRecord { .. })),
            "expected CorruptRecord even though valid data follows, got {:?}",
            result
        );
    }

    #[test]
    fn key_at_max_field_len_boundary_is_accepted() {
        let key = vec![b'k'; MAX_FIELD_LEN];
        let record = Record::new_set(key.clone(), b"v".to_vec());

        let encoded = record
            .encode()
            .expect("key at exactly MAX_FIELD_LEN must encode");

        let (decoded, _) = Record::decode(&encoded).expect("must decode back");

        assert_eq!(decoded.key, key);
    }

    #[test]
    fn key_over_max_field_len_boundary_is_rejected_at_encode_time() {
        let key = vec![b'k'; MAX_FIELD_LEN + 1];
        let record = Record::new_set(key, b"v".to_vec());

        let result = record.encode();

        assert!(
            matches!(result, Err(StoneError::RecordTooLarge { field: "key", .. })),
            "expected RecordTooLarge for key one byte over the boundary, got {:?}",
            result
        );
    }

    #[test]
    fn value_over_max_field_len_boundary_is_rejected_at_encode_time() {
        let value = vec![b'v'; MAX_FIELD_LEN + 1];
        let record = Record::new_set(b"k".to_vec(), value);

        let result = record.encode();

        assert!(
            matches!(
                result,
                Err(StoneError::RecordTooLarge { field: "value", .. })
            ),
            "expected RecordTooLarge for value one byte over the boundary, got {:?}",
            result
        );
    }

    #[test]
    fn moderate_length_corruption_is_still_indistinguishable_from_truncation() {
        // This test documents a real, UNRESOLVED limitation rather than
        // proving a fix. It must keep passing exactly as written — if it
        // ever starts failing, that means decode() has changed in a way
        // that silently narrowed (or widened) this known gap, and the
        // comments on MAX_FIELD_LEN need to be revisited alongside it.
        //
        // MAX_FIELD_LEN only catches implausibly large corrupted lengths
        // (see corrupted_key_len_field_is_rejected_as_corrupt_not_truncated).
        // A length field corrupted to a moderate, still-plausible value
        // that merely exceeds the bytes actually available is
        // indistinguishable from a genuine interrupted-write crash tail
        // under this record format, because the CRC that would catch the
        // mismatch lives at the end of the record — past the point where
        // decode() has already given up and reported TruncatedRecord.
        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());
        let mut encoded = record.encode().unwrap();

        // Original key_len is 3 (b"key"). Corrupt it to a moderate,
        // plausible-looking value that is still far under MAX_FIELD_LEN,
        // but larger than the bytes actually remaining in this buffer.
        let corrupted_key_len: u32 = 1000;
        let corrupted_bytes = corrupted_key_len.to_le_bytes();
        encoded[1] = corrupted_bytes[0];
        encoded[2] = corrupted_bytes[1];
        encoded[3] = corrupted_bytes[2];
        encoded[4] = corrupted_bytes[3];

        let result = Record::decode(&encoded);

        // This is the honest, current behavior — NOT the desired
        // long-term behavior. It is reported as TruncatedRecord, meaning
        // Wal::replay would (incorrectly, in the case of true corruption)
        // treat this as a forgivable crash tail and discard it.
        assert!(
            matches!(result, Err(StoneError::TruncatedRecord { .. })),
            "expected TruncatedRecord (documenting the known limitation), got {:?}",
            result
        );
    }

    #[test]
    fn appended_bytes_do_not_change_bytes_consumed() {
        let record = Record::new_set(b"key".to_vec(), b"value".to_vec());

        let encoded = record.encode().unwrap();
        let expected_consumed = encoded.len();

        let mut combined = encoded;
        combined.extend_from_slice(b"extra bytes");

        let (decoded, consumed) = Record::decode(&combined).unwrap();

        assert_eq!(decoded, record);
        assert_eq!(consumed, expected_consumed);
    }
}
