//! A small port of the parts of Python's `decimal` module the calculator uses:
//! finite decimals `(-1)^sign · coefficient · 10^exponent` (trailing zeros are kept,
//! as Python keeps them), ROUND_HALF_EVEN contexts with precision/Emax/Emin and
//! Overflow/Underflow traps, exact `+ - * /` with Python's ideal exponents, `sqrt`,
//! `ln`, `log10`, `exp` and `**`, `str()`, `format(…, "f")` and `format(…, "E")`.
//!
//! Transcendental results are computed in fixed point with generous guard digits
//! and then rounded once, like libmpdec's "almost always correctly rounded"
//! functions.

use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};
use std::cmp::Ordering;
use std::fmt;

/// A trapped decimal condition (Python's `DecimalException` subclasses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Overflow,
    Underflow,
    DivisionByZero,
    InvalidOperation,
}

pub type DResult = Result<Decimal, Signal>;

/// The arithmetic context (`decimal.localcontext()` fields that matter here).
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub prec: u32,
    pub emax: i64,
    pub emin: i64,
    pub trap_underflow: bool,
}

impl Context {
    /// Python's default context: `prec=28, Emax=999999, Emin=-999999`.
    pub const DEFAULT: Self = Self { prec: 28, emax: 999_999, emin: -999_999, trap_underflow: false };
    /// The calculator's evaluation context (`prec=28, Emax=1000, Emin=-1000`, Underflow trapped).
    pub const EVALUATE: Self = Self { prec: 28, emax: 1000, emin: -1000, trap_underflow: true };

    pub fn with_prec(self, prec: u32) -> Self { Self { prec, ..self } }
    fn etiny(&self) -> i64 { self.emin - self.prec as i64 + 1 }
}

#[derive(Clone, Debug)]
pub struct Decimal {
    negative: bool,
    coefficient: BigUint,
    exponent: i64,
}

fn pow10(n: u64) -> BigUint { BigUint::from(10u32).pow(n as u32) }

fn digits(value: &BigUint) -> i64 {
    if value.is_zero() {
        return 1;
    }
    // to_str_radix is exact; bits give a fast estimate for big numbers.
    let estimate = (value.bits() as f64 * std::f64::consts::LOG10_2) as i64;
    let mut count = estimate.max(1);
    let mut power = pow10(count as u64 - 1);
    while &power > value {
        count -= 1;
        power /= 10u32;
    }
    loop {
        let next = &power * 10u32;
        if &next > value {
            return count;
        }
        power = next;
        count += 1;
    }
}

/// Python's `str.isspace()` for the characters `Decimal()` strips.
fn is_py_space(ch: char) -> bool { ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch) }

impl Decimal {
    pub fn zero() -> Self { Self { negative: false, coefficient: BigUint::zero(), exponent: 0 } }
    pub fn from_parts(negative: bool, coefficient: BigUint, exponent: i64) -> Self { Self { negative, coefficient, exponent } }
    pub fn from_i64(value: i64) -> Self {
        Self { negative: value < 0, coefficient: BigUint::from(value.unsigned_abs()), exponent: 0 }
    }
    pub fn from_biguint(value: BigUint) -> Self { Self { negative: false, coefficient: value, exponent: 0 } }

    /// `Decimal(text)`: finite values only (NaN/Infinity are rejected, as the
    /// calculator rejects them anyway). Exact, no context rounding.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(is_py_space);
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match body.find(['e', 'E']) {
            Some(index) => (&body[..index], Some(&body[index + 1..])),
            None => (body, None),
        };
        let (integer, fraction) = match mantissa.find('.') {
            Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
            None => (mantissa, ""),
        };
        let clean = |part: &str| -> Option<String> {
            // Underscores are allowed between digits (Python 3.6+).
            if part.starts_with('_') || part.ends_with('_') || part.contains("__") {
                return None;
            }
            let digits: String = part.chars().filter(|c| *c != '_').collect();
            digits.bytes().all(|b| b.is_ascii_digit()).then_some(digits)
        };
        let (integer, fraction) = (clean(integer)?, clean(fraction)?);
        if integer.is_empty() && fraction.is_empty() {
            return None;
        }
        let exponent: i64 = match exponent {
            Some(text) => {
                let (sign, digits) = match text.as_bytes().first() {
                    Some(b'-') => (-1i64, &text[1..]),
                    Some(b'+') => (1, &text[1..]),
                    _ => (1, text),
                };
                let digits = clean(digits)?;
                if digits.is_empty() {
                    return None;
                }
                let trimmed = digits.trim_start_matches('0');
                if trimmed.len() > 15 {
                    return None; // far outside any range the calculator accepts
                }
                sign * trimmed.parse::<i64>().unwrap_or(0)
            }
            None => 0,
        };
        let all = format!("{integer}{fraction}");
        let coefficient = all.parse::<BigUint>().ok()?;
        Some(Self { negative, coefficient, exponent: exponent - fraction.len() as i64 })
    }

    pub fn is_zero(&self) -> bool { self.coefficient.is_zero() }
    pub fn is_negative(&self) -> bool { self.negative && !self.is_zero() }
    pub fn is_signed(&self) -> bool { self.negative }
    pub fn coefficient(&self) -> &BigUint { &self.coefficient }
    pub fn exponent(&self) -> i64 { self.exponent }

    /// `Decimal.adjusted()`: exponent of the most significant digit.
    pub fn adjusted(&self) -> i64 {
        if self.is_zero() { self.exponent } else { self.exponent + digits(&self.coefficient) - 1 }
    }

    /// `x == x.to_integral_value()`.
    pub fn is_integer(&self) -> bool {
        if self.exponent >= 0 || self.is_zero() {
            return true;
        }
        let drop = (-self.exponent) as u64;
        if drop as i64 > digits(&self.coefficient) {
            return false;
        }
        (&self.coefficient % pow10(drop)).is_zero()
    }

    /// The exact integer value (callers check `is_integer` and the size first).
    pub fn to_bigint(&self) -> BigInt {
        let magnitude = if self.exponent >= 0 {
            &self.coefficient * pow10(self.exponent as u64)
        } else {
            &self.coefficient / pow10((-self.exponent) as u64)
        };
        BigInt::from_biguint(if self.negative { Sign::Minus } else { Sign::Plus }, magnitude)
    }

    /// `float(x)` (correctly rounded through the decimal string).
    pub fn to_f64(&self) -> f64 {
        let text = format!("{}{}e{}", if self.negative { "-" } else { "" }, self.coefficient, self.exponent);
        text.parse().unwrap_or(f64::NAN)
    }

    pub fn copy_abs(&self) -> Self { Self { negative: false, ..self.clone() } }
    pub fn copy_negate(&self) -> Self { Self { negative: !self.negative, ..self.clone() } }

    /// Compare values (`-0 == 0`; representation is ignored).
    pub fn compare(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return if other.negative { Ordering::Greater } else { Ordering::Less },
            (false, true) => return if self.negative { Ordering::Less } else { Ordering::Greater },
            _ => {}
        }
        if self.negative != other.negative {
            return if self.negative { Ordering::Less } else { Ordering::Greater };
        }
        let magnitude = {
            let (a, b) = (self.adjusted(), other.adjusted());
            if a != b {
                a.cmp(&b)
            } else {
                let exponent = self.exponent.min(other.exponent);
                let left = &self.coefficient * pow10((self.exponent - exponent) as u64);
                let right = &other.coefficient * pow10((other.exponent - exponent) as u64);
                left.cmp(&right)
            }
        };
        if self.negative { magnitude.reverse() } else { magnitude }
    }

    pub fn eq_value(&self, other: &Self) -> bool { self.compare(other) == Ordering::Equal }

    /// Round an exact (or, with `sticky`, slightly larger in magnitude) value in `context`.
    pub fn finish(negative: bool, coefficient: BigUint, exponent: i64, sticky: bool, context: &Context) -> DResult {
        if coefficient.is_zero() {
            // Python clamps the exponent of zero silently.
            let exponent = exponent.clamp(context.etiny(), context.emax);
            return Ok(Self { negative, coefficient, exponent });
        }
        let count = digits(&coefficient);
        let adjusted = exponent + count - 1;
        let target = (adjusted - context.prec as i64 + 1).max(context.etiny());
        let (mut coefficient, mut exponent, mut inexact) = (coefficient, exponent, sticky);
        if exponent < target {
            let drop = (target - exponent) as u64;
            if drop as i64 > count + 1 {
                // Everything is discarded: the remainder is below half a unit.
                coefficient = BigUint::zero();
                inexact = true;
            } else {
                let divisor = pow10(drop);
                let (quotient, remainder) = coefficient.div_rem(&divisor);
                let half = &divisor / 2u32;
                let up = match remainder.cmp(&half) {
                    Ordering::Greater => true,
                    Ordering::Equal => sticky || quotient.is_odd(),
                    Ordering::Less => false,
                };
                inexact |= !remainder.is_zero();
                coefficient = if up { quotient + 1u32 } else { quotient };
            }
            exponent = target;
            if digits(&coefficient) > context.prec as i64 {
                coefficient /= 10u32;
                exponent += 1;
            }
        }
        if coefficient.is_zero() {
            if inexact && context.trap_underflow {
                return Err(Signal::Underflow);
            }
            return Ok(Self { negative, coefficient, exponent: context.etiny() });
        }
        let adjusted = exponent + digits(&coefficient) - 1;
        if adjusted > context.emax {
            return Err(Signal::Overflow);
        }
        if adjusted < context.emin && inexact && context.trap_underflow {
            return Err(Signal::Underflow);
        }
        Ok(Self { negative, coefficient, exponent })
    }

    /// `+x` (round to the context).
    pub fn plus(&self, context: &Context) -> DResult {
        // Python's unary plus/minus turn -0 into 0.
        let negative = self.negative && !self.is_zero();
        Self::finish(negative, self.coefficient.clone(), self.exponent, false, context)
    }

    /// `-x`.
    pub fn neg(&self, context: &Context) -> DResult {
        let negative = !self.negative && !self.is_zero();
        Self::finish(negative, self.coefficient.clone(), self.exponent, false, context)
    }

    /// `abs(x)`.
    pub fn abs(&self, context: &Context) -> DResult {
        Self::finish(false, self.coefficient.clone(), self.exponent, false, context)
    }

    fn signed(&self) -> BigInt {
        BigInt::from_biguint(if self.negative { Sign::Minus } else { Sign::Plus }, self.coefficient.clone())
    }

    pub fn add(&self, other: &Self, context: &Context) -> DResult {
        let exponent = self.exponent.min(other.exponent);
        let left = self.signed() * BigInt::from(pow10((self.exponent - exponent) as u64));
        let right = other.signed() * BigInt::from(pow10((other.exponent - exponent) as u64));
        let sum = left + right;
        let negative = if sum.is_zero() { self.negative && other.negative } else { sum.is_negative() };
        Self::finish(negative, sum.magnitude().clone(), exponent, false, context)
    }

    pub fn sub(&self, other: &Self, context: &Context) -> DResult { self.add(&other.copy_negate(), context) }

    pub fn mul(&self, other: &Self, context: &Context) -> DResult {
        Self::finish(self.negative != other.negative, &self.coefficient * &other.coefficient,
                     self.exponent + other.exponent, false, context)
    }

    pub fn div(&self, other: &Self, context: &Context) -> DResult {
        if other.is_zero() {
            return Err(if self.is_zero() { Signal::InvalidOperation } else { Signal::DivisionByZero });
        }
        let negative = self.negative != other.negative;
        let ideal = self.exponent - other.exponent;
        if self.is_zero() {
            return Self::finish(negative, BigUint::zero(), ideal, false, context);
        }
        let shift = (context.prec as i64 + 2 + digits(&other.coefficient) - digits(&self.coefficient)).max(0);
        let numerator = &self.coefficient * pow10(shift as u64);
        let (mut quotient, remainder) = numerator.div_rem(&other.coefficient);
        let mut exponent = ideal - shift;
        let exact = remainder.is_zero();
        if exact {
            while exponent < ideal && (&quotient % 10u32).is_zero() {
                quotient /= 10u32;
                exponent += 1;
            }
        }
        Self::finish(negative, quotient, exponent, !exact, context)
    }

    pub fn div_int(&self, divisor: u32, context: &Context) -> DResult { self.div(&Self::from_i64(divisor as i64), context) }

    /// `x.sqrt()` (x ≥ 0 is checked by the caller).
    pub fn sqrt(&self, context: &Context) -> DResult {
        if self.negative && !self.is_zero() {
            return Err(Signal::InvalidOperation);
        }
        let ideal = self.exponent.div_euclid(2);
        if self.is_zero() {
            return Self::finish(self.negative, BigUint::zero(), ideal, false, context);
        }
        let (mut coefficient, mut exponent) = (self.coefficient.clone(), self.exponent);
        if exponent.rem_euclid(2) != 0 {
            coefficient *= 10u32;
            exponent -= 1;
        }
        let wanted = 2 * (context.prec as i64 + 2);
        let shift = ((wanted - digits(&coefficient) + 1) / 2).max(0);
        let scaled = coefficient * pow10(2 * shift as u64);
        let mut root = scaled.sqrt();
        let exact = &root * &root == scaled;
        let mut exponent = exponent / 2 - shift;
        if exact {
            while exponent < ideal && (&root % 10u32).is_zero() {
                root /= 10u32;
                exponent += 1;
            }
        }
        Self::finish(false, root, exponent, !exact, context)
    }

    /// `x.ln()` (x > 0 is checked by the caller).
    pub fn ln(&self, context: &Context) -> DResult {
        if self.is_zero() || self.negative {
            return Err(Signal::InvalidOperation);
        }
        if self.eq_value(&Self::from_i64(1)) {
            return Ok(Self::zero());
        }
        let scale = self.log_scale(context);
        let value = ln_fixed(&self.coefficient, self.exponent, scale);
        from_fixed(value, scale, context)
    }

    /// `x.log10()`; exact powers of ten give exact integers, as in Python.
    pub fn log10(&self, context: &Context) -> DResult {
        if self.is_zero() || self.negative {
            return Err(Signal::InvalidOperation);
        }
        let stripped = self.coefficient.to_string();
        if stripped.trim_end_matches('0') == "1" {
            return Self::from_i64(self.adjusted()).plus(context);
        }
        let scale = self.log_scale(context);
        let value = ln_fixed(&self.coefficient, self.exponent, scale + 5);
        let ten = ln10(scale + 5);
        let value = (value * BigInt::from(pow10(scale as u64 + 5))) / ten;
        from_fixed(value / BigInt::from(pow10(5)), scale, context)
    }

    /// Fixed-point digits so that ln(x) keeps `prec` significant digits even next to 1.
    fn log_scale(&self, context: &Context) -> i64 {
        let one = Self::from_i64(1);
        let distance = self.sub(&one, &Context { prec: 4000, emax: i64::MAX / 4, emin: i64::MIN / 4, trap_underflow: false })
            .map(|d| d.adjusted()).unwrap_or(0);
        context.prec as i64 + 30 + (-distance).max(0)
    }

    /// `x.exp()`.
    pub fn exp(&self, context: &Context) -> DResult {
        if self.is_zero() {
            return Ok(Self::from_i64(1));
        }
        let guard = context.prec as i64 + 5;
        if self.adjusted() < -guard {
            // 1 ± tiny: the correctly rounded result is 1 (inexact).
            let shift = guard as u64 + 2;
            let base = pow10(shift);
            let coefficient = if self.negative { base - 1u32 } else { base };
            return Self::finish(false, coefficient, -(shift as i64), true, context);
        }
        // Overflow / underflow far beyond the context without big arithmetic.
        let estimate = self.to_f64() / std::f64::consts::LN_10;
        if estimate > context.emax as f64 + 2.0 {
            return Err(Signal::Overflow);
        }
        if estimate < context.etiny() as f64 - 2.0 {
            return if context.trap_underflow { Err(Signal::Underflow) }
                   else { Self::finish(false, BigUint::zero(), context.etiny(), true, context) };
        }
        let scale = context.prec as i64 + 30;
        let fixed = to_fixed(self, scale);
        let (coefficient, exponent) = exp_fixed(fixed, scale);
        Self::finish(false, coefficient, exponent, true, context)
    }

    /// `x ** y` with the checks the calculator applies first (|y| ≤ 1000,
    /// a negative base only with a whole exponent, no 0 ** non-positive).
    pub fn pow(&self, exponent: &Self, context: &Context) -> DResult {
        if exponent.is_integer() {
            let power = exponent.to_bigint().to_i64().ok_or(Signal::Overflow)?;
            return self.pow_int(power, context);
        }
        if self.negative && !self.is_zero() {
            return Err(Signal::InvalidOperation);
        }
        if self.is_zero() {
            return if exponent.negative { Err(Signal::DivisionByZero) } else { Ok(Self::zero()) };
        }
        if self.eq_value(&Self::from_i64(1)) {
            return Self::finish(false, pow10(context.prec as u64 - 1), -(context.prec as i64 - 1), false, context);
        }
        // Rough size first: y·log10(x).
        let log = self.log10_estimate() * exponent.to_f64();
        if log > context.emax as f64 + 2.0 {
            return Err(Signal::Overflow);
        }
        if log < context.etiny() as f64 - 2.0 {
            return if context.trap_underflow { Err(Signal::Underflow) }
                   else { Self::finish(false, BigUint::zero(), context.etiny(), true, context) };
        }
        let scale = context.prec as i64 + 35;
        let ln = ln_fixed(&self.coefficient, self.exponent, scale + 10);
        let y = to_fixed(exponent, scale + 10);
        let product = (ln * y) / BigInt::from(pow10(scale as u64 + 20));
        let (coefficient, exponent) = exp_fixed(product, scale);
        Self::finish(false, coefficient, exponent, true, context)
    }

    fn pow_int(&self, power: i64, context: &Context) -> DResult {
        if power == 0 {
            if self.is_zero() {
                return Err(Signal::InvalidOperation);
            }
            return Self::finish(false, BigUint::one(), 0, false, context);
        }
        let magnitude = power.unsigned_abs();
        let negative = self.negative && magnitude % 2 == 1;
        if self.is_zero() {
            return if power < 0 { Err(Signal::DivisionByZero) }
                   else { Self::finish(negative, BigUint::zero(), self.exponent.saturating_mul(power), false, context) };
        }
        // Size check before the exact power: n·log10(x).
        let size = self.log10_estimate() * power as f64;
        if size > context.emax as f64 + 2.0 {
            return Err(Signal::Overflow);
        }
        if size < context.etiny() as f64 - 2.0 {
            return if context.trap_underflow { Err(Signal::Underflow) }
                   else { Self::finish(negative, BigUint::zero(), context.etiny(), true, context) };
        }
        let coefficient = num_traits::pow::pow(self.coefficient.clone(), magnitude as usize);
        let exact = Self { negative, coefficient, exponent: self.exponent * magnitude as i64 };
        if power > 0 {
            exact.plus(context).map(|mut value| {
                value.negative = negative;
                value
            })
        } else {
            let mut result = Self::from_i64(1).div(&exact.copy_abs(), context)?;
            result.negative = negative;
            Ok(result)
        }
    }

    fn digit_string(&self) -> String { self.coefficient.to_string() }

    /// log10(|x|) as a float, for range checks (x ≠ 0).
    fn log10_estimate(&self) -> f64 {
        let digits = self.digit_string();
        let lead = &digits[..digits.len().min(17)];
        let mantissa = lead.parse::<f64>().unwrap_or(1.0) / 10f64.powi(lead.len() as i32 - 1);
        self.adjusted() as f64 + mantissa.log10()
    }

    /// `format(x, "f")` without a precision: all coefficient digits, plain notation.
    pub fn format_fixed(&self) -> String {
        let digits = self.digit_string();
        let sign = if self.negative { "-" } else { "" };
        if self.exponent >= 0 {
            let zeros = if self.is_zero() { String::new() } else { "0".repeat(self.exponent as usize) };
            return format!("{sign}{digits}{zeros}");
        }
        let point = digits.len() as i64 + self.exponent;
        if point > 0 {
            format!("{sign}{}.{}", &digits[..point as usize], &digits[point as usize..])
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        }
    }

    /// `format(x, "E")` without a precision.
    pub fn format_scientific(&self) -> String {
        let digits = self.digit_string();
        let sign = if self.negative { "-" } else { "" };
        let exponent = self.adjusted();
        let mantissa = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{mantissa}E{}{}", if exponent < 0 { "-" } else { "+" }, exponent.abs())
    }
}

/// `str(Decimal)` (to-scientific-string).
impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.digit_string();
        let left = self.exponent + digits.len() as i64;
        let dot = if self.exponent <= 0 && left > -6 { left } else { 1 };
        let (integer, fraction) = if dot <= 0 {
            ("0".to_string(), format!(".{}{digits}", "0".repeat((-dot) as usize)))
        } else if dot >= digits.len() as i64 {
            (format!("{digits}{}", "0".repeat((dot - digits.len() as i64) as usize)), String::new())
        } else {
            (digits[..dot as usize].to_string(), format!(".{}", &digits[dot as usize..]))
        };
        let exponent = if left == dot { String::new() } else { format!("E{:+}", left - dot) };
        write!(f, "{}{integer}{fraction}{exponent}", if self.negative { "-" } else { "" })
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool { self.eq_value(other) }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.compare(other)) }
}

// ---- fixed point helpers (integers scaled by 10^scale) ----

fn fx_one(scale: i64) -> BigInt { BigInt::from(pow10(scale as u64)) }

/// `x · 10^scale`, truncated toward zero.
fn to_fixed(value: &Decimal, scale: i64) -> BigInt {
    let shift = value.exponent + scale;
    let magnitude = if shift >= 0 {
        &value.coefficient * pow10(shift as u64)
    } else if -shift > digits(&value.coefficient) + 1 {
        BigUint::zero()
    } else {
        &value.coefficient / pow10((-shift) as u64)
    };
    BigInt::from_biguint(if value.negative { Sign::Minus } else { Sign::Plus }, magnitude)
}

fn from_fixed(value: BigInt, scale: i64, context: &Context) -> DResult {
    Decimal::finish(value.is_negative(), value.magnitude().clone(), -scale, true, context)
}

/// Σ 1/((2k+1)·n^(2k+1)) · 10^scale = atanh(1/n).
fn atanh_inverse(n: u32, scale: i64) -> BigInt {
    let mut power = fx_one(scale) / BigInt::from(n);
    let square = BigInt::from(n) * BigInt::from(n);
    let mut sum = BigInt::zero();
    let mut k = 1u64;
    while !power.is_zero() {
        sum += &power / BigInt::from(k);
        power /= &square;
        k += 2;
    }
    sum
}

fn ln2(scale: i64) -> BigInt { atanh_inverse(3, scale + 5) * 2 / BigInt::from(pow10(5)) }

fn ln10(scale: i64) -> BigInt {
    let s = scale + 5;
    (atanh_inverse(9, s) * 2 + atanh_inverse(3, s) * 6) / BigInt::from(pow10(5))
}

/// ln(coefficient · 10^exponent) · 10^scale.
fn ln_fixed(coefficient: &BigUint, exponent: i64, scale: i64) -> BigInt {
    let work = scale + 10;
    let count = digits(coefficient);
    let adjusted = exponent + count - 1;
    // m = coefficient / 10^(count-1) ∈ [1, 10).
    let mut m = BigInt::from(coefficient * pow10(work as u64)) / BigInt::from(pow10(count as u64 - 1));
    let one = fx_one(work);
    // Divide by 2^k so that m lands near 1 (|z| ≤ 0.2 below).
    let mut k = 0i64;
    let limit = &one * 3 / 2;
    while m >= limit {
        m /= 2;
        k += 1;
    }
    let lower = &one * 3 / 4;
    while m < lower {
        m *= 2;
        k -= 1;
    }
    let z = ((&m - &one) * &one) / (&m + &one);
    let z2 = (&z * &z) / &one;
    let mut term = z.clone();
    let mut sum = BigInt::zero();
    let mut n = 1i64;
    while !term.is_zero() {
        sum += &term / BigInt::from(n);
        term = (&term * &z2) / &one;
        n += 2;
    }
    let result = sum * 2 + ln2(work) * BigInt::from(k) + ln10(work) * BigInt::from(adjusted);
    result / BigInt::from(pow10(10))
}

/// exp(value / 10^scale) as (coefficient, exponent) with ≥ scale - 5 significant digits.
fn exp_fixed(value: BigInt, scale: i64) -> (BigUint, i64) {
    let work = scale + 10;
    let value = value * BigInt::from(pow10(10));
    let ten = ln10(work + 6);
    let ten = ten / BigInt::from(pow10(6));
    // value = k·ln10 + r, 0 ≤ r < ln10.
    let k = value.div_floor(&ten);
    let mut r = &value - &k * &ten;
    let one = fx_one(work);
    // Halve 10 times, Taylor series, square back.
    let halvings = 10u32;
    r /= BigInt::from(1u32 << halvings);
    let mut term = one.clone();
    let mut sum = one.clone();
    let mut n = 1u32;
    loop {
        term = (&term * &r) / (&one * BigInt::from(n));
        if term.is_zero() {
            break;
        }
        sum += &term;
        n += 1;
    }
    for _ in 0..halvings {
        sum = (&sum * &sum) / &one;
    }
    let k = k.to_i64().unwrap_or(0);
    (sum.magnitude().clone(), k - work)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal { Decimal::parse(text).unwrap() }
    const C: Context = Context::EVALUATE;

    #[test]
    fn strings_match_python() {
        for (input, expected) in [("0.3", "0.3"), ("1.5000", "1.5000"), ("1e20", "1E+20"), ("-0", "-0"), (".000001", "0.000001"),
                                  ("0.0000001", "1E-7"), ("123E-2", "1.23"), ("1E+3", "1E+3"), ("100", "100"), ("12.5", "12.5")] {
            assert_eq!(d(input).to_string(), expected, "{input}");
        }
        assert_eq!(d("1.2500e-12").format_scientific(), "1.2500E-12");
        assert_eq!(d("1E+3").format_fixed(), "1000");
        assert_eq!(d("1.5000").format_fixed(), "1.5000");
        assert!(Decimal::parse("NaN").is_none() && Decimal::parse("1..2").is_none() && Decimal::parse("").is_none());
        assert_eq!(d(" 1_000 ").to_string(), "1000");
    }

    #[test]
    fn arithmetic_exponents_match_python() {
        assert_eq!(d("0.1").add(&d("0.2"), &C).unwrap().to_string(), "0.3");
        assert_eq!(d("1").div(&d("3"), &C).unwrap().to_string(), "0.3333333333333333333333333333");
        assert_eq!(d("2").div(&d("3"), &C).unwrap().to_string(), "0.6666666666666666666666666667");
        assert_eq!(d("1").div_int(100, &C).unwrap().to_string(), "0.01");
        assert_eq!(d("200").mul(&d("0.1"), &C).unwrap().to_string(), "20.0");
        assert_eq!(d("6").div(&d("2"), &C).unwrap().to_string(), "3");
        assert_eq!(d("81").sqrt(&C).unwrap().to_string(), "9");
        assert_eq!(d("2").sqrt(&C).unwrap().to_string(), "1.414213562373095048801688724");
        assert_eq!(d("2").ln(&C).unwrap().to_string(), "0.6931471805599453094172321215");
        assert_eq!(d("1000").log10(&C).unwrap().to_string(), "3");
        assert_eq!(d("2").log10(&C).unwrap().to_string(), "0.3010299956639811952137388947");
        assert_eq!(d("1").exp(&C).unwrap().to_string(), "2.718281828459045235360287471");
        assert_eq!(d("-1").exp(&C).unwrap().to_string(), "0.3678794411714423215955237702");
        assert_eq!(d("2").pow(&d("0.5"), &C).unwrap().to_string(), "1.414213562373095048801688724");
        assert_eq!(d("2").pow(&d("-2"), &C).unwrap().to_string(), "0.25");
        assert_eq!(d("1.000000000000000000000000001").ln(&C).unwrap().to_string(), "9.999999999999999999999999995E-28");
        assert_eq!(d("9").pow(&d("999"), &C).unwrap().to_string(), "1.942079168580724010733305132E+953");
        assert_eq!(d("2300").exp(&C).unwrap().to_string(), "7.538906854065543421691995605E+998");
        assert!(matches!(d("1e1000").mul(&d("1e1000"), &C), Err(Signal::Overflow)));
        assert!(matches!(d("1e-1000").div(&d("1e1000"), &C), Err(Signal::Underflow)));
    }
}
