//! Python-compatible `str.format` (named fields only) and `json.dumps`.
//!
//! The suite's catalogs and D-Bus payloads were produced by Python, so the
//! placeholder rules (`{name}`, `{value:.0f}`, `{{` escapes) and the JSON text
//! (separators, `ensure_ascii`, float repr) are reproduced here exactly.

use serde_json::Value as Json;
use std::fmt::Write as _;

/// A value that can fill a `{name}` field.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

macro_rules! arg_from {
    ($variant:ident <- $($t:ty),*) => {$(
        impl From<$t> for Arg { fn from(value: $t) -> Self { Arg::$variant(value.into()) } }
    )*};
}
arg_from!(Str <- String, &str, &String, char);
arg_from!(Int <- i8, i16, i32, i64, u8, u16, u32);
arg_from!(Float <- f32, f64);
arg_from!(Bool <- bool);
impl From<u64> for Arg {
    fn from(value: u64) -> Self { Arg::Int(value.min(i64::MAX as u64) as i64) }
}
impl From<usize> for Arg {
    fn from(value: usize) -> Self { Arg::from(value as u64) }
}
impl From<isize> for Arg {
    fn from(value: isize) -> Self { Arg::Int(value as i64) }
}
impl From<&glib::GString> for Arg {
    fn from(value: &glib::GString) -> Self { Arg::Str(value.to_string()) }
}

/// Error from parsing or formatting (Python's ValueError/KeyError/IndexError).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError(pub String);

fn err<T>(message: &str) -> Result<T, FormatError> {
    Err(FormatError(message.to_owned()))
}

/// One item of `string.Formatter().parse()`: literal text, then an optional field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub literal: String,
    /// `(field_name, format_spec, conversion)`; `None` for trailing literal text.
    pub field: Option<(String, String, Option<char>)>,
}

/// Port of `string.Formatter().parse()` (CPython's MarkupIterator).
pub fn parse(format: &str) -> Result<Vec<Piece>, FormatError> {
    let chars: Vec<char> = format.chars().collect();
    let (mut pieces, mut literal, mut i) = (Vec::new(), String::new(), 0);
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                literal.push('{');
                i += 2;
                continue;
            }
            if i + 1 == chars.len() {
                return err("Single '{' encountered in format string");
            }
            // The field ends at the matching brace; nested braces belong to the spec.
            let (start, mut depth, mut j) = (i + 1, 1, i + 1);
            while j < chars.len() {
                match chars[j] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j == chars.len() {
                return err("expected '}' before end of string");
            }
            pieces.push(Piece { literal: std::mem::take(&mut literal), field: Some(split_field(&chars[start..j])?) });
            i = j + 1;
        } else if c == '}' {
            if chars.get(i + 1) == Some(&'}') {
                literal.push('}');
                i += 2;
                continue;
            }
            return err("Single '}' encountered in format string");
        } else {
            literal.push(c);
            i += 1;
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece { literal, field: None });
    }
    Ok(pieces)
}

fn split_field(field: &[char]) -> Result<(String, String, Option<char>), FormatError> {
    // ':' and '!' inside an index ("a[x:y]") do not end the field name.
    let mut i = 0;
    let mut bracket = false;
    while i < field.len() {
        match field[i] {
            '[' => bracket = true,
            ']' => bracket = false,
            ':' | '!' if !bracket => break,
            _ => {}
        }
        i += 1;
    }
    let name: String = field[..i].iter().collect();
    if i == field.len() {
        return Ok((name, String::new(), None));
    }
    let mut conversion = None;
    if field[i] == '!' {
        let Some(&c) = field.get(i + 1) else {
            return err("end of string while looking for conversion specifier");
        };
        conversion = Some(c);
        i += 2;
        if i < field.len() && field[i] != ':' {
            return err("expected ':' after conversion specifier");
        }
    }
    let spec = if i < field.len() { field[i + 1..].iter().collect() } else { String::new() };
    Ok((name, spec, conversion))
}

/// The set of field names of a message (`suite_i18n.format_fields`).
pub fn format_fields(message: &str) -> Result<std::collections::BTreeSet<String>, FormatError> {
    Ok(parse(message)?.into_iter().filter_map(|piece| piece.field.map(|field| field.0)).collect())
}

/// `message.format(**values)`.
pub fn format(message: &str, values: &[(&str, Arg)]) -> Result<String, FormatError> {
    format_depth(message, values, 2)
}

fn format_depth(message: &str, values: &[(&str, Arg)], depth: u8) -> Result<String, FormatError> {
    if depth == 0 {
        return err("Max string recursion exceeded");
    }
    let mut out = String::new();
    for piece in parse(message)? {
        out.push_str(&piece.literal);
        let Some((name, spec, conversion)) = piece.field else { continue };
        if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return err("Replacement index out of range for positional args tuple");
        }
        if name.contains(['.', '[']) {
            return err("Attribute and index lookups are not supported");
        }
        let Some((_, value)) = values.iter().find(|(key, _)| *key == name) else {
            return Err(FormatError(format!("KeyError: '{name}'")));
        };
        let value = match conversion {
            None => value.clone(),
            Some('s') => Arg::Str(to_str(value)),
            Some('r') => Arg::Str(repr(value)),
            Some('a') => Arg::Str(repr(value).chars().map(|c| if c.is_ascii() { c.to_string() } else { ascii_escape(c) }).collect()),
            Some(c) => return Err(FormatError(format!("Unknown conversion specifier {c}"))),
        };
        let spec = if spec.contains('{') { format_depth(&spec, values, depth - 1)? } else { spec };
        out.push_str(&format_value(&value, &spec)?);
    }
    Ok(out)
}

fn ascii_escape(c: char) -> String {
    let code = c as u32;
    if code <= 0xff { format!("\\x{code:02x}") } else if code <= 0xffff { format!("\\u{code:04x}") } else { format!("\\U{code:08x}") }
}

/// Python `str(value)`.
pub fn to_str(value: &Arg) -> String {
    match value {
        Arg::Str(text) => text.clone(),
        Arg::Int(number) => number.to_string(),
        Arg::Float(number) => float_repr(*number),
        Arg::Bool(flag) => if *flag { "True" } else { "False" }.to_owned(),
    }
}

fn repr(value: &Arg) -> String {
    let Arg::Str(text) = value else { return to_str(value) };
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&ascii_escape(c)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr(float)`: shortest round-trip digits, exponent outside 1e-4..1e16.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let (digits, exponent) = shortest_digits(value.abs());
    let body = if (-4..16).contains(&exponent) {
        let mut text = fixed_from_digits(&digits, exponent);
        if !text.contains('.') {
            text.push_str(".0");
        }
        text
    } else {
        scientific_from_digits(&digits, exponent, 'e')
    };
    format!("{sign}{body}")
}

// Digits without a point and the decimal exponent of the first digit.
fn shortest_digits(value: f64) -> (String, i32) {
    split_exponent(&format!("{value:e}"))
}

fn split_exponent(text: &str) -> (String, i32) {
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
    (mantissa.replace('.', ""), exponent.parse().unwrap_or(0))
}

fn fixed_from_digits(digits: &str, exponent: i32) -> String {
    if exponent < 0 {
        format!("0.{}{}", "0".repeat((-exponent - 1) as usize), digits)
    } else if digits.len() as i32 > exponent + 1 {
        let (int, frac) = digits.split_at(exponent as usize + 1);
        format!("{int}.{frac}")
    } else {
        format!("{digits}{}", "0".repeat((exponent + 1 - digits.len() as i32) as usize))
    }
}

fn scientific_from_digits(digits: &str, exponent: i32, e: char) -> String {
    let (first, rest) = digits.split_at(1);
    let mantissa = if rest.is_empty() { first.to_owned() } else { format!("{first}.{rest}") };
    format!("{mantissa}{}", exponent_suffix(exponent, e))
}

fn exponent_suffix(exponent: i32, e: char) -> String {
    format!("{e}{}{:02}", if exponent < 0 { '-' } else { '+' }, exponent.abs())
}

#[derive(Default)]
struct Spec {
    fill: Option<char>,
    align: Option<char>,
    sign: Option<char>,
    alternate: bool,
    zero: bool,
    width: usize,
    grouping: Option<char>,
    precision: Option<usize>,
    kind: Option<char>,
}

fn parse_spec(spec: &str) -> Result<Spec, FormatError> {
    let chars: Vec<char> = spec.chars().collect();
    let mut result = Spec::default();
    let mut i = 0;
    let is_align = |c: Option<&char>| matches!(c, Some('<' | '>' | '=' | '^'));
    if is_align(chars.get(1)) {
        result.fill = Some(chars[0]);
        result.align = Some(chars[1]);
        i = 2;
    } else if is_align(chars.first()) {
        result.align = Some(chars[0]);
        i = 1;
    }
    if let Some(&c @ ('+' | '-' | ' ')) = chars.get(i) {
        result.sign = Some(c);
        i += 1;
    }
    if chars.get(i) == Some(&'z') {
        i += 1;
    }
    if chars.get(i) == Some(&'#') {
        result.alternate = true;
        i += 1;
    }
    if chars.get(i) == Some(&'0') {
        result.zero = true;
        i += 1;
    }
    let start = i;
    while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
    }
    result.width = chars[start..i].iter().collect::<String>().parse().unwrap_or(0);
    if let Some(&c @ (',' | '_')) = chars.get(i) {
        result.grouping = Some(c);
        i += 1;
    }
    if chars.get(i) == Some(&'.') {
        i += 1;
        let start = i;
        while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
        if start == i {
            return err("Format specifier missing precision");
        }
        result.precision = chars[start..i].iter().collect::<String>().parse().ok();
    }
    if i < chars.len() {
        if i + 1 < chars.len() {
            return err("Invalid format specifier");
        }
        result.kind = Some(chars[i]);
    }
    if result.zero && result.fill.is_none() && result.align.is_none() {
        result.fill = Some('0');
        result.align = Some('=');
    }
    Ok(result)
}

/// Python `format(value, spec)`.
pub fn format_value(value: &Arg, spec: &str) -> Result<String, FormatError> {
    match value {
        Arg::Bool(flag) if spec.is_empty() => Ok(to_str(&Arg::Bool(*flag))),
        Arg::Bool(flag) => format_value(&Arg::Int(*flag as i64), spec),
        Arg::Str(text) => {
            let spec = parse_spec(spec)?;
            if !matches!(spec.kind, None | Some('s')) {
                return Err(FormatError(format!("Unknown format code '{}' for object of type 'str'", spec.kind.unwrap())));
            }
            if spec.sign.is_some() {
                return err("Sign not allowed in string format specifier");
            }
            if spec.alternate {
                return err("Alternate form (#) not allowed in string format specifier");
            }
            if spec.align == Some('=') {
                return err("'=' alignment not allowed in string format specifier");
            }
            if spec.grouping.is_some() {
                return err("Cannot specify ',' with 's'.");
            }
            let text: String = match spec.precision {
                Some(limit) => text.chars().take(limit).collect(),
                None => text.clone(),
            };
            Ok(pad("", &text, &spec, '<'))
        }
        Arg::Int(number) => {
            let spec = parse_spec(spec)?;
            match spec.kind {
                Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%') => format_float(*number as f64, &spec),
                Some('n') if spec.grouping.is_some() => err("Cannot specify ',' with 'n'."),
                kind @ (None | Some('d' | 'n' | 'b' | 'o' | 'x' | 'X' | 'c')) => {
                    if spec.precision.is_some() {
                        return err("Precision not allowed in integer format specifier");
                    }
                    let magnitude = number.unsigned_abs();
                    let (prefix, digits) = match kind {
                        Some('b') => ("0b", format!("{magnitude:b}")),
                        Some('o') => ("0o", format!("{magnitude:o}")),
                        Some('x') => ("0x", format!("{magnitude:x}")),
                        Some('X') => ("0X", format!("{magnitude:X}")),
                        Some('c') => {
                            let c = u32::try_from(*number).ok().and_then(char::from_u32);
                            return c.map(|c| pad("", &c.to_string(), &spec, '<'))
                                .ok_or_else(|| FormatError("%c arg not in range(0x110000)".into()));
                        }
                        _ => ("", magnitude.to_string()),
                    };
                    let digits = group(&digits, spec.grouping, if matches!(kind, Some('b' | 'o' | 'x' | 'X')) { 4 } else { 3 });
                    let sign = sign_text(*number < 0, spec.sign);
                    let prefix = if spec.alternate { prefix } else { "" };
                    Ok(pad(&format!("{sign}{prefix}"), &digits, &spec, '>'))
                }
                Some(c) => Err(FormatError(format!("Unknown format code '{c}' for object of type 'int'"))),
            }
        }
        Arg::Float(number) => {
            let spec = parse_spec(spec)?;
            match spec.kind {
                None | Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%' | 'n') => format_float(*number, &spec),
                Some(c) => Err(FormatError(format!("Unknown format code '{c}' for object of type 'float'"))),
            }
        }
    }
}

fn sign_text(negative: bool, sign: Option<char>) -> &'static str {
    match (negative, sign) {
        (true, _) => "-",
        (false, Some('+')) => "+",
        (false, Some(' ')) => " ",
        _ => "",
    }
}

fn group(digits: &str, separator: Option<char>, size: usize) -> String {
    let Some(separator) = separator else { return digits.to_owned() };
    let (int, rest) = digits.find(|c: char| !c.is_ascii_alphanumeric()).map_or((digits, ""), |at| digits.split_at(at));
    let mut out = String::new();
    for (index, c) in int.chars().enumerate() {
        if index > 0 && (int.len() - index) % size == 0 {
            out.push(separator);
        }
        out.push(c);
    }
    out + rest
}

fn pad(prefix: &str, body: &str, spec: &Spec, default_align: char) -> String {
    let length = prefix.chars().count() + body.chars().count();
    let fill = spec.fill.unwrap_or(' ');
    if length >= spec.width {
        return format!("{prefix}{body}");
    }
    let missing = spec.width - length;
    let filler = |count: usize| fill.to_string().repeat(count);
    match spec.align.unwrap_or(default_align) {
        '<' => format!("{prefix}{body}{}", filler(missing)),
        '^' => format!("{}{prefix}{body}{}", filler(missing / 2), filler(missing - missing / 2)),
        '=' => format!("{prefix}{}{body}", filler(missing)),
        _ => format!("{}{prefix}{body}", filler(missing)),
    }
}

fn strip_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_owned();
    }
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn format_float(number: f64, spec: &Spec) -> Result<String, FormatError> {
    let negative = number.is_sign_negative() && !number.is_nan();
    let value = number.abs();
    let upper = matches!(spec.kind, Some('E' | 'F' | 'G'));
    let body = if !value.is_finite() {
        let text = if value.is_nan() { "nan" } else { "inf" };
        let text = if upper { text.to_uppercase() } else { text.to_owned() };
        if spec.kind == Some('%') { text + "%" } else { text }
    } else {
        match spec.kind {
            Some('f' | 'F') => {
                let text = format!("{value:.*}", spec.precision.unwrap_or(6));
                if spec.alternate && !text.contains('.') { text + "." } else { text }
            }
            Some('%') => format!("{:.*}%", spec.precision.unwrap_or(6), value * 100.0),
            Some('e' | 'E') => {
                let precision = spec.precision.unwrap_or(6);
                let (digits, exponent) = split_exponent(&format!("{value:.precision$e}"));
                let mut text = scientific_from_digits(&digits, exponent, if upper { 'E' } else { 'e' });
                if spec.alternate && precision == 0 {
                    text.insert(1, '.');
                }
                text
            }
            Some('g' | 'G' | 'n') => general(value, spec.precision.unwrap_or(6).max(1), spec.alternate, false, upper),
            _ => match spec.precision {
                None => float_repr(value),
                Some(precision) => general(value, precision.max(1), spec.alternate, true, false),
            },
        }
    };
    let body = group(&body, spec.grouping, 3);
    Ok(pad(sign_text(negative, spec.sign), &body, spec, '>'))
}

// 'g' and the untyped float format with a precision ("repr style").
fn general(value: f64, precision: usize, alternate: bool, repr_style: bool, upper: bool) -> String {
    let (digits, exponent) = split_exponent(&format!("{:.*e}", precision - 1, value));
    // Python switches to scientific notation at exp >= p ('g') or exp >= p - 1 (untyped).
    let limit = if repr_style { precision as i32 - 1 } else { precision as i32 };
    if exponent < -4 || exponent >= limit {
        let text = scientific_from_digits(&digits, exponent, if upper { 'E' } else { 'e' });
        if alternate {
            return text;
        }
        let (mantissa, suffix) = text.split_at(text.find(['e', 'E']).unwrap());
        return strip_zeros(mantissa) + suffix;
    }
    let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if alternate {
        return if text.contains('.') { text } else { text + "." };
    }
    let text = strip_zeros(&text);
    if repr_style && !text.contains('.') { text + ".0" } else { text }
}

/// Options of Python's `json.dumps`.
#[derive(Clone, Copy, Debug)]
pub struct JsonStyle {
    pub ensure_ascii: bool,
    pub sort_keys: bool,
    pub indent: Option<usize>,
    pub item_separator: &'static str,
    pub key_separator: &'static str,
}

impl JsonStyle {
    /// `json.dumps(value)`: `", "` and `": "`, ASCII-only output.
    pub const DEFAULT: Self = Self { ensure_ascii: true, sort_keys: false, indent: None, item_separator: ", ", key_separator: ": " };
    /// `json.dumps(value, separators=(",", ":"))`.
    pub const COMPACT: Self = Self { item_separator: ",", key_separator: ":", ..Self::DEFAULT };
}

/// Python-identical `json.dumps` text for a serde_json value.
pub fn json_dumps(value: &Json, style: &JsonStyle) -> String {
    let mut out = String::new();
    write_json(&mut out, value, style, 0);
    out
}

fn write_json(out: &mut String, value: &Json, style: &JsonStyle, level: usize) {
    let newline = |out: &mut String, level: usize| {
        if let Some(indent) = style.indent {
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
        }
    };
    // With an indent Python's default item separator drops its trailing space.
    let item_separator = if style.indent.is_some() && style.item_separator == ", " { "," } else { style.item_separator };
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(int), _) => out.push_str(&int.to_string()),
            (_, Some(int)) => out.push_str(&int.to_string()),
            _ => {
                let float = number.as_f64().unwrap_or(f64::NAN);
                out.push_str(&match float_repr(float).as_str() {
                    "nan" => "NaN".to_owned(),
                    "inf" => "Infinity".to_owned(),
                    "-inf" => "-Infinity".to_owned(),
                    text => text.to_owned(),
                })
            }
        },
        Json::String(text) => write_string(out, text, style.ensure_ascii),
        Json::Array(items) => {
            if items.is_empty() {
                return out.push_str("[]");
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_separator);
                }
                newline(out, level + 1);
                write_json(out, item, style, level + 1);
            }
            newline(out, level);
            out.push(']');
        }
        Json::Object(map) => {
            if map.is_empty() {
                return out.push_str("{}");
            }
            let mut entries: Vec<_> = map.iter().collect();
            if style.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(item_separator);
                }
                newline(out, level + 1);
                write_string(out, key, style.ensure_ascii);
                out.push_str(style.key_separator);
                write_json(out, item, style, level + 1);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, text: &str, ensure_ascii: bool) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if ensure_ascii && !c.is_ascii() => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(message: &str, values: &[(&str, Arg)]) -> String {
        format(message, values).unwrap()
    }

    #[test]
    fn parse_matches_python_formatter() {
        let pieces = parse("a {x} b {y:.1f} {z!r} {{lit}}").unwrap();
        let fields: Vec<_> = pieces.iter().filter_map(|p| p.field.clone()).collect();
        assert_eq!(fields, vec![("x".into(), "".into(), None), ("y".into(), ".1f".into(), None), ("z".into(), "".into(), Some('r'))]);
        assert_eq!(pieces.last().unwrap().literal, " {lit}");
        for bad in ["{", "}", "a { b", "{x!}", "{x!rr}"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(parse("{}").unwrap()[0].field.as_ref().unwrap().0, "");
        assert_eq!(parse("{x:{w}}").unwrap()[0].field.as_ref().unwrap().1, "{w}");
    }

    #[test]
    fn named_fields_specs_and_errors() {
        assert_eq!(f("{a} and {b}", &[("a", 1.into()), ("b", "two".into())]), "1 and two");
        assert_eq!(f("{p:.0f}%", &[("p", 42.6.into())]), "43%");
        assert_eq!(f("{p:.0f}", &[("p", 0.5.into())]), "0");
        assert_eq!(f("{p:.0f}", &[("p", 7.into())]), "7");
        assert_eq!(f("{w:g}", &[("w", 6500.0.into())]), "6500");
        assert_eq!(f("{w:g}", &[("w", 0.00001.into())]), "1e-05");
        assert_eq!(f("{w:g}", &[("w", 1234567.0.into())]), "1.23457e+06");
        assert_eq!(f("{w}", &[("w", 1.0.into())]), "1.0");
        assert_eq!(f("{w}", &[("w", 1e16.into())]), "1e+16");
        assert_eq!(f("{w}", &[("w", 0.1.into())]), "0.1");
        assert_eq!(f("{w:.3}", &[("w", 1.0.into())]), "1.0");
        assert_eq!(f("{w:.3}", &[("w", 100.0.into())]), "1e+02");
        assert_eq!(f("{w:.3}", &[("w", 1.23456.into())]), "1.23");
        assert_eq!(f("{n:>5}|{n:<5}|{n:^5}|{n:05}", &[("n", 42.into())]), "   42|42   | 42  |00042");
        assert_eq!(f("{n:,}", &[("n", 1234567.into())]), "1,234,567");
        assert_eq!(f("{n:+d}", &[("n", 3.into())]), "+3");
        assert_eq!(f("{n:x}", &[("n", 255.into())]), "ff");
        assert_eq!(f("{s:>6}", &[("s", "ab".into())]), "    ab");
        assert_eq!(f("{s:.1}", &[("s", "ab".into())]), "a");
        assert_eq!(f("{s!r}", &[("s", "ab".into())]), "'ab'");
        assert_eq!(f("{b}", &[("b", true.into())]), "True");
        assert_eq!(f("{v:.1%}", &[("v", 0.256.into())]), "25.6%");
        assert_eq!(f("{v:.2e}", &[("v", 12345.0.into())]), "1.23e+04");
        assert_eq!(f("{v:.1f}", &[("v", (-0.04).into())]), "-0.0");
        assert!(format("{missing}", &[]).is_err());
        assert!(format("{}", &[("a", 1.into())]).is_err());
        assert!(format("{s:d}", &[("s", "x".into())]).is_err());
        assert!(format("{n:.1d}", &[("n", 1.into())]).is_err());
    }

    #[test]
    fn json_dumps_matches_python() {
        let value: Json = serde_json::from_str(r#"{"b": [1, 2.5, 1e16, true, null], "a": "é\n\u0001", "c": {}}"#).unwrap();
        assert_eq!(json_dumps(&value, &JsonStyle::DEFAULT),
                   r#"{"b": [1, 2.5, 1e+16, true, null], "a": "\u00e9\n\u0001", "c": {}}"#);
        let pretty = JsonStyle { ensure_ascii: false, sort_keys: true, indent: Some(2), ..JsonStyle::DEFAULT };
        assert_eq!(json_dumps(&value, &pretty),
                   "{\n  \"a\": \"é\\n\\u0001\",\n  \"b\": [\n    1,\n    2.5,\n    1e+16,\n    true,\n    null\n  ],\n  \"c\": {}\n}");
        assert_eq!(json_dumps(&serde_json::json!({"k": "😀"}), &JsonStyle::COMPACT), r#"{"k":"\ud83d\ude00"}"#);
    }
}
