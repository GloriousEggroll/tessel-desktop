//! Port of calculator_backend.py: bounded decimal expression parser and the
//! private history/memory store. No expression evaluation beyond the tokens,
//! operators and named functions below. Percent means division by one hundred.

use crate::compat::{self, PyValue};
use crate::decimal::{Context, Decimal, Signal};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MAX_EXPRESSION: usize = 512;
pub const MAX_TOKENS: usize = 160;
pub const MAX_DEPTH: usize = 32;
pub const MAX_HISTORY: usize = 100;
pub const MAX_STATE_BYTES: usize = 256 * 1024;
pub const FUNCTIONS: [&str; 11] = ["sqrt", "abs", "ln", "log", "exp", "sin", "cos", "tan", "asin", "acos", "atan"];

pub fn pi() -> Decimal { Decimal::parse("3.141592653589793238462643383").unwrap() }
pub fn e() -> Decimal { Decimal::parse("2.718281828459045235360287471").unwrap() }

/// An expression error suitable for showing directly to the user (English; the UI translates it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalculatorError(pub String);

impl std::fmt::Display for CalculatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

fn fail<T>(message: impl Into<String>) -> Result<T, CalculatorError> { Err(CalculatorError(message.into())) }

const OUTSIDE: &str = "This calculation is outside the supported range.";

fn signal(_: Signal) -> CalculatorError { CalculatorError(OUTSIDE.into()) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Number(String),
    Name(String),
    Op(char),
    End,
}

/// Python `str.isspace()`.
fn is_space(c: char) -> bool { c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) }

/// `NUMBER = (?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?` (ASCII) at `pos`; returns the end.
fn match_number(chars: &[char], pos: usize) -> Option<usize> {
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    let mut i = pos;
    if digit(i) {
        while digit(i) { i += 1; }
        if chars.get(i) == Some(&'.') {
            i += 1;
            while digit(i) { i += 1; }
        }
    } else if chars.get(i) == Some(&'.') && digit(i + 1) {
        i += 1;
        while digit(i) { i += 1; }
    } else {
        return None;
    }
    if matches!(chars.get(i), Some('e' | 'E')) {
        let mut j = i + 1;
        if matches!(chars.get(j), Some('+' | '-')) { j += 1; }
        if digit(j) {
            while digit(j) { j += 1; }
            i = j;
        }
    }
    Some(i)
}

pub fn tokenize(expression: &str) -> Result<Vec<Token>, CalculatorError> {
    if expression.chars().count() > MAX_EXPRESSION {
        return fail("This expression is too long.");
    }
    let expression = expression.replace('×', "*").replace('÷', "/").replace('−', "-").replace('π', "pi").replace("**", "^");
    let chars: Vec<char> = expression.chars().collect();
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < chars.len() {
        let c = chars[pos];
        if is_space(c) {
            pos += 1;
            continue;
        }
        if let Some(end) = match_number(&chars, pos) {
            tokens.push(Token::Number(chars[pos..end].iter().collect()));
            pos = end;
        } else if c.is_ascii_alphabetic() {
            let mut end = pos;
            while chars.get(end).is_some_and(char::is_ascii_alphabetic) { end += 1; }
            tokens.push(Token::Name(chars[pos..end].iter().collect::<String>().to_ascii_lowercase()));
            pos = end;
        } else if "+-*/^%()!".contains(c) {
            tokens.push(Token::Op(c));
            pos += 1;
        } else {
            return fail(format!("Unexpected character: {c}"));
        }
        if tokens.len() > MAX_TOKENS {
            return fail("This expression has too many operations.");
        }
    }
    if tokens.is_empty() {
        return fail("Enter an expression first.");
    }
    // 2pi, 2(3+4), (2+3)(4+5), and 2sin(30) are conventional products.
    let mut result: Vec<Token> = Vec::with_capacity(tokens.len() * 2);
    for token in tokens {
        if let Some(previous) = result.last() {
            let ends_value = matches!(previous, Token::Number(_) | Token::Op(')' | '%' | '!'))
                || matches!(previous, Token::Name(n) if n == "pi" || n == "e" || n == "ans");
            let starts_value = matches!(token, Token::Number(_) | Token::Name(_) | Token::Op('('));
            if ends_value && starts_value {
                // Whitespace does not make two adjacent numbers a product.
                if matches!(previous, Token::Number(_)) && matches!(token, Token::Number(_)) {
                    return fail("Use an operator between numbers.");
                }
                result.push(Token::Op('*'));
            }
        }
        result.push(token);
    }
    result.push(Token::End);
    Ok(result)
}

/// `bounded(value)` in `context` (finite; |adjusted| ≤ 1000; rounded with `+value`).
pub fn bounded_in(value: Decimal, context: &Context) -> Result<Decimal, CalculatorError> {
    if !value.is_zero() && value.adjusted().abs() > 1000 {
        return fail("The result is outside the supported range.");
    }
    value.plus(context).map_err(signal)
}

/// `bounded` outside an evaluation (Python's default context).
pub fn bounded(value: Decimal) -> Result<Decimal, CalculatorError> { bounded_in(value, &Context::DEFAULT) }

pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
    degrees: bool,
    ans: Decimal,
    used_trigonometry: bool,
    context: Context,
}

type R = Result<Decimal, CalculatorError>;

impl Parser {
    pub fn new(expression: &str, degrees: bool, ans: Decimal) -> Result<Self, CalculatorError> {
        Ok(Self { tokens: tokenize(expression)?, position: 0, degrees, ans, used_trigonometry: false, context: Context::EVALUATE })
    }

    fn peek_op(&self, op: char) -> bool { self.tokens[self.position] == Token::Op(op) }

    fn take(&mut self, op: char) -> bool {
        if self.peek_op(op) {
            self.position += 1;
            return true;
        }
        false
    }

    fn bounded(&self, value: Result<Decimal, Signal>) -> R { bounded_in(value.map_err(signal)?, &self.context) }

    pub fn parse(&mut self) -> R {
        let result = self.expression(0)?;
        if self.tokens[self.position] != Token::End {
            return fail("Check the operators and parentheses.");
        }
        let mut result = bounded_in(result, &self.context)?;
        if self.used_trigonometry {
            // math functions provide double precision; do not expose binary
            // rounding noise as extra decimal precision in the final result.
            result = result.plus(&self.context.with_prec(15)).map_err(signal)?;
        }
        Ok(result)
    }

    fn expression(&mut self, depth: usize) -> R {
        let mut result = self.term(depth)?;
        while self.peek_op('+') || self.peek_op('-') {
            let plus = self.peek_op('+');
            self.position += 1;
            let right = self.term(depth)?;
            result = self.bounded(if plus { result.add(&right, &self.context) } else { result.sub(&right, &self.context) })?;
        }
        Ok(result)
    }

    fn term(&mut self, depth: usize) -> R {
        let mut result = self.unary(depth)?;
        while self.peek_op('*') || self.peek_op('/') {
            let times = self.peek_op('*');
            self.position += 1;
            let right = self.unary(depth)?;
            if !times && right.is_zero() {
                return fail("Cannot divide by zero.");
            }
            result = self.bounded(if times { result.mul(&right, &self.context) } else { result.div(&right, &self.context) })?;
        }
        Ok(result)
    }

    fn unary(&mut self, depth: usize) -> R {
        if depth > MAX_DEPTH {
            return fail("This expression is nested too deeply.");
        }
        if self.take('+') {
            return self.unary(depth + 1);
        }
        if self.take('-') {
            return self.unary(depth + 1)?.neg(&self.context).map_err(signal);
        }
        self.power(depth)
    }

    fn power(&mut self, depth: usize) -> R {
        let mut result = self.postfix(depth)?;
        if self.take('^') {
            let exponent = self.unary(depth + 1)?;
            if exponent.copy_abs() > Decimal::from_i64(1000) {
                return fail("Powers are limited to exponents between −1000 and 1000.");
            }
            if result.is_negative() && !exponent.is_integer() {
                return fail("A negative base needs a whole-number exponent.");
            }
            if result.is_zero() && exponent <= Decimal::zero() {
                return fail("Zero cannot have a zero or negative exponent.");
            }
            result = self.bounded(result.pow(&exponent, &self.context))?;
        }
        Ok(result)
    }

    fn postfix(&mut self, depth: usize) -> R {
        let mut result = self.atom(depth)?;
        while self.peek_op('%') || self.peek_op('!') {
            if self.take('%') {
                result = result.div_int(100, &self.context).map_err(signal)?;
            } else if self.take('!') {
                if !result.is_integer() || result < Decimal::zero() || result > Decimal::from_i64(170) {
                    return fail("Factorial needs a whole number from 0 to 170.");
                }
                let n: u32 = num_traits::ToPrimitive::to_u32(&result.to_bigint()).unwrap_or(0);
                let factorial = (1..=n).fold(num_bigint::BigUint::from(1u32), |acc, k| acc * k);
                result = Decimal::from_biguint(factorial);
            }
            result = bounded_in(result, &self.context)?;
        }
        Ok(result)
    }

    fn atom(&mut self, depth: usize) -> R {
        match self.tokens[self.position].clone() {
            Token::Number(value) => {
                self.position += 1;
                // Bound the literal exponent before constructing a Decimal.
                if let Some(index) = value.find(['e', 'E']) {
                    let digits = value[index + 1..].trim_start_matches(['+', '-']).trim_start_matches('0');
                    if digits.len() > 4 || digits.parse::<u32>().unwrap_or(0) > 1000 {
                        return fail("The number's exponent is outside the supported range.");
                    }
                }
                bounded_in(Decimal::parse(&value).ok_or_else(|| CalculatorError(OUTSIDE.into()))?, &self.context)
            }
            Token::Op('(') => {
                self.position += 1;
                let result = self.expression(depth + 1)?;
                if !self.take(')') {
                    return fail("A closing parenthesis is missing.");
                }
                Ok(result)
            }
            Token::Name(name) => {
                self.position += 1;
                match name.as_str() {
                    "pi" => return Ok(pi()),
                    "e" => return Ok(e()),
                    "ans" => return Ok(self.ans.clone()),
                    _ => {}
                }
                if !FUNCTIONS.contains(&name.as_str()) {
                    return fail(format!("Unknown function: {name}"));
                }
                if !self.take('(') {
                    return fail(format!("Use parentheses after {name}."));
                }
                let argument = self.expression(depth + 1)?;
                if !self.take(')') {
                    return fail("A closing parenthesis is missing.");
                }
                self.function(&name, argument)
            }
            _ => fail("A number or function is missing."),
        }
    }

    fn function(&mut self, name: &str, value: Decimal) -> R {
        let c = self.context;
        match name {
            "abs" => return value.abs(&c).map_err(signal),
            "sqrt" => {
                if value.is_negative() {
                    return fail("Square root needs a non-negative number.");
                }
                return self.bounded(value.sqrt(&c));
            }
            "ln" | "log" => {
                if value <= Decimal::zero() {
                    return fail("Logarithms need a positive number.");
                }
                return self.bounded(if name == "ln" { value.ln(&c) } else { value.log10(&c) });
            }
            "exp" => {
                if value.copy_abs() > Decimal::from_i64(2300) {
                    return fail("The exponential argument is too large.");
                }
                return self.bounded(value.exp(&c));
            }
            _ => {}
        }
        if (name == "asin" || name == "acos") && (value < Decimal::from_i64(-1) || value > Decimal::from_i64(1)) {
            return fail("Inverse sine and cosine need a value between −1 and 1.");
        }
        if value.copy_abs() > Decimal::parse("1e12").unwrap() {
            return fail("The trigonometric argument is too large.");
        }
        let mut argument = value.to_f64();
        self.used_trigonometry = true;
        if matches!(name, "sin" | "cos" | "tan") {
            if self.degrees {
                argument = argument.to_radians();
            }
            if name == "tan" && argument.cos().abs() < 1e-14 {
                return fail("Tangent is undefined at this angle.");
            }
        }
        let mut result = match name {
            "sin" => argument.sin(),
            "cos" => argument.cos(),
            "tan" => argument.tan(),
            "asin" => argument.asin(),
            "acos" => argument.acos(),
            _ => argument.atan(),
        };
        if matches!(name, "asin" | "acos" | "atan") && self.degrees {
            result = result.to_degrees();
        }
        if result.abs() < 1e-15 {
            result = 0.0;
        }
        if !result.is_finite() {
            return Err(CalculatorError(OUTSIDE.into()));
        }
        bounded_in(Decimal::parse(&format_g15(result)).ok_or_else(|| CalculatorError(OUTSIDE.into()))?, &c)
    }
}

/// `format(value, ".15g")`.
pub fn format_g15(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let scientific = format!("{value:.14e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let strip = |text: &str| if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text.to_string() };
    if (-4..15).contains(&exponent) {
        let decimals = (14 - exponent).max(0) as usize;
        strip(&format!("{value:.decimals$}"))
    } else {
        format!("{}e{}{:02}", strip(mantissa), if exponent < 0 { '-' } else { '+' }, exponent.abs())
    }
}

pub fn evaluate(expression: &str, degrees: bool, ans: &Decimal) -> R {
    Parser::new(expression, degrees, ans.clone())?.parse()
}

pub fn format_number(value: &Decimal) -> String {
    if value.is_zero() {
        return "0".into();
    }
    if (-6..=16).contains(&value.adjusted()) {
        let text = value.format_fixed();
        return if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text };
    }
    let text = value.format_scientific();
    let (mantissa, exponent) = text.split_once('E').unwrap();
    let mantissa = if mantissa.contains('.') { mantissa.trim_end_matches('0').trim_end_matches('.') } else { mantissa };
    format!("{mantissa}e{}", exponent.parse::<i64>().unwrap())
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub expression: String,
    pub result: String,
    pub degrees: bool,
    pub timestamp: f64,
}

type Replace = fn(&Path, &Path) -> std::io::Result<()>;

pub struct CalculatorStore {
    pub path: PathBuf,
    pub history: Vec<HistoryEntry>,
    pub memory: Option<Decimal>,
    pub scientific: bool,
    pub degrees: bool,
    pub error: String,
    /// `Path.replace` (tests inject failures).
    pub replace: Replace,
}

fn default_replace(from: &Path, to: &Path) -> std::io::Result<()> { fs::rename(from, to) }

/// `$XDG_STATE_HOME/tessel-dock-calculator/state.json` (Python: `os.environ.get(...) or ~/.local/state`).
pub fn default_state_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| tessel_core::paths::home().join(".local/state"));
    base.join("tessel-dock-calculator/state.json")
}

fn os_error(error: &std::io::Error, path: &Path) -> String { tessel_core::pyerr::os_error_path(error, path) }

impl CalculatorStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut store = Self { path: path.unwrap_or_else(default_state_path), history: Vec::new(), memory: None,
                               scientific: false, degrees: true, error: String::new(), replace: default_replace };
        store.load();
        store
    }

    pub fn load(&mut self) {
        if let Err(error) = self.try_load() {
            self.error = error;
        }
    }

    fn try_load(&mut self) -> Result<(), String> {
        let data = match fs::File::open(&self.path) {
            Ok(file) => {
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_STATE_BYTES as u64 + 1), &mut data)
                    .map_err(|e| os_error(&e, &self.path))?;
                data
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(os_error(&error, &self.path)),
        };
        if data.len() > MAX_STATE_BYTES {
            return Err("Saved calculator history is too large.".into());
        }
        let state = compat::loads(&data).map_err(|e| e.0)?;
        if !matches!(state, PyValue::Dict(_)) {
            return Err("Invalid saved calculator history.".into());
        }
        self.scientific = state.get("scientific") == Some(&PyValue::Bool(true));
        self.degrees = state.get("degrees") != Some(&PyValue::Bool(false));
        // state.get("history", [])[:MAX_HISTORY]
        let items: Vec<PyValue> = match state.get("history") {
            None => Vec::new(),
            Some(PyValue::List(items)) => items.iter().take(MAX_HISTORY).cloned().collect(),
            Some(PyValue::Str(text)) => text.chars().take(MAX_HISTORY).map(|c| PyValue::Str(c.into())).collect(),
            Some(PyValue::Dict(_)) => return Err("unhashable type: 'slice'".into()),
            Some(other) => return Err(format!("'{}' object is not subscriptable", other.type_name())),
        };
        for item in items {
            if let Some(entry) = history_entry(&item) {
                self.history.push(entry);
            }
        }
        if let Some(PyValue::Str(memory)) = state.get("memory") {
            if memory.chars().count() <= 1200 {
                if let Some(value) = Decimal::parse(memory).and_then(|v| bounded(v).ok()) {
                    self.memory = Some(value);
                }
            }
        }
        Ok(())
    }

    fn state_json(&self) -> String {
        let mut out = format!("{{\"version\": 1, \"scientific\": {}, \"degrees\": {}, \"memory\": {}, \"history\": [",
                              self.scientific, self.degrees,
                              self.memory.as_ref().map(|m| compat::json_string(&m.to_string())).unwrap_or_else(|| "null".into()));
        for (index, entry) in self.history.iter().take(MAX_HISTORY).enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{{\"expression\": {}, \"result\": {}, \"degrees\": {}, \"timestamp\": {}}}",
                                  compat::json_string(&entry.expression), compat::json_string(&entry.result), entry.degrees,
                                  tessel_core::pyfmt::float_repr(entry.timestamp)));
        }
        out.push_str("]}\n");
        out
    }

    pub fn save(&mut self) -> bool {
        self.error.clear();
        let parent = self.path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        if let Err(error) = create_private_dirs(&parent) {
            self.error = error;
            return false;
        }
        let (temporary, mut file) = match named_temporary(&parent, ".state-") {
            Ok(pair) => pair,
            Err(error) => {
                self.error = error;
                return false;
            }
        };
        let result = (|| -> Result<(), String> {
            file.write_all(self.state_json().as_bytes()).map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
            file.flush().map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
            file.sync_all().map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
            drop(file);
            (self.replace)(&temporary, &self.path).map_err(|e| match e.raw_os_error() {
                Some(_) => format!("{}: '{}' -> '{}'", tessel_core::pyerr::os_error_text(&e), temporary.display(), self.path.display()),
                None => e.to_string(),
            })
        })();
        let _ = fs::remove_file(&temporary);
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = error;
                false
            }
        }
    }

    pub fn record(&mut self, expression: &str, result: &Decimal) -> HistoryEntry {
        let entry = HistoryEntry { expression: expression.to_owned(), result: format_number(result), degrees: self.degrees,
                                   timestamp: tessel_core::timer::now() };
        self.history.retain(|old| (&old.expression, &old.result, old.degrees) != (&entry.expression, &entry.result, entry.degrees));
        self.history.insert(0, entry.clone());
        self.history.truncate(MAX_HISTORY);
        self.save();
        entry
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.save();
    }

    /// `MC`, `MR`, `MS`, `M+`, `M-`.
    pub fn memory_action(&mut self, action: &str, value: Option<&Decimal>) -> Result<Option<Decimal>, CalculatorError> {
        match action {
            "MC" => self.memory = None,
            "MR" => return Ok(self.memory.clone()),
            "MS" | "M+" | "M-" => {
                let value = bounded(value.cloned().unwrap_or_else(Decimal::zero))?;
                let context = Context::DEFAULT;
                let current = self.memory.clone().unwrap_or_else(Decimal::zero);
                let updated = match action {
                    "MS" => Ok(value),
                    "M+" => current.add(&value, &context),
                    _ => current.sub(&value, &context),
                }
                .map_err(signal)?;
                self.memory = Some(bounded(updated)?);
            }
            _ => return fail("Unknown memory operation"),
        }
        self.save();
        Ok(self.memory.clone())
    }
}

fn history_entry(item: &PyValue) -> Option<HistoryEntry> {
    let (Some(PyValue::Str(expression)), Some(PyValue::Str(result))) = (item.get("expression"), item.get("result")) else {
        return None;
    };
    if expression.chars().count() > MAX_EXPRESSION || result.chars().count() > 1200 {
        return None;
    }
    bounded(Decimal::parse(result)?).ok()?;
    let timestamp = match item.get("timestamp") {
        None => 0.0,
        Some(value) => value.to_float().ok()?,
    };
    if !timestamp.is_finite() {
        return None;
    }
    let degrees = item.get("degrees").is_none_or(PyValue::truthy);
    Some(HistoryEntry { expression: expression.clone(), result: result.clone(), degrees, timestamp })
}

/// `Path.mkdir(parents=True, exist_ok=True, mode=0o700)`.
fn create_private_dirs(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty() && !p.is_dir()) {
        fs::DirBuilder::new().recursive(true).create(parent).map_err(|e| os_error(&e, parent))?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_dir() => Ok(()),
        Err(error) => Err(os_error(&error, path)),
    }
}

/// `tempfile.NamedTemporaryFile(dir=…, prefix=…, delete=False)`: O_EXCL, mode 0600.
fn named_temporary(directory: &Path, prefix: &str) -> Result<(PathBuf, fs::File), String> {
    use std::os::unix::fs::OpenOptionsExt;
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut last = None;
    for _ in 0..100 {
        let mut random = [0u8; 8];
        if fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random)).is_err() {
            random = (tessel_core::timer::now().to_bits() ^ std::process::id() as u64).to_le_bytes();
        }
        let name: String = random.iter().map(|b| LETTERS[*b as usize % LETTERS.len()] as char).collect();
        let path = directory.join(format!("{prefix}{name}"));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(os_error(&error, &path)),
        }
    }
    Err(last.map(|e| tessel_core::pyerr::os_error_text(&e)).unwrap_or_default())
}
