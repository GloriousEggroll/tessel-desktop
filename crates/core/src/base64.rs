//! Python `base64` subset: standard alphabet with padding, strict decoding
//! (`b64decode(text, validate=True)`), and the unpadded URL-safe form used by
//! `secrets.token_urlsafe`.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URLSAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `base64.b64encode(data).decode()`.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(STANDARD[(n >> 18) as usize & 63] as char);
        out.push(STANDARD[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { STANDARD[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { STANDARD[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `base64.urlsafe_b64encode(data).rstrip(b"=")`.
pub fn encode_urlsafe_unpadded(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..chunk.len() + 1 {
            out.push(URLSAFE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// `base64.b64decode(text, validate=True)`; `None` where Python raises.
pub fn decode_strict(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let mut n = 0u32;
        let mut padding = 0;
        for (position, &byte) in chunk.iter().enumerate() {
            let value = if byte == b'=' {
                if !last || position < 2 {
                    return None;
                }
                padding += 1;
                0
            } else {
                if padding > 0 {
                    return None;
                }
                STANDARD.iter().position(|&c| c == byte)? as u32
            };
            n = n << 6 | value;
        }
        out.push((n >> 16) as u8);
        if padding < 2 {
            out.push((n >> 8) as u8);
        }
        if padding < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip_and_strictness() {
        for data in [&b""[..], b"r", b"rg", b"rgb", b"\xff\x00\x10\x80"] {
            assert_eq!(decode_strict(&encode(data)).unwrap(), data);
        }
        assert_eq!(encode(b"rgb"), "cmdi");
        assert!(decode_strict("broken").is_none());
        assert!(decode_strict("cm=i").is_none());
        assert!(decode_strict("c$di").is_none());
        assert_eq!(encode_urlsafe_unpadded(b"\xfb\xff"), "-_8");
    }
}
