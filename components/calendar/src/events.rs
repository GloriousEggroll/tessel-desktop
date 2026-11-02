//! Read-only calendar events from local iCalendar files and subscribed URLs
//! (calendar_events.py). Results are identical to the Python engine,
//! including where Python raises `OverflowError` (`Err(Overflow)` here).
//!
//! VEVENT with DTSTART/DTEND/DURATION, all-day and timed events, TZID (via
//! CPython-compatible zoneinfo), RRULE (FREQ, INTERVAL, COUNT, UNTIL, BYDAY,
//! BYMONTHDAY, BYMONTH), EXDATE and RECURRENCE-ID overrides.

use crate::civil::{days_in_month, Checked, Date, Delta, Naive, Overflow, DAY_US};
use crate::py;
use crate::tz::{self, Zone, ZoneRef};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const URL_REFRESH_SECONDS: f64 = 30.0 * 60.0;
pub const MAX_PERIODS: i128 = 20000;
pub const USER_AGENT: &str = "tessel-calendar/1";

pub fn cache_directory() -> PathBuf {
    tessel_core::paths::cache_home().join("tessel/calendar")
}

/// A parsed DTSTART/DTEND/EXDATE/RECURRENCE-ID/UNTIL value.
#[derive(Clone, Debug)]
pub enum Value {
    Date(Date),
    /// Floating local time.
    Naive(Naive),
    /// Wall time, fold and zone (`timezone.utc` or a ZoneInfo).
    Aware(Naive, u8, ZoneRef),
}

impl Value {
    fn is_date(&self) -> bool {
        matches!(self, Value::Date(_))
    }

    /// Python `a < b` for two values of the same type.
    fn lt(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Date(a), Value::Date(b)) => a < b,
            (Value::Naive(a), Value::Naive(b)) => a < b,
            (Value::Aware(a, fa, za), Value::Aware(b, fb, zb)) => {
                if Arc::ptr_eq(za, zb) {
                    a < b
                } else {
                    utc_micros(*a, *fa, za) < utc_micros(*b, *fb, zb)
                }
            }
            _ => false,
        }
    }

    /// Python `a - b` for two values of the same type.
    fn sub(&self, other: &Value) -> Delta {
        match (self, other) {
            (Value::Date(a), Value::Date(b)) => a.sub_date(*b),
            (Value::Naive(a), Value::Naive(b)) => a.sub(*b),
            (Value::Aware(a, fa, za), Value::Aware(b, fb, zb)) => {
                if Arc::ptr_eq(za, zb) {
                    a.sub(*b)
                } else {
                    Delta(utc_micros(*a, *fa, za) - utc_micros(*b, *fb, zb))
                }
            }
            _ => Delta::ZERO,
        }
    }

    /// Python `value + timedelta` (aware: wall-clock arithmetic, fold reset).
    fn add(&self, delta: Delta) -> Checked<Value> {
        Ok(match self {
            Value::Date(d) => Value::Date(d.add(delta)?),
            Value::Naive(n) => Value::Naive(n.add(delta)?),
            Value::Aware(n, _, z) => Value::Aware(n.add(delta)?, 0, z.clone()),
        })
    }
}

fn utc_micros(wall: Naive, fold: u8, zone: &Zone) -> i128 {
    wall.0 as i128 - zone.utcoffset(wall, fold) as i128 * 1_000_000
}

/// A display value: a date or a naive local wall time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Local {
    Date(Date),
    Time(Naive),
}

impl Local {
    pub fn date(self) -> Date {
        match self {
            Local::Date(d) => d,
            Local::Time(t) => t.date(),
        }
    }

    pub fn time(self) -> Option<Naive> {
        match self {
            Local::Time(t) => Some(t),
            Local::Date(_) => None,
        }
    }

    pub fn iso(self) -> String {
        match self {
            Local::Date(d) => d.iso(),
            Local::Time(t) => t.iso(),
        }
    }
}

/// `datetime.astimezone()` into the C library's local zone, then naive.
fn local_of(wall: Naive, fold: u8, zone: &Zone, local: &Zone) -> Checked<Naive> {
    let utc = zone.to_utc(wall, fold)?;
    Ok(local.fromutc(utc)?.0)
}

/// `to_local`: naive local wall time for display; dates and floating times unchanged.
pub fn to_local(value: &Value, local: &Zone) -> Checked<Local> {
    Ok(match value {
        Value::Date(d) => Local::Date(*d),
        Value::Naive(n) => Local::Time(*n),
        Value::Aware(n, fold, zone) => Local::Time(local_of(*n, *fold, zone, local)?),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub freq: Freq,
    pub interval: i128,
    pub count: Option<i128>,
    pub until: Option<Value>,
    pub byday: Vec<(i128, i64)>,
    pub bymonthday: Vec<i64>,
    pub bymonth: Vec<i64>,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub start: Value,
    pub end: Value,
    pub all_day: bool,
    pub rule: Option<Rule>,
    pub exdates: Vec<Value>,
    pub recurrence_id: Option<Value>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub summary: String,
    pub location: String,
    pub source: String,
    pub start: Local,
    pub end: Local,
    pub all_day: bool,
}

impl Occurrence {
    fn new(event: &Event, start: &Value, end: &Value, local: &Zone) -> Checked<Occurrence> {
        Ok(Occurrence { summary: event.summary.clone(), location: event.location.clone(), source: event.source.clone(),
                        start: to_local(start, local)?, end: to_local(end, local)?, all_day: event.all_day })
    }

    /// First and last local date this occurrence covers (end is exclusive).
    pub fn day_span(&self) -> (Date, Date) {
        let first = self.start.date();
        let last = match (self.start, self.end) {
            (Local::Date(_), Local::Date(end)) => Date(end.0 - 1),
            (Local::Time(start), Local::Time(end)) => {
                if end > start { Naive(end.0 - 1).date() } else { first }
            }
            _ => first,
        };
        (first, last.max(first))
    }

    /// Every local date this occurrence covers.
    pub fn days(&self) -> Vec<Date> {
        let (first, last) = self.day_span();
        (first.0..=last.0).map(Date).collect()
    }

    fn overlaps(&self, first: Date, last: Date) -> bool {
        let (a, b) = self.day_span();
        a <= last && b >= first && first <= last
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n' | 'N') => {
                    chars.next();
                    out.push('\n');
                    continue;
                }
                Some(&e @ ('\\' | ';' | ',')) => {
                    chars.next();
                    out.push(e);
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

pub type Params = HashMap<String, String>;

/// Unfold RFC 5545 lines and split into `(name, params, value)`.
pub fn content_lines(text: &str) -> Vec<(String, Params, String)> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut unfolded: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.last_mut().unwrap().push_str(&line[1..]);
        } else if !line.is_empty() {
            unfolded.push(line.to_owned());
        }
    }
    let mut result = Vec::new();
    for line in unfolded {
        let mut quoted = false;
        let mut split = None;
        for (index, character) in line.char_indices() {
            if character == '"' {
                quoted = !quoted;
            } else if character == ':' && !quoted {
                split = Some(index);
                break;
            }
        }
        let Some(split) = split else { continue };
        let (head, value) = (&line[..split], &line[split + 1..]);
        let mut parts = head.split(';');
        let name = parts.next().unwrap_or("").to_uppercase();
        let mut params = Params::new();
        for part in parts {
            let (key, raw) = part.split_once('=').unwrap_or((part, ""));
            params.insert(key.to_uppercase(), raw.trim_matches('"').to_owned());
        }
        result.push((name, params, value.to_owned()));
    }
    result
}

#[derive(Debug)]
pub struct Invalid;

fn slice_int(chars: &[char], from: usize, to: usize) -> Option<i128> {
    let to = to.min(chars.len());
    let from = from.min(to);
    py::int(&chars[from..to].iter().collect::<String>())
}

fn take_digits(chars: &[char], at: usize, n: usize) -> Option<i128> {
    let run = chars.get(at..at + n)?;
    run.iter().all(|c| py::is_decimal(*c)).then(|| py::digits_value(run))
}

/// Return `(value, all_day)`; `Err` is Python's ValueError.
pub fn parse_datetime(value: &str, params: &Params) -> Result<(Value, bool), Invalid> {
    let value = py::strip(value);
    let chars: Vec<char> = value.chars().collect();
    let is_date_param = params.get("VALUE").is_some_and(|v| v.to_uppercase() == "DATE");
    if is_date_param || (chars.len() == 8 && chars.iter().all(|c| py::is_decimal(*c))) {
        let (y, m, d) = (slice_int(&chars, 0, 4).ok_or(Invalid)?, slice_int(&chars, 4, 6).ok_or(Invalid)?,
                         slice_int(&chars, 6, 8).ok_or(Invalid)?);
        let in_i64 = |v: i128| i64::try_from(v).map_err(|_| Invalid);
        return Date::new(in_i64(y)?, in_i64(m)?, in_i64(d)?).map(|d| (Value::Date(d), true)).ok_or(Invalid);
    }
    // (\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})?(Z?)
    let n = chars.len();
    if !(13..=16).contains(&n) || chars[8] != 'T' {
        return Err(Invalid);
    }
    let zulu = (n == 14 || n == 16) && chars[n - 1] == 'Z';
    let has_seconds = n >= 15;
    if (n == 14 && !zulu) || (n == 16 && !zulu) {
        return Err(Invalid);
    }
    let field = |at: usize, len: usize| take_digits(&chars, at, len).ok_or(Invalid);
    let (year, month, day) = (field(0, 4)?, field(4, 2)?, field(6, 2)?);
    let (hour, minute) = (field(9, 2)?, field(11, 2)?);
    let second = if has_seconds { field(13, 2)?.min(59) } else { 0 };
    let date = Date::new(year as i64, month as i64, day as i64).ok_or(Invalid)?;
    if hour > 23 || minute > 59 {
        return Err(Invalid);
    }
    let wall = Naive::new(date, hour as i64, minute as i64, second as i64, 0);
    if zulu {
        return Ok((Value::Aware(wall, 0, Zone::utc()), false));
    }
    if let Some(zone) = params.get("TZID").filter(|z| !z.is_empty()) {
        if let Ok(zone) = tz::zoneinfo(zone.trim_matches('/')) {
            return Ok((Value::Aware(wall, 0, zone), false));
        }
    }
    Ok((Value::Naive(wall), false))
}

#[derive(Debug)]
pub enum DurationError {
    Invalid,
    Overflow,
}

/// `parse_duration`: `Invalid` is ValueError, `Overflow` is timedelta's OverflowError.
pub fn parse_duration(value: &str) -> Result<Delta, DurationError> {
    let chars: Vec<char> = py::strip(value).chars().collect();
    let mut i = 0;
    let negative = match chars.first() {
        Some('-') => { i += 1; true }
        Some('+') => { i += 1; false }
        _ => false,
    };
    if chars.get(i) != Some(&'P') {
        return Err(DurationError::Invalid);
    }
    i += 1;
    // (\d+)<letter>, consumed only when the letter follows the digits.
    let group = |i: &mut usize, letter: char| -> i128 {
        let start = *i;
        let mut j = start;
        while j < chars.len() && py::is_decimal(chars[j]) {
            j += 1;
        }
        if j > start && chars.get(j) == Some(&letter) {
            *i = j + 1;
            py::digits_value(&chars[start..j])
        } else {
            0
        }
    };
    let weeks = group(&mut i, 'W');
    let days = group(&mut i, 'D');
    let (mut hours, mut minutes, mut seconds) = (0, 0, 0);
    if chars.get(i) == Some(&'T') {
        i += 1;
        hours = group(&mut i, 'H');
        minutes = group(&mut i, 'M');
        seconds = group(&mut i, 'S');
    }
    if i != chars.len() {
        return Err(DurationError::Invalid);
    }
    let total = weeks.checked_mul(7).and_then(|w| w.checked_add(days)).and_then(|d| d.checked_mul(86400))
        .and_then(|s| hours.checked_mul(3600).and_then(|h| s.checked_add(h)))
        .and_then(|s| minutes.checked_mul(60).and_then(|m| s.checked_add(m)))
        .and_then(|s| s.checked_add(seconds))
        .ok_or(DurationError::Overflow)?;
    let result = Delta::seconds(total).map_err(|_| DurationError::Overflow)?;
    if negative { result.neg().map_err(|_| DurationError::Overflow) } else { Ok(result) }
}

const WEEKDAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

pub fn parse_rule(value: &str) -> Option<Rule> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for part in value.split(';') {
        let (key, raw) = part.split_once('=').unwrap_or((part, ""));
        let key = key.to_uppercase();
        if let Some(entry) = parts.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = raw.to_owned();
        } else {
            parts.push((key, raw.to_owned()));
        }
    }
    let get = |key: &str| parts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let freq = match get("FREQ").unwrap_or("").to_uppercase().as_str() {
        "DAILY" => Freq::Daily,
        "WEEKLY" => Freq::Weekly,
        "MONTHLY" => Freq::Monthly,
        "YEARLY" => Freq::Yearly,
        _ => return None,
    };
    let interval = py::int(get("INTERVAL").unwrap_or("1"))?.max(1);
    let count = match get("COUNT") { Some(raw) => Some(py::int(raw)?.max(0)), None => None };
    let until = match get("UNTIL") { Some(raw) => Some(parse_datetime(raw, &Params::new()).ok()?.0), None => None };
    let mut byday = Vec::new();
    for item in get("BYDAY").unwrap_or("").to_uppercase().split(',').filter(|s| !s.is_empty()) {
        let chars: Vec<char> = item.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        let code: String = chars[chars.len() - 2..].iter().collect();
        let Some(weekday) = WEEKDAYS.iter().position(|w| *w == code) else { continue };
        let prefix = &chars[..chars.len() - 2];
        let digits = match prefix.first() { Some('+' | '-') => &prefix[1..], _ => prefix };
        if !prefix.is_empty() && (digits.is_empty() || digits.len() > 2 || !digits.iter().all(|c| py::is_decimal(*c))) {
            continue;
        }
        let ordinal = if prefix.is_empty() { 0 } else { py::int(&prefix.iter().collect::<String>())? };
        byday.push((ordinal, weekday as i64));
    }
    let mut bymonthday = Vec::new();
    for item in get("BYMONTHDAY").unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        if py::isdigit(item.trim_start_matches(['+', '-'])) {
            let number = py::int(item)?;
            if (1..=31).contains(&number.abs()) {
                bymonthday.push(number as i64);
            }
        }
    }
    let mut bymonth = Vec::new();
    for item in get("BYMONTH").unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        if py::isdigit(item) {
            let number = py::int(item)?;
            if (1..=12).contains(&number) {
                bymonth.push(number as i64);
            }
        }
    }
    Some(Rule { freq, interval, count, until, byday, bymonthday, bymonth })
}

#[derive(Default)]
struct Pending {
    exdates: Vec<Value>,
    dtstart: Option<(Value, bool)>,
    dtend: Option<(Value, bool)>,
    recurrence: Option<(Value, bool)>,
    values: HashMap<String, String>,
}

/// Parse VEVENT components; malformed events are skipped, not fatal.
/// `Err(Overflow)` where Python's `parse_calendar` raises OverflowError.
pub fn parse_calendar(text: &str, source: &str) -> Checked<Vec<Event>> {
    let mut events = Vec::new();
    let mut current: Option<Pending> = None;
    let mut depth = 0usize;
    for (name, params, value) in content_lines(text) {
        if name == "BEGIN" {
            if value.to_uppercase() == "VEVENT" && current.is_none() {
                current = Some(Pending::default());
                depth = 0;
            } else if current.is_some() {
                depth += 1;
            }
            continue;
        }
        if name == "END" {
            if current.is_some() && depth > 0 {
                depth -= 1;
            } else if current.is_some() && value.to_uppercase() == "VEVENT" {
                if let Some(event) = build_event(current.take().unwrap(), source)? {
                    events.push(event);
                }
            }
            continue;
        }
        let Some(pending) = current.as_mut() else { continue };
        if depth > 0 {
            continue;
        }
        match name.as_str() {
            "EXDATE" => {
                for item in value.split(',') {
                    if let Ok((parsed, _)) = parse_datetime(item, &params) {
                        pending.exdates.push(parsed);
                    }
                }
            }
            "DTSTART" | "DTEND" | "RECURRENCE-ID" => {
                if let Ok(parsed) = parse_datetime(&value, &params) {
                    match name.as_str() {
                        "DTSTART" => pending.dtstart = Some(parsed),
                        "DTEND" => pending.dtend = Some(parsed),
                        _ => pending.recurrence = Some(parsed),
                    }
                }
            }
            _ => {
                pending.values.insert(name, value);
            }
        }
    }
    Ok(events)
}

fn build_event(pending: Pending, source: &str) -> Checked<Option<Event>> {
    let values = &pending.values;
    let Some((start, all_day)) = pending.dtstart else { return Ok(None) };
    if values.get("STATUS").is_some_and(|s| s.to_uppercase() == "CANCELLED") {
        return Ok(None);
    }
    let mut end = pending.dtend.map(|(v, _)| v);
    if end.is_none() {
        if let Some(raw) = values.get("DURATION") {
            end = match parse_duration(raw) {
                Ok(delta) => Some(start.add(delta)?),
                Err(DurationError::Invalid) => None,
                Err(DurationError::Overflow) => return Err(Overflow),
            };
        }
    }
    // Mixed aware/naive: end takes start's tzinfo (fold kept).
    if let Some(e) = &end {
        match (e, &start) {
            (Value::Aware(wall, _, _), Value::Naive(_)) => end = Some(Value::Naive(*wall)),
            (Value::Naive(wall), Value::Aware(_, _, zone)) => end = Some(Value::Aware(*wall, 0, zone.clone())),
            _ => {}
        }
    }
    let one_day = Delta::days(1)?;
    let mut end = match end {
        Some(e) if e.is_date() == start.is_date() && !e.lt(&start) => e,
        _ => if all_day { start.add(one_day)? } else { start.clone() },
    };
    if all_day {
        if let (Value::Date(a), Value::Date(b)) = (&end, &start) {
            if a == b {
                end = start.add(one_day)?;
            }
        }
    }
    let rule = values.get("RRULE").and_then(|r| parse_rule(r));
    let recurrence_id = pending.recurrence.map(|(v, _)| v);
    let text = |key: &str| py::strip(&unescape(values.get(key).map(String::as_str).unwrap_or(""))).to_owned();
    Ok(Some(Event { uid: values.get("UID").cloned().unwrap_or_default(), summary: text("SUMMARY"), location: text("LOCATION"),
                    start, end, all_day, rule, exdates: pending.exdates, recurrence_id, source: source.into() }))
}

fn months_between(first: Date, second: Date) -> i128 {
    let (a, b) = (first.parts(), second.parts());
    ((b.0 - a.0) * 12 + b.1 - a.1) as i128
}

fn weekday_set(rule: &Rule) -> Vec<i64> {
    let mut set: Vec<i64> = rule.byday.iter().map(|(_, w)| *w).collect();
    set.sort();
    set.dedup();
    set
}

fn month_days(year: i64, month: i64, rule: &Rule, default_day: i64) -> Vec<Date> {
    let length = days_in_month(year, month);
    let weekday = |day: i64| Date::ymd(year, month, day).weekday();
    if !rule.bymonthday.is_empty() {
        let mut days: Vec<i64> = rule.bymonthday.iter().map(|d| if *d > 0 { *d } else { length + d + 1 })
            .filter(|d| (1..=length).contains(d)).collect();
        if !rule.byday.is_empty() {
            let allowed = weekday_set(rule);
            days.retain(|d| allowed.contains(&weekday(*d)));
        }
        days.sort();
        days.dedup();
        return days.into_iter().map(|d| Date::ymd(year, month, d)).collect();
    }
    if !rule.byday.is_empty() {
        let mut days: Vec<i64> = Vec::new();
        for (ordinal, wd) in &rule.byday {
            let matches: Vec<i64> = (1..=length).filter(|d| weekday(*d) == *wd).collect();
            if *ordinal == 0 {
                days.extend(&matches);
            } else if (1..=matches.len() as i128).contains(&ordinal.abs()) {
                let index = if *ordinal > 0 { *ordinal - 1 } else { matches.len() as i128 + ordinal };
                days.push(matches[index as usize]);
            }
        }
        days.sort();
        days.dedup();
        return days.into_iter().map(|d| Date::ymd(year, month, d)).collect();
    }
    if default_day <= length { vec![Date::ymd(year, month, default_day)] } else { Vec::new() }
}

/// Candidate dates for one period `index` of the rule.
fn period_days(rule: &Rule, start_day: Date, index: i128) -> Checked<Vec<Date>> {
    let step = index.saturating_mul(rule.interval);
    let (year, month, day) = start_day.parts();
    Ok(match rule.freq {
        Freq::Daily => {
            let day = start_day.add(Delta::days(step)?)?;
            if !rule.bymonth.is_empty() && !rule.bymonth.contains(&day.month()) {
                return Ok(Vec::new());
            }
            if !rule.byday.is_empty() && !weekday_set(rule).contains(&day.weekday()) {
                return Ok(Vec::new());
            }
            vec![day]
        }
        Freq::Weekly => {
            let monday = start_day.add(Delta::days(-(start_day.weekday() as i128))?)?
                .add(Delta::days(step.checked_mul(7).ok_or(Overflow)?)?)?;
            let mut weekdays = weekday_set(rule);
            if weekdays.is_empty() {
                weekdays.push(start_day.weekday());
            }
            let mut days = Vec::new();
            for weekday in weekdays {
                days.push(monday.add(Delta::days(weekday as i128)?)?);
            }
            days.retain(|d| rule.bymonth.is_empty() || rule.bymonth.contains(&d.month()));
            days
        }
        Freq::Monthly => {
            let months = (year as i128 * 12 + month as i128 - 1).saturating_add(step);
            let (y, m) = (months.div_euclid(12), months.rem_euclid(12));
            if y > 9999 || (!rule.bymonth.is_empty() && !rule.bymonth.contains(&(m as i64 + 1))) {
                return Ok(Vec::new());
            }
            month_days(y as i64, m as i64 + 1, rule, day)
        }
        Freq::Yearly => {
            let y = (year as i128).saturating_add(step);
            if y > 9999 {
                return Ok(Vec::new());
            }
            let y = y as i64;
            let mut days = Vec::new();
            let months = if rule.bymonth.is_empty() { vec![month] } else { rule.bymonth.clone() };
            for each in months {
                if rule.bymonth.is_empty() && !rule.byday.is_empty() && rule.bymonthday.is_empty() {
                    for m in 1..=12 {
                        days.extend(month_days(y, m, rule, day));
                    }
                    break;
                }
                days.extend(month_days(y, each, rule, day));
            }
            days.sort();
            days
        }
    })
}

fn period_beyond(rule: &Rule, start_day: Date, index: i128, last: Date) -> Checked<bool> {
    let step = index.saturating_mul(rule.interval);
    let (year, month, _) = start_day.parts();
    Ok(match rule.freq {
        Freq::Daily => start_day.add(Delta::days(step)?)? > last,
        Freq::Weekly => {
            let left = start_day.add(Delta::days(step.checked_mul(7).ok_or(Overflow)?)?)?;
            left > last.add_days(7)?
        }
        Freq::Monthly => {
            let months = (year as i128 * 12 + month as i128 - 1).saturating_add(step);
            let (last_year, last_month) = (last.year() as i128, last.month() as i128);
            months.div_euclid(12) > last_year || (months.div_euclid(12) == last_year && months.rem_euclid(12) + 1 > last_month)
        }
        Freq::Yearly => (year as i128).saturating_add(step) > last.year() as i128,
    })
}

/// `datetime.combine(day, start.time(), tzinfo=start.tzinfo)` normalized through UTC.
fn begin_for(event: &Event, day: Date) -> Checked<Value> {
    Ok(match &event.start {
        Value::Date(_) => Value::Date(day),
        Value::Naive(start) => Value::Naive(Naive::combine(day, start.time_micros())),
        Value::Aware(start, _, zone) => {
            let wall = Naive::combine(day, start.time_micros());
            if zone.is_utc() {
                // astimezone() to its own tzinfo is a no-op.
                Value::Aware(wall, 0, zone.clone())
            } else {
                let utc = zone.to_utc(wall, 0)?;
                let (wall, fold) = zone.fromutc(utc)?;
                Value::Aware(wall, fold, zone.clone())
            }
        }
    })
}

/// Occurrences overlapping local dates `first..=last` (inclusive).
pub fn occurrences(event: &Event, first: Date, last: Date, overrides: &[Value], local: &Zone) -> Checked<Vec<Occurrence>> {
    let duration = event.end.sub(&event.start);
    let start_day = match &event.start {
        Value::Date(d) => *d,
        Value::Naive(n) | Value::Aware(n, _, _) => n.date(),
    };
    let span = if event.all_day { duration.whole_days() } else { Delta(duration.0 - 1).whole_days() }.max(0);
    let make = |day: Date| -> Checked<Occurrence> {
        let begin = begin_for(event, day)?;
        let end = begin.add(duration)?;
        Occurrence::new(event, &begin, &end, local)
    };
    let Some(rule) = &event.rule else {
        let occurrence = make(start_day)?;
        return Ok(if occurrence.overlaps(first, last) { vec![occurrence] } else { Vec::new() });
    };
    let mut until = match &rule.until { Some(value) => Some(to_local(value, local)?), None => None };
    match until {
        Some(Local::Time(t)) if event.all_day => until = Some(Local::Date(t.date())),
        Some(Local::Date(d)) if !event.all_day => until = Some(Local::Time(Naive::combine(d, DAY_US - 1))),
        _ => {}
    }
    let mut excluded: Vec<Local> = Vec::with_capacity(event.exdates.len() + overrides.len());
    for value in event.exdates.iter().chain(overrides) {
        excluded.push(to_local(value, local)?);
    }
    let mut index: i128 = 0;
    if rule.count.is_none() {
        // Without COUNT, earlier periods cannot affect the result; skip them.
        let window_start = first.add(Delta::days(span + 2)?.neg()?)?;
        if window_start > start_day {
            match rule.freq {
                Freq::Daily | Freq::Weekly => {
                    let length = rule.interval.saturating_mul(if rule.freq == Freq::Daily { 1 } else { 7 });
                    index = (window_start.sub_date(start_day).whole_days().div_euclid(length) - 1).max(0);
                }
                _ => {
                    let mut months = months_between(start_day, window_start);
                    if rule.freq == Freq::Yearly {
                        months = months.div_euclid(12);
                    }
                    index = (months.div_euclid(rule.interval) - 1).max(0);
                }
            }
        }
    }
    let mut produced: i128 = 0;
    let mut found = Vec::new();
    for index in index..index + MAX_PERIODS {
        let days = period_days(rule, start_day, index)?;
        if days.is_empty() && period_beyond(rule, start_day, index, last)? {
            break;
        }
        let mut stop = false;
        for day in &days {
            if *day < start_day {
                continue;
            }
            if *day > last.add_days(1)? {
                stop = true;
                break;
            }
            let occurrence = make(*day)?;
            if let Some(until) = until {
                if occurrence.start > until {
                    stop = true;
                    break;
                }
            }
            produced += 1;
            if rule.count.is_some_and(|count| produced > count) {
                stop = true;
                break;
            }
            let key = occurrence.start;
            if excluded.contains(&key) || (!event.all_day && excluded.contains(&Local::Date(key.date()))) {
                continue;
            }
            if occurrence.overlaps(first, last) {
                found.push(occurrence);
            }
        }
        if stop || (!days.is_empty() && days[0] > last.add_days(1)?) {
            break;
        }
    }
    Ok(found)
}

pub type Expansion = BTreeMap<Date, Vec<Occurrence>>;

/// Map each date in the range to its sorted occurrences.
pub fn expand(events: &[Event], first: Date, last: Date, local: &Zone) -> Checked<Expansion> {
    let mut overrides: HashMap<&str, Vec<Value>> = HashMap::new();
    for event in events {
        if let (Some(id), false) = (&event.recurrence_id, event.uid.is_empty()) {
            overrides.entry(event.uid.as_str()).or_default().push(id.clone());
        }
    }
    let mut result = Expansion::new();
    let none: Vec<Value> = Vec::new();
    for event in events {
        let extra = if event.rule.is_some() { overrides.get(event.uid.as_str()).unwrap_or(&none) } else { &none };
        for occurrence in occurrences(event, first, last, extra, local)? {
            let (a, b) = occurrence.day_span();
            let (from, to) = (a.max(first), b.min(last));
            for ordinal in from.0..=to.0 {
                result.entry(Date(ordinal)).or_default().push(occurrence.clone());
            }
        }
    }
    for items in result.values_mut() {
        items.sort_by(|a, b| {
            let key = |o: &Occurrence| (!o.all_day, if o.all_day { None } else { o.start.time() }, o.summary.to_lowercase());
            key(a).cmp(&key(b))
        });
    }
    Ok(result)
}

// ------------------------------------------------------------------ loading

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub kind: String,
    pub location: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    NotFound,
    PermissionDenied,
    Http(u16),
    /// URLError / TimeoutError / other OSError.
    Connect(String),
    /// ValueError with its message.
    Value(String),
    /// Anything else (e.g. OverflowError while parsing).
    Other,
}

impl LoadError {
    pub fn from_io(error: &std::io::Error) -> LoadError {
        match error.kind() {
            std::io::ErrorKind::NotFound => LoadError::NotFound,
            std::io::ErrorKind::PermissionDenied => LoadError::PermissionDenied,
            _ => LoadError::Connect(error.to_string()),
        }
    }
}

/// Short English per-source message (translate at display time).
pub fn friendly_error(error: &LoadError) -> String {
    match error {
        LoadError::NotFound => "File not found".into(),
        LoadError::PermissionDenied => "Permission denied".into(),
        LoadError::Http(code) => format!("Server error {code}"),
        LoadError::Connect(_) => "Could not connect".into(),
        LoadError::Value(message) if !message.is_empty() => message.clone(),
        _ => "Could not read the calendar".into(),
    }
}

pub fn url_cache_path(url: &str) -> PathBuf {
    let digest = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, url.as_bytes()).map(|d| d.to_string()).unwrap_or_default();
    cache_directory().join(format!("{}.ics", &digest[..32.min(digest.len())]))
}

fn decode(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

/// The HTTP layer `fetch_url` uses (urllib's `urlopen` in Python).
pub trait Opener: Send + Sync {
    /// GET `url` with `User-Agent`, returning at most `limit` bytes of the body.
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, LoadError>;
}

pub struct HttpOpener;

impl Opener for HttpOpener {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, LoadError> {
        crate::http::get(url, limit, 20)
    }
}

/// Return calendar text for `url`, using a fresh cache or falling back to it.
pub fn fetch_url(url: &str, opener: &dyn Opener, now: Option<f64>) -> Result<String, LoadError> {
    let path = url_cache_path(url);
    let now = now.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0));
    if let Ok(info) = std::fs::metadata(&path) {
        let mtime = info.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64())
            .unwrap_or_else(|| {
                use std::os::unix::fs::MetadataExt;
                info.mtime() as f64 + info.mtime_nsec() as f64 / 1e9
            });
        if now - mtime < URL_REFRESH_SECONDS {
            if let Ok(data) = std::fs::read(&path) {
                return Ok(decode(&data));
            }
        }
    }
    let attempt = (|| -> Result<String, LoadError> {
        let data = opener.get(url, MAX_BYTES + 1)?;
        if data.len() > MAX_BYTES {
            return Err(LoadError::Value("The calendar is too large.".into()));
        }
        if !data[..data.len().min(4096)].to_ascii_uppercase().windows(15).any(|w| w == b"BEGIN:VCALENDAR") {
            return Err(LoadError::Value("The address did not return a calendar.".into()));
        }
        tessel_core::clock::atomic_write(&path, &data, ".fetch-").map_err(|e| LoadError::from_io(&e))?;
        Ok(decode(&data))
    })();
    match attempt {
        Ok(text) => Ok(text),
        Err(error) => std::fs::read(&path).map(|d| decode(&d)).map_err(|_| error),
    }
}

pub fn load_source(source: &Source, opener: &dyn Opener) -> Result<Vec<Event>, LoadError> {
    let text = if source.kind == "url" {
        fetch_url(&source.location, opener, None)?
    } else {
        let path = std::path::Path::new(&source.location);
        let info = std::fs::metadata(path).map_err(|e| LoadError::from_io(&e))?;
        if info.len() > MAX_BYTES as u64 {
            return Err(LoadError::Value("The calendar is too large.".into()));
        }
        decode(&std::fs::read(path).map_err(|e| LoadError::from_io(&e))?)
    };
    let label = if source.name.is_empty() { &source.location } else { &source.name };
    parse_calendar(&text, label).map_err(|_| LoadError::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_and_lines() {
        assert_eq!(unescape(r"a\,b\;c\\d\ne\Nf\x"), "a,b;c\\d\ne\nf\\x");
        let lines = content_lines("DTSTART;TZID=\"A:B\";X=1:2026\r\n folded\nNOCOLON\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "DTSTART");
        assert_eq!(lines[0].1["TZID"], "A:B");
        assert_eq!(lines[0].2, "2026folded");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M").unwrap(), Delta::seconds(5400).unwrap());
        assert_eq!(parse_duration("-P1W").unwrap(), Delta::seconds(-604800).unwrap());
        assert_eq!(parse_duration("P").unwrap(), Delta::ZERO);
        assert!(matches!(parse_duration("PT"), Ok(_)));
        assert!(matches!(parse_duration("P1H"), Err(DurationError::Invalid)));
        assert!(matches!(parse_duration("P9999999999D"), Err(DurationError::Overflow)));
    }
}
