# Contributing to AXC

AXC is an open research and engineering project — a working proof-of-concept
that a modern archive format can be designed cleanly, safely, and openly.
The codebase is intentionally small and readable. Every module has a clear
responsibility and a test suite you can run in seconds.

This document explains where the project is heading, what help is most
valuable right now, and how to get started.

---

## The vision

ZIP has been the default archive format for 35 years. Its design decisions
(central directory at EOF, DEFLATE as the only practical codec, no streaming,
no random access) made sense for floppy disks in 1989. They are friction in
2025: pipeline tools, object storage, HTTP range requests, multi-core CPUs,
and memory-safe systems languages all exist now.

AXC is an attempt to design that format from scratch — with a specification
first, a reference implementation second, and adoption third. The goal is not
to replace ZIP overnight. The goal is to prove it can be done better, openly,
with a codebase anyone can read and verify.

We need people who disagree with our decisions, people who know compression
theory better than we do, and people who want to build things on top of it.

---

## What we need most

### 1. Codec improvements

The current rANS implementation is correct and passes all tests, but it is
a minimal implementation. The highest-leverage improvements here are:

**Faster rANS decode.** The decode loop is a tight inner loop. Interleaving
two or four independent ANS states eliminates serial data dependencies and
maps naturally to modern out-of-order CPUs. This is the technique used in
`zstd`'s FSE decoder and can 2–4× decode throughput with no format change.

```rust
// Current: one state, sequential
let mut x: u64 = initial_state;
for byte in output.iter_mut() { ... x = step(x); }

// Target: four interleaved states
let mut x = [s0, s1, s2, s3];
for chunk in output.chunks_mut(4) { ... x[i] = step(x[i]); }
```

**LZ77 optimal parser.** The current encoder uses a greedy hash-chain match
finder. An optimal (or near-optimal) parser using lazy matching or a
price-based model would improve compression ratio by 5–15% on most corpora
with no format change and no decoder change.

**Pre-filters.** Structured binary data (x86 executables, ARM code, DWARF
debug info, BMP images) compresses much better after a reversible
transformation that converts absolute addresses to relative ones (BCJ filter)
or that differences adjacent values (Delta filter). These are well-understood
techniques used in 7z and xz. Adding them as optional chunk-level filters
requires a one-byte codec ID change that is already reserved in the format.

### 2. Benchmark harness

We have no rigorous benchmarks yet. This is the most important gap between
"it works" and "it is credible."

What we need:

- A benchmark runner that tests AXC against ZIP (`miniz` / `flate2`), zstd,
  and optionally 7z, across standard corpora:
  - [Canterbury Corpus](https://corpus.canterbury.ac.nz/)
  - [Silesia Corpus](http://sun.aei.polsl.pl/~sdeor/index.php?page=silesia)
  - [enwik8](http://mattmahoney.net/dc/textdata.html) (first 100MB of Wikipedia)
- Results reported as: ratio (compressed/original), encode MB/s, decode MB/s,
  peak RAM, with confidence intervals across 5+ runs
- A CI workflow (GitHub Actions) that runs the benchmark on PRs touching codec
  code and posts a comparison comment

If you build this, you will have answered the question "is AXC actually better"
with real numbers. That is the single most important thing the project needs.

### 3. Format specification

The binary format is currently defined only in code comments in `format.rs`.
A proper specification should exist as a standalone document that allows
independent implementations to achieve bit-for-bit compatibility.

The specification should cover:

- All field widths, byte orders, and valid ranges
- Behaviour on malformed input (required error types, not just "undefined")
- Test vectors: known inputs → known compressed outputs with checksums
- Versioning and extension mechanism (the `flags` field in the file header is
  reserved but currently undefined)

The model to follow is the
[Zstandard format specification](https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md):
thorough, precise, written for implementors, with explicit test vectors.

### 4. Alternative implementations

A format is only credible when it has more than one implementation. If you
want to implement AXC in another language — Go, Python, C, Zig, Swift — this
is extremely welcome. The format is simple enough that a read-only decoder can
be written in a weekend.

A Python decoder is particularly valuable because it lets people inspect and
experiment with AXC archives in a scripting context without a Rust toolchain.

### 5. Security review

The archive parser (`format.rs`, `archive.rs`) is a classic attack surface.
We have tested the obvious cases (path traversal, decompression bombs, truncated
inputs, bad checksums) but we have not fuzz-tested the parser, and we have not
reviewed it against a threat model.

Contributions welcome:

- A `cargo fuzz` target covering the full decode pipeline
- An AFL++ or libFuzzer integration
- A documented threat model covering the attack surface
- Review of the checksum verification logic for TOCTOU issues

---

## How to contribute

### Getting started

```bash
git clone https://github.com/your-org/axc
cd axc
cargo test           # should be 24/24 green
cargo build --release
```

The codebase is ~1,400 lines across 8 files. Reading all of it takes about
an hour. We recommend starting with `format.rs` (the data model), then
`codec/ans.rs` (the entropy coder), then `archive.rs` (the orchestration layer).

### Opening an issue

Before starting significant work, open an issue describing what you want to
change and why. For codec changes especially, we want to discuss the approach
before implementation — the format has to remain stable once published.

Good issue titles:
- `[perf] Interleaved rANS decoder — design proposal`
- `[spec] Draft: AXC binary format v1 specification`
- `[bug] Checksum verification order in test_archive`
- `[bench] Silesia corpus results vs zstd level 3`

### Pull request checklist

- [ ] `cargo test` passes with no new failures
- [ ] New code has tests covering the new behaviour
- [ ] Codec changes include a before/after ratio comparison on at least one
  real corpus (even a single file is fine for a draft PR)
- [ ] Format-changing PRs include an update to `format.rs` doc comments
  and a note in the PR description about backward compatibility

### Code style

- No `unwrap()` in library code (only in tests and CLI where error context is clear)
- Errors use `AxcError` from `error.rs` — add variants rather than using strings
- Comments on non-obvious algorithms should cite a reference (paper, spec section,
  or Wikipedia article) so future readers can verify the logic
- Keep modules single-responsibility: codec changes go in `codec/`, format
  changes go in `format.rs`, safety logic goes in `archive.rs`

---

## Design decisions open for challenge

These are decisions we made for the MVP that we are not certain are correct.
If you have a strong argument for a different approach, open an issue.

**CRC-32 vs BLAKE3.** CRC-32 detects accidental corruption but not malicious
modification. For a general archive format, a cryptographic checksum (BLAKE3
is fast and modern) would be more robust. The counter-argument is that AXC is
not an integrity-verification tool and the overhead matters. RAR5 moved to
BLAKE2 — was that the right call?

**rANS vs tANS.** We use rANS (byte-aligned renorm) because it is simpler to
implement correctly. tANS (table-based) has faster decode at the cost of
implementation complexity. `zstd` uses tANS (FSE). For AXC's target use case,
does the decode speed difference matter enough to justify the complexity?

**Chunk size default (512 KiB).** This affects the ratio/random-access tradeoff.
Larger chunks compress better (more context) but mean more decompression work
for random access. Is 512 KiB the right default? Should it be adaptive?

**No encryption.** AXC currently has no encryption layer. RAR5 and 7z both
support AES-256 archive encryption. Adding encryption requires careful design
(key derivation, IV handling, authenticated encryption) and is out of scope for
the MVP. Is this a blocker for real-world adoption?

---

## Community

- **Issues:** [github.com/your-org/axc/issues](https://github.com/your-org/axc/issues)
- **Discussions:** [github.com/your-org/axc/discussions](https://github.com/your-org/axc/discussions)

If you use AXC for something real — even an experiment — we want to hear about it.
If you find a bug, please file an issue with a minimal reproducer. If you have
a question about the format or the implementation, open a Discussion.

---

## What success looks like

In six months, success is: a benchmark showing AXC compresses text corpora
better than ZIP and extracts faster, a published binary format specification,
and at least one independent decoder in a second language.

In two years, success is: a `tar | axc` workflow that developers use because
it is genuinely better, and a specification stable enough that we are confident
in committing to backward compatibility.

Neither of those happens without contributors. If you read this far, you are
probably one of them.

— The AXC team
