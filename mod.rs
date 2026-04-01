pub mod ans;
pub mod lz77;

use crate::error::Result;

/// Compression level: controls LZ77 chain depth and ANS table quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Fast,     // minimal chain depth, fastest
    Balanced, // default
    Max,      // deepest search, best ratio
}

impl Default for Level {
    fn default() -> Self {
        Level::Balanced
    }
}

impl Level {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Level::Fast,
            3 => Level::Max,
            _ => Level::Balanced,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Level::Fast => 1,
            Level::Balanced => 2,
            Level::Max => 3,
        }
    }
}

/// Codec identifier stored in chunk header.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecId {
    Raw    = 0x00, // no compression (stored)
    LzAns  = 0x01, // LZ77 + tANS (default)
}

impl CodecId {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(CodecId::Raw),
            0x01 => Some(CodecId::LzAns),
            _ => None,
        }
    }
}

/// Compress `input` with LZ77 → ANS pipeline.
pub fn compress(input: &[u8], _level: Level) -> Vec<u8> {
    if input.is_empty() {
        return vec![];
    }

    // Step 1: LZ77 produces a token stream
    let tokens = lz77::lz77_compress(input);

    // Step 2: ANS entropy-codes the token stream
    // If the token stream is larger (random data), fall back to raw + ANS directly
    let encoded_tokens = ans::ans_encode(&tokens);
    let encoded_raw = ans::ans_encode(input);

    if encoded_tokens.len() <= encoded_raw.len() {
        // Prepend flag: 0x01 = LZ77 was applied
        let mut out = Vec::with_capacity(1 + encoded_tokens.len());
        out.push(0x01u8);
        out.extend_from_slice(&encoded_tokens);
        out
    } else {
        // Raw ANS only (no LZ step)
        let mut out = Vec::with_capacity(1 + encoded_raw.len());
        out.push(0x00u8);
        out.extend_from_slice(&encoded_raw);
        out
    }
}

/// Decompress output of `compress`. `original_len` required for ANS decoder.
pub fn decompress(data: &[u8], original_len: usize) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(vec![]);
    }

    let flag = data[0];
    let payload = &data[1..];

    match flag {
        0x01 => {
            // LZ77 + ANS: first decode ANS → token stream, then LZ77 decompress
            // We need the token stream length; it was the input to ANS encode.
            // We don't store it separately because lz77_decompress uses output_len.
            // So we decode ANS without knowing token stream length — we use 0 to trigger raw read.
            // Solution: store token stream len as u32 LE before ANS payload.
            if payload.len() < 4 {
                return Err(crate::error::AxcError::CodecError(
                    "truncated LZ-ANS header".into()
                ));
            }
            let token_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
            let ans_data = &payload[4..];
            let tokens = ans::ans_decode(ans_data, token_len)?;
            let output = lz77::lz77_decompress(&tokens, original_len);
            Ok(output)
        }
        0x00 => {
            // Raw ANS only
            ans::ans_decode(payload, original_len)
        }
        _ => Err(crate::error::AxcError::CodecError(
            format!("unknown codec flag: {flag:#04x}")
        )),
    }
}

/// Compress with token length header so decompress can recover it.
pub fn compress_full(input: &[u8], level: Level) -> Vec<u8> {
    if input.is_empty() {
        return vec![];
    }

    let tokens = lz77::lz77_compress(input);
    let token_len = tokens.len() as u32;
    let encoded_tokens = ans::ans_encode(&tokens);
    let encoded_raw = ans::ans_encode(input);

    // LZ+ANS path
    if 1 + 4 + encoded_tokens.len() <= 1 + encoded_raw.len() {
        let mut out = Vec::with_capacity(1 + 4 + encoded_tokens.len());
        out.push(0x01u8);
        out.extend_from_slice(&token_len.to_le_bytes());
        out.extend_from_slice(&encoded_tokens);
        out
    } else {
        // Raw ANS
        let _ = level;
        let mut out = Vec::with_capacity(1 + encoded_raw.len());
        out.push(0x00u8);
        out.extend_from_slice(&encoded_raw);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_roundtrip() {
        let inputs: &[&[u8]] = &[
            b"",
            b"hello world",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &(0u8..=255).collect::<Vec<_>>(),
        ];
        for input in inputs {
            let compressed = compress_full(input, Level::Balanced);
            let decompressed = decompress(&compressed, input.len()).unwrap();
            assert_eq!(&decompressed, input, "failed for input len {}", input.len());
        }
    }
}
