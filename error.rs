use std::fmt;
use std::io;

#[derive(Debug)]
pub enum AxcError {
    Io(io::Error),
    InvalidMagic,
    InvalidHeader(String),
    InvalidChunk(String),
    ChecksumMismatch { chunk_id: u64, expected: u32, got: u32 },
    DecompressionBomb { output_size: u64, limit: u64 },
    PathTraversal(String),
    UnsupportedVersion(u8),
    CorruptIndex,
    CodecError(String),
    EmptyArchive,
    FileNotFound(String),
}

impl fmt::Display for AxcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AxcError::Io(e) => write!(f, "I/O error: {e}"),
            AxcError::InvalidMagic => write!(f, "Not a valid AXC archive (bad magic bytes)"),
            AxcError::InvalidHeader(s) => write!(f, "Invalid header: {s}"),
            AxcError::InvalidChunk(s) => write!(f, "Invalid chunk: {s}"),
            AxcError::ChecksumMismatch { chunk_id, expected, got } => {
                write!(f, "Checksum mismatch in chunk {chunk_id}: expected {expected:#010x}, got {got:#010x}")
            }
            AxcError::DecompressionBomb { output_size, limit } => {
                write!(f, "Decompression bomb: output {output_size} bytes exceeds limit {limit} bytes")
            }
            AxcError::PathTraversal(p) => write!(f, "Path traversal attempt blocked: '{p}'"),
            AxcError::UnsupportedVersion(v) => write!(f, "Unsupported AXC format version: {v}"),
            AxcError::CorruptIndex => write!(f, "Archive index is corrupt or missing"),
            AxcError::CodecError(s) => write!(f, "Codec error: {s}"),
            AxcError::EmptyArchive => write!(f, "Archive contains no files"),
            AxcError::FileNotFound(s) => write!(f, "File not found in archive: '{s}'"),
        }
    }
}

impl std::error::Error for AxcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AxcError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for AxcError {
    fn from(e: io::Error) -> Self {
        AxcError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, AxcError>;
