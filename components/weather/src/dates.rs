//! Civil dates (proleptic Gregorian, like Python's `date`), Python-like ISO
//! parsing and IANA time zones resolved exactly like `zoneinfo.ZoneInfo`
//! (keys looked up in TZPATH; the conversion itself uses GLib's tzdata reader).

use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn leap(year: i32) -> bool { year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) }

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        _ => 28,
    }
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        ((1..=9999).contains(&year) && (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month))
            .then_some(Self { year, month, day })
    }

    /// Days since 1970-01-01.
    pub fn to_days(self) -> i64 {
        let (y, m, d) = (self.year as i64 - (self.month <= 2) as i64, self.month as i64, self.day as i64);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    pub fn from_days(days: i64) -> Self {
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + (month <= 2) as i64) as i32;
        Self { year, month, day }
    }

    pub fn add_days(self, days: i64) -> Self { Self::from_days(self.to_days() + days) }

    /// Python `date.weekday()`: Monday 0 … Sunday 6.
    pub fn weekday(self) -> u32 { (self.to_days() + 3).rem_euclid(7) as u32 }

    pub fn iso(self) -> String { format!("{:04}-{:02}-{:02}", self.year, self.month, self.day) }

    /// `date.fromisoformat` (3.11+): `YYYY-MM-DD`, `YYYYMMDD`, `YYYY-Www-D`, `YYYYWwwD`.
    pub fn from_iso(text: &str) -> Option<Self> {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let bytes = text.as_bytes();
        if text.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-' && bytes[5] != b'W' {
            let (y, m, d) = (&text[..4], &text[5..7], &text[8..]);
            if digits(y) && digits(m) && digits(d) {
                return Self::new(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
            }
            return None;
        }
        if text.len() == 8 && digits(text) {
            return Self::new(text[..4].parse().ok()?, text[4..6].parse().ok()?, text[6..].parse().ok()?);
        }
        let week = text.strip_prefix(|_: char| false).unwrap_or(text);
        let (year, rest) = (week.get(..4)?, week.get(4..)?);
        let rest = rest.strip_prefix('-').map(|r| (r, true)).unwrap_or((rest, false));
        let (rest, dashed) = rest;
        let rest = rest.strip_prefix('W')?;
        let (w, d) = if dashed {
            let (w, d) = rest.split_once('-').unwrap_or((rest, "1"));
            (w, d)
        } else {
            (rest.get(..2)?, rest.get(2..).filter(|d| !d.is_empty()).unwrap_or("1"))
        };
        if !digits(year) || !digits(w) || w.len() != 2 || !digits(d) || d.len() != 1 {
            return None;
        }
        let (year, week, day): (i32, i64, i64) = (year.parse().ok()?, w.parse().ok()?, d.parse().ok()?);
        if !(1..=7).contains(&day) || !(1..=53).contains(&week) {
            return None;
        }
        let jan4 = Self::new(year, 1, 4)?;
        let week1 = jan4.add_days(-(jan4.weekday() as i64));
        let result = week1.add_days((week - 1) * 7 + day - 1);
        if week == 53 {
            let next_jan4 = Self::new(year + 1, 1, 4)?;
            if result >= next_jan4.add_days(-(next_jan4.weekday() as i64)) {
                return None;
            }
        }
        Some(result)
    }
}

/// `datetime.fromisoformat` acceptance check (3.11+, common forms): a date,
/// optionally `T` or space then `HH[:MM[:SS[.ffffff]]]` or the compact forms,
/// optionally `Z` or `±HH[:MM]`.
pub fn valid_iso_datetime(text: &str) -> bool {
    if text.is_ascii() && text.len() >= 8 {
        let (date, rest) = if text.len() >= 10 && text.as_bytes()[4] == b'-' { text.split_at(10) } else {
            let cut = text.find(['T', ' ']).unwrap_or(text.len());
            text.split_at(cut)
        };
        if Date::from_iso(date).is_none() {
            return false;
        }
        if rest.is_empty() {
            return true;
        }
        let rest = &rest[1..];
        if !matches!(text.as_bytes()[date.len()], b'T' | b' ') {
            return false;
        }
        let (time, zone) = match rest.find(['+', '-', 'Z']) {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let valid_zone = zone.is_empty() || zone == "Z" || {
            let z = &zone[1..];
            let parts: Vec<&str> = z.split(':').collect();
            (z.len() == 4 && z.bytes().all(|b| b.is_ascii_digit()))
                || (parts.len() <= 3 && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
                    && parts[0].parse::<u32>().is_ok_and(|h| h < 24))
        };
        let (clock, fraction) = match time.split_once(['.', ',']) {
            Some((clock, fraction)) => (clock, Some(fraction)),
            None => (time, None),
        };
        let fields: Vec<&str> = if clock.contains(':') {
            clock.split(':').collect()
        } else {
            (0..clock.len()).step_by(2).map(|i| clock.get(i..i + 2).unwrap_or("x")).collect()
        };
        let limits = [24, 60, 60];
        let valid_clock = !fields.is_empty() && fields.len() <= 3
            && fields.iter().zip(limits).all(|(f, limit)| f.len() == 2 && f.bytes().all(|b| b.is_ascii_digit())
                && f.parse::<u32>().is_ok_and(|v| v < limit))
            && fraction.is_none_or(|f| fields.len() == 3 && !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
        return valid_zone && valid_clock;
    }
    false
}

const TZPATH: [&str; 4] = ["/usr/share/zoneinfo", "/usr/lib/zoneinfo", "/usr/share/lib/zoneinfo", "/etc/zoneinfo"];

fn tz_path() -> Vec<String> {
    match std::env::var("PYTHONTZPATH") {
        Ok(value) => value.split(':').filter(|p| p.starts_with('/')).map(String::from).collect(),
        Err(_) => TZPATH.iter().map(|p| (*p).to_owned()).collect(),
    }
}

/// `ZoneInfo(key)` succeeds: a normalized relative key naming a TZif file.
pub fn zone_exists(key: &str) -> bool {
    if key.is_empty() || key.starts_with('/') || key.contains('\0') {
        return false;
    }
    let path = Path::new(key);
    if path.components().any(|c| !matches!(c, Component::Normal(_))) || key.ends_with('/') || key.contains("//") {
        return false;
    }
    tz_path().iter().any(|root| {
        let file = Path::new(root).join(key);
        file.is_file() && std::fs::File::open(&file).ok().and_then(|mut f| {
            use std::io::Read;
            let mut magic = [0u8; 4];
            f.read_exact(&mut magic).ok().map(|_| &magic == b"TZif")
        }).unwrap_or(false)
    })
}

fn zone(key: &str) -> Option<glib::TimeZone> {
    if !zone_exists(key) {
        return None;
    }
    glib::TimeZone::from_identifier(Some(key))
}

/// Local wall clock of a Unix time in an IANA zone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    pub date: Date,
    pub hour: u32,
    pub minute: u32,
}

pub fn local(timestamp: f64, key: &str) -> Option<Local> {
    if !timestamp.is_finite() {
        return None;
    }
    let tz = zone(key)?;
    let seconds = timestamp.floor();
    let utc = glib::DateTime::from_unix_utc(seconds as i64).ok()?;
    let value = utc.to_timezone(&tz).ok()?;
    let date = Date::new(value.year(), value.month() as u32, value.day_of_month() as u32)?;
    Some(Local { date, hour: value.hour() as u32, minute: value.minute() as u32 })
}

pub fn local_date(timestamp: f64, key: &str) -> Option<Date> { local(timestamp, key).map(|l| l.date) }

/// Unix time of 00:00 on `date` in zone `key`.
pub fn midnight(date: Date, key: &str) -> Option<f64> {
    let tz = zone(key)?;
    let value = glib::DateTime::new(&tz, date.year, date.month as i32, date.day as i32, 0, 0, 0.0).ok()?;
    Some(value.to_unix() as f64)
}

/// Local (machine time zone) wall clock, for `datetime.fromtimestamp(t)`.
pub fn machine_local(timestamp: f64) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_local(timestamp.floor() as i64).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip_and_weekdays() {
        let date = Date::new(2026, 9, 20).unwrap();
        assert_eq!(date.weekday(), 6);
        assert_eq!(Date::from_days(date.to_days()), date);
        assert_eq!(Date::new(2026, 12, 31).unwrap().add_days(3), Date::new(2027, 1, 3).unwrap());
        assert_eq!(Date::from_iso("2026-09-24"), Date::new(2026, 9, 24));
        assert_eq!(Date::from_iso("20260924"), Date::new(2026, 9, 24));
        assert_eq!(Date::from_iso("2026-W39-4"), Date::new(2026, 9, 24));
        assert_eq!(Date::from_iso("2026-02-30"), None);
        assert_eq!(Date::from_iso("26-09-24"), None);
        assert!(valid_iso_datetime("2026-09-24T12:00"));
        assert!(valid_iso_datetime("2026-09-24T12:00:30.5+02:00"));
        assert!(valid_iso_datetime("2026-09-24"));
        assert!(!valid_iso_datetime("2026-09-24T25:00"));
        assert!(!valid_iso_datetime("yesterday"));
    }

    #[test]
    fn zones_follow_zoneinfo_rules() {
        assert!(zone_exists("America/Denver"));
        assert!(zone_exists("UTC"));
        for bad in ["Bad/Zone", "", "/etc/localtime", "../zoneinfo/UTC", "America/../UTC", "Invalid/timezone"] {
            assert!(!zone_exists(bad), "{bad}");
        }
        let at = 1790272800.0; // 2026-09-24 12:00 MDT
        assert_eq!(local(at, "America/Denver").unwrap(), Local { date: Date::new(2026, 9, 24).unwrap(), hour: 12, minute: 0 });
        let next = midnight(Date::new(2026, 9, 25).unwrap(), "America/Denver").unwrap();
        assert_eq!(next - at, 12.0 * 3600.0);
    }
}
