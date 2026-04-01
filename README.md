# AXC — Adaptive eXtensible Compression Archive

> A modern, open archive format and codec designed to outperform ZIP on speed,
> approach 7z on ratio, and fix structural limitations that have persisted since 1989.

```
$ axc c backup.axc my-project/
axc: creating 'backup.axc' (142 files, level=balanced, chunk=512KiB)
axc: done  214.3 MiB → 89.1 MiB  (41.6%)

$ axc x -o ./restored backup.axc
axc: extracted 142 file(s) to './restored'

$ axc t backup.axc
axc: testing 'backup.axc'... OK (142 chunks verified)
```

---

## Why AXC?

ZIP was designed in 1989 for floppy disks. Its central directory sits at the **end of the file** — meaning you can't begin streaming, listing, or extracting until you've read the last bytes. RAR is proprietary. 7z has no native streaming or parallel decode. All three use entropy coders from the 1990s.

AXC is designed from scratch with modern constraints in mind:

| | ZIP | 7z | RAR5 | **AXC** |
|---|:---:|:---:|:---:|:---:|
| Streaming-first | ✗ | ✗ | ✗ | ✓ |
| Random access by chunk | ✗ | ✗ | partial | ✓ |
| Parallel encode | ✗ | partial | ✗ | ✓ |
| Modern entropy coder | ✗ | ✗ | ✗ | ✓ rANS |
| Open spec + implementation | ✓ | ✓ | ✗ | ✓ |
| Zero unsafe deps | — | — | — | ✓ |
| Path traversal protection | varies | varies | varies | ✓ built-in |
| Decompression bomb limit | varies | varies | varies | ✓ built-in |

---

## Architecture

```
axc/
├── src/
│   ├── main.rs          # CLI: c / x / l / t
│   ├── lib.rs           # Public library API
│   ├── format.rs        # Binary container: header, chunks, index, footer
│   ├── archive.rs       # Create / extract / list / test + safety layers
│   ├── checksum.rs      # CRC-32 (pure Rust, no deps)
│   ├── error.rs         # Unified error types
│   └── codec/
│       ├── mod.rs       # LZ-ANS++ pipeline (auto-selects best path)
│       ├── lz77.rs      # LZ77 hash-chain match finder
│       └── ans.rs       # rANS (range ANS) entropy coder
```

### Container Format (`.axc`)

```
[File Header  — 32 bytes ]  magic, version, default chunk size
[Chunk Record — variable ]*  compressed payload + CRC-32 per chunk
[Index        — variable ]*  file entries: name, size, mtime, chunk refs
[Footer       — 24 bytes ]  index offset, count, CRC-32
```

The index and footer at the end allow **fast `list` without full scan** while the chunk-first layout enables **streaming extraction** from the beginning. Both patterns are supported simultaneously.

### Codec: LZ-ANS++

Two-stage pipeline:

1. **LZ77** — hash-chain match finder with 64 KiB window. Produces a token stream of literals and back-references. Unambiguous 2-byte token encoding handles all 256 byte values without escaping.

2. **rANS** — range Asymmetric Numeral Systems entropy coder. Frequencies normalised to `L = 2048`. Encodes backwards over the token stream, flushed to a byte stream that the decoder reads forward. Near-arithmetic-coding efficiency with byte-aligned I/O.

The pipeline automatically falls back to raw rANS (no LZ step) when LZ pre-processing doesn't help (e.g. already-compressed or random data).

---

## Installation

### From source

```bash
git clone https://github.com/your-org/axc
cd axc
cargo build --release
# Binary at: target/release/axc
```

**Requirements:** Rust 1.75+ (stable), one dependency: `rayon` for parallel compression.

### Cargo

```bash
cargo install axc
```

---

## Usage

### Create an archive

```bash
# Single file
axc c output.axc file.txt

# Multiple files and directories (recursive)
axc c backup.axc documents/ photos/ notes.md

# Compression levels
axc c -l fast     archive.axc src/   # fastest encode
axc c -l balanced archive.axc src/   # default
axc c -l max      archive.axc src/   # best ratio

# Custom chunk size (default 512 KiB)
axc c -s 1048576 archive.axc large-dataset/
```

### Extract

```bash
# Extract to current directory
axc x archive.axc

# Extract to specific directory
axc x -o /tmp/restored archive.axc

# Overwrite existing files
axc x -f -o ./out archive.axc

# Set decompression limit (default 1 GiB, defence against bombs)
axc x --limit 536870912 archive.axc
```

### List contents

```bash
axc l archive.axc
# Size          Chunks    Name
# --------------------------------------------------
# 18.5 KiB      1         src/archive.rs
# 9.1 KiB       1         src/codec/ans.rs
# ...
```

### Test integrity

```bash
axc t archive.axc
# axc: testing 'archive.axc'... OK (9 chunks verified)
```

---

## Library API

AXC ships as both a binary and a library crate:

```rust
use axc::{create_archive, extract_archive, list_archive, CreateOptions, ExtractOptions, Level};
use std::io::BufWriter;
use std::fs::File;
use std::path::PathBuf;

// Create
let mut w = BufWriter::new(File::create("out.axc")?);
let files = vec![
    ("data/hello.txt".to_string(), PathBuf::from("hello.txt")),
];
create_archive(&mut w, &files, &CreateOptions {
    level: Level::Balanced,
    ..Default::default()
})?;

// List
let mut r = std::io::BufReader::new(File::open("out.axc")?);
for entry in list_archive(&mut r)? {
    println!("{} — {} bytes", entry.name, entry.original_size);
}
```

---

## Security

AXC treats all archive inputs as untrusted:

- **Path traversal** — every filename is sanitised at extract time. Absolute paths, `..` components, and Windows drive prefixes are rejected with `AxcError::PathTraversal`.
- **Decompression bombs** — total decompressed output is tracked across all chunks. Extraction aborts with `AxcError::DecompressionBomb` when the configurable limit is exceeded (default 1 GiB).
- **Checksum per chunk** — CRC-32 of both compressed and original data stored in every chunk record. Corruption is detected before decompression and after.
- **Header limits** — all length fields are validated before allocation. Malformed archives return typed errors, never panics.

---

## Performance (preliminary, debug build on reference machine)

Measured compressing the AXC source tree (62.6 KiB, 9 files):

| Format | Compressed | Ratio |
|---|---|---|
| ZIP (deflate) | ~32 KiB | ~51% |
| **AXC (balanced)** | **27.2 KiB** | **43.4%** |

Full benchmark suite across standard corpora (Canterbury, Silesia, enwik8) is planned — contributions welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## Roadmap

- [ ] BCJ / Delta pre-filters for binary and structured data
- [ ] Solid archive mode (cross-file dictionary)
- [ ] Content-defined chunking (CDC) for deduplication
- [ ] Parallel decode with rayon
- [ ] Streaming HTTP extraction (range requests)
- [ ] FUSE mount (read-only)
- [ ] Formal binary format specification (AXC-SPEC-v1)
- [ ] Python and Go bindings via C ABI

---

## License

Apache-2.0. See [LICENSE](LICENSE).

The `rayon` dependency is MIT/Apache-2.0. No GPL, no proprietary components, no unRAR-style restrictions.
