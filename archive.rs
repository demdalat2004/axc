//! High-level archive operations: create, extract, list, test.
//!
//! Parallel compression: files are split into chunks; each chunk is
//! compressed independently using rayon's thread pool.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::checksum::Crc32;
use crate::codec::{self, CodecId, Level};
use crate::error::{AxcError, Result};
use crate::format::{
    ChunkRecord, DEFAULT_CHUNK_SIZE, FileEntry, FileHeader, Footer, CHUNK_HEADER_SIZE,
};

// ── Safety limits ─────────────────────────────────────────────────────────────

/// Maximum decompressed size per archive (1 GiB default).
pub const MAX_DECOMPRESS_SIZE: u64 = 1024 * 1024 * 1024;

/// Sanitise a path from the archive so it never escapes the output directory.
/// Blocks: absolute paths, `..` components, Windows drive prefixes.
pub fn sanitise_path(raw: &str) -> Result<PathBuf> {
    let p = Path::new(raw);
    let mut out = PathBuf::new();

    for component in p.components() {
        match component {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AxcError::PathTraversal(raw.to_string()));
            }
        }
    }

    if out.as_os_str().is_empty() {
        return Err(AxcError::PathTraversal(raw.to_string()));
    }

    Ok(out)
}

// ── Chunk splitting helpers ───────────────────────────────────────────────────

fn split_into_chunks(data: &[u8], chunk_size: usize) -> Vec<&[u8]> {
    data.chunks(chunk_size).collect()
}

fn mtime_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
        .unwrap_or_else(|_| mtime_now())
}

fn file_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o644)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o644
    }
}

// ── Create archive ────────────────────────────────────────────────────────────

pub struct CreateOptions {
    pub level: Level,
    pub chunk_size: u32,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            level: Level::Balanced,
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

/// Compress a list of (archive_name, source_path) pairs into an AXC archive
/// written to `output`. Compression is parallelised per-chunk using rayon.
pub fn create_archive<W: Write + Seek>(
    output: &mut W,
    files: &[(String, PathBuf)],
    opts: &CreateOptions,
) -> Result<()> {
    let chunk_size = opts.chunk_size as usize;
    let header = FileHeader::new(opts.chunk_size);
    header.write(output).map_err(AxcError::Io)?;

    let chunk_id_counter = AtomicU64::new(0);
    let mut file_entries: Vec<FileEntry> = Vec::new();

    let mut file_id = 0u64;

    for (archive_name, src_path) in files {
        // Validate archive name
        sanitise_path(archive_name)?;

        let data = fs::read(src_path).map_err(AxcError::Io)?;
        let original_size = data.len() as u64;
        let mtime = file_mtime(src_path);
        let mode = file_mode(src_path);

        // Split into chunks
        let raw_chunks: Vec<&[u8]> = split_into_chunks(&data, chunk_size);

        // Parallel compress each chunk
        let compressed_chunks: Vec<(ChunkRecord, Vec<u8>)> = raw_chunks
            .par_iter()
            .map(|chunk_data| {
                let cid = chunk_id_counter.fetch_add(1, Ordering::Relaxed);
                let checksum_orig = Crc32::oneshot(chunk_data);
                let compressed = codec::compress_full(chunk_data, opts.level);
                let checksum_comp = Crc32::oneshot(&compressed);

                let record = ChunkRecord {
                    codec_id: CodecId::LzAns as u8,
                    chunk_id: cid,
                    original_len: chunk_data.len() as u64,
                    compressed_len: compressed.len() as u64,
                    checksum_orig,
                    checksum_comp,
                };
                (record, compressed)
            })
            .collect();

        let first_chunk = compressed_chunks
            .first()
            .map(|(r, _)| r.chunk_id)
            .unwrap_or(0);
        let chunk_count = compressed_chunks.len() as u32;

        // Write chunks sequentially (order matters for sequential readers)
        for (record, comp_data) in &compressed_chunks {
            record.write(output, comp_data).map_err(AxcError::Io)?;
        }

        file_entries.push(FileEntry {
            file_id,
            mtime,
            mode,
            first_chunk,
            chunk_count,
            original_size,
            name: archive_name.clone(),
        });

        file_id += 1;
    }

    // Write index
    let index_offset = output.seek(SeekFrom::Current(0)).map_err(AxcError::Io)?;
    let index_count = file_entries.len() as u32;

    for entry in &file_entries {
        entry.write(output).map_err(AxcError::Io)?;
    }

    // Write footer
    let footer = Footer { index_offset, index_count };
    footer.write(output).map_err(AxcError::Io)?;

    Ok(())
}

// ── Read index ────────────────────────────────────────────────────────────────

pub fn read_index<R: Read + Seek>(r: &mut R) -> Result<Vec<FileEntry>> {
    let footer = Footer::read(r)?;
    r.seek(SeekFrom::Start(footer.index_offset)).map_err(AxcError::Io)?;

    let mut entries = Vec::with_capacity(footer.index_count as usize);
    for _ in 0..footer.index_count {
        entries.push(FileEntry::read(r)?);
    }

    Ok(entries)
}

// ── List archive ──────────────────────────────────────────────────────────────

pub struct ListEntry {
    pub name: String,
    pub original_size: u64,
    pub mtime: u64,
    pub chunk_count: u32,
}

pub fn list_archive<R: Read + Seek>(r: &mut R) -> Result<Vec<ListEntry>> {
    FileHeader::read(r)?;
    let entries = read_index(r)?;
    Ok(entries
        .into_iter()
        .map(|e| ListEntry {
            name: e.name,
            original_size: e.original_size,
            mtime: e.mtime,
            chunk_count: e.chunk_count,
        })
        .collect())
}

// ── Build chunk offset map ────────────────────────────────────────────────────

/// Scan the data section (after file header) and build a map:
/// chunk_id → (byte_offset, ChunkRecord metadata)
pub fn build_chunk_map<R: Read + Seek>(
    r: &mut R,
    index_offset: u64,
) -> Result<HashMap<u64, (u64, ChunkRecord)>> {
    let mut map = HashMap::new();
    // Seek to after file header
    r.seek(SeekFrom::Start(32)).map_err(AxcError::Io)?;

    loop {
        let pos = r.seek(SeekFrom::Current(0)).map_err(AxcError::Io)?;
        if pos >= index_offset {
            break;
        }

        // Peek tag
        let mut tag = [0u8; 1];
        match r.read_exact(&mut tag) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(AxcError::Io(e)),
        }

        if tag[0] != crate::format::CHUNK_TAG {
            break;
        }

        // Read rest of chunk header (we already consumed the tag byte)
        let mut buf = [0u8; CHUNK_HEADER_SIZE - 1];
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        let codec_id       = buf[0];
        let chunk_id       = u64::from_le_bytes(buf[1..9].try_into().unwrap());
        let original_len   = u64::from_le_bytes(buf[9..17].try_into().unwrap());
        let compressed_len = u64::from_le_bytes(buf[17..25].try_into().unwrap());
        let checksum_orig  = u32::from_le_bytes(buf[25..29].try_into().unwrap());
        let checksum_comp  = u32::from_le_bytes(buf[29..33].try_into().unwrap());

        let data_offset = r.seek(SeekFrom::Current(0)).map_err(AxcError::Io)?;
        let record = ChunkRecord { codec_id, chunk_id, original_len, compressed_len, checksum_orig, checksum_comp };

        map.insert(chunk_id, (data_offset, record));

        // Skip data
        r.seek(SeekFrom::Current(compressed_len as i64)).map_err(AxcError::Io)?;
    }

    Ok(map)
}

// ── Extract archive ───────────────────────────────────────────────────────────

pub struct ExtractOptions {
    pub output_dir: PathBuf,
    pub overwrite: bool,
    pub decompress_limit: u64,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            output_dir: PathBuf::from("."),
            overwrite: false,
            decompress_limit: MAX_DECOMPRESS_SIZE,
        }
    }
}

/// Extract all files from an AXC archive.
pub fn extract_archive<R: Read + Seek>(
    r: &mut R,
    opts: &ExtractOptions,
) -> Result<Vec<PathBuf>> {
    FileHeader::read(r)?;

    let footer = Footer::read(r)?;
    let entries = {
        r.seek(SeekFrom::Start(footer.index_offset)).map_err(AxcError::Io)?;
        let mut v = Vec::with_capacity(footer.index_count as usize);
        for _ in 0..footer.index_count {
            v.push(FileEntry::read(r)?);
        }
        v
    };

    let chunk_map = build_chunk_map(r, footer.index_offset)?;

    let mut total_decompressed = 0u64;
    let mut extracted_paths = Vec::new();

    for entry in &entries {
        let rel_path = sanitise_path(&entry.name)?;
        let dest = opts.output_dir.join(&rel_path);

        // Create parent dirs
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(AxcError::Io)?;
        }

        if dest.exists() && !opts.overwrite {
            // Skip existing files unless overwrite
            continue;
        }

        // Reconstruct file from chunks in order
        let mut file_data: Vec<u8> = Vec::with_capacity(entry.original_size as usize);

        for chunk_idx in 0..entry.chunk_count {
            let chunk_id = entry.first_chunk + chunk_idx as u64;
            let (data_offset, record) = chunk_map
                .get(&chunk_id)
                .ok_or_else(|| AxcError::InvalidChunk(format!("chunk {chunk_id} not found")))?;

            // Read compressed data
            r.seek(SeekFrom::Start(*data_offset)).map_err(AxcError::Io)?;
            let mut comp_data = vec![0u8; record.compressed_len as usize];
            r.read_exact(&mut comp_data).map_err(AxcError::Io)?;

            // Verify compressed checksum
            let actual_comp = Crc32::oneshot(&comp_data);
            if actual_comp != record.checksum_comp {
                return Err(AxcError::ChecksumMismatch {
                    chunk_id,
                    expected: record.checksum_comp,
                    got: actual_comp,
                });
            }

            // Decompress
            let decompressed = codec::decompress(&comp_data, record.original_len as usize)?;

            // Verify original checksum
            let actual_orig = Crc32::oneshot(&decompressed);
            if actual_orig != record.checksum_orig {
                return Err(AxcError::ChecksumMismatch {
                    chunk_id,
                    expected: record.checksum_orig,
                    got: actual_orig,
                });
            }

            total_decompressed += decompressed.len() as u64;
            if total_decompressed > opts.decompress_limit {
                return Err(AxcError::DecompressionBomb {
                    output_size: total_decompressed,
                    limit: opts.decompress_limit,
                });
            }

            file_data.extend_from_slice(&decompressed);
        }

        // Write output file
        {
            let f = fs::File::create(&dest).map_err(AxcError::Io)?;
            let mut bw = BufWriter::new(f);
            bw.write_all(&file_data).map_err(AxcError::Io)?;
        }

        // Restore permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(entry.mode);
            let _ = fs::set_permissions(&dest, perms);
        }

        extracted_paths.push(dest);
    }

    Ok(extracted_paths)
}

// ── Test archive ──────────────────────────────────────────────────────────────

/// Verify all chunks in the archive without writing any files.
pub fn test_archive<R: Read + Seek>(r: &mut R) -> Result<usize> {
    FileHeader::read(r)?;

    let footer = Footer::read(r)?;
    r.seek(SeekFrom::Start(32)).map_err(AxcError::Io)?;

    let mut chunk_count = 0usize;

    loop {
        let pos = r.seek(SeekFrom::Current(0)).map_err(AxcError::Io)?;
        if pos >= footer.index_offset {
            break;
        }

        let mut tag = [0u8; 1];
        match r.read_exact(&mut tag) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(AxcError::Io(e)),
        }
        if tag[0] != crate::format::CHUNK_TAG {
            break;
        }

        let mut buf = [0u8; CHUNK_HEADER_SIZE - 1];
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        let chunk_id       = u64::from_le_bytes(buf[1..9].try_into().unwrap());
        let original_len   = u64::from_le_bytes(buf[9..17].try_into().unwrap());
        let compressed_len = u64::from_le_bytes(buf[17..25].try_into().unwrap());
        let checksum_orig  = u32::from_le_bytes(buf[25..29].try_into().unwrap());
        let checksum_comp  = u32::from_le_bytes(buf[29..33].try_into().unwrap());

        let mut comp_data = vec![0u8; compressed_len as usize];
        r.read_exact(&mut comp_data).map_err(AxcError::Io)?;

        let actual_comp = Crc32::oneshot(&comp_data);
        if actual_comp != checksum_comp {
            return Err(AxcError::ChecksumMismatch { chunk_id, expected: checksum_comp, got: actual_comp });
        }

        let decompressed = codec::decompress(&comp_data, original_len as usize)?;
        let actual_orig = Crc32::oneshot(&decompressed);
        if actual_orig != checksum_orig {
            return Err(AxcError::ChecksumMismatch { chunk_id, expected: checksum_orig, got: actual_orig });
        }

        chunk_count += 1;
    }

    Ok(chunk_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let pairs: Vec<(String, PathBuf)> = files
            .iter()
            .map(|(name, _)| (name.to_string(), PathBuf::from("/dev/null")))
            .collect();

        // Write directly without reading from disk: build manually
        let opts = CreateOptions::default();
        let hdr = FileHeader::new(opts.chunk_size);
        hdr.write(&mut buf).unwrap();

        let mut file_entries = vec![];
        let mut chunk_id = 0u64;

        for (name, data) in files {
            let checksum_orig = Crc32::oneshot(data);
            let compressed = codec::compress_full(data, Level::Balanced);
            let checksum_comp = Crc32::oneshot(&compressed);
            let record = ChunkRecord {
                codec_id: CodecId::LzAns as u8,
                chunk_id,
                original_len: data.len() as u64,
                compressed_len: compressed.len() as u64,
                checksum_orig,
                checksum_comp,
            };
            record.write(&mut buf, &compressed).unwrap();
            file_entries.push(FileEntry {
                file_id: chunk_id,
                mtime: 0,
                mode: 0o644,
                first_chunk: chunk_id,
                chunk_count: 1,
                original_size: data.len() as u64,
                name: name.to_string(),
            });
            chunk_id += 1;
        }

        let index_offset = buf.seek(SeekFrom::Current(0)).unwrap();
        for e in &file_entries {
            e.write(&mut buf).unwrap();
        }
        let footer = Footer { index_offset, index_count: file_entries.len() as u32 };
        footer.write(&mut buf).unwrap();

        buf.into_inner()
    }

    #[test]
    fn list_roundtrip() {
        let archive = make_archive(&[("hello.txt", b"hello world"), ("data.bin", b"\x00\x01\x02\x03")]);
        let mut cur = Cursor::new(archive);
        let entries = list_archive(&mut cur).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].original_size, 11);
        assert_eq!(entries[1].name, "data.bin");
    }

    #[test]
    fn test_archive_passes() {
        let archive = make_archive(&[("a.txt", b"aaabbbccc"), ("b.txt", b"xyz")]);
        let mut cur = Cursor::new(archive);
        let count = test_archive(&mut cur).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn path_traversal_blocked() {
        assert!(sanitise_path("../etc/passwd").is_err());
        assert!(sanitise_path("/etc/passwd").is_err());
        assert!(sanitise_path("foo/../../etc/passwd").is_err());
        assert!(sanitise_path("safe/path/file.txt").is_ok());
    }

    #[test]
    fn decompression_bomb_limit() {
        // A "bomb": one chunk that decompresses to over MAX
        // We test the limit enforcement by using a tiny limit
        let archive = make_archive(&[("big.txt", &vec![b'A'; 1024])]);
        let mut cur = Cursor::new(archive);
        let opts = ExtractOptions {
            output_dir: std::env::temp_dir(),
            overwrite: true,
            decompress_limit: 10, // tiny limit
        };
        let result = extract_archive(&mut cur, &opts);
        assert!(matches!(result, Err(AxcError::DecompressionBomb { .. })));
    }
}
