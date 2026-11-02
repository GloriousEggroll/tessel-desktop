//! Python `datetime` arithmetic: proleptic Gregorian ordinals (0001-01-01 = 1),
//! microsecond naive date-times and `timedelta`, with Python's range checks.
//! Every operation that can raise `OverflowError` in Python returns
//! `Err(Overflow)` so the event engine fails exactly where Python fails.

pub const MAX_ORDINAL: i64 = 3_652_059; // date(9999, 12, 31)
pub const MAX_DELTA_DAYS: i128 = 999_999_999;
pub const DAY_US: i64 = 86_400_000_000;
pub const SECOND_US: i64 = 1_000_000;
/// `datetime(1970, 1, 1).toordinal()`.
pub const EPOCH_ORDINAL: i64 = 719_163;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow;

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("date value out of range")
    }
}

pub type Checked<T> = Result<T, Overflow>;

const DAYS_IN_MONTH: [i64; 13] = [-1, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const DAYS_BEFORE_MONTH: [i64; 13] = [-1, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub fn days_in_month(year: i64, month: i64) -> i64 {
    if month == 2 && is_leap(year) { 29 } else { DAYS_IN_MONTH[month as usize] }
}

fn days_before_year(year: i64) -> i64 {
    let y = year - 1;
    y * 365 + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)
}

fn days_before_month(year: i64, month: i64) -> i64 {
    DAYS_BEFORE_MONTH[month as usize] + i64::from(month > 2 && is_leap(year))
}

pub fn ymd_to_ordinal(year: i64, month: i64, day: i64) -> i64 {
    days_before_year(year) + days_before_month(year, month) + day
}

pub fn ordinal_to_ymd(n: i64) -> (i64, i64, i64) {
    let mut n = n - 1;
    let n400 = n.div_euclid(146_097);
    n = n.rem_euclid(146_097);
    let mut year = n400 * 400 + 1;
    let n100 = n / 36_524;
    n %= 36_524;
    let n4 = n / 1461;
    n %= 1461;
    let n1 = n / 365;
    n %= 365;
    year += n100 * 100 + n4 * 4 + n1;
    if n1 == 4 || n100 == 4 {
        return (year - 1, 12, 31);
    }
    let leap = n1 == 3 && (n4 != 24 || n100 == 3);
    let mut month = (n + 50) >> 5;
    let mut preceding = DAYS_BEFORE_MONTH[month as usize] + i64::from(month > 2 && leap);
    if preceding > n {
        month -= 1;
        preceding -= DAYS_IN_MONTH[month as usize] + i64::from(month == 2 && leap);
    }
    (year, month, n - preceding + 1)
}

/// A `datetime.timedelta` in microseconds (`days` within ±999999999).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Delta(pub i128);

impl Delta {
    pub const ZERO: Delta = Delta(0);

    pub fn new(micros: i128) -> Checked<Delta> {
        let days = micros.div_euclid(DAY_US as i128);
        if days.abs() > MAX_DELTA_DAYS { Err(Overflow) } else { Ok(Delta(micros)) }
    }
    pub fn days(n: i128) -> Checked<Delta> {
        n.checked_mul(DAY_US as i128).ok_or(Overflow).and_then(Delta::new)
    }
    pub fn seconds(n: i128) -> Checked<Delta> {
        n.checked_mul(SECOND_US as i128).ok_or(Overflow).and_then(Delta::new)
    }
    pub fn micros(n: i128) -> Checked<Delta> {
        Delta::new(n)
    }
    /// `timedelta.days` (floor).
    pub fn whole_days(self) -> i128 {
        self.0.div_euclid(DAY_US as i128)
    }
    pub fn neg(self) -> Checked<Delta> {
        Delta::new(-self.0)
    }
    pub fn sub(self, other: Delta) -> Checked<Delta> {
        Delta::new(self.0 - other.0)
    }
    pub fn add(self, other: Delta) -> Checked<Delta> {
        Delta::new(self.0 + other.0)
    }
}

/// A `datetime.date` (ordinal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date(pub i64);

impl Date {
    pub const MIN: Date = Date(1);
    pub const MAX: Date = Date(MAX_ORDINAL);

    pub fn new(year: i64, month: i64, day: i64) -> Option<Date> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date(ymd_to_ordinal(year, month, day)))
    }
    pub fn ymd(y: i64, m: i64, d: i64) -> Date {
        Date::new(y, m, d).expect("valid date")
    }
    pub fn from_ordinal(ordinal: i64) -> Checked<Date> {
        if (1..=MAX_ORDINAL).contains(&ordinal) { Ok(Date(ordinal)) } else { Err(Overflow) }
    }
    pub fn ordinal(self) -> i64 { self.0 }
    pub fn parts(self) -> (i64, i64, i64) { ordinal_to_ymd(self.0) }
    pub fn year(self) -> i64 { self.parts().0 }
    pub fn month(self) -> i64 { self.parts().1 }
    pub fn day(self) -> i64 { self.parts().2 }
    /// Monday = 0.
    pub fn weekday(self) -> i64 { (self.0 + 6) % 7 }
    /// `date + timedelta` uses only `timedelta.days`.
    pub fn add(self, delta: Delta) -> Checked<Date> {
        let days = delta.whole_days();
        let ordinal = self.0 as i128 + days;
        if (1..=MAX_ORDINAL as i128).contains(&ordinal) { Ok(Date(ordinal as i64)) } else { Err(Overflow) }
    }
    pub fn add_days(self, days: i128) -> Checked<Date> {
        self.add(Delta::days(days)?)
    }
    pub fn sub_date(self, other: Date) -> Delta {
        Delta((self.0 - other.0) as i128 * DAY_US as i128)
    }
    pub fn iso(self) -> String {
        let (y, m, d) = self.parts();
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// A naive `datetime.datetime` (microseconds since 0001-01-01T00:00).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Naive(pub i64);

impl Naive {
    pub const MIN: Naive = Naive(0);
    pub const MAX: Naive = Naive(MAX_ORDINAL * DAY_US - 1);

    pub fn new(date: Date, hour: i64, minute: i64, second: i64, micro: i64) -> Naive {
        Naive((date.0 - 1) * DAY_US + ((hour * 60 + minute) * 60 + second) * SECOND_US + micro)
    }
    pub fn from_micros(value: i128) -> Checked<Naive> {
        if (0..=Naive::MAX.0 as i128).contains(&value) { Ok(Naive(value as i64)) } else { Err(Overflow) }
    }
    pub fn date(self) -> Date { Date(self.0.div_euclid(DAY_US) + 1) }
    pub fn time_micros(self) -> i64 { self.0.rem_euclid(DAY_US) }
    pub fn hour(self) -> i64 { self.time_micros() / 3_600_000_000 }
    pub fn minute(self) -> i64 { self.time_micros() / 60_000_000 % 60 }
    pub fn second(self) -> i64 { self.time_micros() / SECOND_US % 60 }
    pub fn microsecond(self) -> i64 { self.time_micros() % SECOND_US }
    pub fn combine(date: Date, time_micros: i64) -> Naive { Naive((date.0 - 1) * DAY_US + time_micros) }
    pub fn add(self, delta: Delta) -> Checked<Naive> { Naive::from_micros(self.0 as i128 + delta.0) }
    pub fn sub(self, other: Naive) -> Delta { Delta(self.0 as i128 - other.0 as i128) }
    /// Seconds since the epoch of this wall time (ZoneInfo `_get_local_timestamp`).
    pub fn local_seconds(self) -> i64 { self.0.div_euclid(SECOND_US) - (EPOCH_ORDINAL - 1) * 86_400 }
    pub fn from_epoch_micros(micros: i128) -> Checked<Naive> {
        Naive::from_micros(micros + ((EPOCH_ORDINAL - 1) * DAY_US) as i128)
    }
    pub fn epoch_micros(self) -> i128 { self.0 as i128 - ((EPOCH_ORDINAL - 1) * DAY_US) as i128 }
    /// `datetime.isoformat()` (microseconds only when non-zero).
    pub fn iso(self) -> String {
        let mut text = format!("{}T{:02}:{:02}:{:02}", self.date().iso(), self.hour(), self.minute(), self.second());
        if self.microsecond() != 0 {
            text += &format!(".{:06}", self.microsecond());
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_round_trip() {
        for ordinal in (1..MAX_ORDINAL).step_by(97).chain([1, 59, 60, 61, 730_120, MAX_ORDINAL]) {
            let (y, m, d) = ordinal_to_ymd(ordinal);
            assert_eq!(ymd_to_ordinal(y, m, d), ordinal);
        }
        assert_eq!(Date::ymd(1970, 1, 1).0, EPOCH_ORDINAL);
        assert_eq!(Date::ymd(2026, 10, 29).weekday(), 3);
        assert_eq!(Date::ymd(1, 1, 1).weekday(), 0);
        assert_eq!(Date::ymd(9999, 12, 31).0, MAX_ORDINAL);
        assert!(Date::new(2023, 2, 29).is_none());
        assert!(Date::ymd(9999, 12, 31).add_days(1).is_err());
        assert!(Delta::days(1_000_000_000).is_err());
        assert_eq!(Delta(-1).whole_days(), -1);
    }
}
