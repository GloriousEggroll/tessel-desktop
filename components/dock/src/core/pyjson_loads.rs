//! `json.loads` with Python's acceptance rules and exact `JSONDecodeError`
//! texts (CPython 3.13+ `_json` scanner), plus `bytes.decode("utf-8")` error
//! texts. Used by `ConfigStore.load` so a malformed settings file reports
//! the same message as the Python dock.
//!
//! Differences forced by `serde_json::Value`: `NaN`/`Infinity`/`-Infinity`
//! (and floats/ints that overflow to infinity) become the strings
//! [`NAN`]/[`INFINITY`]/[`NEG_INFINITY`] (they contain NUL, so every
//! `DockConfig` check rejects them with the same message Python gives for a
//! non-finite number); lone UTF-16 surrogates become U+FFFD; integers beyond
//! 64 bits become floats.

use serde_json::{Map, Number, Value};

pub const NAN: &str = "\u{0}NaN";
pub const INFINITY: &str = "\u{0}Infinity";
pub const NEG_INFINITY: &str = "\u{0}-Infinity";

struct Error {
    message: &'static str,
    position: usize,
}

fn fail<T>(message: &'static str, position: usize) -> Result<T, Error> {
    Err(Error { message, position })
}

struct Parser<'a> {
    s: &'a [char],
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn float_value(value: f64) -> Value {
    if value.is_nan() {
        Value::from(NAN)
    } else if value == f64::INFINITY {
        Value::from(INFINITY)
    } else if value == f64::NEG_INFINITY {
        Value::from(NEG_INFINITY)
    } else {
        Value::Number(Number::from_f64(value).expect("finite"))
    }
}

impl Parser<'_> {
    fn ws(&self, mut idx: usize) -> usize {
        while idx < self.s.len() && is_ws(self.s[idx]) {
            idx += 1;
        }
        idx
    }

    fn starts(&self, idx: usize, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        self.s.get(idx..idx + chars.len()) == Some(&chars[..])
    }

    /// `scan_once`: `Err` with "Expecting value" means StopIteration(idx).
    fn value(&self, idx: usize) -> Result<(Value, usize), Error> {
        let Some(&c) = self.s.get(idx) else { return fail("Expecting value", idx) };
        match c {
            '"' => self.string(idx + 1).map(|(s, end)| (Value::String(s), end)),
            '{' => self.object(idx + 1),
            '[' => self.array(idx + 1),
            'n' if self.starts(idx, "null") => Ok((Value::Null, idx + 4)),
            't' if self.starts(idx, "true") => Ok((Value::Bool(true), idx + 4)),
            'f' if self.starts(idx, "false") => Ok((Value::Bool(false), idx + 5)),
            'N' if self.starts(idx, "NaN") => Ok((Value::from(NAN), idx + 3)),
            'I' if self.starts(idx, "Infinity") => Ok((Value::from(INFINITY), idx + 8)),
            '-' if self.starts(idx, "-Infinity") => Ok((Value::from(NEG_INFINITY), idx + 9)),
            '-' | '0'..='9' => self.number(idx),
            _ => fail("Expecting value", idx),
        }
    }

    fn number(&self, start: usize) -> Result<(Value, usize), Error> {
        let digit = |i: usize| self.s.get(i).is_some_and(|c| c.is_ascii_digit());
        let mut idx = start;
        if self.s[idx] == '-' {
            idx += 1;
        }
        if !digit(idx) {
            return fail("Expecting value", start);
        }
        if self.s[idx] == '0' {
            idx += 1;
        } else {
            while digit(idx) {
                idx += 1;
            }
        }
        let mut is_float = false;
        if self.s.get(idx) == Some(&'.') && digit(idx + 1) {
            is_float = true;
            idx += 1;
            while digit(idx) {
                idx += 1;
            }
        }
        if matches!(self.s.get(idx), Some('e' | 'E')) {
            let mut exp = idx + 1;
            if matches!(self.s.get(exp), Some('+' | '-')) {
                exp += 1;
            }
            if digit(exp) {
                is_float = true;
                idx = exp;
                while digit(idx) {
                    idx += 1;
                }
            }
        }
        let text: String = self.s[start..idx].iter().collect();
        let value = if is_float {
            float_value(text.parse::<f64>().unwrap_or(f64::NAN))
        } else if let Ok(int) = text.parse::<i64>() {
            Value::from(int)
        } else if let Ok(int) = text.parse::<u64>() {
            Value::from(int)
        } else {
            float_value(text.parse::<f64>().unwrap_or(f64::NAN))
        };
        Ok((value, idx))
    }

    fn hex4(&self, from: usize) -> Option<u32> {
        let digits: String = self.s.get(from..from + 4)?.iter().collect();
        if digits.chars().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(&digits, 16).ok()
        } else {
            None
        }
    }

    /// `scanstring(s, end)`; `end` is just past the opening quote.
    fn string(&self, end: usize) -> Result<(String, usize), Error> {
        let begin = end - 1;
        let len = self.s.len();
        let mut out = String::new();
        let mut idx = end;
        loop {
            let Some(&c) = self.s.get(idx) else { return fail("Unterminated string starting at", begin) };
            match c {
                '"' => return Ok((out, idx + 1)),
                '\\' => {
                    idx += 1;
                    let Some(&escape) = self.s.get(idx) else { return fail("Unterminated string starting at", begin) };
                    if escape != 'u' {
                        out.push(match escape {
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            'b' => '\u{8}',
                            'f' => '\u{c}',
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            _ => return fail("Invalid \\escape", idx - 1),
                        });
                        idx += 1;
                        continue;
                    }
                    let u = idx;
                    if u + 5 > len {
                        return fail("Invalid \\uXXXX escape", u);
                    }
                    let Some(mut code) = self.hex4(u + 1) else { return fail("Invalid \\uXXXX escape", u) };
                    idx = u + 5;
                    if (0xd800..=0xdbff).contains(&code) && idx + 6 < len && self.s[idx] == '\\' && self.s[idx + 1] == 'u' {
                        let Some(low) = self.hex4(idx + 2) else { return fail("Invalid \\uXXXX escape", idx + 1) };
                        if (0xdc00..=0xdfff).contains(&low) {
                            code = 0x10000 + (((code - 0xd800) << 10) | (low - 0xdc00));
                            idx += 6;
                        }
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                c if (c as u32) <= 0x1f => return fail("Invalid control character at", idx),
                c => {
                    out.push(c);
                    idx += 1;
                }
            }
        }
    }

    fn object(&self, start: usize) -> Result<(Value, usize), Error> {
        let mut map = Map::new();
        let mut idx = self.ws(start);
        if self.s.get(idx) == Some(&'}') {
            return Ok((Value::Object(map), idx + 1));
        }
        loop {
            if self.s.get(idx) != Some(&'"') {
                return fail("Expecting property name enclosed in double quotes", idx);
            }
            let (key, end) = self.string(idx + 1)?;
            idx = self.ws(end);
            if self.s.get(idx) != Some(&':') {
                return fail("Expecting ':' delimiter", idx);
            }
            idx = self.ws(idx + 1);
            let (value, end) = self.value(idx)?;
            map.insert(key, value);
            idx = self.ws(end);
            match self.s.get(idx) {
                Some('}') => return Ok((Value::Object(map), idx + 1)),
                Some(',') => {
                    let comma = idx;
                    idx = self.ws(idx + 1);
                    if self.s.get(idx) == Some(&'}') {
                        return fail("Illegal trailing comma before end of object", comma);
                    }
                }
                _ => return fail("Expecting ',' delimiter", idx),
            }
        }
    }

    fn array(&self, start: usize) -> Result<(Value, usize), Error> {
        let mut items = Vec::new();
        let mut idx = self.ws(start);
        if self.s.get(idx) == Some(&']') {
            return Ok((Value::Array(items), idx + 1));
        }
        loop {
            let (value, end) = self.value(idx)?;
            items.push(value);
            idx = self.ws(end);
            match self.s.get(idx) {
                Some(']') => return Ok((Value::Array(items), idx + 1)),
                Some(',') => {
                    let comma = idx;
                    idx = self.ws(idx + 1);
                    if self.s.get(idx) == Some(&']') {
                        return fail("Illegal trailing comma before end of array", comma);
                    }
                }
                _ => return fail("Expecting ',' delimiter", idx),
            }
        }
    }
}

fn error_text(chars: &[char], error: Error) -> String {
    let before = &chars[..error.position.min(chars.len())];
    let line = before.iter().filter(|c| **c == '\n').count() + 1;
    let column = match before.iter().rposition(|c| *c == '\n') {
        Some(newline) => error.position - newline,
        None => error.position + 1,
    };
    format!("{}: line {line} column {column} (char {})", error.message, error.position)
}

/// `json.loads(text)`; `Err` is `str(JSONDecodeError)`.
pub fn loads(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'\u{feff}') {
        return Err(error_text(&chars, Error { message: "Unexpected UTF-8 BOM (decode using utf-8-sig)", position: 0 }));
    }
    let parser = Parser { s: &chars };
    let start = parser.ws(0);
    let (value, end) = parser.value(start).map_err(|e| error_text(&chars, e))?;
    let end = parser.ws(end);
    if end != chars.len() {
        return Err(error_text(&chars, Error { message: "Extra data", position: end }));
    }
    Ok(value)
}

/// `bytes.decode("utf-8")`; `Err` is `str(UnicodeDecodeError)`.
pub fn decode_utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|error| {
        let position = error.valid_up_to();
        let lead = bytes[position];
        let (length, reason) = match error.error_len() {
            None => (bytes.len() - position, "unexpected end of data"),
            Some(n) if (0xc2..=0xf4).contains(&lead) => (n, "invalid continuation byte"),
            Some(n) => (n, "invalid start byte"),
        };
        if length == 1 {
            format!("'utf-8' codec can't decode byte 0x{lead:02x} in position {position}: {reason}")
        } else {
            format!("'utf-8' codec can't decode bytes in position {position}-{}: {reason}", position + length - 1)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testutil::{python, TempDir};

    #[test]
    fn values_follow_python_rules() {
        assert_eq!(loads(" {\"a\": [1, 2.5, -0, 1e2, true, null, \"\\u00e9\\ud83d\\ude00\"]} ").unwrap(), serde_json::json!({"a": [1, 2.5, 0, 100.0, true, null, "é😀"]}));
        assert_eq!(loads("[NaN, Infinity, -Infinity, 1e400]").unwrap(), serde_json::json!([NAN, INFINITY, NEG_INFINITY, INFINITY]));
        assert_eq!(loads("{\"a\": 1, \"a\": 2}").unwrap(), serde_json::json!({"a": 2}));
        assert_eq!(loads("\"\\ud800\"").unwrap(), serde_json::json!("\u{fffd}"));
        assert_eq!(loads("18446744073709551616").unwrap(), serde_json::json!(18446744073709551616.0));
    }

    #[test]
    fn errors_match_python_json_loads() {
        let cases = [
            "\"\\x\"", "\"\\u12\"", "\"\\u12g4\"", "\"ab", "\"a\\", "\"a\u{1}b\"", "{\"a\":1,\"b\"}", "{\"a\" : }", "{,}", "[,]",
            "[1,]", "{\"a\":1 \"b\":2}", "-", "01", "1.", "1e", "tru", "{\"a\":1}\n x", "\u{feff}{}", "[", "{", "{\"a\"", "  ",
            "{\"a\":[1,{\"b\":}]}", "{\"a\":1,}", "[1 2]", "\n\n  {\"é\": x}", "[\"\\ud83d\\uzzzz\"]", "\"\\u1234", "{\"a\":1}}", "",
            "nul", "-x", "[1,\n 2,]", "{\"k\":\t\"v\u{1f}\"}",
        ];
        let dir = TempDir::new();
        std::fs::write(dir.join("cases.json"), serde_json::Value::from(cases.to_vec()).to_string()).unwrap();
        let code = r#"
import json
for case in json.load(open("cases.json", encoding="utf-8")):
    try:
        json.loads(case)
        print("OK")
    except ValueError as error:
        print(error)
"#;
        let Some(output) = python(dir.path(), code, &[]) else { return };
        for (case, expected) in cases.iter().zip(output.lines()) {
            let actual = match loads(case) {
                Ok(_) => "OK".to_owned(),
                Err(error) => error,
            };
            assert_eq!(actual, expected, "{case:?}");
        }
    }

    #[test]
    fn utf8_errors_match_python() {
        let cases: &[&[u8]] = &[b"ab\xffcd", b"a\xc3\x28", b"a\xe2\x82\"", b"x\xe2\x82", b"\xf0\x9f\x98", b"\x80", b"\xed\xa0\x80", b"\xf4\x90\x80\x80", b"\xc0\xaf"];
        let dir = TempDir::new();
        for (index, case) in cases.iter().enumerate() {
            std::fs::write(dir.join(&index.to_string()), case).unwrap();
        }
        let code = format!(
            "for i in range({}):\n    try:\n        open(str(i), 'rb').read().decode('utf-8')\n        print('OK')\n    except UnicodeDecodeError as e:\n        print(e)\n",
            cases.len()
        );
        let Some(output) = python(dir.path(), &code, &[]) else { return };
        for (case, expected) in cases.iter().zip(output.lines()) {
            assert_eq!(decode_utf8(case).unwrap_err(), expected, "{case:?}");
        }
    }
}
