//! Python `str` behaviour the generators rely on: `splitlines`, `split()`,
//! universal-newline decoding, `html.escape`/`html.unescape`, `repr`.

pub use suite_core::pyjson::{casefold, is_py_space, py_strip};

fn is_line_break(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `str.splitlines(keepends)`.
pub fn splitlines(text: &str, keepends: bool) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if !is_line_break(ch) {
            continue;
        }
        let mut end = index + ch.len_utf8();
        if ch == '\r' {
            if let Some(&(_, '\n')) = chars.peek() {
                chars.next();
                end += 1;
            }
        }
        lines.push(if keepends { &text[start..end] } else { &text[start..index] });
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `str.split()` (no separator).
pub fn split_whitespace(text: &str) -> Vec<&str> {
    text.split(is_py_space).filter(|word| !word.is_empty()).collect()
}

/// `str.rstrip(chars)`.
pub fn rstrip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_end_matches(|c| chars.contains(c))
}

/// `str.rstrip()`.
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_py_space)
}

/// `bytes.decode()` (strict UTF-8) with Python's `UnicodeDecodeError` text.
pub fn decode(data: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(data) {
        Ok(text) => Ok(text.to_owned()),
        Err(error) => {
            let position = error.valid_up_to();
            let byte = data[position];
            let reason = match error.error_len() {
                None => "unexpected end of data",
                Some(_) if (0x80..0xc2).contains(&byte) || byte >= 0xf5 => "invalid start byte",
                Some(_) => "invalid continuation byte",
            };
            Err(format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}"))
        }
    }
}

/// `Path.read_text()` decoding: UTF-8 with universal newlines (`\r\n`, `\r` → `\n`).
pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `html.escape(text, quote)`.
pub fn html_escape(text: &str, quote: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quote => out.push_str("&quot;"),
            '\'' if quote => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

const CP1252: [u32; 32] = [
    0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8d,
    0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc, 0x2122, 0x0161, 0x203a,
    0x0153, 0x9d, 0x017e, 0x0178,
];

fn numeric_reference(code: u64) -> String {
    if code == 0x0d {
        return "\r".into();
    }
    if (0x80..=0x9f).contains(&code) {
        return char::from_u32(CP1252[(code - 0x80) as usize]).unwrap().to_string();
    }
    if code == 0 || (0xd800..=0xdfff).contains(&code) || code > 0x10ffff {
        return "\u{fffd}".into();
    }
    char::from_u32(code as u32).map(String::from).unwrap_or_else(|| "\u{fffd}".into())
}

/// `html.unescape` for the references valid XML text can hold (numeric references and the
/// XML/HTML core names); other names are left untouched.
pub fn html_unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let Some(offset) = text[index..].find('&') else {
            out.push_str(&text[index..]);
            break;
        };
        out.push_str(&text[index..index + offset]);
        let start = index + offset;
        let rest = &text[start + 1..];
        if let Some(digits) = rest.strip_prefix('#') {
            let (hex, body) = match digits.strip_prefix(['x', 'X']) {
                Some(body) => (true, body),
                None => (false, digits),
            };
            let count = body
                .bytes()
                .take_while(|b| if hex { b.is_ascii_hexdigit() } else { b.is_ascii_digit() })
                .count();
            if count > 0 {
                let number = &body[..count];
                let code = u64::from_str_radix(number, if hex { 16 } else { 10 }).unwrap_or(u64::MAX);
                out.push_str(&numeric_reference(code));
                let mut end = start + 1 + 1 + usize::from(hex) + count;
                if bytes.get(end) == Some(&b';') {
                    end += 1;
                }
                index = end;
                continue;
            }
        } else {
            // html5 names that also match without a trailing ';' come first.
            let names: [(&str, &str); 8] = [
                ("amp;", "&"), ("lt;", "<"), ("gt;", ">"), ("quot;", "\""), ("apos;", "'"),
                ("amp", "&"), ("lt", "<"), ("gt", ">"),
            ];
            let names_extra: [(&str, &str); 3] = [("quot", "\""), ("nbsp;", "\u{a0}"), ("nbsp", "\u{a0}")];
            if let Some((name, value)) = names.iter().chain(names_extra.iter()).find(|(name, _)| rest.starts_with(name)) {
                out.push_str(value);
                index = start + 1 + name.len();
                continue;
            }
        }
        out.push('&');
        index = start + 1;
    }
    out
}

/// `repr(str)`.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(list_of_str)`.
pub fn py_repr_list<S: AsRef<str>>(items: &[S]) -> String {
    format!("[{}]", items.iter().map(|s| py_repr(s.as_ref())).collect::<Vec<_>>().join(", "))
}

/// Python `\w` for one character (`str.isalnum()` or `_`).
pub fn is_word(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd\x0be\u{2028}f", true), ["a\n", "b\r\n", "c\r", "d\x0b", "e\u{2028}", "f"]);
        assert_eq!(splitlines("a\n\nb\n", false), ["a", "", "b"]);
        assert!(splitlines("", true).is_empty());
    }

    #[test]
    fn escape_and_unescape() {
        assert_eq!(html_escape("<a href=\"x\">'&", true), "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;");
        assert_eq!(html_escape("\"'", false), "\"'");
        assert_eq!(html_unescape("&lt;&lt;'X' &amp;&#65;&#x42;&quot;&apos;&bogus;"), "<<'X' &AB\"'&bogus;");
        assert_eq!(html_unescape("&#150;&#0;"), "\u{2013}\u{fffd}");
    }

    #[test]
    fn decode_errors_match_python() {
        assert_eq!(decode(b"ab\xff").unwrap_err(), "'utf-8' codec can't decode byte 0xff in position 2: invalid start byte");
        assert_eq!(decode(b"a\xe2\x82").unwrap_err(), "'utf-8' codec can't decode byte 0xe2 in position 1: unexpected end of data");
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(py_repr_list(&["sh", "-n", "/a b"]), "['sh', '-n', '/a b']");
        assert_eq!(py_repr("it's"), "\"it's\"");
    }
}
