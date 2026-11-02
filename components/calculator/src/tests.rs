//! Port of test_calculator_backend.py (ArithmeticTests, StoreTests, LauncherTests).

use crate::backend::*;
use crate::cli;
use crate::decimal::Decimal;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn d(text: &str) -> Decimal { Decimal::parse(text).unwrap() }
fn ev(expression: &str) -> Result<Decimal, CalculatorError> { evaluate(expression, true, &Decimal::zero()) }
fn close(value: Decimal, expected: f64) -> bool { (value.to_f64() - expected).abs() < 1e-7 }

#[test]
fn precedence_parentheses_powers_and_unary() {
    for (expression, answer) in [("2+3*4", "14"), ("(2+3)*4", "20"), ("2^3^2", "512"), ("-2^2", "-4"), ("(-2)^2", "4"),
                                 ("2^-2", ".25"), ("2**3", "8"), ("4×3−2÷2", "11"), ("--2", "2")] {
        assert_eq!(ev(expression).unwrap(), d(answer), "{expression}");
    }
}

#[test]
fn decimal_arithmetic_and_percent() {
    for (expression, answer) in [("0.1+0.2", ".3"), (".25 + .75", "1"), ("2.5e-3*1000", "2.5"), ("25%", ".25"), ("200*10%", "20"),
                                 ("100+10%", "100.1"), ("-25%", "-.25"), ("(50+50)%", "1")] {
        assert_eq!(ev(expression).unwrap(), d(answer), "{expression}");
    }
}

#[test]
fn implicit_multiplication_constants_and_previous_answer() {
    assert_eq!(ev("2(3+4)").unwrap(), d("14"));
    assert_eq!(ev("(2+3)(4+5)").unwrap(), d("45"));
    assert!(close(ev("2pi").unwrap(), 2.0 * std::f64::consts::PI));
    assert!(close(ev("π").unwrap(), std::f64::consts::PI));
    assert_eq!(evaluate("2ans+1", true, &d("2.5")).unwrap(), d("6"));
    assert!(close(ev("2sin(30)").unwrap(), 1.0));
}

#[test]
fn scientific_functions_and_angle_units() {
    assert_eq!(ev("sqrt(81)+abs(-4)").unwrap(), d("13"));
    assert_eq!(ev("log(1000)").unwrap(), d("3"));
    assert_eq!(ev("ln(1)").unwrap(), d("0"));
    assert_eq!(ev("exp(0)").unwrap(), d("1"));
    assert!(close(ev("sin(30)").unwrap(), 0.5));
    assert!(close(ev("cos(60)").unwrap(), 0.5));
    assert!(close(ev("tan(45)").unwrap(), 1.0));
    assert_eq!(ev("cos(90)").unwrap(), d("0"));
    assert!(close(ev("asin(.5)").unwrap(), 30.0));
    assert!(close(ev("acos(.5)").unwrap(), 60.0));
    assert!(close(ev("atan(1)").unwrap(), 45.0));
    assert!(close(evaluate("sin(pi/2)", false, &Decimal::zero()).unwrap(), 1.0));
    assert!(close(evaluate("asin(1)", false, &Decimal::zero()).unwrap(), 1.5707963267948966));
    assert_eq!(ev("5!").unwrap(), d("120"));
    assert_eq!(ev("0!").unwrap(), d("1"));
}

#[test]
fn invalid_domains_and_unfinished_input_explain_error() {
    for expression in ["", "2+", "(2+3", "2+3)", "sqrt(-1)", "1/0", "0^0", "0^-1", "ln(0)", "log(-2)", "asin(2)", "acos(-2)",
                       "tan(90)", "(-2)^.5", "2.5!", "171!", "(-1)!", "sin 30", "1 2", "1..2"] {
        let error = ev(expression).expect_err(expression);
        assert!(!error.0.is_empty(), "{expression}");
    }
}

#[test]
fn arbitrary_python_is_rejected() {
    for expression in ["__import__('os').system('true')", "open('/tmp/calc-test','w')", "[1,2]", "lambda:1", "a.__class__", "True",
                       "Infinity", "nan", "1;2", "1//2", "sum(1)", "x=2"] {
        assert!(ev(expression).is_err(), "{expression}");
    }
}

#[test]
fn resource_limits_reject_expensive_inputs_quickly() {
    let started = Instant::now();
    let long = "9".repeat(513);
    let plus = vec!["1"; 100].join("+");
    let nested = format!("{}1{}", "(".repeat(40), ")".repeat(40));
    for expression in [long.as_str(), &plus, &nested, "9^999999999", "9^9^9", "1e9999999999999999999999999999999999", "exp(9999999)",
                       "sin(1e100)", "1e1000*1e1000", "1e-1000/1e1000"] {
        assert!(ev(expression).is_err(), "{expression}");
    }
    assert!(started.elapsed().as_secs_f64() < 1.0);
}

#[test]
fn formatting_stays_readable_and_round_trips() {
    for (value, display) in [("0", "0"), ("-0", "0"), ("1.5000", "1.5"), ("1000", "1000"), (".000001", "0.000001"), ("1e20", "1e20"),
                             ("1.2500e-12", "1.25e-12")] {
        assert_eq!(format_number(&d(value)), display);
        assert_eq!(ev(display).unwrap(), d(value));
    }
}

/// Messages exactly as calculator_backend.py raises them (the UI shows tr(message)).
#[test]
fn error_messages_match_python() {
    for (expression, message) in [
        ("", "Enter an expression first."), ("2+", "A number or function is missing."), ("(2+3", "A closing parenthesis is missing."),
        ("2+3)", "Check the operators and parentheses."), ("sqrt(-1)", "Square root needs a non-negative number."),
        ("1/0", "Cannot divide by zero."), ("0^0", "Zero cannot have a zero or negative exponent."),
        ("ln(0)", "Logarithms need a positive number."), ("asin(2)", "Inverse sine and cosine need a value between −1 and 1."),
        ("tan(90)", "Tangent is undefined at this angle."), ("(-2)^.5", "A negative base needs a whole-number exponent."),
        ("171!", "Factorial needs a whole number from 0 to 170."), ("sin 30", "Use parentheses after sin."), ("1 2", "Use an operator between numbers."),
        ("x=2", "Unexpected character: ="), ("sum(1)", "Unknown function: sum"), ("9^9^9", "Powers are limited to exponents between −1000 and 1000."),
        ("1e1001", "The number's exponent is outside the supported range."), ("999e999", "The result is outside the supported range."),
        ("1e1000*1e1000", "This calculation is outside the supported range."), ("1e-1000/1e1000", "This calculation is outside the supported range."),
        ("exp(9999999)", "The exponential argument is too large."), ("sin(1e100)", "The trigonometric argument is too large."),
    ] {
        assert_eq!(ev(expression).unwrap_err().0, message, "{expression}");
    }
    assert_eq!(ev(&vec!["1"; 100].join("+")).unwrap_err().0, "This expression has too many operations.");
    assert_eq!(ev(&format!("{}1{}", "(".repeat(40), ")".repeat(40))).unwrap_err().0, "This expression is nested too deeply.");
    assert_eq!(ev(&"9".repeat(513)).unwrap_err().0, "This expression is too long.");
}

/// Results generated with Python 3.14 `calculator_backend.evaluate`: (expression, format_number, str(Decimal)).
#[test]
fn results_match_python_reference() {
    for (expression, formatted, text) in [
        ("1/3", "0.3333333333333333333333333333", "0.3333333333333333333333333333"),
        ("2/3", "0.6666666666666666666666666667", "0.6666666666666666666666666667"),
        ("sqrt(2)", "1.414213562373095048801688724", "1.414213562373095048801688724"),
        ("ln(2)", "0.6931471805599453094172321215", "0.6931471805599453094172321215"),
        ("log(2)", "0.3010299956639811952137388947", "0.3010299956639811952137388947"),
        ("exp(1)", "2.718281828459045235360287471", "2.718281828459045235360287471"),
        ("2^0.5", "1.414213562373095048801688724", "1.414213562373095048801688724"),
        ("9^999", "1.942079168580724010733305132e953", "1.942079168580724010733305132E+953"),
        ("170!", "7.257415615307998967396728211e306", "7.257415615307998967396728211E+306"),
        ("sin(30)", "0.5", "0.5"),
        ("cos(30)", "0.866025403784439", "0.866025403784439"),
        ("tan(30)", "0.577350269189626", "0.577350269189626"),
        ("sin(1)", "0.0174524064372835", "0.0174524064372835"),
        ("asin(0.3)", "17.4576031237221", "17.4576031237221"),
        ("2pi", "6.283185307179586476925286766", "6.283185307179586476925286766"),
        ("e^2", "7.389056098930650227230427459", "7.389056098930650227230427459"),
        ("10^(-3)", "0.001", "0.001"),
        ("1e-7", "1e-7", "1E-7"),
        ("123456789012345678", "1.23456789012345678e17", "123456789012345678"),
        ("0.1*3", "0.3", "0.3"),
        ("exp(2300)", "7.538906854065543421691995605e998", "7.538906854065543421691995605E+998"),
        ("exp(-2300)", "1.326452255423112272408042182e-999", "1.326452255423112272408042182E-999"),
        ("ln(1e-1000)", "-2302.585092994045684017991455", "-2302.585092994045684017991455"),
        ("log(1.0000000000000000000000000001)", "0", "0"),
        ("2^-1000", "9.332636185032188789900895447e-302", "9.332636185032188789900895447E-302"),
        ("sqrt(1e-999)", "3.162277660168379331998893544e-500", "3.162277660168379331998893544E-500"),
        ("100!/98!", "9900", "9900.000000000000000000000000"),
        ("7^0.5^2", "1.626576561697785743211232345", "1.626576561697785743211232345"),
        ("1/7*7", "1", "1.000000000000000000000000000"),
        ("2^1000", "1.071508607186267320948425049e301", "1.071508607186267320948425049E+301"),
        ("2^0.1", "1.071773462536293164213006325", "1.071773462536293164213006325"),
        ("3^-0.7", "0.4634630567719697801617514315", "0.4634630567719697801617514315"),
        ("0.5^0.5", "0.7071067811865475244008443621", "0.7071067811865475244008443621"),
        ("1.5^1000", "1.233840596906173479227439099e176", "1.233840596906173479227439099E+176"),
        ("sqrt(0.0001)", "0.01", "0.01"),
        ("ln(10)", "2.302585092994045684017991455", "2.302585092994045684017991455"),
        ("exp(-1)", "0.3678794411714423215955237702", "0.3678794411714423215955237702"),
        ("1e-990/1e10", "1e-1000", "1E-1000"),
        ("10^1000", "1e1000", "1.000000000000000000000000000E+1000"),
        ("99^500", "6.570483042414632926501121938e997", "6.570483042414632926501121938E+997"),
        ("(-2)^3", "-8", "-8"),
        ("-0", "0", "0"),
        ("ln(0.999)", "-0.001000500333583533500142982254", "-0.001000500333583533500142982254"),
        ("exp(0.0001)", "1.000100005000166670833416668", "1.000100005000166670833416668"),
        ("exp(-1e-40)", "1", "1.000000000000000000000000000"),
        ("2.5^-3.5", "0.04047715405015525544958583737", "0.04047715405015525544958583737"),
        ("atan(1e11)", "89.999999999427", "89.999999999427"),
        ("acos(-1)", "180", "180"),
        ("1.1^-999", "4.45356224851692593956420724e-42", "4.453562248516925939564207240E-42"),
        ("0.3^999", "4.406902731602688789634850866e-523", "4.406902731602688789634850866E-523"),
        ("sqrt(16)/4%", "100", "1E+2"),
        ("ln(1.5)*log(7.25)", "0.3488370428438705977560020166", "0.3488370428438705977560020166"),
        ("cos(pi)", "0.998497149863864", "0.998497149863864"),
        ("tan(-45)", "-1", "-1"),
        ("(1/3)^3", "0.03703703703703703703703703703", "0.03703703703703703703703703703"),
        ("1-0.9999999999999999999999999999", "1e-28", "1E-28"),
        ("ans*2", "0", "0"),
    ] {
        let value = ev(expression).unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(format_number(&value), formatted, "{expression}");
        assert_eq!(value.to_string(), text, "{expression}");
    }
    for (expression, message) in [
        ("1e-1000/3", "This calculation is outside the supported range."),
        ("exp(-2310)", "The exponential argument is too large."),
        ("2^-1074", "Powers are limited to exponents between −1000 and 1000."),
        ("sqrt(1e-1001)", "The number's exponent is outside the supported range."),
        ("999e999", "The result is outside the supported range."),
        ("(-1)^0.5", "A negative base needs a whole-number exponent."),
        ("0^-2", "Zero cannot have a zero or negative exponent."),
        ("1e500^3", "This calculation is outside the supported range."),
        ("1e-500^3", "This calculation is outside the supported range."),
        ("1e-999*1e-2", "The result is outside the supported range."),
        ("1e-1000*0.5", "The result is outside the supported range."),
    ] {
        assert_eq!(ev(expression).unwrap_err().0, message, "{expression}");
    }
}

fn umask() -> u32 {
    let mask = unsafe { libc::umask(0o022) };
    unsafe { libc::umask(mask) };
    mask as u32
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn temp(name: &str) -> Dir {
    let path = std::env::temp_dir().join(format!("calculator-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    Dir(path)
}

fn store_at(dir: &Dir) -> (PathBuf, CalculatorStore) {
    let path = dir.0.join("calculator/state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    (path.clone(), CalculatorStore::new(Some(path)))
}

#[test]
fn history_memory_preferences_round_trip_and_clear() {
    let dir = temp("roundtrip");
    let (path, mut store) = store_at(&dir);
    store.scientific = true;
    store.degrees = false;
    store.record("2+2", &d("4"));
    store.memory_action("MS", Some(&d(".1"))).unwrap();
    store.memory_action("M+", Some(&d(".2"))).unwrap();
    assert_eq!(store.memory_action("MR", None).unwrap(), Some(d(".3")));
    store.memory_action("M-", Some(&d(".1"))).unwrap();
    let mut loaded = CalculatorStore::new(Some(path.clone()));
    assert!(loaded.scientific);
    assert!(!loaded.degrees);
    assert_eq!(loaded.memory, Some(d(".2")));
    assert_eq!(loaded.history[0].expression, "2+2");
    assert_eq!(loaded.history[0].result, "4");
    loaded.memory_action("MC", None).unwrap();
    loaded.clear_history();
    let again = CalculatorStore::new(Some(path.clone()));
    assert_eq!(again.memory, None);
    assert!(again.history.is_empty());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    // A missing state directory is created private (mkdir mode=0o700).
    let fresh = dir.0.join("fresh/state.json");
    CalculatorStore::new(Some(fresh.clone())).clear_history();
    assert_eq!(fs::metadata(fresh.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700 & !umask());
}

#[test]
fn state_file_bytes_match_python_json_dump() {
    let dir = temp("bytes");
    let (path, mut store) = store_at(&dir);
    store.history.push(HistoryEntry { expression: "π×\"2\"".into(), result: "6.28".into(), degrees: false, timestamp: 1730000000.25 });
    store.history.push(HistoryEntry { expression: "1".into(), result: "1".into(), degrees: true, timestamp: 123.0 });
    store.memory_action("MS", Some(&d("12.50"))).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(),
               "{\"version\": 1, \"scientific\": false, \"degrees\": true, \"memory\": \"12.50\", \"history\": [{\"expression\": \"π×\\\"2\\\"\", \
                \"result\": \"6.28\", \"degrees\": false, \"timestamp\": 1730000000.25}, {\"expression\": \"1\", \"result\": \"1\", \
                \"degrees\": true, \"timestamp\": 123.0}]}\n");
}

#[test]
fn history_limit_and_duplicate_reuse() {
    let dir = temp("limit");
    let (_, mut store) = store_at(&dir);
    for number in 0..MAX_HISTORY + 5 {
        store.record(&number.to_string(), &Decimal::from_i64(number as i64));
    }
    assert_eq!(store.history.len(), MAX_HISTORY);
    store.record("10", &d("10"));
    assert_eq!(store.history.len(), MAX_HISTORY);
    assert_eq!(store.history[0].expression, "10");
    assert_eq!(store.history.iter().filter(|e| e.expression == "10").count(), 1);
}

#[test]
fn invalid_saved_values_are_not_executed_or_used() {
    let dir = temp("invalid");
    let (path, _) = store_at(&dir);
    fs::write(&path, r#"{"memory": "NaN", "history": [{"expression": "__import__('os')", "result": "NaN"}, {"expression": "2+2", "result": "4"}]}"#).unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert_eq!(loaded.memory, None);
    assert_eq!(loaded.history.len(), 1);
    assert_eq!(loaded.history[0].result, "4");
    fs::write(&path, "broken json").unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert_eq!(loaded.error, "Expecting value: line 1 column 1 (char 0)");
    assert!(loaded.history.is_empty());
}

#[test]
fn unwritable_location_does_not_destroy_session_history() {
    let dir = temp("unwritable");
    let obstructing = dir.0.join("not-a-directory");
    fs::write(&obstructing, "keep").unwrap();
    let mut store = CalculatorStore::new(Some(obstructing.join("state.json")));
    store.record("3+3", &d("6"));
    assert_eq!(store.error, format!("[Errno 17] File exists: '{}'", obstructing.display()));
    assert_eq!(store.history[0].result, "6");
    assert_eq!(fs::read_to_string(&obstructing).unwrap(), "keep");
}

#[test]
fn corrupt_timestamp_and_deep_json_do_not_crash_startup() {
    let dir = temp("corrupt");
    let (path, _) = store_at(&dir);
    fs::write(&path, format!(r#"{{"history": [{{"expression": "1+1", "result": "2", "timestamp": 1{}}},
        {{"expression": "2+2", "result": "4", "timestamp": "NaN"}}, {{"expression": "3+3", "result": "6", "timestamp": 123}}]}}"#,
        "0".repeat(400))).unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert_eq!(loaded.history.iter().map(|e| e.result.as_str()).collect::<Vec<_>>(), ["6"]);
    fs::write(&path, format!("{}0{}", "[".repeat(2000), "]".repeat(2000))).unwrap();
    let loaded = CalculatorStore::new(Some(path));
    assert!(!loaded.error.is_empty());
    assert!(loaded.history.is_empty());
}

#[test]
fn failed_atomic_replace_preserves_existing_file() {
    let dir = temp("replace");
    let (path, mut store) = store_at(&dir);
    store.record("2+2", &d("4"));
    let original = fs::read(&path).unwrap();
    store.replace = |_, _| Err(std::io::Error::other("Disk unavailable"));
    store.record("3+3", &d("6"));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(store.error.contains("Disk unavailable"));
    assert_eq!(store.history[0].result, "6");
    assert!(!fs::read_dir(path.parent().unwrap()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".state-")));
}

#[test]
fn loader_type_rules_follow_python() {
    let dir = temp("types");
    let (path, _) = store_at(&dir);
    fs::write(&path, r#"{"scientific": 1, "degrees": 0, "history": "ab", "memory": "1e1001"}"#).unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert!(!loaded.scientific && loaded.degrees && loaded.history.is_empty() && loaded.memory.is_none() && loaded.error.is_empty());
    fs::write(&path, r#"{"scientific": true, "degrees": false, "history": {}, "memory": "5"}"#).unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert!(loaded.scientific && !loaded.degrees && loaded.memory.is_none());
    assert_eq!(loaded.error, "unhashable type: 'slice'");
    fs::write(&path, r#"{"history": null}"#).unwrap();
    assert_eq!(CalculatorStore::new(Some(path.clone())).error, "'NoneType' object is not subscriptable");
    fs::write(&path, "[]").unwrap();
    assert_eq!(CalculatorStore::new(Some(path.clone())).error, "Invalid saved calculator history.");
    fs::write(&path, r#"{"history": [{"expression": "1", "result": "1", "timestamp": "12", "degrees": null}, [1], "x",
        {"expression": 1, "result": "1"}, {"expression": "2", "result": "2", "timestamp": true}]}"#).unwrap();
    let loaded = CalculatorStore::new(Some(path.clone()));
    assert_eq!(loaded.history.len(), 2);
    assert_eq!((loaded.history[0].timestamp, loaded.history[0].degrees), (12.0, false));
    assert_eq!(loaded.history[1].timestamp, 1.0);
    fs::write(&path, vec![b' '; MAX_STATE_BYTES + 1]).unwrap();
    assert_eq!(CalculatorStore::new(Some(path)).error, "Saved calculator history is too large.");
}

#[test]
fn overlong_cli_expression_is_rejected_without_truncating_math() {
    let failure = cli::arguments(&["1".repeat(513)]).unwrap_err();
    assert_eq!(failure.code, 2);
    assert!(failure.stderr.contains("expression is too long"));
}

#[test]
fn cli_matches_argparse() {
    let args = |list: &[&str]| cli::arguments(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(args(&["--sci", "7*8"]).unwrap(), cli::Options { expression: Some("7*8".into()), scientific: true, ..Default::default() });
    assert_eq!(args(&["-5"]).unwrap().expression.as_deref(), Some("-5"));
    assert_eq!(args(&["--", "-(2)"]).unwrap().expression.as_deref(), Some("-(2)"));
    assert!(args(&["--quit"]).unwrap().quit);
    assert_eq!(args(&["--help"]).unwrap_err(), cli::Exit { code: 0, stdout: cli::HELP.into(), stderr: String::new() });
    assert_eq!(args(&["--bogus"]).unwrap_err().stderr,
               "usage: calculator [-h] [--scientific] [--check] [expression]\ncalculator: error: unrecognized arguments: --bogus\n");
    assert_eq!(args(&["a", "b"]).unwrap_err().stderr,
               "usage: calculator [-h] [--scientific] [--check] [expression]\ncalculator: error: unrecognized arguments: b\n");
    assert_eq!(args(&["--check=1"]).unwrap_err().stderr.lines().nth(1).unwrap(),
               "calculator: error: argument --check: ignored explicit argument '1'");
}

#[test]
fn check_reports_dependencies_as_json() {
    let status = cli::check();
    assert_eq!(status, "{\"ready\": true, \"application_id\": \"org.tessel.Calculator\", \"gtk4\": true, \"layer_shell\": true, \
                        \"calculator\": true, \"error\": \"\"}");
}
