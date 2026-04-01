//! AXC command-line interface.
//!
//! Usage:
//!   axc c [-l fast|balanced|max] [-s CHUNK_SIZE] <archive.axc> <file> [file...]
//!   axc x [-o OUTPUT_DIR] [-f] <archive.axc> [file...]
//!   axc l <archive.axc>
//!   axc t <archive.axc>
//!   axc help

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process;

use axc::{
    archive::{CreateOptions, ExtractOptions},
    codec::Level,
    create_archive, extract_archive, list_archive, test_archive,
};

fn usage() {
    eprintln!(
        r#"AXC — Adaptive eXtensible Compression archive  v{version}

USAGE:
  axc c [OPTIONS] <archive.axc> <file> [file ...]
  axc x [OPTIONS] <archive.axc>
  axc l <archive.axc>
  axc t <archive.axc>

COMMANDS:
  c   Create archive
  x   Extract archive
  l   List archive contents
  t   Test archive integrity

OPTIONS (c):
  -l <level>      Compression level: fast | balanced (default) | max
  -s <bytes>      Chunk size in bytes (default: 524288)

OPTIONS (x):
  -o <dir>        Output directory (default: current directory)
  -f              Overwrite existing files
  --limit <bytes> Decompression limit in bytes (default: 1073741824)

EXAMPLES:
  axc c backup.axc documents/ photos/
  axc x -o /tmp/out backup.axc
  axc l backup.axc
  axc t backup.axc
"#,
        version = env!("CARGO_PKG_VERSION")
    );
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("axc: error: {msg}");
    process::exit(1);
}

fn collect_files(paths: &[String]) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for raw in paths {
        let p = PathBuf::from(raw);
        collect_recursive(&p, &p, &mut files);
    }
    files
}

fn collect_recursive(root: &PathBuf, current: &PathBuf, out: &mut Vec<(String, PathBuf)>) {
    if current.is_file() {
        // Archive name: relative to the parent of root
        let name = if let Some(parent) = root.parent() {
            current
                .strip_prefix(parent)
                .unwrap_or(current)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            current.to_string_lossy().replace('\\', "/")
        };
        out.push((name, current.to_path_buf()));
    } else if current.is_dir() {
        match std::fs::read_dir(current) {
            Ok(entries) => {
                let mut children: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .collect();
                children.sort();
                for child in children {
                    collect_recursive(root, &child, out);
                }
            }
            Err(e) => eprintln!("axc: warning: cannot read dir {}: {e}", current.display()),
        }
    }
}

fn cmd_create(args: &[String]) {
    let mut level = Level::Balanced;
    let mut chunk_size = axc::format::DEFAULT_CHUNK_SIZE;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-l" | "--level" => {
                i += 1;
                level = match args.get(i).map(String::as_str) {
                    Some("fast") => Level::Fast,
                    Some("balanced") => Level::Balanced,
                    Some("max") => Level::Max,
                    Some(other) => die(format!("unknown level '{other}'. Use: fast | balanced | max")),
                    None => die("-l requires an argument"),
                };
            }
            "-s" | "--chunk-size" => {
                i += 1;
                chunk_size = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die("-s requires a numeric argument"));
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    if positional.len() < 2 {
        die("usage: axc c [OPTIONS] <archive.axc> <file> [file ...]");
    }

    let archive_path = &positional[0];
    let input_paths = &positional[1..];

    let files = collect_files(input_paths);
    if files.is_empty() {
        die("no input files found");
    }

    println!("axc: creating '{}' ({} files, level={:?}, chunk={}B)", archive_path, files.len(), level, chunk_size);

    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(archive_path)
        .unwrap_or_else(|e| die(format!("cannot open '{}': {e}", archive_path)));

    let mut bw = BufWriter::new(f);
    let opts = CreateOptions { level, chunk_size };

    create_archive(&mut bw, &files, &opts).unwrap_or_else(|e| die(e));

    let archive_size = std::fs::metadata(archive_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let original_size: u64 = files
        .iter()
        .filter_map(|(_, p)| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    let ratio = if original_size > 0 {
        100.0 * archive_size as f64 / original_size as f64
    } else {
        100.0
    };

    println!(
        "axc: done  {} → {}  ({:.1}%)",
        format_size(original_size),
        format_size(archive_size),
        ratio
    );
}

fn cmd_extract(args: &[String]) {
    let mut output_dir = PathBuf::from(".");
    let mut overwrite = false;
    let mut decompress_limit = axc::archive::MAX_DECOMPRESS_SIZE;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output_dir = PathBuf::from(args.get(i).unwrap_or_else(|| die("-o requires an argument")));
            }
            "-f" | "--overwrite" => overwrite = true,
            "--limit" => {
                i += 1;
                decompress_limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die("--limit requires a numeric argument"));
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    let archive_path = positional.first().unwrap_or_else(|| die("usage: axc x [OPTIONS] <archive.axc>"));

    let f = File::open(archive_path).unwrap_or_else(|e| die(format!("cannot open '{}': {e}", archive_path)));
    let mut br = BufReader::new(f);

    let opts = ExtractOptions { output_dir: output_dir.clone(), overwrite, decompress_limit };
    let extracted = extract_archive(&mut br, &opts).unwrap_or_else(|e| die(e));

    for p in &extracted {
        println!("  extracted: {}", p.display());
    }

    println!("axc: extracted {} file(s) to '{}'", extracted.len(), output_dir.display());
}

fn cmd_list(args: &[String]) {
    let archive_path = args.first().unwrap_or_else(|| die("usage: axc l <archive.axc>"));

    let f = File::open(archive_path).unwrap_or_else(|e| die(format!("cannot open '{}': {e}", archive_path)));
    let mut br = BufReader::new(f);

    let entries = list_archive(&mut br).unwrap_or_else(|e| die(e));

    if entries.is_empty() {
        println!("(empty archive)");
        return;
    }

    println!("{:<12}  {:<8}  {}", "Size", "Chunks", "Name");
    println!("{}", "-".repeat(50));
    let mut total = 0u64;
    for e in &entries {
        println!("{:<12}  {:<8}  {}", format_size(e.original_size), e.chunk_count, e.name);
        total += e.original_size;
    }
    println!("{}", "-".repeat(50));
    println!("{:<12}  {:>8}  {} file(s)", format_size(total), "", entries.len());
}

fn cmd_test(args: &[String]) {
    let archive_path = args.first().unwrap_or_else(|| die("usage: axc t <archive.axc>"));

    let f = File::open(archive_path).unwrap_or_else(|e| die(format!("cannot open '{}': {e}", archive_path)));
    let mut br = BufReader::new(f);

    print!("axc: testing '{archive_path}'... ");
    match test_archive(&mut br) {
        Ok(n) => println!("OK ({n} chunks verified)"),
        Err(e) => {
            println!("FAILED");
            die(e);
        }
    }
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "c" | "create" => cmd_create(&args[2..]),
        "x" | "extract" => cmd_extract(&args[2..]),
        "l" | "list" => cmd_list(&args[2..]),
        "t" | "test" => cmd_test(&args[2..]),
        "help" | "--help" | "-h" => { usage(); }
        other => {
            eprintln!("axc: unknown command '{other}'. Try 'axc help'.");
            process::exit(1);
        }
    }
}
