//! Helpers missing from suite-core (upstream candidates).
//!
//! `loads`: Python's `json.loads` value model and error texts. serde_json rejects
//! what Python accepts (`NaN`, `Infinity`, integers beyond f64, nesting beyond 128)
//! and the calculator's state loader must skip such entries one by one exactly
//! like the Python version instead of failing the whole file.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum PyValue {
    Null,
    Bool(bool),
    /// Integer literal digits (Python ints are unbounded), e.g. `"-12"`.
    Int(String),
    Float(f64),
    Str(String),
    List(Vec<PyValue>),
    Dict(Vec<(String, PyValue)>),
}

impl PyValue {
    pub fn get(&self, key: &str) -> Option<&PyValue> {
        match self {
            PyValue::Dict(items) => items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            PyValue::Null => false,
            PyValue::Bool(value) => *value,
            PyValue::Int(digits) => digits.trim_start_matches('-').trim_start_matches('0') != "",
            PyValue::Float(value) => *value != 0.0,
            PyValue::Str(text) => !text.is_empty(),
            PyValue::List(items) => !items.is_empty(),
            PyValue::Dict(items) => !items.is_empty(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            PyValue::Null => "NoneType",
            PyValue::Bool(_) => "bool",
            PyValue::Int(_) => "int",
            PyValue::Float(_) => "float",
            PyValue::Str(_) => "str",
            PyValue::List(_) => "list",
            PyValue::Dict(_) => "dict",
        }
    }

    /// `float(value)`: `Err` stands for Python's TypeError/ValueError/OverflowError.
    pub fn to_float(&self) -> Result<f64, ()> {
        match self {
            PyValue::Bool(value) => Ok(*value as i32 as f64),
            PyValue::Float(value) => Ok(*value),
            PyValue::Int(digits) => {
                let value: f64 = digits.parse().map_err(|_| ())?;
                if value.is_finite() { Ok(value) } else { Err(()) }
            }
            PyValue::Str(text) => py_float_text(text).ok_or(()),
            _ => Err(()),
        }
    }
}

/// Python `float(str)`.
pub fn py_float_text(text: &str) -> Option<f64> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    let lower = text.to_ascii_lowercase();
    let (sign, body) = match lower.as_bytes().first() {
        Some(b'-') => (-1.0, &lower[1..]),
        Some(b'+') => (1.0, &lower[1..]),
        _ => (1.0, lower.as_str()),
    };
    match body {
        "nan" => return Some(f64::NAN),
        "inf" | "infinity" => return Some(sign * f64::INFINITY),
        _ => {}
    }
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__")
        || !body.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-' | b'_')) {
        return None;
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok().map(|v| sign * v)
}

/// Python's maximum JSON nesting before RecursionError (default recursion limit).
const MAX_DEPTH: usize = 990;

pub struct JsonError(pub String);

struct Parser<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    pos: usize,
}

/// `json.loads(bytes)` (UTF-8 input; a leading BOM is rejected like Python).
pub fn loads(data: &[u8]) -> Result<PyValue, JsonError> {
    let text = std::str::from_utf8(data).map_err(|error| {
        JsonError(format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte",
                          data[error.valid_up_to()], error.valid_up_to()))
    })?;
    if text.starts_with('\u{feff}') {
        return Err(JsonError("Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)".into()));
    }
    let mut parser = Parser { text, chars: text.char_indices().collect(), pos: 0 };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos < parser.chars.len() {
        return Err(parser.error("Extra data"));
    }
    Ok(value)
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> { self.chars.get(self.pos).map(|(_, c)| *c) }

    fn error(&self, message: &str) -> JsonError {
        let line = 1 + self.chars[..self.pos].iter().filter(|(_, c)| *c == '\n').count();
        let column = match self.chars[..self.pos].iter().rposition(|(_, c)| *c == '\n') {
            Some(index) => self.pos - index,
            None => self.pos + 1,
        };
        JsonError(format!("{message}: line {line} column {column} (char {})", self.pos))
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn starts(&self, word: &str) -> bool {
        let offset = self.chars.get(self.pos).map(|(o, _)| *o).unwrap_or(self.text.len());
        self.text[offset..].starts_with(word)
    }

    fn value(&mut self, depth: usize) -> Result<PyValue, JsonError> {
        match self.peek() {
            Some('{') | Some('[') if depth >= MAX_DEPTH => {
                let kind = if self.peek() == Some('{') { "object" } else { "array" };
                Err(JsonError(format!("maximum recursion depth exceeded while decoding a JSON {kind} from a unicode string")))
            }
            Some('{') => self.object(depth + 1),
            Some('[') => self.array(depth + 1),
            Some('"') => {
                self.pos += 1;
                self.string().map(PyValue::Str)
            }
            _ if self.starts("null") => { self.pos += 4; Ok(PyValue::Null) }
            _ if self.starts("true") => { self.pos += 4; Ok(PyValue::Bool(true)) }
            _ if self.starts("false") => { self.pos += 5; Ok(PyValue::Bool(false)) }
            _ if self.starts("NaN") => { self.pos += 3; Ok(PyValue::Float(f64::NAN)) }
            _ if self.starts("Infinity") => { self.pos += 8; Ok(PyValue::Float(f64::INFINITY)) }
            _ if self.starts("-Infinity") => { self.pos += 9; Ok(PyValue::Float(f64::NEG_INFINITY)) }
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.error("Expecting value")),
        }
    }

    fn number(&mut self) -> Result<PyValue, JsonError> {
        let start = self.pos;
        let mut text = String::new();
        if self.peek() == Some('-') {
            text.push('-');
            self.pos += 1;
        }
        match self.peek() {
            Some('0') => { text.push('0'); self.pos += 1; }
            Some(c) if c.is_ascii_digit() => {
                while let Some(c) = self.peek().filter(char::is_ascii_digit) { text.push(c); self.pos += 1; }
            }
            _ => { self.pos = start; return Err(self.error("Expecting value")); }
        }
        let mut float = false;
        if self.peek() == Some('.') && self.chars.get(self.pos + 1).is_some_and(|(_, c)| c.is_ascii_digit()) {
            float = true;
            text.push('.');
            self.pos += 1;
            while let Some(c) = self.peek().filter(char::is_ascii_digit) { text.push(c); self.pos += 1; }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.pos;
            let mut exponent = String::from("e");
            self.pos += 1;
            if let Some(c @ ('+' | '-')) = self.peek() { exponent.push(c); self.pos += 1; }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                while let Some(c) = self.peek().filter(char::is_ascii_digit) { exponent.push(c); self.pos += 1; }
                float = true;
                text.push_str(&exponent);
            } else {
                self.pos = save;
            }
        }
        Ok(if float { PyValue::Float(text.parse().unwrap_or(f64::INFINITY)) } else { PyValue::Int(text) })
    }

    fn string(&mut self) -> Result<String, JsonError> {
        let begin = self.pos - 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                self.pos = begin;
                return Err(self.error("Unterminated string starting at"));
            };
            self.pos += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(e) = self.peek() else {
                        self.pos = begin;
                        return Err(self.error("Unterminated string starting at"));
                    };
                    self.pos += 1;
                    match e {
                        '"' => out.push('"'), '\\' => out.push('\\'), '/' => out.push('/'),
                        'b' => out.push('\u{8}'), 'f' => out.push('\u{c}'), 'n' => out.push('\n'),
                        'r' => out.push('\r'), 't' => out.push('\t'),
                        'u' => {
                            let code = self.hex4()?;
                            if (0xd800..0xdc00).contains(&code) && self.starts("\\u") {
                                let save = self.pos;
                                self.pos += 2;
                                let low = self.hex4()?;
                                if (0xdc00..0xe000).contains(&low) {
                                    let joined = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                                    out.push(char::from_u32(joined).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                self.pos = save;
                            }
                            // Lone surrogates cannot live in a Rust String.
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => {
                            self.pos -= 2;
                            return Err(self.error("Invalid \\escape"));
                        }
                    }
                }
                c if (c as u32) < 0x20 => {
                    self.pos -= 1;
                    return Err(self.error("Invalid control character at"));
                }
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits: String = self.chars[self.pos..].iter().take(4).map(|(_, c)| *c).collect();
        if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
            self.pos += 4;
            Ok(u32::from_str_radix(&digits, 16).unwrap())
        } else {
            self.pos -= 1;
            Err(self.error("Invalid \\uXXXX escape"))
        }
    }

    fn array(&mut self, depth: usize) -> Result<PyValue, JsonError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(PyValue::List(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.pos = comma;
                        return Err(self.error("Illegal trailing comma before end of array"));
                    }
                }
                Some(']') => { self.pos += 1; return Ok(PyValue::List(items)); }
                _ => return Err(self.error("Expecting ',' delimiter")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<PyValue, JsonError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(PyValue::Dict(items));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(self.error("Expecting property name enclosed in double quotes"));
            }
            self.pos += 1;
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(self.error("Expecting ':' delimiter"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value(depth)?;
            items.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.pos = comma;
                        return Err(self.error("Illegal trailing comma before end of object"));
                    }
                }
                Some('}') => { self.pos += 1; return Ok(PyValue::Dict(items)); }
                _ => return Err(self.error("Expecting ',' delimiter")),
            }
        }
    }
}

/// `json.dumps(text, ensure_ascii=False)` for one string.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
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
            c if (c as u32) < 0x20 => { let _ = write!(out, "\\u{:04x}", c as u32); }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> String { loads(text.as_bytes()).err().map(|e| e.0).unwrap_or_default() }

    #[test]
    fn python_values_and_errors() {
        assert_eq!(error("broken json"), "Expecting value: line 1 column 1 (char 0)");
        assert_eq!(error("{\"a\": 1,}"), "Illegal trailing comma before end of object: line 1 column 8 (char 7)");
        assert_eq!(error("[1] x"), "Extra data: line 1 column 5 (char 4)");
        assert!(error(&format!("{}0{}", "[".repeat(2000), "]".repeat(2000))).starts_with("maximum recursion depth exceeded"));
        let value = loads(format!("{{\"t\": 1{}, \"n\": NaN, \"s\": \"\\u00e9\"}}", "0".repeat(400)).as_bytes()).ok().unwrap();
        assert!(value.get("t").unwrap().to_float().is_err());
        assert!(value.get("n").unwrap().to_float().unwrap().is_nan());
        assert_eq!(value.get("s"), Some(&PyValue::Str("é".into())));
        assert_eq!(py_float_text(" 12.5 "), Some(12.5));
        assert!(py_float_text("NaN").unwrap().is_nan());
        assert_eq!(py_float_text("x"), None);
        assert_eq!(json_string("a\"\u{1}é"), "\"a\\\"\\u0001é\"");
    }
}
