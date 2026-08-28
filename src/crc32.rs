use std::sync::LazyLock;

const POLY: u32 = 0xEDB88320;

static TABLE: LazyLock<[u32; 256]> = LazyLock::new(build_table);

fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];

    for i in 0..256 {
        let mut crc = i as u32;

        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ POLY;
            } else {
                crc >>= 1;
            }
        }

        table[i] = crc;
    }

    table
}

pub fn checksum(data: &[u8]) -> u32 {
    let mut crc = 0xFFFFFFFFu32;

    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;

        crc = (crc >> 8) ^ TABLE[index];
    }

    crc ^ 0xFFFFFFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_standard_test_vector() {
        assert_eq!(checksum(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn crc32_empty_input() {
        assert_eq!(checksum(b""), 0);
    }

    #[test]
    fn crc32_different_inputs_produce_different_checksums() {
        let first = checksum(b"stone");
        let second = checksum(b"Stone");

        assert_ne!(first, second);
    }

    #[test]
    fn crc32_same_input_is_deterministic() {
        let first = checksum(b"hello world");
        let second = checksum(b"hello world");

        assert_eq!(first, second);
    }
}
