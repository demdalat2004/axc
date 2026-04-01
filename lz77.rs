//! LZ77 compressor with hash-chain match finder.
//!
//! Token encoding (byte stream fed to ANS):
//!   LITERAL: 0x00 <byte>          (2 bytes, works for ALL byte values uniformly)
//!   MATCH:   0x01 <off_hi> <off_lo> <len_minus_min>  (4 bytes)
//!
//! Using a 2-byte scheme keeps the encoding unambiguous regardless of byte value.

const WINDOW_BITS: usize = 16;
pub const WINDOW_SIZE: usize = 1 << WINDOW_BITS; // 64 KiB
const HASH_BITS: usize = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const MIN_MATCH: usize = 4;
const MAX_MATCH: usize = 255 + MIN_MATCH;
const MAX_CHAIN_DEPTH: usize = 128;

#[inline]
fn hash4(data: &[u8], pos: usize) -> usize {
    if pos + 4 > data.len() { return 0; }
    let v = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
    ((v.wrapping_mul(0x9E37_79B1)) >> (32 - HASH_BITS)) as usize & HASH_MASK
}

const TAG_LIT: u8   = 0x00;
const TAG_MATCH: u8 = 0x01;

pub fn lz77_compress(input: &[u8]) -> Vec<u8> {
    if input.is_empty() { return vec![]; }

    let n = input.len();
    let mut out = Vec::with_capacity(n + n / 4);

    let mut hash_head = vec![u32::MAX; HASH_SIZE];
    let mut prev = vec![u32::MAX; WINDOW_SIZE];
    let mut pos = 0usize;

    while pos < n {
        if pos + MIN_MATCH > n {
            out.push(TAG_LIT);
            out.push(input[pos]);
            pos += 1;
            continue;
        }

        let h = hash4(input, pos);
        let mut best_len = MIN_MATCH - 1;
        let mut best_offset = 0usize;

        let mut cur = hash_head[h];
        let mut depth = 0;

        while cur != u32::MAX && depth < MAX_CHAIN_DEPTH {
            let cp = cur as usize;
            if cp >= pos { break; }
            let offset = pos - cp;
            if offset > WINDOW_SIZE { break; }

            let max_len = (n - pos).min(MAX_MATCH);
            let mut mlen = 0;
            while mlen < max_len && input[cp + mlen] == input[pos + mlen] {
                mlen += 1;
            }

            if mlen > best_len {
                best_len = mlen;
                best_offset = offset;
                if mlen == MAX_MATCH { break; }
            }

            cur = prev[cp & (WINDOW_SIZE - 1)];
            depth += 1;
        }

        // Update hash chain for current position
        prev[pos & (WINDOW_SIZE - 1)] = hash_head[h];
        hash_head[h] = pos as u32;

        if best_len >= MIN_MATCH {
            // Emit match token: TAG_MATCH <off_hi> <off_lo> <len - MIN_MATCH>
            out.push(TAG_MATCH);
            out.push((best_offset >> 8) as u8);
            out.push(best_offset as u8);
            out.push((best_len - MIN_MATCH) as u8);

            // Update hashes for skipped positions
            for skip in 1..best_len.min(8) {
                let sp = pos + skip;
                if sp + MIN_MATCH <= n {
                    let sh = hash4(input, sp);
                    prev[sp & (WINDOW_SIZE - 1)] = hash_head[sh];
                    hash_head[sh] = sp as u32;
                }
            }
            pos += best_len;
        } else {
            // Emit literal token: TAG_LIT <byte>
            out.push(TAG_LIT);
            out.push(input[pos]);
            pos += 1;
        }
    }

    out
}

pub fn lz77_decompress(tokens: &[u8], output_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(output_len);
    let mut i = 0;

    while i < tokens.len() {
        let tag = tokens[i];
        i += 1;

        match tag {
            TAG_LIT => {
                if i < tokens.len() {
                    out.push(tokens[i]);
                    i += 1;
                }
            }
            TAG_MATCH => {
                if i + 2 >= tokens.len() { break; }
                let off_hi = tokens[i] as usize;
                let off_lo = tokens[i + 1] as usize;
                let len_byte = tokens[i + 2] as usize;
                i += 3;

                let offset = (off_hi << 8) | off_lo;
                let mlen = len_byte + MIN_MATCH;

                if offset == 0 || offset > out.len() {
                    // Corrupt — skip this token
                    continue;
                }

                let start = out.len() - offset;
                // Copy byte-by-byte to handle overlapping (run-length) matches
                for k in 0..mlen {
                    let byte = out[start + k];
                    out.push(byte);
                }
            }
            _ => {
                // Unknown tag — treat as corrupt, stop
                break;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_repeated() {
        let input: Vec<u8> = b"abcabcabcabc_hello_hello_hello".to_vec();
        let tokens = lz77_compress(&input);
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn roundtrip_binary() {
        let input: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let tokens = lz77_compress(&input);
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn compresses_repetitive_data() {
        let input: Vec<u8> = b"x".repeat(4096);
        let tokens = lz77_compress(&input);
        assert!(tokens.len() < input.len() / 4, "should compress well: token_len={}", tokens.len());
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn roundtrip_high_bytes() {
        let input: Vec<u8> = vec![0xAB, 0xCD, 0xEF, 0xAB, 0xCD, 0xEF, 0x80, 0x81, 0xFF, 0x80, 0x81];
        let tokens = lz77_compress(&input);
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn roundtrip_all_zeros() {
        let input = vec![0u8; 1024];
        let tokens = lz77_compress(&input);
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }

    #[test]
    fn roundtrip_single_byte() {
        let input = vec![0xFFu8];
        let tokens = lz77_compress(&input);
        let output = lz77_decompress(&tokens, input.len());
        assert_eq!(output, input);
    }
}
