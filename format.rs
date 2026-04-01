//! AXC container format — binary layout.
//!
//! File layout:
//!   [File Header 32 bytes]
//!   [Chunk Record]*          ← one per file, back-referenced by Index
//!   [Index Record]*          ← written after all chunks
//!   [Footer 24 bytes]
//!
//! All integers: little-endian.
//!
//! ┌─────────────────────────────────────────────┐
//! │ FILE HEADER (32 bytes)                      │
//! │  magic     : [u8; 8]  = b"AXCv1\x00\x00\x00"│
//! │  version   : u8                             │
//! │  flags     : u8                             │
//! │  reserved  : [u8; 6]                        │
//! │  chunk_size: u32   default chunk size       │
//! │  reserved2 : [u8; 12]                       │
//! └─────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────┐
//! │ CHUNK RECORD (variable)                     │
//! │  tag          : u8  = 0xC1                  │
//! │  codec_id     : u8                          │
//! │  chunk_id     : u64                         │
//! │  original_len : u64                         │
//! │  compressed_len: u64                        │
//! │  checksum_orig: u32  CRC32 of original      │
//! │  checksum_comp: u32  CRC32 of compressed    │
//! │  data         : [u8; compressed_len]        │
//! └─────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────┐
//! │ FILE ENTRY in Index (variable)              │
//! │  tag          : u8  = 0xE1                  │
//! │  file_id      : u64                         │
//! │  mtime        : u64  Unix seconds           │
//! │  mode         : u32  Unix permissions       │
//! │  first_chunk  : u64  chunk_id               │
//! │  chunk_count  : u32                         │
//! │  original_size: u64  total file bytes       │
//! │  name_len     : u16                         │
//! │  name         : [u8; name_len]  UTF-8       │
//! └─────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────┐
//! │ FOOTER (24 bytes)                           │
//! │  tag          : u8  = 0xFF                  │
//! │  reserved     : [u8; 3]                     │
//! │  index_offset : u64  byte offset from start │
//! │  index_count  : u32  number of file entries │
//! │  checksum     : u32  CRC32 of footer[0..16] │
//! │  magic_end    : [u8; 4] = b"AXCE"           │
//! └─────────────────────────────────────────────┘

use std::io::{self, Read, Write, Seek, SeekFrom};
use crate::error::{AxcError, Result};
use crate::checksum::Crc32;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const MAGIC: &[u8; 8] = b"AXCv1\x00\x00\x00";
pub const MAGIC_END: &[u8; 4] = b"AXCE";
pub const VERSION: u8 = 1;
pub const CHUNK_TAG: u8 = 0xC1;
pub const ENTRY_TAG: u8 = 0xE1;
pub const FOOTER_TAG: u8 = 0xFF;
pub const DEFAULT_CHUNK_SIZE: u32 = 512 * 1024; // 512 KiB

// ── File Header ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub version: u8,
    pub flags: u8,
    pub chunk_size: u32,
}

impl FileHeader {
    pub fn new(chunk_size: u32) -> Self {
        FileHeader { version: VERSION, flags: 0, chunk_size }
    }

    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(MAGIC)?;
        w.write_all(&[self.version, self.flags])?;
        w.write_all(&[0u8; 6])?; // reserved
        w.write_all(&self.chunk_size.to_le_bytes())?;
        w.write_all(&[0u8; 12])?; // reserved2
        Ok(())
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self> {
        let mut buf = [0u8; 32];
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        if &buf[0..8] != MAGIC {
            return Err(AxcError::InvalidMagic);
        }
        let version = buf[8];
        if version != VERSION {
            return Err(AxcError::UnsupportedVersion(version));
        }
        let flags = buf[9];
        let chunk_size = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        Ok(FileHeader { version, flags, chunk_size })
    }
}

// ── Chunk Record ──────────────────────────────────────────────────────────────

pub const CHUNK_HEADER_SIZE: usize = 1 + 1 + 8 + 8 + 8 + 4 + 4; // 34 bytes

#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub codec_id: u8,
    pub chunk_id: u64,
    pub original_len: u64,
    pub compressed_len: u64,
    pub checksum_orig: u32,
    pub checksum_comp: u32,
}

impl ChunkRecord {
    pub fn write<W: Write>(&self, w: &mut W, data: &[u8]) -> io::Result<()> {
        w.write_all(&[CHUNK_TAG, self.codec_id])?;
        w.write_all(&self.chunk_id.to_le_bytes())?;
        w.write_all(&self.original_len.to_le_bytes())?;
        w.write_all(&self.compressed_len.to_le_bytes())?;
        w.write_all(&self.checksum_orig.to_le_bytes())?;
        w.write_all(&self.checksum_comp.to_le_bytes())?;
        w.write_all(data)?;
        Ok(())
    }

    pub fn read<R: Read>(r: &mut R) -> Result<(Self, Vec<u8>)> {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag).map_err(AxcError::Io)?;
        if tag[0] != CHUNK_TAG {
            return Err(AxcError::InvalidChunk(format!("expected tag {CHUNK_TAG:#04x}, got {:#04x}", tag[0])));
        }

        let mut buf = [0u8; CHUNK_HEADER_SIZE - 1]; // already read tag
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        let codec_id = buf[0];
        let chunk_id = u64::from_le_bytes(buf[1..9].try_into().unwrap());
        let original_len = u64::from_le_bytes(buf[9..17].try_into().unwrap());
        let compressed_len = u64::from_le_bytes(buf[17..25].try_into().unwrap());
        let checksum_orig = u32::from_le_bytes(buf[25..29].try_into().unwrap());
        let checksum_comp = u32::from_le_bytes(buf[29..33].try_into().unwrap());

        let mut data = vec![0u8; compressed_len as usize];
        r.read_exact(&mut data).map_err(AxcError::Io)?;

        // Verify compressed checksum
        let actual_comp = Crc32::oneshot(&data);
        if actual_comp != checksum_comp {
            return Err(AxcError::ChecksumMismatch {
                chunk_id,
                expected: checksum_comp,
                got: actual_comp,
            });
        }

        Ok((ChunkRecord { codec_id, chunk_id, original_len, compressed_len, checksum_orig, checksum_comp }, data))
    }
}

// ── Index (File Entry) ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub file_id: u64,
    pub mtime: u64,
    pub mode: u32,
    pub first_chunk: u64,
    pub chunk_count: u32,
    pub original_size: u64,
    pub name: String,
}

impl FileEntry {
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let name_bytes = self.name.as_bytes();
        w.write_all(&[ENTRY_TAG])?;
        w.write_all(&self.file_id.to_le_bytes())?;
        w.write_all(&self.mtime.to_le_bytes())?;
        w.write_all(&self.mode.to_le_bytes())?;
        w.write_all(&self.first_chunk.to_le_bytes())?;
        w.write_all(&self.chunk_count.to_le_bytes())?;
        w.write_all(&self.original_size.to_le_bytes())?;
        w.write_all(&(name_bytes.len() as u16).to_le_bytes())?;
        w.write_all(name_bytes)?;
        Ok(())
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self> {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag).map_err(AxcError::Io)?;
        if tag[0] != ENTRY_TAG {
            return Err(AxcError::InvalidHeader(format!("expected entry tag {ENTRY_TAG:#04x}, got {:#04x}", tag[0])));
        }

        let mut buf = [0u8; 8 + 8 + 4 + 8 + 4 + 8 + 2]; // 42 bytes
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        let file_id     = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let mtime       = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let mode        = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let first_chunk = u64::from_le_bytes(buf[20..28].try_into().unwrap());
        let chunk_count = u32::from_le_bytes(buf[28..32].try_into().unwrap());
        let original_size = u64::from_le_bytes(buf[32..40].try_into().unwrap());
        let name_len    = u16::from_le_bytes(buf[40..42].try_into().unwrap()) as usize;

        let mut name_bytes = vec![0u8; name_len];
        r.read_exact(&mut name_bytes).map_err(AxcError::Io)?;

        let name = String::from_utf8(name_bytes)
            .map_err(|_| AxcError::InvalidHeader("non-UTF8 filename".into()))?;

        Ok(FileEntry { file_id, mtime, mode, first_chunk, chunk_count, original_size, name })
    }
}

// ── Footer ────────────────────────────────────────────────────────────────────

pub const FOOTER_SIZE: usize = 24;

#[derive(Debug, Clone)]
pub struct Footer {
    pub index_offset: u64,
    pub index_count: u32,
}

impl Footer {
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let mut buf = [0u8; 16];
        buf[0] = FOOTER_TAG;
        // [1..4] reserved
        buf[4..12].copy_from_slice(&self.index_offset.to_le_bytes());
        buf[12..16].copy_from_slice(&self.index_count.to_le_bytes());

        let checksum = Crc32::oneshot(&buf);
        w.write_all(&buf)?;
        w.write_all(&checksum.to_le_bytes())?;
        w.write_all(MAGIC_END)?;
        Ok(())
    }

    pub fn read<R: Read + Seek>(r: &mut R) -> Result<Self> {
        r.seek(SeekFrom::End(-(FOOTER_SIZE as i64))).map_err(AxcError::Io)?;
        let mut buf = [0u8; FOOTER_SIZE];
        r.read_exact(&mut buf).map_err(AxcError::Io)?;

        if &buf[20..24] != MAGIC_END {
            return Err(AxcError::InvalidMagic);
        }

        let body = &buf[0..16];
        let stored_crc = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let actual_crc = Crc32::oneshot(body);

        if stored_crc != actual_crc {
            return Err(AxcError::CorruptIndex);
        }

        if body[0] != FOOTER_TAG {
            return Err(AxcError::InvalidHeader("bad footer tag".into()));
        }

        let index_offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
        let index_count = u32::from_le_bytes(body[12..16].try_into().unwrap());

        Ok(Footer { index_offset, index_count })
    }
}
