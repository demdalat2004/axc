//! CRC-32 (ISO 3309 / ITU-T V.42) — table-driven, no external deps.

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            if c & 1 != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_table();

#[derive(Clone, Default)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub fn new() -> Self {
        Crc32 { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let idx = ((self.state ^ byte as u32) & 0xFF) as usize;
            self.state = CRC_TABLE[idx] ^ (self.state >> 8);
        }
    }

    pub fn finalise(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }

    pub fn oneshot(data: &[u8]) -> u32 {
        let mut h = Self::new();
        h.update(data);
        h.finalise()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_vector() {
        // CRC32("123456789") == 0xCBF43926
        assert_eq!(Crc32::oneshot(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_empty() {
        assert_eq!(Crc32::oneshot(b""), 0x0000_0000);
    }
}
