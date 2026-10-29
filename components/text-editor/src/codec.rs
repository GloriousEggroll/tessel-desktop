//! Strict text codecs with Python `codecs` names and error texts.
//!
//! The editor offers exactly the Python twin's `ENCODINGS`. UTF-8/16/32,
//! ISO-8859-1 and windows-1252 (with Python's undefined bytes 0x81, 0x8D,
//! 0x8F, 0x90, 0x9D) are implemented here; KOI8-R, Shift_JIS, GB18030 and
//! Big5 use `encoding_rs` without replacement (WHATWG tables, see PARITY.md).

/// One codec, named as `codecs.lookup(name).name` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Utf8,
    Utf8Sig,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    Cp1252,
    Latin1,
    Koi8R,
    ShiftJis,
    Gb18030,
    Big5,
}

pub const BOM_UTF8: &[u8] = b"\xef\xbb\xbf";
pub const BOM_UTF16_LE: &[u8] = b"\xff\xfe";
pub const BOM_UTF16_BE: &[u8] = b"\xfe\xff";
pub const BOM_UTF32_LE: &[u8] = b"\xff\xfe\x00\x00";
pub const BOM_UTF32_BE: &[u8] = b"\x00\x00\xfe\xff";

/// `encodings.normalize_encoding` + lowercase: non-alphanumerics (except `.`) become `_`.
fn normalize(name: &str) -> String {
    let mut out = String::new();
    let mut punct = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            punct = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            punct = true;
        }
    }
    out
}

impl Codec {
    /// `codecs.lookup(name)`; `Err` is Python's `LookupError` text.
    pub fn lookup(name: &str) -> Result<Codec, String> {
        let codec = match normalize(name).as_str() {
            "utf_8" | "utf8" | "u8" | "utf" | "utf8_ucs2" | "utf8_ucs4" | "cp65001" => Codec::Utf8,
            "utf_8_sig" | "utf8_sig" => Codec::Utf8Sig,
            "utf_16_le" | "utf_16le" | "utf16le" | "utf16_le" | "unicodelittleunmarked" => Codec::Utf16Le,
            "utf_16_be" | "utf_16be" | "utf16be" | "utf16_be" | "unicodebigunmarked" => Codec::Utf16Be,
            "utf_32_le" | "utf_32le" | "utf32le" | "utf32_le" => Codec::Utf32Le,
            "utf_32_be" | "utf_32be" | "utf32be" | "utf32_be" => Codec::Utf32Be,
            "cp1252" | "windows_1252" | "1252" => Codec::Cp1252,
            "latin_1" | "latin1" | "latin" | "l1" | "iso8859_1" | "iso_8859_1" | "iso8859" | "8859" | "cp819"
            | "iso_ir_100" | "csisolatin1" | "ibm819" | "iso_8859_1_1987" => Codec::Latin1,
            "koi8_r" | "koi8r" | "cskoi8r" => Codec::Koi8R,
            "shift_jis" | "shiftjis" | "sjis" | "s_jis" | "csshiftjis" => Codec::ShiftJis,
            "gb18030" | "gb18030_2000" => Codec::Gb18030,
            "big5" | "big5_tw" | "csbig5" => Codec::Big5,
            _ => return Err(format!("unknown encoding: {name}")),
        };
        Ok(codec)
    }

    /// `codecs.lookup(..).name`.
    pub fn name(self) -> &'static str {
        match self {
            Codec::Utf8 => "utf-8",
            Codec::Utf8Sig => "utf-8-sig",
            Codec::Utf16Le => "utf-16-le",
            Codec::Utf16Be => "utf-16-be",
            Codec::Utf32Le => "utf-32-le",
            Codec::Utf32Be => "utf-32-be",
            Codec::Cp1252 => "cp1252",
            Codec::Latin1 => "iso8859-1",
            Codec::Koi8R => "koi8-r",
            Codec::ShiftJis => "shift_jis",
            Codec::Gb18030 => "gb18030",
            Codec::Big5 => "big5",
        }
    }

    /// The codec name Python puts in Unicode error messages.
    fn error_name(self) -> &'static str {
        match self {
            Codec::Utf8 | Codec::Utf8Sig => "utf-8",
            Codec::Cp1252 | Codec::Koi8R => "charmap",
            Codec::Latin1 => "latin-1",
            other => other.name(),
        }
    }

    fn whatwg(self) -> Option<&'static encoding_rs::Encoding> {
        match self {
            Codec::Koi8R => Some(encoding_rs::KOI8_R),
            Codec::ShiftJis => Some(encoding_rs::SHIFT_JIS),
            Codec::Gb18030 => Some(encoding_rs::GB18030),
            Codec::Big5 => Some(encoding_rs::BIG5),
            _ => None,
        }
    }

    /// `bytes.decode(name, errors="strict")`; `Err` is the UnicodeDecodeError text.
    pub fn decode(self, data: &[u8]) -> Result<String, String> {
        let fail = |position: usize, reason: &str| {
            let byte = data.get(position).copied().unwrap_or(0);
            format!("'{}' codec can't decode byte 0x{byte:02x} in position {position}: {reason}", self.error_name())
        };
        match self {
            Codec::Utf8 | Codec::Utf8Sig => {
                let (data, offset) = match data.strip_prefix(BOM_UTF8) {
                    Some(rest) if self == Codec::Utf8Sig => (rest, 3),
                    _ => (data, 0),
                };
                std::str::from_utf8(data).map(str::to_owned).map_err(|e| {
                    let reason = if e.error_len().is_none() { "unexpected end of data" } else { "invalid continuation byte" };
                    let position = e.valid_up_to() + offset;
                    format!("'utf-8' codec can't decode byte 0x{:02x} in position {position}: {reason}",
                            data.get(e.valid_up_to()).copied().unwrap_or(0))
                })
            }
            Codec::Utf16Le | Codec::Utf16Be => {
                let units: Vec<u16> = data.chunks(2).filter(|c| c.len() == 2)
                    .map(|c| if self == Codec::Utf16Le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                    .collect();
                let mut text = String::with_capacity(units.len());
                let mut index = 0;
                for result in char::decode_utf16(units.iter().copied()) {
                    match result {
                        Ok(ch) => {
                            index += ch.len_utf16() * 2;
                            text.push(ch);
                        }
                        Err(_) => return Err(fail(index, "illegal encoding")),
                    }
                }
                if data.len() % 2 == 1 {
                    return Err(fail(data.len() - 1, "truncated data"));
                }
                Ok(text)
            }
            Codec::Utf32Le | Codec::Utf32Be => {
                let mut text = String::with_capacity(data.len() / 4);
                for (index, chunk) in data.chunks(4).enumerate() {
                    if chunk.len() < 4 {
                        return Err(fail(index * 4, "truncated data"));
                    }
                    let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
                    let value = if self == Codec::Utf32Le { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) };
                    match char::from_u32(value) {
                        Some(ch) => text.push(ch),
                        None if (0xd800..0xe000).contains(&value) => return Err(fail(index * 4, "code point in surrogate code point range(0xd800, 0xe000)")),
                        None => return Err(fail(index * 4, "code point not in range(0x110000)")),
                    }
                }
                Ok(text)
            }
            Codec::Latin1 => Ok(data.iter().map(|&b| b as char).collect()),
            Codec::Cp1252 | Codec::Koi8R => {
                let table = single_byte_table(self);
                let mut text = String::with_capacity(data.len());
                for (position, &byte) in data.iter().enumerate() {
                    match table[byte as usize] {
                        Some(ch) => text.push(ch),
                        None => return Err(fail(position, "character maps to <undefined>")),
                    }
                }
                Ok(text)
            }
            Codec::ShiftJis | Codec::Gb18030 | Codec::Big5 => {
                let encoding = self.whatwg().expect("multibyte codec");
                match encoding.decode_without_bom_handling_and_without_replacement(data) {
                    Some(text) => Ok(text.into_owned()),
                    None => {
                        // Find the first undecodable byte for the message.
                        let mut decoder = encoding.new_decoder_without_bom_handling();
                        let mut out = String::with_capacity(data.len() * 3 + 16);
                        let (result, read) = decoder.decode_to_string_without_replacement(data, &mut out, true);
                        let position = match result {
                            encoding_rs::DecoderResult::Malformed(bad, consumed) => read.saturating_sub(bad as usize + consumed as usize),
                            _ => read,
                        };
                        Err(fail(position, "illegal multibyte sequence"))
                    }
                }
            }
        }
    }

    /// `str.encode(name, errors="strict")`; `Err` is the UnicodeEncodeError text.
    pub fn encode(self, text: &str) -> Result<Vec<u8>, String> {
        match self {
            Codec::Utf8 => Ok(text.as_bytes().to_vec()),
            Codec::Utf8Sig => {
                let mut data = BOM_UTF8.to_vec();
                data.extend_from_slice(text.as_bytes());
                Ok(data)
            }
            Codec::Utf16Le => Ok(text.encode_utf16().flat_map(u16::to_le_bytes).collect()),
            Codec::Utf16Be => Ok(text.encode_utf16().flat_map(u16::to_be_bytes).collect()),
            Codec::Utf32Le => Ok(text.chars().flat_map(|c| (c as u32).to_le_bytes()).collect()),
            Codec::Utf32Be => Ok(text.chars().flat_map(|c| (c as u32).to_be_bytes()).collect()),
            Codec::Latin1 | Codec::Cp1252 | Codec::Koi8R => {
                let reverse = |ch: char| -> Option<u8> {
                    if self == Codec::Latin1 {
                        return u8::try_from(ch as u32).ok();
                    }
                    let table = single_byte_table(self);
                    table.iter().position(|entry| *entry == Some(ch)).map(|p| p as u8)
                };
                let chars: Vec<char> = text.chars().collect();
                let mut data = Vec::with_capacity(chars.len());
                let mut index = 0;
                while index < chars.len() {
                    match reverse(chars[index]) {
                        Some(byte) => data.push(byte),
                        None => {
                            // Python reports the whole run of unencodable characters.
                            let mut end = index + 1;
                            while end < chars.len() && reverse(chars[end]).is_none() {
                                end += 1;
                            }
                            let reason = if self == Codec::Latin1 { "ordinal not in range(256)" } else { "character maps to <undefined>" };
                            return Err(encode_error(self.error_name(), &chars, index, end, reason));
                        }
                    }
                    index += 1;
                }
                Ok(data)
            }
            Codec::ShiftJis | Codec::Gb18030 | Codec::Big5 => {
                let encoding = self.whatwg().expect("multibyte codec");
                let mut encoder = encoding.new_encoder();
                let mut out = Vec::with_capacity(text.len() * 2 + 16);
                let mut source = text;
                let mut consumed = 0usize;
                loop {
                    let needed = encoder.max_buffer_length_from_utf8_without_replacement(source.len()).unwrap_or(source.len() * 4 + 16);
                    out.reserve(needed);
                    let (result, read) = encoder.encode_from_utf8_to_vec_without_replacement(source, &mut out, true);
                    match result {
                        encoding_rs::EncoderResult::InputEmpty => return Ok(out),
                        encoding_rs::EncoderResult::OutputFull => {
                            consumed += read;
                            source = &text[consumed..];
                        }
                        encoding_rs::EncoderResult::Unmappable(ch) => {
                            let byte_index = consumed + read - ch.len_utf8();
                            let chars: Vec<char> = text.chars().collect();
                            let index = text[..byte_index].chars().count();
                            return Err(encode_error(self.error_name(), &chars, index, index + 1, "illegal multibyte sequence"));
                        }
                    }
                }
            }
        }
    }
}

/// Python's `'\xNN'`, `'\uNNNN'`, `'\UNNNNNNNN'` escapes in Unicode errors.
fn escape(ch: char) -> String {
    let value = ch as u32;
    if value <= 0xff {
        format!("\\x{value:02x}")
    } else if value <= 0xffff {
        format!("\\u{value:04x}")
    } else {
        format!("\\U{value:08x}")
    }
}

fn encode_error(codec: &str, chars: &[char], start: usize, end: usize, reason: &str) -> String {
    if end - start == 1 {
        format!("'{codec}' codec can't encode character '{}' in position {start}: {reason}", escape(chars[start]))
    } else {
        format!("'{codec}' codec can't encode characters in position {start}-{}: {reason}", end - 1)
    }
}

const CP1252_HIGH: [u16; 32] = [
    0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017d, 0,
    0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
];

fn single_byte_table(codec: Codec) -> &'static [Option<char>; 256] {
    use std::sync::OnceLock;
    static CP1252: OnceLock<[Option<char>; 256]> = OnceLock::new();
    static KOI8R: OnceLock<[Option<char>; 256]> = OnceLock::new();
    match codec {
        Codec::Cp1252 => CP1252.get_or_init(|| {
            let mut table = [None; 256];
            for (byte, slot) in table.iter_mut().enumerate() {
                *slot = match byte {
                    0x80..=0x9f => Some(CP1252_HIGH[byte - 0x80]).filter(|v| *v != 0).and_then(|v| char::from_u32(v as u32)),
                    _ => char::from_u32(byte as u32),
                };
            }
            table
        }),
        _ => KOI8R.get_or_init(|| {
            let mut table = [None; 256];
            for (byte, slot) in table.iter_mut().enumerate() {
                let input = [byte as u8];
                let (text, _) = encoding_rs::KOI8_R.decode_without_bom_handling(&input);
                *slot = text.chars().next();
            }
            table
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_python_codecs_lookup() {
        for (name, expected) in [("utf-8", "utf-8"), ("UTF8", "utf-8"), ("windows-1252", "cp1252"), ("iso-8859-1", "iso8859-1"),
                                 ("latin1", "iso8859-1"), ("shift_jis", "shift_jis"), ("koi8-r", "koi8-r"), ("utf-16-le", "utf-16-le")] {
            assert_eq!(Codec::lookup(name).unwrap().name(), expected, "{name}");
        }
        assert_eq!(Codec::lookup("klingon").unwrap_err(), "unknown encoding: klingon");
    }

    #[test]
    fn error_texts_match_python() {
        assert_eq!(Codec::Latin1.encode("🖥").unwrap_err(),
                   "'latin-1' codec can't encode character '\\U0001f5a5' in position 0: ordinal not in range(256)");
        assert_eq!(Codec::Latin1.encode("a€€b").unwrap_err(),
                   "'latin-1' codec can't encode characters in position 1-2: ordinal not in range(256)");
        assert_eq!(Codec::Cp1252.encode("\u{81}x").unwrap_err(),
                   "'charmap' codec can't encode character '\\x81' in position 0: character maps to <undefined>");
        assert_eq!(Codec::ShiftJis.encode("€").unwrap_err(),
                   "'shift_jis' codec can't encode character '\\u20ac' in position 0: illegal multibyte sequence");
        assert_eq!(Codec::Koi8R.encode("€€").unwrap_err(),
                   "'charmap' codec can't encode characters in position 0-1: character maps to <undefined>");
        assert_eq!(Codec::Cp1252.decode(b"\x81").unwrap_err(),
                   "'charmap' codec can't decode byte 0x81 in position 0: character maps to <undefined>");
    }

    #[test]
    fn single_byte_tables() {
        assert_eq!(Codec::Cp1252.decode(b"caf\xe9 \x80").unwrap(), "café €");
        assert_eq!(Codec::Cp1252.encode("café €").unwrap(), b"caf\xe9 \x80");
        assert_eq!(Codec::Koi8R.decode(b"\x9a").unwrap(), "\u{a0}");
        assert_eq!(Codec::Koi8R.encode("Привет").unwrap(), Codec::Koi8R.encode(&Codec::Koi8R.decode(&Codec::Koi8R.encode("Привет").unwrap()).unwrap()).unwrap());
        assert_eq!(Codec::Latin1.decode(b"\xff").unwrap(), "ÿ");
    }

    #[test]
    fn utf16_and_utf32_are_strict() {
        assert_eq!(Codec::Utf16Le.encode("αβ").unwrap(), b"\xb1\x03\xb2\x03");
        assert!(Codec::Utf16Le.decode(b"a").is_err());
        assert!(Codec::Utf16Le.decode(b"\x00\xd8").is_err());
        assert_eq!(Codec::Utf32Be.decode(&Codec::Utf32Be.encode("🖥x").unwrap()).unwrap(), "🖥x");
        assert!(Codec::Utf32Le.decode(b"\x00\xd8\x00\x00").is_err());
        assert_eq!(Codec::Utf8Sig.decode(b"\xef\xbb\xbfhi").unwrap(), "hi");
        assert_eq!(Codec::Gb18030.decode(&Codec::Gb18030.encode("中文").unwrap()).unwrap(), "中文");
    }
}
