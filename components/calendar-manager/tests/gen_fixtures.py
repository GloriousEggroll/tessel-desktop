#!/usr/bin/env python3
"""Generate iCalendar parse/expansion fixtures from the Python engine.

    python3 -I tests/gen_fixtures.py [--python-dir ~/btmanager/calendar-manager] > tests/fixtures/expansion.jsonl

Each line: {"id", "ics", "parse": <canonical event dump or error>, "runs": [[tz, first, last, expected]]}
where `expected` is the full canonical expansion for curated cases and
"sha256:<hex>" of it for fuzz cases. Canonical text = json.dumps(value,
ensure_ascii=True, separators=(",", ":")). The Rust test (engine_parity.rs)
recomputes the same text and compares exactly. Read-only: imports the Python
engine, never writes outside stdout.
"""
import argparse
from datetime import date, datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import random
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("--python-dir", default=str(Path.home() / "btmanager/calendar-manager"))
parser.add_argument("--fuzz", type=int, default=3000)
parser.add_argument("--seed", type=int, default=20261029)
args = parser.parse_args()
sys.path.insert(0, args.python_dir)
sys.dont_write_bytecode = True
import calendar_events as engine  # noqa: E402


def dumps(value):
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def value_repr(value):
    if value is None:
        return None
    if isinstance(value, datetime):
        if value.tzinfo is None:
            return "N:" + value.isoformat()
        key = getattr(value.tzinfo, "key", None) or "UTC"
        return f"A:{value.replace(tzinfo=None).isoformat()}:{value.fold}:{key}"
    return "D:" + value.isoformat()


def rule_repr(rule):
    if rule is None:
        return None
    return [rule["freq"], rule["interval"], rule["count"], value_repr(rule["until"]),
            [list(item) for item in rule["byday"]], rule["bymonthday"], rule["bymonth"]]


def parse_dump(text):
    try:
        events = engine.parse_calendar(text, "src")
    except Exception as error:  # noqa: BLE001
        return {"error": type(error).__name__}, None
    dump = [[e.uid, e.summary, e.location, value_repr(e.start), value_repr(e.end), e.all_day, rule_repr(e.rule),
             sorted(value_repr(v) for v in e.exdates), value_repr(e.recurrence_id), e.source] for e in events]
    return {"events": dump}, events


def local_repr(value):
    return value.isoformat()


def expansion(text, first, last):
    try:
        events = engine.parse_calendar(text, "src")
    except Exception as error:  # noqa: BLE001
        return {"parse_error": type(error).__name__}
    try:
        result = engine.expand(events, first, last)
    except Exception as error:  # noqa: BLE001
        return {"error": type(error).__name__}
    return {"days": [[day.isoformat(), [[o.summary, local_repr(o.start), local_repr(o.end), o.all_day, o.location, o.source]
                                         for o in result[day]]] for day in sorted(result)]}


def wrap(*bodies):
    return "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n" + "".join(
        "BEGIN:VEVENT\r\n" + body.strip("\n").replace("\n", "\r\n") + "\r\nEND:VEVENT\r\n" for body in bodies) + "END:VCALENDAR\r\n"


LOCAL_ZONES = ["UTC", "America/Denver", "Europe/Berlin", "Australia/Sydney", "Asia/Kolkata", "America/St_Johns",
               "Pacific/Kiritimati", "Europe/Dublin"]
SOURCE_ZONES = ["America/New_York", "Europe/London", "Australia/Lord_Howe", "Asia/Kolkata", "Pacific/Chatham",
                "America/Sao_Paulo", "Europe/Dublin", "Africa/Casablanca", "America/St_Johns", "Antarctica/Troll",
                "UTC", "Etc/GMT+5", "Mars/Base", "/America/New_York/", "America//New_York", "zone.tab", "", "EST5EDT"]

cases = []  # (id, ics, [(tz, first, last)], full)


def add(case_id, ics, runs, full=True):
    cases.append((case_id, ics, runs, full))


def window(y1, m1, d1, y2, m2, d2):
    return date(y1, m1, d1), date(y2, m2, d2)


# ---------------------------------------------------------------- curated
year_2026 = window(2026, 1, 1, 2026, 12, 31)
add("python-tests-all-day-timed", wrap("""
UID:1
SUMMARY:Team\\, offsite
 day
LOCATION:Room 4\\nNorth
DTSTART;VALUE=DATE:20261026
DTEND;VALUE=DATE:20261028
BEGIN:VALARM
TRIGGER:-PT15M
SUMMARY:alarm text must not replace the event summary
END:VALARM
""", "UID:2\nSUMMARY:Lunch\nDTSTART:20261027T120000\nDURATION:PT1H30M",
    "UID:3\nSUMMARY:Cancelled\nSTATUS:CANCELLED\nDTSTART:20261027T090000"),
    [(tz, date(2026, 10, 25), date(2026, 10, 31)) for tz in LOCAL_ZONES])
add("python-tests-malformed", wrap("SUMMARY:No start", "DTSTART:notadate\nSUMMARY:Bad",
                                   "DTSTART;VALUE=DATE:20260101\nSUMMARY:Good\nRRULE:FREQ=SECONDLY"),
    [("UTC", *year_2026)])
add("python-tests-utc-tzid", wrap("SUMMARY:UTC\nDTSTART:20260115T150000Z\nDTEND:20260115T160000Z",
                                  "SUMMARY:Tokyo\nDTSTART;TZID=Asia/Tokyo:20260116T090000"),
    [(tz, date(2026, 1, 14), date(2026, 1, 17)) for tz in LOCAL_ZONES])
for rule, start, first, last, extra in [
        ("FREQ=DAILY;INTERVAL=2;COUNT=3", "DTSTART;VALUE=DATE:20260105", date(2026, 1, 1), date(2026, 12, 31), ""),
        ("FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20260114", "DTSTART;VALUE=DATE:20260105", date(2026, 1, 1), date(2026, 12, 31),
         "EXDATE;VALUE=DATE:20260107"),
        ("FREQ=WEEKLY;COUNT=4", "DTSTART;VALUE=DATE:20260105", date(2026, 1, 20), date(2026, 3, 1), ""),
        ("FREQ=MONTHLY;BYDAY=-1FR;COUNT=3", "DTSTART;VALUE=DATE:20260105", date(2026, 1, 1), date(2026, 12, 31), ""),
        ("FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=2", "DTSTART;VALUE=DATE:20260131", date(2026, 1, 1), date(2026, 12, 31), ""),
        ("FREQ=MONTHLY;COUNT=3", "DTSTART;VALUE=DATE:20260131", date(2026, 1, 1), date(2026, 12, 31), ""),
        ("FREQ=YEARLY", "DTSTART;VALUE=DATE:19800310", date(2026, 1, 1), date(2026, 12, 31), ""),
        ("FREQ=YEARLY", "DTSTART;VALUE=DATE:20240229", date(2025, 1, 1), date(2028, 12, 31), ""),
        ("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH", "DTSTART;VALUE=DATE:20201126", date(2026, 1, 1), date(2026, 12, 31), "")]:
    add(f"python-tests-{rule}-{start[-8:]}", wrap(f"UID:r\nSUMMARY:R\n{start}\nRRULE:{rule}\n{extra}"),
        [("UTC", first, last), ("America/Denver", first, last)])
add("python-tests-dst-standup", wrap("UID:s\nSUMMARY:Standup\nDTSTART;TZID=America/New_York:20260302T090000\n"
                                     "DTEND;TZID=America/New_York:20260302T091500\nRRULE:FREQ=WEEKLY;COUNT=3"),
    [(tz, date(2026, 3, 1), date(2026, 3, 31)) for tz in LOCAL_ZONES])
add("python-tests-recurrence-id", wrap("UID:o\nSUMMARY:Weekly\nDTSTART:20260105T100000\nRRULE:FREQ=WEEKLY;COUNT=3",
                                       "UID:o\nRECURRENCE-ID:20260112T100000\nSUMMARY:Moved\nDTSTART:20260113T150000"),
    [("UTC", date(2026, 1, 1), date(2026, 1, 31))])
add("gui-test-planning", "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:1\nSUMMARY:Planning\nLOCATION:Room 2\n"
    "DTSTART:20260918T100000\nDTEND:20260918T110000\nRRULE:FREQ=WEEKLY;COUNT=2\n"
    "END:VEVENT\nBEGIN:VEVENT\nUID:2\nSUMMARY:Holiday\nDTSTART;VALUE=DATE:20260921\n"
    "END:VEVENT\nEND:VCALENDAR\n", [("America/Denver", date(2026, 8, 31), date(2026, 10, 11))])

# DST edges in source and local zones: gap, fold, cross-midnight, multi-day.
for zone in ["America/New_York", "Europe/London", "Australia/Lord_Howe", "Pacific/Chatham", "Europe/Dublin",
             "Africa/Casablanca", "America/St_Johns", "Antarctica/Troll"]:
    for clock in ["000000", "013000", "020000", "023000", "030000", "120000", "233000"]:
        body = (f"UID:z\nSUMMARY:Zone {zone} {clock}\nDTSTART;TZID={zone}:20260101T{clock}\n"
                f"DTEND;TZID={zone}:20260101T{clock}\nDURATION:PT90M\nRRULE:FREQ=DAILY")
        body2 = (f"UID:y\nSUMMARY:Span\nDTSTART;TZID={zone}:20260101T{clock}\nDURATION:P1DT2H\n"
                 f"RRULE:FREQ=WEEKLY;BYDAY=SA,SU;UNTIL=20261231T235959Z")
        runs = []
        for tz in ("UTC", "America/Denver", "Australia/Sydney", "Europe/Dublin"):
            runs += [(tz, date(2026, 3, 1), date(2026, 4, 15)), (tz, date(2026, 9, 25), date(2026, 11, 10))]
        add(f"dst-{zone}-{clock}", wrap(body, body2), runs, full=clock in ("023000", "013000"))

# Overflow and range boundaries.
add("overflow-daily-huge-interval", wrap("SUMMARY:I\nDTSTART;VALUE=DATE:20260101\nRRULE:FREQ=DAILY;INTERVAL=5000000"),
    [("UTC", *year_2026)])
add("overflow-near-max", wrap("SUMMARY:M\nDTSTART;VALUE=DATE:99991225\nRRULE:FREQ=DAILY",
                              "SUMMARY:W\nDTSTART:99991220T230000\nRRULE:FREQ=WEEKLY",
                              "SUMMARY:Y\nDTSTART;VALUE=DATE:99990101\nRRULE:FREQ=YEARLY"),
    [("UTC", date(9999, 12, 1), date(9999, 12, 30)), ("UTC", date(9999, 12, 1), date(9999, 12, 31)),
     ("Asia/Kolkata", date(9999, 12, 20), date(9999, 12, 31))])
add("overflow-near-min", wrap("SUMMARY:A\nDTSTART;TZID=Asia/Kolkata:00010101T010000",
                              "SUMMARY:B\nDTSTART;VALUE=DATE:00010101\nRRULE:FREQ=MONTHLY"),
    [("UTC", date(1, 1, 1), date(1, 2, 1)), ("America/Denver", date(1, 1, 1), date(1, 3, 1))])
add("overflow-duration", wrap("SUMMARY:D\nDTSTART:20260101T000000\nDURATION:P999999999D"), [("UTC", *year_2026)])
add("overflow-negative-duration", wrap("SUMMARY:D\nDTSTART:20260101T000000\nDURATION:-P999999999DT23H59M59S"),
    [("UTC", *year_2026)])
add("overflow-date-duration", wrap("SUMMARY:D\nDTSTART;VALUE=DATE:99991231\nDURATION:P2D"), [("UTC", *year_2026)])
add("all-day-max", wrap("SUMMARY:D\nDTSTART;VALUE=DATE:99991231"), [("UTC", date(9999, 12, 30), date(9999, 12, 31))])

# Parser oddities.
add("parser-oddities", "\n".join([
    "BEGIN:VCALENDAR",
    "BEGIN:VEVENT", "uid:low", "summary:lower case names", "dtstart;value=date:20260301", "END:VEVENT",
    "BEGIN:VEVENT", "UID:q", "SUMMARY;X-A=\"a:b;c\":Quoted colon", "DTSTART;TZID=\"America/New_York\":20260302T080000",
    "DTEND;TZID=Europe/London:20260302T120000", "END:VEVENT",
    "BEGIN:VEVENT", "UID:u", "SUMMARY:Unicode digits", "DTSTART:٢٠٢٦٠٣٠٣T١٠٠٠", "END:VEVENT",
    "BEGIN:VEVENT", "UID:ud", "SUMMARY:Unicode date", "DTSTART;VALUE=DATE:2026 3 4", "END:VEVENT",
    "BEGIN:VEVENT", "UID:dup", "SUMMARY:first", "SUMMARY:second", "DTSTART:20260305T100000", "DTSTART:bad",
    "DTEND:20260305T090000", "END:VEVENT",
    "BEGIN:VEVENT", "UID:mix", "SUMMARY:mixed end", "DTSTART;VALUE=DATE:20260306", "DTEND:20260308T100000", "END:VEVENT",
    "BEGIN:VEVENT", "UID:mix2", "SUMMARY:aware end naive start", "DTSTART:20260307T100000",
    "DTEND:20260307T120000Z", "END:VEVENT",
    "BEGIN:VEVENT", "UID:mix3", "SUMMARY:naive end aware start", "DTSTART;TZID=Asia/Kolkata:20260308T100000",
    "DTEND:20260308T120000", "END:VEVENT",
    "BEGIN:VEVENT", "UID:sec", "SUMMARY:  leap second  ", "DTSTART:20260309T235960", "DURATION:PT0S", "END:VEVENT",
    "BEGIN:VEVENT", "UID:nest", "BEGIN:VEVENT", "SUMMARY:nested", "END:VEVENT", "SUMMARY:outer",
    "DTSTART:20260310T100000", "END:VEVENT",
    "BEGIN:VEVENT", "UID:esc", "SUMMARY:a\\\\b\\;c\\,d\\Ne\\x", "LOCATION:\\", "DTSTART:20260311", "END:VEVENT",
    "BEGIN:VEVENT", "UID:fold", "SUMMARY:fol", "\tded", " more", "DTSTART:20260312T000000Z", "END:VEVENT",
    "BEGIN:VEVENT", "UID:cancel", "STATUS:cancelled", "DTSTART:20260313", "END:VEVENT",
    "BEGIN:VEVENT", "UID:neg", "SUMMARY:negative duration", "DTSTART:20260314T100000", "DURATION:-PT1H", "END:VEVENT",
    "BEGIN:VEVENT", "UID:bad-dur", "SUMMARY:bad duration", "DTSTART:20260315T100000", "DURATION:P1H", "END:VEVENT",
    "BEGIN:VEVENT", "UID:zero-all-day", "SUMMARY:zero", "DTSTART;VALUE=DATE:20260316", "DTEND;VALUE=DATE:20260316", "END:VEVENT",
    "BEGIN:VEVENT", "UID:week", "SUMMARY:week duration", "DTSTART;VALUE=DATE:20260317", "DURATION:P1W", "END:VEVENT",
    "BEGIN:VEVENT", "UID:hours", "SUMMARY:all-day hours", "DTSTART;VALUE=DATE:20260318", "DURATION:PT36H", "END:VEVENT",
    "END:VCALENDAR"]), [(tz, date(2026, 2, 25), date(2026, 3, 31)) for tz in ("UTC", "America/Denver", "Asia/Kolkata")])

# Rule-parser corner cases (parse dump catches rule objects exactly).
for rrule in ["FREQ=daily;count=2", "FREQ=WEEKLY;INTERVAL=0", "FREQ=WEEKLY;INTERVAL=-3;COUNT=4", "FREQ=WEEKLY;INTERVAL=x",
              "FREQ=MONTHLY;COUNT=", "FREQ=MONTHLY;BYMONTHDAY=+-5", "FREQ=MONTHLY;BYMONTHDAY=²",
              "FREQ=MONTHLY;BYMONTHDAY= 5", "FREQ=MONTHLY;BYMONTHDAY=0,32,-32,15", "FREQ=YEARLY;BYMONTH=13,0,2",
              "FREQ=YEARLY;BYMONTH=¹", "FREQ=YEARLY;BYMONTH=3,3;COUNT=6", "FREQ=WEEKLY;BYDAY=+MO,1,MON,XX,-0FR,00SU",
              "FREQ=MONTHLY;BYDAY=5FR,53MO,-5MO", "FREQ=YEARLY;BYDAY=20MO", "FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3,10",
              "FREQ=YEARLY;BYDAY=FR;BYMONTHDAY=13", "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", "FREQ=DAILY;BYDAY=MO,TU;BYMONTH=2",
              "FREQ=WEEKLY;UNTIL=bad", "FREQ=WEEKLY;UNTIL=20260301T120000", "FREQ=WEEKLY;UNTIL=20260301T120000Z",
              "FREQ=WEEKLY;UNTIL=20260301", "FREQ=WEEKLY;COUNT=1_0", "FREQ=WEEKLY;COUNT=٣", "FREQ=WEEKLY;FREQ=DAILY;COUNT=3",
              "FREQ=MONTHLY;BYMONTHDAY=31", "FREQ=MONTHLY;BYMONTHDAY=29,30,31;BYMONTH=2", "FREQ=YEARLY;INTERVAL=4",
              "COUNT=3", "FREQ=YEARLY;BYMONTHDAY=-1;BYDAY=MO,TU,WE,TH,FR", "FREQ=DAILY;INTERVAL=1000;COUNT=2"]:
    for start in ["DTSTART;VALUE=DATE:20260131", "DTSTART;TZID=America/New_York:20260308T023000",
                  "DTSTART:20260115T120000Z"]:
        add(f"rule-{rrule}-{start}", wrap(f"UID:r\nSUMMARY:{rrule}\n{start}\nRRULE:{rrule}"),
            [("UTC", date(2026, 1, 1), date(2026, 6, 30)), ("Australia/Sydney", date(2027, 1, 1), date(2028, 12, 31))])

# EXDATE and overrides in every value form.
add("exdates", wrap(
    "UID:x\nSUMMARY:Ex\nDTSTART;TZID=Europe/London:20260105T090000\nDURATION:PT1H\nRRULE:FREQ=DAILY;COUNT=20\n"
    "EXDATE;TZID=Europe/London:20260106T090000,20260107T090000\nEXDATE:20260108T090000Z\nEXDATE;VALUE=DATE:20260109\n"
    "EXDATE:20260110T090000\nEXDATE:garbage,20260111T100000Z\nEXDATE;TZID=America/New_York:20260112T040000",
    "UID:x\nRECURRENCE-ID;TZID=Europe/London:20260113T090000\nSUMMARY:Moved\nDTSTART;TZID=Europe/London:20260113T150000",
    "UID:x\nRECURRENCE-ID;VALUE=DATE:20260114\nSUMMARY:Date override\nDTSTART:20260114T200000",
    "UID:\nRECURRENCE-ID:20260115T090000\nSUMMARY:No uid\nDTSTART:20260115T210000",
    "UID:ad\nSUMMARY:All-day ex\nDTSTART;VALUE=DATE:20260105\nRRULE:FREQ=DAILY;COUNT=10\nEXDATE:20260106T000000\n"
    "EXDATE;VALUE=DATE:20260107\nEXDATE:20260108"),
    [(tz, date(2026, 1, 1), date(2026, 1, 31)) for tz in ("UTC", "Europe/London", "America/Denver")])

# ---------------------------------------------------------------- fuzz
rng = random.Random(args.seed)


def pick(*items):
    return rng.choice(items)


def rand_date(lo=1995, hi=2032):
    return date(rng.randint(lo, hi), rng.randint(1, 12), rng.randint(1, 28 if rng.random() < 0.8 else 31 if True else 28))


def safe_date(lo=1995, hi=2032):
    while True:
        try:
            year, month = rng.randint(lo, hi), rng.randint(1, 12)
            day = rng.randint(1, 31)
            return date(year, month, day)
        except ValueError:
            continue


def rand_clock():
    if rng.random() < 0.3:
        return pick("000000", "010000", "013000", "020000", "023000", "030000", "023000", "235959", "120000")
    return f"{rng.randint(0, 23):02d}{rng.randint(0, 59):02d}{rng.randint(0, 59):02d}"


def value_text(day, kind, zone=None, clock=None):
    clock = clock or rand_clock()
    stamp = day.strftime("%Y%m%d")
    if kind == "date":
        return f";VALUE=DATE:{stamp}" if rng.random() < 0.8 else f":{stamp}"
    if kind == "utc":
        return f":{stamp}T{clock}Z"
    if kind == "floating":
        return f":{stamp}T{clock}"
    return f";TZID={zone}:{stamp}T{clock}"


def rand_rule(start):
    parts = []
    parts.append("FREQ=" + pick("DAILY", "WEEKLY", "MONTHLY", "YEARLY", "WEEKLY", "MONTHLY", "yearly", "HOURLY", ""))
    if rng.random() < 0.5:
        parts.append("INTERVAL=" + pick("1", "2", "3", "4", "7", "13", "0", "-2", "52", "x"))
    r = rng.random()
    if r < 0.4:
        parts.append("COUNT=" + str(pick(0, 1, 2, 3, 5, 10, 25, 100, 400)))
    elif r < 0.75:
        until = start + timedelta(days=rng.randint(-30, 900))
        stamp = until.strftime("%Y%m%d")
        parts.append("UNTIL=" + pick(stamp, stamp + "T" + rand_clock() + "Z", stamp + "T" + rand_clock(), "bogus"))
    if rng.random() < 0.5:
        days = rng.sample(["MO", "TU", "WE", "TH", "FR", "SA", "SU"], rng.randint(1, 3))
        items = []
        for code in days:
            if rng.random() < 0.4:
                items.append(pick("1", "2", "3", "4", "5", "-1", "-2", "+1", "53", "-5", "0") + code)
            else:
                items.append(code)
        parts.append("BYDAY=" + ",".join(items))
    if rng.random() < 0.3:
        parts.append("BYMONTHDAY=" + ",".join(pick("1", "15", "-1", "31", "30", "29", "-7", "0", "32", "10", "28")
                                             for _ in range(rng.randint(1, 3))))
    if rng.random() < 0.3:
        parts.append("BYMONTH=" + ",".join(pick("1", "2", "3", "6", "9", "10", "11", "12", "13", "2")
                                          for _ in range(rng.randint(1, 3))))
    rng.shuffle(parts)
    return ";".join(parts)


def rand_event(index):
    start = safe_date()
    kind = pick("date", "date", "utc", "floating", "tzid", "tzid", "tzid")
    zone = pick(*SOURCE_ZONES)
    clock = rand_clock()
    lines = [f"UID:{pick('a', 'b', 'c', '')}", f"SUMMARY:{pick('Alpha', 'beta', 'Gamma', 'delta', '', 'Éclair', 'Ωmega')} {index}"]
    if rng.random() < 0.3:
        lines.append("LOCATION:" + pick("Room 1", "Hall\\, B", "Line\\nBreak"))
    lines.append("DTSTART" + value_text(start, kind, zone, clock))
    r = rng.random()
    if r < 0.35:
        end_kind = kind if rng.random() < 0.85 else pick("date", "utc", "floating", "tzid")
        end_zone = zone if rng.random() < 0.7 else pick(*SOURCE_ZONES)
        end = start + timedelta(days=pick(0, 0, 0, 1, 1, 2, 3, -1, 10))
        lines.append("DTEND" + value_text(end, end_kind, end_zone))
    elif r < 0.7:
        lines.append("DURATION:" + pick("PT1H", "PT30M", "PT0S", "P1D", "P2D", "PT25H", "P1DT2H", "-PT1H", "P1W",
                                        "PT90M", "PT23H59M59S", "P", "PT", "bad"))
    if rng.random() < 0.85:
        lines.append("RRULE:" + rand_rule(start))
    for _ in range(pick(0, 0, 1, 2, 3)):
        ex = start + timedelta(days=rng.randint(0, 120))
        lines.append("EXDATE" + value_text(ex, pick(kind, kind, "date", "utc", "floating", "tzid"), zone, clock))
    if rng.random() < 0.05:
        lines.append("STATUS:CANCELLED")
    return start, lines, kind, zone, clock


for index in range(args.fuzz):
    bodies, starts = [], []
    for sub in range(pick(1, 1, 1, 2, 3)):
        start, lines, kind, zone, clock = rand_event(f"{index}.{sub}")
        bodies.append("\n".join(lines))
        starts.append(start)
        if rng.random() < 0.15 and kind != "date":
            # RECURRENCE-ID override of a later instance.
            moved = start + timedelta(days=rng.choice([7, 14, 1, 30]))
            uid = lines[0][4:]
            bodies.append("\n".join([f"UID:{uid}", "RECURRENCE-ID" + value_text(moved, kind, zone, clock),
                                     "SUMMARY:Moved", "DTSTART" + value_text(moved + timedelta(days=1), kind, zone)]))
    runs = []
    for _ in range(3):
        anchor = rng.choice(starts) + timedelta(days=rng.randint(-40, 1500))
        length = pick(0, 0, 6, 30, 41, 41, 41, 90, 365)
        first = anchor
        last = anchor + timedelta(days=length)
        runs.append((pick(*LOCAL_ZONES), first, last))
    add(f"fuzz-{index}", wrap(*bodies), runs, full=False)


def emit():
    by_tz = {}
    for case in cases:
        for tz, _first, _last in case[2]:
            by_tz.setdefault(tz, None)
    results = {}
    for tz in by_tz:
        os.environ["TZ"] = tz
        time.tzset()
        for number, (case_id, ics, runs, full) in enumerate(cases):
            for run_index, (run_tz, first, last) in enumerate(runs):
                if run_tz == tz:
                    text = dumps(expansion(ics, first, last))
                    results[(number, run_index)] = text if full else "sha256:" + hashlib.sha256(text.encode()).hexdigest()
    os.environ["TZ"] = "UTC"
    time.tzset()
    out = sys.stdout
    for number, (case_id, ics, runs, full) in enumerate(cases):
        dump, _ = parse_dump(ics)
        parse_text = dumps(dump)
        if not full:
            parse_text = "sha256:" + hashlib.sha256(parse_text.encode()).hexdigest()
        out.write(json.dumps({"id": case_id, "ics": ics, "parse": parse_text,
                              "runs": [[tz, first.isoformat(), last.isoformat(), results[(number, i)]]
                                       for i, (tz, first, last) in enumerate(runs)]}, ensure_ascii=False) + "\n")


emit()
