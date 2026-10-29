//! Python `html.unescape` (full HTML5 entity table, generated from CPython) and
//! the tag stripping used by `status_backend.plain_text` and
//! `status_tray.tooltip_text`.

use super::html_entities::{HTML5, INVALID_CHARREFS, INVALID_CODEPOINTS};

fn html5(name: &str) -> Option<&'static str> {
    HTML5.binary_search_by(|(key, _)| (*key).cmp(name)).ok().map(|index| HTML5[index].1)
}

fn numeric(digits: &str, radix: u32) -> String {
    let trimmed = digits.trim_start_matches('0');
    // Anything longer than 8 significant digits is far beyond U+10FFFF.
    let number = if trimmed.len() > 8 { u64::MAX } else { u64::from_str_radix(if trimmed.is_empty() { "0" } else { trimmed }, radix).unwrap_or(u64::MAX) };
    if let Ok(small) = u32::try_from(number) {
        if let Some((_, value)) = INVALID_CHARREFS.iter().find(|(code, _)| *code == small) {
            return (*value).to_owned();
        }
    }
    if (0xD800..=0xDFFF).contains(&number) || number > 0x10FFFF {
        return "\u{FFFD}".into();
    }
    let number = number as u32;
    if INVALID_CODEPOINTS.contains(&number) {
        return String::new();
    }
    char::from_u32(number).map(String::from).unwrap_or_else(|| "\u{FFFD}".into())
}

/// `html.unescape(text)`.
pub fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(offset) = rest.find('&') {
        out.push_str(&rest[..offset]);
        let after = &rest[offset + 1..];
        if let Some(body) = after.strip_prefix('#') {
            let (radix, digits_start) = match body.as_bytes().first() {
                Some(b'x' | b'X') => (16, 1),
                _ => (10, 0),
            };
            let digits = &body[digits_start..];
            let count = digits.bytes().take_while(|b| if radix == 16 { b.is_ascii_hexdigit() } else { b.is_ascii_digit() }).count();
            if count > 0 {
                out.push_str(&numeric(&digits[..count], radix));
                let mut consumed = 1 + digits_start + count;
                if body.as_bytes().get(digits_start + count) == Some(&b';') {
                    consumed += 1;
                }
                rest = &after[consumed..];
                continue;
            }
            out.push('&');
            rest = after;
            continue;
        }
        // [^\t\n\f <&#;]{1,32};?
        let mut end = 0;
        for (count, (index, ch)) in after.char_indices().enumerate() {
            if count == 32 || matches!(ch, '\t' | '\n' | '\x0c' | ' ' | '<' | '&' | '#' | ';') {
                break;
            }
            end = index + ch.len_utf8();
        }
        if end == 0 {
            out.push('&');
            rest = after;
            continue;
        }
        let mut name_end = end;
        if after.as_bytes().get(end) == Some(&b';') {
            name_end += 1;
        }
        let name = &after[..name_end];
        rest = &after[name_end..];
        if let Some(value) = html5(name) {
            out.push_str(value);
            continue;
        }
        let boundaries: Vec<usize> = name.char_indices().map(|(index, _)| index).skip(2).collect();
        // for x in range(len(s) - 1, 1, -1): s[:x]
        if let Some((prefix, value)) = boundaries.iter().rev().find_map(|&cut| html5(&name[..cut]).map(|value| (cut, value))) {
            out.push_str(value);
            out.push_str(&name[prefix..]);
        } else {
            out.push('&');
            out.push_str(name);
        }
    }
    out.push_str(rest);
    out
}

/// `re.sub(r"<[^>]*>", "", value)`.
pub fn strip_tags(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                // No closing '>': the pattern cannot match here, keep '<'.
                out.push('<');
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_matches_python_cases() {
        let cases = [
            ("A &amp; B", "A & B"), ("&lt;b&gt;", "<b>"), ("&amp", "&"), ("&ampx", "&x"), ("&notit;", "¬it;"),
            ("&#65;&#x42;&#X43", "ABC"), ("&#0;", "\u{FFFD}"), ("&#x80;", "€"), ("&#1;", ""), ("&#xD800;", "\u{FFFD}"),
            ("&#99999999999;", "\u{FFFD}"), ("&#;", "&#;"), ("&unknown;", "&unknown;"), ("& x", "& x"), ("&&amp;", "&&"),
            ("&nbsp;x", "\u{a0}x"), ("&#13;", "\r"),
        ];
        for (input, expected) in cases {
            assert_eq!(unescape(input), expected, "{input}");
        }
    }

    #[test]
    fn tags_are_stripped_like_the_regex() {
        assert_eq!(strip_tags("<span alpha='50%'>eye</span>"), "eye");
        assert_eq!(strip_tags("a < b"), "a < b");
        assert_eq!(strip_tags("<<b>x"), "x");
    }
}
