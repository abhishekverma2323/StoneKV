use stone::{StoneError, Store};

use std::path::PathBuf;
use std::process;

const DEFAULT_DATA_DIR: &str = "./stone-data";

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {}", error);
        process::exit(1);
    }
}

fn run() -> Result<(), StoneError> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_usage_stderr();
        process::exit(1);
    }

    let command = args[0].as_str();

    match command {
        "set" => command_set(&args[1..]),

        "get" => command_get(&args[1..]),

        "del" | "delete" => command_delete(&args[1..]),

        "compact" => command_compact(&args[1..]),

        "stats" => command_stats(&args[1..]),

        "verify" => command_verify(&args[1..]),

        "help" | "--help" | "-h" => {
            print_usage_stdout();
            Ok(())
        }

        _ => Err(StoneError::InvalidArgument(format!(
            "unknown command '{}'",
            command
        ))),
    }
}

fn command_set(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if positionals.len() != 2 {
        return Err(StoneError::InvalidArgument(
            "usage: stone set <key> <value> [--dir PATH]".to_string(),
        ));
    }

    let key = positionals[0].as_bytes();

    let value = positionals[1].as_bytes();

    let mut store = Store::open(&dir)?;

    store.set(key, value)?;

    println!("OK");

    Ok(())
}

fn command_get(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if positionals.len() != 1 {
        return Err(StoneError::InvalidArgument(
            "usage: stone get <key> [--dir PATH]".to_string(),
        ));
    }

    let key = positionals[0].as_bytes();

    let mut store = Store::open(&dir)?;

    match store.get(key)? {
        Some(value) => {
            println!("{}", String::from_utf8_lossy(&value));

            Ok(())
        }

        None => {
            eprintln!("not found");
            process::exit(1);
        }
    }
}

fn command_delete(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if positionals.len() != 1 {
        return Err(StoneError::InvalidArgument(
            "usage: stone del <key> [--dir PATH]".to_string(),
        ));
    }

    let key = positionals[0].as_bytes();

    let mut store = Store::open(&dir)?;

    store.delete(key)?;

    println!("OK");

    Ok(())
}

fn command_compact(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if !positionals.is_empty() {
        return Err(StoneError::InvalidArgument(
            "usage: stone compact [--dir PATH]".to_string(),
        ));
    }

    let mut store = Store::open(&dir)?;

    let stats = store.compact()?;

    println!("segments_merged: {}", stats.segments_merged);

    println!("records_before: {}", stats.records_before);

    println!("live_records_after: {}", stats.live_records_after);

    println!("bytes_before: {}", stats.bytes_before);

    println!("bytes_after: {}", stats.bytes_after);

    Ok(())
}

fn command_stats(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if !positionals.is_empty() {
        return Err(StoneError::InvalidArgument(
            "usage: stone stats [--dir PATH]".to_string(),
        ));
    }

    let store = Store::open(&dir)?;

    let stats = store.stats();

    println!("segments: {}", stats.segment_count);

    println!("segment_bytes: {}", stats.total_segment_bytes);

    println!("wal_bytes: {}", stats.wal_bytes);

    println!("memtable_entries: {}", stats.memtable_entries);

    println!("memtable_bytes: {}", stats.memtable_size_bytes);

    Ok(())
}

fn command_verify(args: &[String]) -> Result<(), StoneError> {
    let (positionals, dir) = parse_args_with_dir(args)?;

    if !positionals.is_empty() {
        return Err(StoneError::InvalidArgument(
            "usage: stone verify [--dir PATH]".to_string(),
        ));
    }

    let mut store = Store::open(&dir)?;

    let stats = store.verify()?;

    println!("OK");

    println!("wal_records: {}", stats.wal_records);

    println!("segments_checked: {}", stats.segments_checked);

    println!("records_checked: {}", stats.records_checked);

    Ok(())
}

fn parse_args_with_dir(args: &[String]) -> Result<(Vec<String>, PathBuf), StoneError> {
    let mut positionals = Vec::new();

    let mut dir = PathBuf::from(DEFAULT_DATA_DIR);

    let mut dir_seen = false;

    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--dir" => {
                if dir_seen {
                    return Err(StoneError::InvalidArgument(
                        "--dir may only be specified once".to_string(),
                    ));
                }

                let path = args.get(index + 1).ok_or_else(|| {
                    StoneError::InvalidArgument("--dir requires a path".to_string())
                })?;

                if path.is_empty() {
                    return Err(StoneError::InvalidArgument(
                        "--dir path cannot be empty".to_string(),
                    ));
                }

                dir = PathBuf::from(path);

                dir_seen = true;

                index += 2;
            }

            argument if argument.starts_with("--") => {
                return Err(StoneError::InvalidArgument(format!(
                    "unknown option '{}'",
                    argument
                )));
            }

            _ => {
                positionals.push(args[index].clone());

                index += 1;
            }
        }
    }

    Ok((positionals, dir))
}

fn print_usage_stderr() {
    eprintln!(
        "\
stone — zero-dependency embedded key-value store

Usage:
  stone set <key> <value> [--dir PATH]
  stone get <key> [--dir PATH]
  stone del <key> [--dir PATH]
  stone compact [--dir PATH]
  stone stats [--dir PATH]
  stone verify [--dir PATH]
  stone help

Default data directory:
  ./stone-data"
    );
}

fn print_usage_stdout() {
    println!(
        "\
stone — zero-dependency embedded key-value store

Usage:
  stone set <key> <value> [--dir PATH]
  stone get <key> [--dir PATH]
  stone del <key> [--dir PATH]
  stone compact [--dir PATH]
  stone stats [--dir PATH]
  stone verify [--dir PATH]
  stone help

Default data directory:
  ./stone-data"
    );
}
