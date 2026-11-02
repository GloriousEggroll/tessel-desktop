//! Time zones with CPython `zoneinfo` semantics (TZif v1–v3 + POSIX TZ footer,
//! `fold`, wall-time lookups and `fromutc`), plus `timezone.utc`, fixed
//! offsets and the process-local zone the C library would use (`TZ`, else
//! `/etc/localtime`) for `datetime.astimezone()`.

use crate::civil::{Checked, Delta, Naive};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const TZPATH: [&str; 4] = ["/usr/share/zoneinfo", "/usr/lib/zoneinfo", "/usr/share/lib/zoneinfo", "/etc/zoneinfo"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ttinfo {
    pub utcoff: i64,
    pub dstoff: i64,
    pub abbr: String,
}

#[derive(Clone, Debug)]
enum DayRule {
    Julian { d: i64, julian: bool, secs: i64 },
    Calendar { m: i64, w: i64, d: i64, secs: i64 },
}

fn post_epoch_days_before_year(year: i64) -> i64 {
    let y = year - 1;
    y * 365 + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400) - crate::civil::EPOCH_ORDINAL
}

impl DayRule {
    fn year_to_epoch(&self, year: i64) -> i64 {
        match *self {
            DayRule::Julian { d, julian, secs } => {
                let mut d = d;
                if julian && d >= 59 && crate::civil::is_leap(year) {
                    d += 1;
                }
                (post_epoch_days_before_year(year) + d) * 86400 + secs
            }
            DayRule::Calendar { m, w, d, secs } => {
                // calendar.monthrange: weekday of the 1st (Monday = 0).
                let first = crate::civil::ymd_to_ordinal(year, m, 1);
                let first_day = (first + 6) % 7;
                let days_in_month = crate::civil::days_in_month(year, m);
                let mut month_day = (d - (first_day + 1)).rem_euclid(7) + 1;
                month_day += (w - 1) * 7;
                if month_day > days_in_month {
                    month_day -= 7;
                }
                const BEFORE: [i64; 13] = [-1, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
                let ordinal = post_epoch_days_before_year(year) + BEFORE[m as usize]
                    + i64::from(m > 2 && crate::civil::is_leap(year)) + month_day;
                ordinal * 86400 + secs
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct TzStr {
    std: Ttinfo,
    dst: Ttinfo,
    dst_diff: i64,
    start: DayRule,
    end: DayRule,
}

impl TzStr {
    fn transitions(&self, year: i64) -> (i64, i64) {
        (self.start.year_to_epoch(year), self.end.year_to_epoch(year))
    }

    fn trans_info(&self, ts: i64, year: i64, fold: u8) -> &Ttinfo {
        let (mut start, mut end) = self.transitions(year);
        if (fold == 1) == (self.dst_diff >= 0) {
            end -= self.dst_diff;
        } else {
            start += self.dst_diff;
        }
        let isdst = if start < end { start <= ts && ts < end } else { !(end <= ts && ts < start) };
        if isdst { &self.dst } else { &self.std }
    }

    fn trans_info_fromutc(&self, ts: i64, year: i64) -> (&Ttinfo, u8) {
        let (mut start, mut end) = self.transitions(year);
        start -= self.std.utcoff;
        end -= self.dst.utcoff;
        let isdst = if start < end { start <= ts && ts < end } else { !(end <= ts && ts < start) };
        let (ambig_start, ambig_end) = if self.dst_diff > 0 { (end, end + self.dst_diff) } else { (start, start - self.dst_diff) };
        let fold = u8::from(ambig_start <= ts && ts < ambig_end);
        (if isdst { &self.dst } else { &self.std }, fold)
    }
}

#[derive(Clone, Debug)]
enum After {
    Fixed(Ttinfo),
    Rule(TzStr),
}

#[derive(Clone, Debug)]
pub struct TzInfo {
    trans_utc: Vec<i64>,
    trans_local: [Vec<i64>; 2],
    ttinfos: Vec<Ttinfo>,
    tti_before: Option<Ttinfo>,
    after: After,
}

#[derive(Debug)]
pub enum ZoneKind {
    /// `datetime.timezone.utc`.
    Utc,
    /// `datetime.timezone(timedelta(seconds=offset), name)`.
    Fixed { offset: i64, name: String },
    Info(TzInfo),
}

#[derive(Debug)]
pub struct Zone {
    /// ZoneInfo key ("" for UTC / fixed / from_file without a key).
    pub key: String,
    pub kind: ZoneKind,
}

pub type ZoneRef = Arc<Zone>;

fn bisect_right(values: &[i64], x: i64) -> usize {
    values.partition_point(|v| *v <= x)
}

impl Zone {
    pub fn utc() -> ZoneRef {
        static UTC: OnceLock<ZoneRef> = OnceLock::new();
        UTC.get_or_init(|| Arc::new(Zone { key: String::new(), kind: ZoneKind::Utc })).clone()
    }

    pub fn fixed(offset: i64, name: &str) -> ZoneRef {
        Arc::new(Zone { key: String::new(), kind: ZoneKind::Fixed { offset, name: name.into() } })
    }

    pub fn is_utc(&self) -> bool {
        matches!(self.kind, ZoneKind::Utc)
    }

    fn find_trans(&self, wall: Naive, fold: u8) -> Ttinfo {
        match &self.kind {
            ZoneKind::Utc => Ttinfo { utcoff: 0, dstoff: 0, abbr: "UTC".into() },
            ZoneKind::Fixed { offset, name } => Ttinfo { utcoff: *offset, dstoff: 0, abbr: name.clone() },
            ZoneKind::Info(info) => {
                let ts = wall.local_seconds();
                let local = &info.trans_local[fold.min(1) as usize];
                if !local.is_empty() && ts < local[0] {
                    return info.tti_before.clone().unwrap_or_else(|| info.ttinfos[0].clone());
                }
                if local.is_empty() || ts > *local.last().unwrap() {
                    return match &info.after {
                        After::Fixed(tti) => tti.clone(),
                        After::Rule(rule) => rule.trans_info(ts, wall.date().year(), fold).clone(),
                    };
                }
                info.ttinfos[bisect_right(local, ts) - 1].clone()
            }
        }
    }

    /// `utcoffset()` in seconds for a wall time with `fold`.
    pub fn utcoffset(&self, wall: Naive, fold: u8) -> i64 {
        self.find_trans(wall, fold).utcoff
    }

    pub fn tzname(&self, wall: Naive, fold: u8) -> String {
        self.find_trans(wall, fold).abbr
    }

    /// `tz.fromutc(utc)`: the wall time and fold of a UTC instant.
    pub fn fromutc(&self, utc: Naive) -> Checked<(Naive, u8)> {
        let (offset, fold) = match &self.kind {
            ZoneKind::Utc => (0, 0),
            ZoneKind::Fixed { offset, .. } => (*offset, 0),
            ZoneKind::Info(info) => info.fromutc(utc),
        };
        Ok((utc.add(Delta::seconds(offset as i128)?)?, fold))
    }

    /// UTC naive time of a wall time (`dt - dt.utcoffset()`).
    pub fn to_utc(&self, wall: Naive, fold: u8) -> Checked<Naive> {
        wall.add(Delta::seconds(-(self.utcoffset(wall, fold) as i128))?)
    }
}

impl TzInfo {
    /// CPython `_zoneinfo.c` `zoneinfo_fromutc`: `(utcoff, fold)`.
    fn fromutc(&self, utc: Naive) -> (i64, u8) {
        let ts = utc.local_seconds();
        let n = self.trans_utc.len();
        let before = || self.tti_before.clone().unwrap_or_else(|| self.ttinfos[0].clone());
        if n >= 1 && ts < self.trans_utc[0] {
            return (before().utcoff, 0);
        }
        if n == 0 || ts > self.trans_utc[n - 1] {
            let (tti, mut fold) = match &self.after {
                After::Fixed(tti) => (tti.clone(), 0),
                After::Rule(rule) => {
                    let (tti, fold) = rule.trans_info_fromutc(ts, utc.date().year());
                    (tti.clone(), fold)
                }
            };
            if n > 0 {
                let previous = if n == 1 { before() } else { self.ttinfos[n - 2].clone() };
                let diff = previous.utcoff - tti.utcoff;
                if diff > 0 && ts < self.trans_utc[n - 1] + diff {
                    fold = 1;
                }
            }
            return (tti.utcoff, fold);
        }
        let idx = bisect_right(&self.trans_utc, ts);
        let (previous, tti) = if idx >= 2 { (self.ttinfos[idx - 2].clone(), self.ttinfos[idx - 1].clone()) }
                              else { (before(), self.ttinfos[0].clone()) };
        let shift = previous.utcoff - tti.utcoff;
        (tti.utcoff, u8::from(shift > ts - self.trans_utc[idx - 1]))
    }
}

// ---------------------------------------------------------------- TZif files

#[derive(Debug)]
pub struct InvalidZone(pub String);

fn be_i32(data: &[u8], at: usize) -> Result<i64, InvalidZone> {
    data.get(at..at + 4).map(|b| i32::from_be_bytes(b.try_into().unwrap()) as i64)
        .ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))
}

fn be_i64(data: &[u8], at: usize) -> Result<i64, InvalidZone> {
    data.get(at..at + 8).map(|b| i64::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))
}

struct Header {
    version: u32,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

fn header(data: &[u8], at: usize) -> Result<Header, InvalidZone> {
    if data.get(at..at + 4) != Some(b"TZif") {
        return Err(InvalidZone("Invalid TZif file: magic not found".into()));
    }
    let version = match data.get(at + 4) {
        Some(0) => 1,
        Some(c) if c.is_ascii_digit() => (c - b'0') as u32,
        _ => return Err(InvalidZone("Invalid TZif file: bad version".into())),
    };
    let count = |i: usize| -> Result<usize, InvalidZone> {
        let value = be_i32(data, at + 20 + 4 * i)?;
        usize::try_from(value).map_err(|_| InvalidZone("Invalid TZif file: negative count".into()))
    };
    Ok(Header { version, isutcnt: count(0)?, isstdcnt: count(1)?, leapcnt: count(2)?, timecnt: count(3)?,
                typecnt: count(4)?, charcnt: count(5)? })
}

/// `_common.load_data` + `ZoneInfo._load_file`.
pub fn parse_tzif(data: &[u8]) -> Result<TzInfo, InvalidZone> {
    let mut h = header(data, 0)?;
    let mut at = 44;
    let mut time_size = 4;
    if h.version >= 2 {
        at += h.timecnt * 5 + h.typecnt * 6 + h.charcnt + h.leapcnt * 8 + h.isstdcnt + h.isutcnt;
        h = header(data, at)?;
        at += 44;
        time_size = 8;
    }
    let mut trans_utc = Vec::with_capacity(h.timecnt);
    for i in 0..h.timecnt {
        trans_utc.push(if time_size == 8 { be_i64(data, at + i * 8)? } else { be_i32(data, at + i * 4)? });
    }
    at += h.timecnt * time_size;
    let trans_idx: Vec<usize> = data.get(at..at + h.timecnt)
        .ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))?
        .iter().map(|b| *b as usize).collect();
    at += h.timecnt;
    if trans_idx.iter().any(|i| *i >= h.typecnt) {
        return Err(InvalidZone("Invalid transition index found while reading TZif".into()));
    }
    let mut utcoff = Vec::new();
    let mut isdst = Vec::new();
    let mut abbrind = Vec::new();
    for i in 0..h.typecnt {
        let base = at + i * 6;
        utcoff.push(be_i32(data, base)?);
        let rest = data.get(base + 4..base + 6).ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))?;
        isdst.push(rest[0] != 0);
        abbrind.push(rest[1] as i8);
    }
    at += h.typecnt * 6;
    let chars = data.get(at..at + h.charcnt).ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))?;
    at += h.charcnt;
    let abbr: Vec<String> = abbrind.iter().map(|index| {
        // Python indexes bytes with a signed char; negative → from the end.
        let index = if *index < 0 { (chars.len() as i64 + *index as i64).max(0) as usize } else { *index as usize };
        let tail = chars.get(index..).unwrap_or(&[]);
        let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len().saturating_sub(1));
        String::from_utf8_lossy(&tail[..end]).into_owned()
    }).collect();
    let mut tz_str: Option<String> = None;
    if h.version >= 2 {
        at += h.isutcnt + h.isstdcnt + h.leapcnt * 12;
        if data.get(at) != Some(&b'\n') {
            return Err(InvalidZone("Invalid TZif file: missing footer".into()));
        }
        let rest = &data[at + 1..];
        let end = rest.iter().position(|b| *b == b'\n').ok_or_else(|| InvalidZone("Invalid TZif file: unexpected end of file".into()))?;
        tz_str = Some(String::from_utf8_lossy(&rest[..end]).into_owned());
    }

    let dstoff = utcoff_to_dstoff(&trans_idx, &utcoff, &isdst);
    let trans_local = ts_to_local(&trans_idx, &trans_utc, &utcoff);
    let list: Vec<Ttinfo> = (0..h.typecnt).map(|i| Ttinfo { utcoff: utcoff[i], dstoff: dstoff[i], abbr: abbr[i].clone() }).collect();
    let ttinfos: Vec<Ttinfo> = trans_idx.iter().map(|i| list[*i].clone()).collect();
    let tti_before = isdst.iter().position(|d| !*d).map(|i| list[i].clone()).or_else(|| ttinfos.first().cloned()).or_else(|| list.first().cloned());
    let after = match tz_str.filter(|s| !s.is_empty()) {
        Some(text) => parse_tz_str(&text)?,
        None => {
            if ttinfos.is_empty() && list.is_empty() {
                return Err(InvalidZone("No time zone information found.".into()));
            }
            After::Fixed(ttinfos.last().cloned().unwrap_or_else(|| list.last().cloned().unwrap()))
        }
    };
    Ok(TzInfo { trans_utc, trans_local, ttinfos, tti_before, after })
}

fn utcoff_to_dstoff(trans_idx: &[usize], utcoffsets: &[i64], isdsts: &[bool]) -> Vec<i64> {
    let typecnt = isdsts.len();
    let mut dstoffs = vec![0i64; typecnt];
    let dst_cnt = isdsts.iter().filter(|d| **d).count();
    let mut dst_found = 0;
    let mut broke = false;
    for i in 1..trans_idx.len() {
        if dst_cnt == dst_found {
            broke = true;
            break;
        }
        let idx = trans_idx[i];
        if !isdsts[idx] || dstoffs[idx] != 0 {
            continue;
        }
        let mut dstoff = 0;
        let utcoff = utcoffsets[idx];
        let comp = trans_idx[i - 1];
        if !isdsts[comp] {
            dstoff = utcoff - utcoffsets[comp];
        }
        if dstoff == 0 && idx < typecnt - 1 && i + 1 < trans_idx.len() {
            let comp = trans_idx[i + 1];
            if isdsts[comp] {
                continue;
            }
            dstoff = utcoff - utcoffsets[comp];
        }
        if dstoff != 0 {
            dst_found += 1;
            dstoffs[idx] = dstoff;
        }
    }
    if !broke {
        for idx in 0..typecnt {
            if dstoffs[idx] == 0 && isdsts[idx] {
                dstoffs[idx] = 3600;
            }
        }
    }
    dstoffs
}

fn ts_to_local(trans_idx: &[usize], trans_utc: &[i64], utcoffsets: &[i64]) -> [Vec<i64>; 2] {
    if trans_utc.is_empty() {
        return [Vec::new(), Vec::new()];
    }
    let mut wall = [trans_utc.to_vec(), trans_utc.to_vec()];
    let (mut o0, mut o1) = if utcoffsets.len() > 1 { (utcoffsets[0], utcoffsets[trans_idx[0]]) } else { (utcoffsets[0], utcoffsets[0]) };
    if o1 > o0 {
        std::mem::swap(&mut o0, &mut o1);
    }
    wall[0][0] += o0;
    wall[1][0] += o1;
    for i in 1..trans_idx.len() {
        let (mut o0, mut o1) = (utcoffsets[trans_idx[i - 1]], utcoffsets[trans_idx[i]]);
        if o1 > o0 {
            std::mem::swap(&mut o0, &mut o1);
        }
        wall[0][i] += o0;
        wall[1][i] += o1;
    }
    wall
}

// ---------------------------------------------------------- POSIX TZ strings

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn abbr(&mut self) -> Option<String> {
        if self.s.get(self.i) == Some(&b'<') {
            let start = self.i;
            let mut j = self.i + 1;
            while j < self.s.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'+' || self.s[j] == b'-') {
                j += 1;
            }
            if j == start + 1 || self.s.get(j) != Some(&b'>') {
                return None;
            }
            self.i = j + 1;
            return Some(String::from_utf8_lossy(&self.s[start + 1..j]).into_owned());
        }
        let start = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_alphabetic() {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// `[+-]?\d{1,max}(:\d{2}(:\d{2})?)?` → (sign, h, m, s)
    fn hms(&mut self, max: usize) -> Option<(i64, i64, i64, i64)> {
        let mut sign = 1;
        let save = self.i;
        if let Some(c @ (b'+' | b'-')) = self.s.get(self.i) {
            sign = if *c == b'-' { -1 } else { 1 };
            self.i += 1;
        }
        let digits = |cur: &mut Self, min: usize, max: usize| -> Option<i64> {
            let start = cur.i;
            while cur.i < cur.s.len() && cur.s[cur.i].is_ascii_digit() && cur.i - start < max {
                cur.i += 1;
            }
            if cur.i - start < min {
                return None;
            }
            std::str::from_utf8(&cur.s[start..cur.i]).ok()?.parse().ok()
        };
        let Some(h) = digits(self, 1, max) else {
            self.i = save;
            return None;
        };
        let (mut m, mut s) = (0, 0);
        if self.s.get(self.i) == Some(&b':') {
            let back = self.i;
            self.i += 1;
            match digits(self, 2, 2) {
                Some(v) => {
                    m = v;
                    if self.s.get(self.i) == Some(&b':') {
                        let back = self.i;
                        self.i += 1;
                        match digits(self, 2, 2) {
                            Some(v) => s = v,
                            None => self.i = back,
                        }
                    }
                }
                None => self.i = back,
            }
        }
        Some((sign, h, m, s))
    }
}

fn invalid(text: &str) -> InvalidZone {
    InvalidZone(format!("{text} is not a valid TZ string"))
}

fn tz_delta(sign: i64, h: i64, m: i64, s: i64, text: &str) -> Result<i64, InvalidZone> {
    if h > 24 {
        return Err(invalid(text));
    }
    let total = h * 3600 + m * 60 + s;
    Ok(if sign < 0 { total } else { -total })
}

fn parse_rule(text: &str, whole: &str) -> Result<DayRule, InvalidZone> {
    let (date, time) = match text.split_once('/') {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };
    let secs = match time {
        None => 7200,
        Some(t) => {
            let mut cur = Cursor { s: t.as_bytes(), i: 0 };
            let (sign, h, m, s) = cur.hms(3).ok_or_else(|| invalid(whole))?;
            if cur.i != t.len() || h > 167 {
                return Err(invalid(whole));
            }
            sign * (h * 3600 + m * 60 + s)
        }
    };
    if let Some(rest) = date.strip_prefix('M') {
        let parts: Vec<&str> = rest.split('.').collect();
        let ok = parts.len() == 3 && (1..=2).contains(&parts[0].len()) && parts[1].len() == 1 && parts[2].len() == 1
            && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return Err(invalid(whole));
        }
        let (m, w, d): (i64, i64, i64) = (parts[0].parse().unwrap(), parts[1].parse().unwrap(), parts[2].parse().unwrap());
        if !(1..=12).contains(&m) || !(1..=5).contains(&w) || !(0..=6).contains(&d) {
            return Err(invalid(whole));
        }
        return Ok(DayRule::Calendar { m, w, d, secs });
    }
    let (julian, digits) = match date.strip_prefix('J') { Some(rest) => (true, rest), None => (false, date) };
    if !(1..=3).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(whole));
    }
    let d: i64 = digits.parse().unwrap();
    if !(i64::from(julian)..=365).contains(&d) {
        return Err(invalid(whole));
    }
    Ok(DayRule::Julian { d, julian, secs })
}

fn parse_tz_str(text: &str) -> Result<After, InvalidZone> {
    let (offset_part, rules) = match text.split_once(',') { Some((a, b)) => (a, Some(b)), None => (text, None) };
    let mut cur = Cursor { s: offset_part.as_bytes(), i: 0 };
    let std_abbr = cur.abbr().ok_or_else(|| invalid(text))?;
    let Some((sign, h, m, s)) = cur.hms(3) else {
        return Err(invalid(text));
    };
    let std_offset = tz_delta(sign, h, m, s, text)?;
    let mut dst: Option<(String, i64)> = None;
    if cur.i < offset_part.len() {
        let abbr = cur.abbr().ok_or_else(|| invalid(text))?;
        let offset = match cur.hms(3) {
            Some((sign, h, m, s)) => tz_delta(sign, h, m, s, text)?,
            None => std_offset + 3600,
        };
        dst = Some((abbr, offset));
    }
    if cur.i != offset_part.len() {
        return Err(invalid(text));
    }
    let std = Ttinfo { utcoff: std_offset, dstoff: 0, abbr: std_abbr };
    match (dst, rules) {
        (Some((abbr, offset)), Some(rules)) => {
            let (a, b) = rules.split_once(',').ok_or_else(|| invalid(text))?;
            let start = parse_rule(a, text)?;
            let end = parse_rule(b, text)?;
            let dst_diff = offset - std_offset;
            Ok(After::Rule(TzStr { std, dst: Ttinfo { utcoff: offset, dstoff: dst_diff, abbr }, dst_diff, start, end }))
        }
        (Some(_), None) => Err(InvalidZone(format!("Missing transition rules: {text}"))),
        (None, Some(_)) => Err(InvalidZone(format!("Transition rule present without DST: {text}"))),
        (None, None) => Ok(After::Fixed(std)),
    }
}

// --------------------------------------------------------------- key lookup

/// `posixpath.normpath` for relative paths.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") { "//" } else if path.starts_with('/') { "/" } else { "" };
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (initial.is_empty() && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(comp);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let joined = initial.to_owned() + &parts.join("/");
    if joined.is_empty() { ".".into() } else { joined }
}

/// `_tzpath._validate_tzfile_path` + `find_tzfile`.
pub fn find_tzfile(key: &str) -> Result<Option<PathBuf>, InvalidZone> {
    if key.starts_with('/') {
        return Err(InvalidZone(format!("ZoneInfo keys may not be absolute paths, got: {key}")));
    }
    let normalized = normpath(key);
    if normalized.chars().count() != key.chars().count() {
        return Err(InvalidZone(format!("ZoneInfo keys must be normalized relative paths, got: {key}")));
    }
    let resolved = normpath(&format!("_/{normalized}"));
    if !resolved.starts_with("_/") {
        return Err(InvalidZone(format!("ZoneInfo keys must refer to subdirectories of TZPATH, got: {key}")));
    }
    if key.contains('\0') {
        return Err(InvalidZone("embedded null byte".into()));
    }
    Ok(TZPATH.iter().map(|base| Path::new(base).join(key)).find(|p| p.is_file()))
}

type Cache = Mutex<HashMap<String, ZoneRef>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `ZoneInfo(key)` (cached by key like CPython, so equal keys are the same
/// tzinfo object). `Err` covers ValueError/KeyError/OSError.
pub fn zoneinfo(key: &str) -> Result<ZoneRef, InvalidZone> {
    if let Some(zone) = cache().lock().unwrap().get(key) {
        return Ok(zone.clone());
    }
    let path = find_tzfile(key)?.ok_or_else(|| InvalidZone(format!("No time zone found with key {key}")))?;
    let data = std::fs::read(&path).map_err(|e| InvalidZone(e.to_string()))?;
    let zone = Arc::new(Zone { key: key.into(), kind: ZoneKind::Info(parse_tzif(&data)?) });
    Ok(cache().lock().unwrap().entry(key.into()).or_insert(zone).clone())
}

/// `ZoneInfo.from_file(open(path, "rb"))` (no key).
pub fn zone_from_file(path: &Path) -> Result<ZoneRef, InvalidZone> {
    let data = std::fs::read(path).map_err(|e| InvalidZone(e.to_string()))?;
    Ok(Arc::new(Zone { key: String::new(), kind: ZoneKind::Info(parse_tzif(&data)?) }))
}

/// The zone glibc's `localtime()` uses for this `TZ` value (`None` = unset).
pub fn local_zone_for(tz: Option<&str>) -> ZoneRef {
    let Some(value) = tz else {
        return zone_from_file(Path::new("/etc/localtime")).unwrap_or_else(|_| Zone::utc());
    };
    let value = value.strip_prefix(':').unwrap_or(value);
    if value.is_empty() {
        return Zone::utc();
    }
    let dir = std::env::var("TZDIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| TZPATH[0].into());
    let path = if value.starts_with('/') { PathBuf::from(value) } else { Path::new(&dir).join(value) };
    if let Ok(zone) = zone_from_file(&path) {
        return zone;
    }
    let mut parsed = parse_tz_str(value);
    if parsed.is_err() && !value.contains(',') {
        // glibc applies the US rules to "EST5EDT"-style strings without rules.
        parsed = parse_tz_str(&format!("{value},M3.2.0,M11.1.0"));
    }
    match parsed {
        Ok(after) => Arc::new(Zone { key: String::new(), kind: ZoneKind::Info(TzInfo {
            trans_utc: Vec::new(), trans_local: [Vec::new(), Vec::new()], ttinfos: Vec::new(), tti_before: None, after }) }),
        Err(_) => Zone::utc(),
    }
}

/// The current process's local zone (re-read when `TZ` changes, like `tzset`).
pub fn local_zone() -> ZoneRef {
    static LOCAL: OnceLock<Mutex<Option<(Option<String>, ZoneRef)>>> = OnceLock::new();
    let tz = std::env::var("TZ").ok();
    let cell = LOCAL.get_or_init(Default::default);
    let mut guard = cell.lock().unwrap();
    if let Some((key, zone)) = guard.as_ref() {
        if *key == tz {
            return zone.clone();
        }
    }
    let zone = local_zone_for(tz.as_deref());
    *guard = Some((tz, zone.clone()));
    zone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civil::Date;

    fn wall(y: i64, m: i64, d: i64, h: i64, mi: i64) -> Naive {
        Naive::new(Date::ymd(y, m, d), h, mi, 0, 0)
    }

    #[test]
    fn new_york_gap_fold_and_footer() {
        let ny = zoneinfo("America/New_York").unwrap();
        assert!(Arc::ptr_eq(&ny, &zoneinfo("America/New_York").unwrap()));
        assert_eq!(ny.utcoffset(wall(2026, 1, 15, 12, 0), 0), -5 * 3600);
        assert_eq!(ny.utcoffset(wall(2026, 7, 15, 12, 0), 0), -4 * 3600);
        // Gap: fold=0 uses the offset before the transition.
        assert_eq!(ny.utcoffset(wall(2026, 3, 8, 2, 30), 0), -5 * 3600);
        assert_eq!(ny.utcoffset(wall(2026, 3, 8, 2, 30), 1), -4 * 3600);
        // Repeated hour.
        assert_eq!(ny.utcoffset(wall(2026, 11, 1, 1, 30), 0), -4 * 3600);
        assert_eq!(ny.utcoffset(wall(2026, 11, 1, 1, 30), 1), -5 * 3600);
        let (w, fold) = ny.fromutc(wall(2026, 11, 1, 6, 30)).unwrap();
        assert_eq!((w, fold), (wall(2026, 11, 1, 1, 30), 1));
        assert_eq!(ny.utcoffset(wall(2300, 7, 1, 0, 0), 0), -4 * 3600);
        assert_eq!(ny.tzname(wall(2026, 7, 1, 0, 0), 0), "EDT");
    }

    #[test]
    fn invalid_keys() {
        for key in ["", "/etc/localtime", "../x", "America/../Denver", "America//Denver", "Mars/Olympus", "America/"] {
            assert!(zoneinfo(key).is_err(), "{key}");
        }
        assert_eq!(normpath("a/./b"), "a/b");
    }

    #[test]
    fn tz_strings() {
        assert!(matches!(parse_tz_str("<+0530>-5:30"), Ok(After::Fixed(Ttinfo { utcoff: 19800, .. }))));
        assert!(matches!(parse_tz_str("EST5EDT,M3.2.0,M11.1.0"), Ok(After::Rule(_))));
        assert!(parse_tz_str("EST5EDT").is_err());
        assert!(local_zone_for(Some("UTC")).utcoffset(wall(2026, 1, 1, 0, 0), 0) == 0);
        assert_eq!(local_zone_for(Some("EST5EDT")).utcoffset(wall(2026, 7, 1, 0, 0), 0), -4 * 3600);
    }
}
