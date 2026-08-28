use crate::crc32;
use crate::error::{Result, StoneError};

const OP_SET: u8 = 0;
const OP_DELETE: u8 = 1;

const U32_SIZE: usize = 4;
const CRC_SIZE: usize = 4;
const KEY_LEN_OFFSET: usize = 1;
const KEY_START_OFFSET: usize = KEY_LEN_OFFSET + U32_SIZE;

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
        let key_len = u32::try_from(self.key.len()).map_err(|_| StoneError::RecordTooLarge {
            field: "key",
            len: self.key.len(),
        })?;

        let value: &[u8] = match self.op {
            Op::Set => &self.val,
            Op::Delete => &[],
        };

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
