//! rANS (range Asymmetric Numeral Systems) entropy coder.
//!
//! State x ∈ [L, bL) where L = TABLE_SIZE = 2^TABLE_LOG, b = 256.
//!
//! ENCODE (backwards over input):
//!   for each symbol s (in reverse order):
//!     renorm: while x ∈ [fs*b, ∞), push low byte, x >>= 8
//!     x = (x/fs)*L + cumfreq[s] + (x%fs)
//!   push state x as 8 bytes (LE)
//!   reverse the entire output buffer   ← bytes now in decode order
//!
//! DECODE (forwards):
//!   read initial state x from first 8 bytes
//!   for each symbol:
//!     slot = x % L
//!     s = alias[slot]
//!     x = fs * (x/L) + slot - cumfreq[s]
//!     renorm: while x < L, x = (x<<8) | next_byte

use crate::error::{AxcError, Result};

pub const TABLE_LOG: u32 = 11;
pub const TABLE_SIZE: u32 = 1 << TABLE_LOG; // L = 2048
const MAX_SYM: usize = 256;

// ── Frequencies ───────────────────────────────────────────────────────────────

pub fn count_freq(data: &[u8]) -> [u32; MAX_SYM] {
    let mut f = [0u32; MAX_SYM];
    for &b in data { f[b as usize] += 1; }
    f
}

pub fn normalise(raw: &[u32; MAX_SYM]) -> [u32; MAX_SYM] {
    let total: u64 = raw.iter().map(|&x| x as u64).sum();
    if total == 0 { return [0; MAX_SYM]; }

    let mut norm = [0u32; MAX_SYM];
    let mut assigned = 0u32;
    let mut rem: Vec<(u64, usize)> = Vec::new();

    for (i, &r) in raw.iter().enumerate() {
        if r == 0 { continue; }
        let exact = r as u64 * TABLE_SIZE as u64;
        let floor = (exact / total).max(1) as u32;
        norm[i] = floor;
        assigned += floor;
        rem.push((exact % total, i));
    }

    let need = TABLE_SIZE;
    if assigned < need {
        rem.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for &(_, i) in rem.iter().take((need - assigned) as usize) { norm[i] += 1; }
    } else if assigned > need {
        rem.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut excess = assigned - need;
        for &(_, i) in rem.iter() {
            if excess == 0 { break; }
            if norm[i] > 1 { norm[i] -= 1; excess -= 1; }
        }
    }

    norm
}

// ── CDF + alias lookup ────────────────────────────────────────────────────────

fn build_cdf(freq: &[u32; MAX_SYM]) -> [u32; MAX_SYM + 1] {
    let mut c = [0u32; MAX_SYM + 1];
    for i in 0..MAX_SYM { c[i + 1] = c[i] + freq[i]; }
    c
}

fn build_alias(freq: &[u32; MAX_SYM]) -> Vec<u8> {
    let mut a = vec![0u8; TABLE_SIZE as usize];
    let mut pos = 0u32;
    for s in 0..MAX_SYM {
        for _ in 0..freq[s] { a[pos as usize] = s as u8; pos += 1; }
    }
    a
}

// ── Freq header ───────────────────────────────────────────────────────────────

fn ser_freq(freq: &[u32; MAX_SYM]) -> Vec<u8> {
    let mut bmap = [0u8; 32];
    for i in 0..MAX_SYM { if freq[i] > 0 { bmap[i >> 3] |= 1 << (i & 7); } }
    let cnt = freq.iter().filter(|&&f| f > 0).count();
    let mut out = Vec::with_capacity(32 + cnt * 2);
    out.extend_from_slice(&bmap);
    for i in 0..MAX_SYM {
        if freq[i] > 0 { out.extend_from_slice(&(freq[i] as u16).to_le_bytes()); }
    }
    out
}

fn deser_freq(data: &[u8]) -> Result<([u32; MAX_SYM], usize)> {
    if data.len() < 32 { return Err(AxcError::CodecError("freq hdr short".into())); }
    let mut freq = [0u32; MAX_SYM];
    let mut pos = 32;
    for i in 0..MAX_SYM {
        if data[i >> 3] & (1 << (i & 7)) != 0 {
            if pos + 2 > data.len() { return Err(AxcError::CodecError("freq hdr truncated".into())); }
            freq[i] = u16::from_le_bytes([data[pos], data[pos + 1]]) as u32;
            pos += 2;
        }
    }
    Ok((freq, pos))
}

// ── Encode ────────────────────────────────────────────────────────────────────

pub fn ans_encode(input: &[u8]) -> Vec<u8> {
    if input.is_empty() { return vec![]; }

    let raw = count_freq(input);
    let freq = normalise(&raw);
    let cdf  = build_cdf(&freq);
    let l    = TABLE_SIZE as u64;

    // Encode backwards; collect renorm bytes in a Vec, then append final state.
    // We'll reverse the Vec at the end so decoder reads forward.
    let mut state: u64 = l; // start in [L, bL)
    let mut stream: Vec<u8> = Vec::with_capacity(input.len() + 8);

    for &sym in input.iter().rev() {
        let s  = sym as usize;
        let fs = freq[s] as u64;
        let cs = cdf[s] as u64;

        // Renorm: emit low bytes until state ∈ [fs, fs*256)
        // i.e. while x >= fs << 8, push byte, x >>= 8
        while state >= (fs << 8) {
            stream.push(state as u8);
            state >>= 8;
        }

        // Encode step
        state = (state / fs) * l + cs + (state % fs);
    }

    // Emit final state as little-endian u64 (8 bytes, high bytes first when reversed)
    stream.extend_from_slice(&state.to_le_bytes());
    // Reverse so decoder sees: [state_bytes(8)] [renorm_bytes...] in forward order
    stream.reverse();

    // Assemble output: freq_header | payload_len(u32) | payload
    let hdr = ser_freq(&freq);
    let mut out = Vec::with_capacity(hdr.len() + 4 + stream.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&(stream.len() as u32).to_le_bytes());
    out.extend_from_slice(&stream);
    out
}

// ── Decode ────────────────────────────────────────────────────────────────────

pub fn ans_decode(data: &[u8], out_len: usize) -> Result<Vec<u8>> {
    if data.is_empty() || out_len == 0 { return Ok(vec![]); }

    let (freq, hdr_end) = deser_freq(data)?;
    let rest = &data[hdr_end..];
    if rest.len() < 4 { return Err(AxcError::CodecError("missing payload len".into())); }
    let plen = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    let payload = rest.get(4..4 + plen)
        .ok_or_else(|| AxcError::CodecError("payload truncated".into()))?;

    if payload.len() < 8 { return Err(AxcError::CodecError("payload < 8 bytes".into())); }

    let cdf   = build_cdf(&freq);
    let alias = build_alias(&freq);
    let l     = TABLE_SIZE as u64;

    // Initial state: first 8 bytes (written by encoder as reversed LE u64)
    // After reversing during encode, the 8 state bytes are at the front.
    // They were pushed as LE u64 then reversed, so now they're in reverse-LE order = BE effectively.
    // Re-reverse those 8 bytes to recover the u64.
    let mut state_bytes = [0u8; 8];
    state_bytes.copy_from_slice(&payload[..8]);
    state_bytes.reverse(); // undo the outer reverse
    let mut x: u64 = u64::from_le_bytes(state_bytes);

    let mut pos = 8usize; // next byte to read from payload
    let mut output = vec![0u8; out_len];

    for byte in output.iter_mut() {
        // Decode step
        let slot = (x % l) as usize;
        let s    = alias[slot] as usize;
        let fs   = freq[s] as u64;
        let cs   = cdf[s] as u64;

        *byte = s as u8;

        x = fs * (x / l) + slot as u64 - cs;

        // Renorm: read bytes while x < L
        while x < l && pos < payload.len() {
            x = (x << 8) | payload[pos] as u64;
            pos += 1;
        }
    }

    Ok(output)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(input: &[u8]) {
        let enc = ans_encode(input);
        let dec = ans_decode(&enc, input.len()).unwrap();
        assert_eq!(dec, input, "roundtrip failed len={}", input.len());
    }

    #[test] fn simple()        { rt(b"hello world hello world hello"); }
    #[test] fn all_bytes()     { rt(&(0u8..=255).cycle().take(1024).collect::<Vec<_>>()); }
    #[test] fn single_sym()    { rt(&vec![0x41u8; 512]); }
    #[test] fn binary_zeroes() { rt(&vec![0u8; 256]); }
    #[test] fn one_byte()      { rt(&[0x42]); }
    #[test] fn high_bytes()    { rt(&[0xFF, 0x80, 0xFF, 0x80, 0xFF]); }
    #[test] fn all_ff()        { rt(&vec![0xFFu8; 100]); }
    #[test] fn two_symbols()   { rt(b"ababababababababab"); }
    #[test] fn long_random()   {
        // Pseudo-random but deterministic
        let mut v = Vec::with_capacity(8192);
        let mut s = 0xDEADBEEFu32;
        for _ in 0..8192 { s ^= s << 13; s ^= s >> 17; s ^= s << 5; v.push(s as u8); }
        rt(&v);
    }

    #[test]
    fn normalise_sums_correctly() {
        let raw = count_freq(b"abcabc xyz xyz xyz");
        let norm = normalise(&raw);
        assert_eq!(norm.iter().sum::<u32>(), TABLE_SIZE);
    }

    #[test]
    fn compresses_well() {
        let input: Vec<u8> = b"a".repeat(4096);
        let enc = ans_encode(&input);
        assert!(enc.len() < input.len() / 4, "enc.len()={}", enc.len());
    }
}
