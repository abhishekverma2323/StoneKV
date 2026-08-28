use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Level::Info => write!(f, "INFO"),
            Level::Warn => write!(f, "WARN"),
            Level::Error => write!(f, "ERROR"),
        }
    }
}

pub fn log(level: Level, msg: &str) {
    eprintln!("[{}] {}", level, msg);
}

pub fn info(msg: &str) {
    log(Level::Info, msg);
}

pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

pub fn error(msg: &str) {
    log(Level::Error, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_display_info() {
        assert_eq!(Level::Info.to_string(), "INFO");
    }

    #[test]
    fn level_display_warn() {
        assert_eq!(Level::Warn.to_string(), "WARN");
    }

    #[test]
    fn level_display_error() {
        assert_eq!(Level::Error.to_string(), "ERROR");
    }

    #[test]
    fn logger_functions_are_callable() {
        info("info test");
        warn("warn test");
        error("error test");
    }
}
