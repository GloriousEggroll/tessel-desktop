//! Python `str` helpers the iCalendar parser relies on: `int()`, `isdigit`,
//! `isdecimal`, `isspace`/`strip`, decimal values of Unicode digits.

mod tables {
    // Generated from CPython 3.14.7 (Unicode 16.0.0): str.isdecimal / isdigit-only / isspace.
    pub const DECIMAL: &[(u32, u32)] = &[(0x30, 0x39), (0x660, 0x669), (0x6F0, 0x6F9), (0x7C0, 0x7C9), (0x966, 0x96F), (0x9E6, 0x9EF), (0xA66, 0xA6F), (0xAE6, 0xAEF), (0xB66, 0xB6F), (0xBE6, 0xBEF), (0xC66, 0xC6F), (0xCE6, 0xCEF), (0xD66, 0xD6F), (0xDE6, 0xDEF), (0xE50, 0xE59), (0xED0, 0xED9), (0xF20, 0xF29), (0x1040, 0x1049), (0x1090, 0x1099), (0x17E0, 0x17E9), (0x1810, 0x1819), (0x1946, 0x194F), (0x19D0, 0x19D9), (0x1A80, 0x1A89), (0x1A90, 0x1A99), (0x1B50, 0x1B59), (0x1BB0, 0x1BB9), (0x1C40, 0x1C49), (0x1C50, 0x1C59), (0xA620, 0xA629), (0xA8D0, 0xA8D9), (0xA900, 0xA909), (0xA9D0, 0xA9D9), (0xA9F0, 0xA9F9), (0xAA50, 0xAA59), (0xABF0, 0xABF9), (0xFF10, 0xFF19), (0x104A0, 0x104A9), (0x10D30, 0x10D39), (0x10D40, 0x10D49), (0x11066, 0x1106F), (0x110F0, 0x110F9), (0x11136, 0x1113F), (0x111D0, 0x111D9), (0x112F0, 0x112F9), (0x11450, 0x11459), (0x114D0, 0x114D9), (0x11650, 0x11659), (0x116C0, 0x116C9), (0x116D0, 0x116E3), (0x11730, 0x11739), (0x118E0, 0x118E9), (0x11950, 0x11959), (0x11BF0, 0x11BF9), (0x11C50, 0x11C59), (0x11D50, 0x11D59), (0x11DA0, 0x11DA9), (0x11F50, 0x11F59), (0x16130, 0x16139), (0x16A60, 0x16A69), (0x16AC0, 0x16AC9), (0x16B50, 0x16B59), (0x16D70, 0x16D79), (0x1CCF0, 0x1CCF9), (0x1D7CE, 0x1D7FF), (0x1E140, 0x1E149), (0x1E2F0, 0x1E2F9), (0x1E4F0, 0x1E4F9), (0x1E5F1, 0x1E5FA), (0x1E950, 0x1E959), (0x1FBF0, 0x1FBF9)];
    pub const DIGIT_ONLY: &[(u32, u32)] = &[(0xB2, 0xB3), (0xB9, 0xB9), (0x1369, 0x1371), (0x19DA, 0x19DA), (0x2070, 0x2070), (0x2074, 0x2079), (0x2080, 0x2089), (0x2460, 0x2468), (0x2474, 0x247C), (0x2488, 0x2490), (0x24EA, 0x24EA), (0x24F5, 0x24FD), (0x24FF, 0x24FF), (0x2776, 0x277E), (0x2780, 0x2788), (0x278A, 0x2792), (0x10A40, 0x10A43), (0x10E60, 0x10E68), (0x11052, 0x1105A), (0x1F100, 0x1F10A)];
    pub const SPACE: &[(u32, u32)] = &[(0x9, 0xD), (0x1C, 0x20), (0x85, 0x85), (0xA0, 0xA0), (0x1680, 0x1680), (0x2000, 0x200A), (0x2028, 0x2029), (0x202F, 0x202F), (0x205F, 0x205F), (0x3000, 0x3000)];
}

fn in_table(table: &[(u32, u32)], c: char) -> Option<u32> {
    let c = c as u32;
    let index = table.partition_point(|(_, end)| *end < c);
    table.get(index).filter(|(start, _)| *start <= c).map(|(start, _)| *start)
}

/// `str.isdecimal()` for one character; its value (Unicode Nd runs start at 0).
pub fn decimal(c: char) -> Option<u32> {
    in_table(tables::DECIMAL, c).map(|start| (c as u32 - start) % 10)
}

pub fn is_decimal(c: char) -> bool {
    decimal(c).is_some()
}

/// `str.isdigit()` for one character.
pub fn is_digit(c: char) -> bool {
    is_decimal(c) || in_table(tables::DIGIT_ONLY, c).is_some()
}

pub fn is_space(c: char) -> bool {
    in_table(tables::SPACE, c).is_some()
}

/// `str.isdigit()` (false for "").
pub fn isdigit(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_digit)
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `int(text)`: surrounding whitespace, one sign, Unicode decimal digits with
/// single underscores between them. `None` is Python's ValueError.
pub fn int(text: &str) -> Option<i128> {
    let text = strip(text);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    // CPython's default int_max_str_digits.
    if digits.chars().filter(|c| *c != '_').count() > 4300 {
        return None;
    }
    let mut value: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = decimal(c)?;
        // Values beyond i128 are far outside every range checked later.
        value = value.saturating_mul(10).saturating_add(digit as i128);
    }
    Some(if negative { -value } else { value })
}

/// Decimal value of a run of digits already known to be `\d` characters.
pub fn digits_value(chars: &[char]) -> i128 {
    chars.iter().fold(0i128, |acc, c| acc.saturating_mul(10).saturating_add(decimal(*c).unwrap_or(0) as i128))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_int_rules() {
        assert_eq!(int(" 12 "), Some(12));
        assert_eq!(int("+3"), Some(3));
        assert_eq!(int("-0"), Some(0));
        assert_eq!(int("1_000"), Some(1000));
        assert_eq!(int("١٢"), Some(12));
        assert_eq!(int("²"), None);
        assert_eq!(int("+-5"), None);
        assert_eq!(int("1__0"), None);
        assert_eq!(int(""), None);
        assert!(isdigit("²") && !isdigit("") && !isdigit("1a"));
        assert!(is_space('\u{1c}') && is_space('\u{a0}') && !is_space('\u{200b}'));
    }
}
