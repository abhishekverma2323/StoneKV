use std::fmt;
use std::io;

#[derive(Debug)]
pub enum StoneError {
    Io(io::Error),

    TruncatedRecord { needed: usize, available: usize },

    CorruptRecord { reason: String },

    ChecksumMismatch { expected: u32, actual: u32 },

    RecordTooLarge { field: &'static str, len: usize },

    InvalidSegmentFile { path: String, reason: String },

    InvalidArgument(String),

    Other(String),
}

pub type Result<T> = std::result::Result<T, StoneError>;

impl fmt::Display for StoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoneError::Io(err) => {
                write!(f, "I/O error: {}", err)
            }

            StoneError::TruncatedRecord { needed, available } => {
                write!(
                    f,
                    "truncated record: needed {} bytes, but only {} bytes available",
                    needed, available
                )
            }

            StoneError::CorruptRecord { reason } => {
                write!(f, "corrupt record: {}", reason)
            }

            StoneError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: expected {:#010X}, got {:#010X}",
                    expected, actual
                )
            }

            StoneError::RecordTooLarge { field, len } => {
                write!(f, "record field '{}' is too large: {} bytes", field, len)
            }

            StoneError::InvalidSegmentFile { path, reason } => {
                write!(f, "invalid segment file '{}': {}", path, reason)
            }

            StoneError::InvalidArgument(message) => {
                write!(f, "invalid argument: {}", message)
            }

            StoneError::Other(message) => {
                write!(f, "{}", message)
            }
        }
    }
}

impl std::error::Error for StoneError {}

impl From<io::Error> for StoneError {
    fn from(error: io::Error) -> Self {
        StoneError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_record_error_message() {
        let error = StoneError::TruncatedRecord {
            needed: 20,
            available: 10,
        };

        assert_eq!(
            error.to_string(),
            "truncated record: needed 20 bytes, but only 10 bytes available"
        );
    }

    #[test]
    fn checksum_mismatch_error_message() {
        let error = StoneError::ChecksumMismatch {
            expected: 0x12345678,
            actual: 0x87654321,
        };

        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn io_error_converts_to_stone_error() {
        let io_error = io::Error::new(io::ErrorKind::NotFound, "file missing");

        let stone_error: StoneError = io_error.into();

        match stone_error {
            StoneError::Io(_) => {}
            _ => panic!("expected StoneError::Io"),
        }
    }
}
