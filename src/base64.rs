//! Minimal standard-alphabet Base64 encode/decode, enough for HTTP Basic
//! credentials. Avoids pulling in a dependency for such a small need.

// `encode` (and hence the alphabet) is currently only exercised by tests.
#[cfg_attr(not(test), allow(dead_code))]
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard Base64 with `=` padding.
#[cfg_attr(not(test), allow(dead_code))]
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard Base64, ignoring padding and whitespace. Returns `None` on
/// invalid input.
pub fn decode(input: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let symbols: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();

    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    for chunk in symbols.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | value(c)?;
        }
        // Left-align to 24 bits, then take the whole bytes the chunk encodes.
        n <<= 6 * (4 - chunk.len());
        let bytes = [(n >> 16 & 0xff) as u8, (n >> 8 & 0xff) as u8, (n & 0xff) as u8];
        let count = chunk.len() * 6 / 8;
        out.extend_from_slice(&bytes[..count]);
    }
    Some(out)
}
