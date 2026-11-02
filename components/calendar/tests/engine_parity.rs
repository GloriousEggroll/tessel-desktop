//! Exhaustive check of the iCalendar parser and recurrence engine against
//! recorded fixtures (curated cases with full expected output, seeded fuzz
//! cases with a SHA-256 of it): each is re-computed here and compared byte for byte.

use tessel_calendar::civil::Date;
use tessel_calendar::events::{expand, parse_calendar, Event, Freq, Value};
use tessel_calendar::tz::{self, ZoneRef};
use serde_json::{json, Value as Json};
use std::sync::Arc;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

fn dumps(value: &Json) -> String {
    json_dumps(value, &JsonStyle::COMPACT)
}

fn zone_key(zone: &ZoneRef) -> String {
    if zone.is_utc() || zone.key.is_empty() { "UTC".into() } else { zone.key.clone() }
}

fn value_repr(value: &Value) -> String {
    match value {
        Value::Date(d) => format!("D:{}", d.iso()),
        Value::Naive(n) => format!("N:{}", n.iso()),
        Value::Aware(n, fold, zone) => format!("A:{}:{fold}:{}", n.iso(), zone_key(zone)),
    }
}

fn number(value: i128) -> Json {
    i64::try_from(value).map(Json::from).unwrap_or_else(|_| Json::from(value.to_string()))
}

/// Python `==` between two parsed values (set de-duplication of EXDATEs).
fn python_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Date(x), Value::Date(y)) => x == y,
        (Value::Naive(x), Value::Naive(y)) => x == y,
        (Value::Aware(x, _, zx), Value::Aware(y, _, zy)) => {
            if Arc::ptr_eq(zx, zy) {
                return x == y;
            }
            let problematic = |w: &tessel_calendar::civil::Naive, z: &ZoneRef| z.utcoffset(*w, 0) != z.utcoffset(*w, 1);
            if problematic(x, zx) || problematic(y, zy) {
                return false;
            }
            x.0 as i128 - zx.utcoffset(*x, 0) as i128 * 1_000_000 == y.0 as i128 - zy.utcoffset(*y, 0) as i128 * 1_000_000
        }
        _ => false,
    }
}

fn parse_dump(text: &str) -> Json {
    let events: Vec<Event> = match parse_calendar(text, "src") {
        Ok(events) => events,
        Err(_) => return json!({"error": "OverflowError"}),
    };
    let dump: Vec<Json> = events.iter().map(|e| {
        let rule = e.rule.as_ref().map(|r| {
            let freq = match r.freq { Freq::Daily => "DAILY", Freq::Weekly => "WEEKLY", Freq::Monthly => "MONTHLY", Freq::Yearly => "YEARLY" };
            json!([freq, number(r.interval), r.count.map(number), r.until.as_ref().map(value_repr),
                   r.byday.iter().map(|(o, w)| json!([number(*o), w])).collect::<Vec<_>>(), r.bymonthday, r.bymonth])
        });
        let mut unique: Vec<&Value> = Vec::new();
        for value in &e.exdates {
            if !unique.iter().any(|u| python_eq(u, value)) {
                unique.push(value);
            }
        }
        let mut exdates: Vec<String> = unique.into_iter().map(value_repr).collect();
        exdates.sort();
        json!([e.uid, e.summary, e.location, value_repr(&e.start), value_repr(&e.end), e.all_day, rule, exdates,
               e.recurrence_id.as_ref().map(value_repr), e.source])
    }).collect();
    json!({"events": dump})
}

fn expansion(text: &str, first: Date, last: Date, local: &ZoneRef) -> Json {
    let Ok(events) = parse_calendar(text, "src") else { return json!({"parse_error": "OverflowError"}) };
    let Ok(result) = expand(&events, first, last, local) else { return json!({"error": "OverflowError"}) };
    let days: Vec<Json> = result.iter().map(|(day, items)| {
        json!([day.iso(), items.iter().map(|o| json!([o.summary, o.start.iso(), o.end.iso(), o.all_day, o.location, o.source]))
                              .collect::<Vec<_>>()])
    }).collect();
    json!({"days": days})
}

fn parse_date(text: &str) -> Date {
    let parts: Vec<i64> = text.split('-').map(|p| p.parse().unwrap()).collect();
    Date::ymd(parts[0], parts[1], parts[2])
}

fn matches(expected: &str, actual: &str) -> bool {
    match expected.strip_prefix("sha256:") {
        Some(digest) => glib::compute_checksum_for_data(glib::ChecksumType::Sha256, actual.as_bytes()).unwrap().as_str() == digest,
        None => expected == actual,
    }
}

#[test]
fn engine_matches_recorded_fixtures() {
    // CALENDAR_FIXTURES=<file> checks an extra generated set (e.g. a larger fuzz run).
    let default = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/expansion.jsonl");
    let path = std::env::var("CALENDAR_FIXTURES").unwrap_or_else(|_| default.into());
    let text = std::fs::read_to_string(path).expect("tests/fixtures/expansion.jsonl");
    let mut zones: std::collections::HashMap<String, ZoneRef> = Default::default();
    let (mut cases, mut runs, mut failures) = (0, 0, Vec::new());
    for line in text.lines() {
        let case: Json = serde_json::from_str(line).unwrap();
        let id = case["id"].as_str().unwrap();
        let ics = case["ics"].as_str().unwrap();
        cases += 1;
        let parsed = dumps(&parse_dump(ics));
        if !matches(case["parse"].as_str().unwrap(), &parsed) {
            failures.push(format!("{id}: parse\n  rust:   {parsed}\n  fixture: {}", case["parse"]));
        }
        for run in case["runs"].as_array().unwrap() {
            runs += 1;
            let tz_name = run[0].as_str().unwrap();
            let local = zones.entry(tz_name.into()).or_insert_with(|| tz::local_zone_for(Some(tz_name))).clone();
            let (first, last) = (parse_date(run[1].as_str().unwrap()), parse_date(run[2].as_str().unwrap()));
            let actual = dumps(&expansion(ics, first, last, &local));
            let expected = run[3].as_str().unwrap();
            if !matches(expected, &actual) {
                failures.push(format!("{id} TZ={tz_name} {first:?}..{last:?}\n  rust:   {}\n  fixture: {}",
                                      &actual[..actual.len().min(600)], &expected[..expected.len().min(600)]));
            }
        }
    }
    assert!(cases > 3000 && runs > 9000, "fixture file is incomplete: {cases} cases, {runs} runs");
    if !failures.is_empty() {
        for failure in failures.iter().take(15) {
            eprintln!("{failure}\n");
        }
        panic!("{} of {runs} expansions/parses differ from the fixtures ({cases} cases)", failures.len());
    }
    eprintln!("{cases} cases, {runs} expansions identical to Python");
}
