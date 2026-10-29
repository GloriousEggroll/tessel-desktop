//! test_calibration.py + test_sdr_workspace.py. Every compositor interaction
//! goes to `FakeHost` (recorded signals, scripted status); files go to a
//! private directory under `std::env::temp_dir()`.

use crate::calibration::{self as c, CalError, Host, Profile};
use crate::correction;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use way_displays::color_config::{self, Status};

/// The XDG variables are process-wide: serialize the tests that set them.
static ENV: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    path: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("hdr-calibration-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        Fixture { path: root.join("rc.xml"), root, _guard: guard }
    }

    fn write(&self, text: &str) {
        std::fs::write(&self.path, text).unwrap();
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn profile() -> Profile {
    json!({"version": 1, "connector": "DP-1", "displayId": "0123456789abcdef", "mode": "HDR True Black",
           "method": "visual", "black": 0, "peak": 1000, "fullFrame": 300, "white": 160}).as_object().unwrap().clone()
}

fn with(base: &Profile, changes: Value) -> Profile {
    let mut result = base.clone();
    for (k, v) in changes.as_object().unwrap() {
        result.insert(k.clone(), v.clone());
    }
    result
}

fn monitor() -> BTreeMap<String, String> {
    [("enabled", "true"), ("hdr", "true"), ("bit_depth", "10"), ("display_id", "0123456789abcdef")]
        .into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

fn section(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn status(compositor: &[(&str, &str)], monitor: BTreeMap<String, String>) -> Status {
    let mut status = Status::new();
    status.insert("compositor".into(), section(compositor));
    status.insert("DP-1".into(), monitor);
    status
}

fn active_monitor() -> BTreeMap<String, String> {
    let mut active = monitor();
    active.insert("hdr_calibration_active".into(), "true".into());
    for (key, value) in c::STATUS_FIELDS.iter().zip(["0", "1000", "300", "160"]) {
        active.insert((*key).into(), value.into());
    }
    active
}

/// Scripted `session()` results, recorded signals and a fixed labwc command line.
struct FakeHost {
    sessions: Mutex<VecDeque<Result<Status, CalError>>>,
    repeat: Option<Status>,
    signals: Mutex<Vec<(i32, i32)>>,
    cmdline: Vec<u8>,
}

impl FakeHost {
    fn new(sessions: Vec<Result<Status, CalError>>, config: &Path) -> Self {
        Self { sessions: Mutex::new(sessions.into()), repeat: None, signals: Mutex::new(Vec::new()),
               cmdline: format!("labwc\0-c\0{}\0", config.display()).into_bytes() }
    }

    fn always(status: Status, config: &Path) -> Self {
        let mut host = Self::new(Vec::new(), config);
        host.repeat = Some(status);
        host
    }

    fn signals(&self) -> Vec<(i32, i32)> {
        self.signals.lock().unwrap().clone()
    }
}

impl Host for FakeHost {
    fn read_status(&self) -> Status { self.repeat.clone().unwrap_or_default() }
    fn session(&self) -> Result<Status, CalError> {
        match self.sessions.lock().unwrap().pop_front() {
            Some(result) => result,
            None => self.repeat.clone().ok_or_else(|| CalError::Value("no scripted status".into())),
        }
    }
    fn signal(&self, pid: i32, signal: i32) -> std::io::Result<()> {
        self.signals.lock().unwrap().push((pid, signal));
        Ok(())
    }
    fn cmdline(&self, _pid: i64) -> std::io::Result<Vec<u8>> { Ok(self.cmdline.clone()) }
    fn cwd(&self, _pid: i64) -> PathBuf { PathBuf::from("/") }
    fn sleep(&self, _duration: Duration) {}
}

struct ArgsHost(Vec<u8>);

impl Host for ArgsHost {
    fn read_status(&self) -> Status { Status::new() }
    fn signal(&self, _: i32, _: i32) -> std::io::Result<()> { panic!("never signals") }
    fn cmdline(&self, _: i64) -> std::io::Result<Vec<u8>> { Ok(self.0.clone()) }
    fn cwd(&self, _: i64) -> PathBuf { PathBuf::from("/proc-cwd") }
}

#[test]
fn active_config_file_takes_precedence() {
    let _f = Fixture::new("precedence");
    let status = status(&[("pid", "1234")], monitor());
    for args in ["/usr/bin/labwc\0-C\0/tmp/configdir\0-c\0/tmp/custom.xml\0", "labwc\0--config=/tmp/custom.xml\0", "labwc\0-c/tmp/custom.xml\0"] {
        assert_eq!(c::config_path(&status, &ArgsHost(args.as_bytes().to_vec())).unwrap(), PathBuf::from("/tmp/custom.xml"));
    }
    assert_eq!(c::config_path(&status, &ArgsHost(b"labwc\0--config-dir=/tmp/configdir\0".to_vec())).unwrap(), PathBuf::from("/tmp/configdir/rc.xml"));
    assert_eq!(c::config_path(&status, &ArgsHost(b"labwc\0-Crel\0".to_vec())).unwrap(), PathBuf::from("/proc-cwd/rel/rc.xml"));
    for refused in ["labwc\0-m\0", "labwc\0--merge-config\0", "sway\0"] {
        assert!(c::config_path(&status, &ArgsHost(refused.as_bytes().to_vec())).is_err());
    }
}

#[test]
fn limits_and_provenance() {
    assert_eq!(c::validate(&profile()).unwrap()["white"], json!(160.0));
    for change in [json!({"white": f64::NAN.to_string()}), json!({"peak": "inf"}), json!({"black": -1}), json!({"white": 501}),
                   json!({"fullFrame": 1001}), json!({"white": 301}), json!({"displayId": "0000000000000000"}),
                   json!({"method": "meter-readings"}), json!({"mode": ""})] {
        assert!(c::validate(&with(&profile(), change.clone())).is_err(), "{change}");
    }
    c::validate(&with(&profile(), json!({"method": "meter-readings", "instrument": "Example instrument"}))).unwrap();
    // Python float() accepts numeric strings; key order is kept and preset added.
    let normalized = c::validate(&with(&profile(), json!({"peak": " 1000 "}))).unwrap();
    assert_eq!(normalized["peak"], json!(1000.0));
    assert_eq!(normalized.keys().last().map(String::as_str), Some("preset"));
}

#[test]
fn monitor_identity_sdr_and_8bit_rejected() {
    let identity = Some("0123456789abcdef");
    c::check_monitor(&status(&[], monitor()), "DP-1", identity).unwrap();
    for (key, value) in [("hdr", "false"), ("enabled", "false"), ("bit_depth", "8"), ("display_id", "fedcba9876543210")] {
        let mut changed = monitor();
        changed.insert(key.into(), value.into());
        assert!(c::check_monitor(&status(&[], changed), "DP-1", identity).is_err());
    }
}

fn outputs(text: &str) -> Vec<BTreeMap<String, String>> {
    let document = roxmltree::Document::parse(text).unwrap();
    document.descendants().filter(|n| n.has_tag_name("output") && n.parent().is_some_and(|p| p.has_tag_name("hdrCalibration")))
        .map(|n| n.attributes().map(|a| (a.name().to_owned(), a.value().to_owned())).collect()).collect()
}

#[test]
fn independent_monitors_preserve_sdr_vrr_and_comments() {
    let f = Fixture::new("independent");
    let original = "<labwc_config>\n<!-- user comment -->\n<core><adaptiveSync>yes</adaptiveSync></core><colorManagement><output name=\"DP-1\" sdrBitDepth=\"10\"/></colorManagement>\n</labwc_config><!-- end -->";
    f.write(original);
    c::write_config(&f.path, "DP-1", Some(&profile())).unwrap();
    c::write_config(&f.path, "HDMI-A-1", Some(&with(&profile(), json!({"connector": "HDMI-A-1", "white": 200})))).unwrap();
    let data = f.read();
    assert!(data.contains("<core><adaptiveSync>yes</adaptiveSync></core>"));
    assert!(data.contains("<!-- user comment -->"));
    assert!(data.ends_with("<!-- end -->"));
    assert_eq!(color_config::read_colors(&f.path).unwrap()["DP-1"].sdr_bit_depth, 10);
    let found: BTreeMap<String, Option<String>> = outputs(&data).into_iter().map(|o| (o["name"].clone(), o.get("white").cloned())).collect();
    assert_eq!(found, BTreeMap::from([("DP-1".into(), Some("160".into())), ("HDMI-A-1".into(), Some("200".into()))]));
    c::write_config(&f.path, "DP-1", None).unwrap();
    let found: BTreeMap<String, Option<String>> = outputs(&f.read()).into_iter().map(|o| (o["name"].clone(), o.get("enabled").cloned())).collect();
    assert_eq!(found, BTreeMap::from([("DP-1".into(), Some("false".into())), ("HDMI-A-1".into(), None)]));
}

/// The exact bytes calibration.py writes (checked against CPython's ElementTree).
#[test]
fn managed_block_bytes_match_python() {
    let f = Fixture::new("bytes");
    f.write("<labwc_config/>");
    let mut corrected = with(&profile(), json!({"mode": "A \"B\" & <C>", "black": 0.0005}));
    corrected.insert("correction".into(), json!({"path": "/x/y.cube", "sha256": "a".repeat(64), "encoding": "BT2020-PQ", "size": 2}));
    c::write_config(&f.path, "DP-1", Some(&corrected)).unwrap();
    c::write_config(&f.path, "HDMI-A-1", None).unwrap();
    // The second write leaves the first block's trailing newline (Python does the same).
    assert_eq!(f.read(), format!("<labwc_config>\n\n<!-- hdr-calibration begin -->\n<hdrCalibration>\n  <output name=\"DP-1\" displayId=\"0123456789abcdef\" black=\"0.0005\" peak=\"1000\" fullFrame=\"300\" white=\"160\" pictureMode=\"A &quot;B&quot; &amp; &lt;C&gt;\" correction=\"/x/y.cube\" correctionSha256=\"{}\" />\n  <output name=\"HDMI-A-1\" enabled=\"false\" />\n</hdrCalibration>\n<!-- hdr-calibration end -->\n</labwc_config>\n", "a".repeat(64)));
}

#[test]
fn rollback_and_concurrent_changes() {
    let f = Fixture::new("rollback");
    f.write("<labwc_config/>");
    let tx = c::write_config(&f.path, "DP-1", Some(&profile())).unwrap();
    assert!(tx.backup.starts_with(f.root.join("state/desktop-suite/backups/hdr")));
    color_config::rollback(&tx).unwrap();
    assert_eq!(f.read(), "<labwc_config/>");
    let tx = c::write_config(&f.path, "DP-1", Some(&profile())).unwrap();
    f.write("<labwc_config><keyboard/></labwc_config>");
    assert!(matches!(color_config::rollback(&tx), Err(color_config::ColorError::Runtime(_))));
    assert!(f.read().contains("<keyboard/>"));
}

#[test]
fn saved_modes_are_separate() {
    let _f = Fixture::new("saved");
    c::save_profile(&profile()).unwrap();
    c::save_profile(&with(&profile(), json!({"mode": "HDR Peak 1000", "peak": 1100}))).unwrap();
    c::save_profile(&with(&profile(), json!({"white": 140}))).unwrap();
    let profiles = c::read_profiles().unwrap();
    assert_eq!(profiles.len(), 2);
    let found: BTreeMap<String, Value> = profiles.iter().map(|p| (p["mode"].as_str().unwrap().to_owned(), p["white"].clone())).collect();
    assert_eq!(found, BTreeMap::from([("HDR True Black".into(), json!(140.0)), ("HDR Peak 1000".into(), json!(160.0))]));
    assert_eq!(std::fs::metadata(c::profiles_path()).unwrap().permissions().mode() & 0o777, 0o600);
    let text = std::fs::read_to_string(c::profiles_path()).unwrap();
    assert!(text.starts_with("{\n  \"version\": 1,\n  \"profiles\": [\n    {\n      \"version\": 1,"));
    assert!(text.ends_with("}\n"));
}

#[test]
fn malformed_config_does_not_write() {
    let f = Fixture::new("malformed");
    for text in ["<labwc_config>".to_owned(), "<invalid/>".to_owned(), format!("<labwc_config>{}</labwc_config>", c::BEGIN)] {
        f.write(&text);
        assert!(matches!(c::write_config(&f.path, "DP-1", Some(&profile())), Err(CalError::Value(_))));
        assert_eq!(f.read(), text);
    }
}

#[test]
fn failed_apply_restores_config() {
    let f = Fixture::new("failed");
    f.write("<labwc_config/>");
    let host = FakeHost::new(vec![Ok(status(&[("pid", "1234")], monitor())), Err(CalError::Value("Monitor disconnected".into()))], &f.path);
    let error = c::apply("DP-1", Some(&profile()), &host).unwrap_err();
    assert!(error.to_string().contains("disconnected"));
    assert_eq!(host.signals(), vec![(1234, libc::SIGHUP), (1234, libc::SIGHUP)]);
    assert_eq!(f.read(), "<labwc_config/>");
}

#[test]
fn success_requires_committed_values() {
    let f = Fixture::new("success");
    f.write("<labwc_config/>");
    let host = FakeHost::new(vec![Ok(status(&[("pid", "1234")], monitor())), Ok(status(&[("pid", "1234")], active_monitor()))], &f.path);
    c::apply("DP-1", Some(&profile()), &host).unwrap();
    assert_eq!(host.signals(), vec![(1234, libc::SIGHUP)]);
    assert_eq!(c::read_profiles().unwrap().len(), 1);
    let mut changed = active_monitor();
    changed.insert("hdr_reference_white_nits".into(), "203".into());
    assert!(!c::matches(&changed, Some(&profile())).unwrap());
}

#[test]
fn apply_times_out_and_restores() {
    let f = Fixture::new("timeout");
    f.write("<labwc_config/>");
    // Status never commits: the 8 s deadline uses real time, so keep it short by
    // making the host's sleep advance quickly.
    struct Slow(FakeHost);
    impl Host for Slow {
        fn read_status(&self) -> Status { self.0.read_status() }
        fn session(&self) -> Result<Status, CalError> { self.0.session() }
        fn signal(&self, pid: i32, signal: i32) -> std::io::Result<()> { self.0.signal(pid, signal) }
        fn cmdline(&self, pid: i64) -> std::io::Result<Vec<u8>> { self.0.cmdline(pid) }
        fn cwd(&self, pid: i64) -> PathBuf { self.0.cwd(pid) }
        fn sleep(&self, _: Duration) { std::thread::sleep(Duration::from_millis(400)) }
    }
    let host = Slow(FakeHost::always(status(&[("pid", "1234")], monitor()), &f.path));
    let error = c::apply("DP-1", Some(&profile()), &host).unwrap_err();
    assert!(error.to_string().contains("previous configuration has been restored"));
    assert_eq!(f.read(), "<labwc_config/>");
    assert_eq!(host.0.signals().len(), 2);
}

// ------------------------------------------------------- test_sdr_workspace.py

fn cube(identity: &str, mode: &str) -> Vec<u8> {
    let mut text = format!("# labwc-hdr-pq-correction-v1\n# display-id: {identity}\n# picture-mode: {mode}\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
    for i in 0..8 {
        text.push_str(&format!("{} {} {}\n", i & 1, (i >> 1) & 1, (i >> 2) & 1));
    }
    text.into_bytes()
}

fn default_cube() -> Vec<u8> {
    cube("0123456789abcdef", "HDR True Black")
}

fn replace(data: &[u8], from: &str, to: &str) -> Vec<u8> {
    String::from_utf8(data.to_vec()).unwrap().replacen(from, to, 1).into_bytes()
}

#[test]
fn import_and_strict_rejection() {
    let f = Fixture::new("import");
    let file = f.root.join("test.cube");
    std::fs::write(&file, default_cube()).unwrap();
    let result = correction::import_cube(&file, "0123456789abcdef", "HDR True Black").unwrap();
    assert_eq!(std::fs::read(result["path"].as_str().unwrap()).unwrap(), default_cube());
    assert_eq!(std::fs::metadata(result["path"].as_str().unwrap()).unwrap().permissions().mode() & 0o777, 0o400);
    assert_eq!(result["sha256"].as_str().unwrap(), correction::sha256_hex(&default_cube()));
    assert_eq!(result.keys().map(String::as_str).collect::<Vec<_>>(), ["path", "sha256", "encoding", "size"]);
    let base = default_cube();
    let mut extra = base.clone();
    extra.extend_from_slice(b"0 0 0\n");
    let mutations = [replace(&base, "1 1 1\n", "nan 1 1\n"), extra, replace(&base, "LUT_3D_SIZE 2", "LUT_3D_SIZE 66"),
                     replace(&base, "0 0 0\n", "0 0 0 0\n"), replace(&base, "labwc-hdr-pq-correction-v1", "creative-look"),
                     replace(&base, "0123456789abcdef", "1123456789abcdef"), replace(&base, "HDR True Black", "SDR"),
                     replace(&base, "DOMAIN_MAX 1 1 1", "DOMAIN_MAX 0.8 1 1"), replace(&base, "LUT_3D_SIZE 2", "LUT_3D_SIZE 2\nLUT_3D_SIZE 2")];
    for data in mutations {
        std::fs::write(&file, &data).unwrap();
        assert!(correction::read_cube(&file, "0123456789abcdef", "HDR True Black").is_err(), "{}", String::from_utf8_lossy(&data));
    }
    // A FIFO is rejected without blocking.
    let fifo = f.root.join("pipe.cube");
    let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(correction::read_cube(&fifo, "0123456789abcdef", "HDR True Black").is_err());
}

#[test]
fn separate_presets_same_picture_mode() {
    let _f = Fixture::new("presets");
    for (preset, white) in [("sdr-video", 100), ("sdr-photo", 120)] {
        c::save_profile(&with(&profile(), json!({"preset": preset, "white": white}))).unwrap();
    }
    assert_eq!(c::read_profiles().unwrap().len(), 2);
    assert!(c::validate(&with(&profile(), json!({"preset": "sdr-video", "white": 120}))).is_err());
}

#[test]
fn correction_commit_required_and_reset() {
    let f = Fixture::new("commit");
    let file = f.root.join("test.cube");
    std::fs::write(&file, default_cube()).unwrap();
    let result = correction::import_cube(&file, "0123456789abcdef", "HDR True Black").unwrap();
    let mut corrected = profile();
    corrected.insert("correction".into(), Value::Object(result.clone()));
    let mut active = active_monitor();
    assert!(!c::matches(&active, Some(&corrected)).unwrap());
    active.insert("hdr_correction_active".into(), "true".into());
    active.insert("hdr_correction_path".into(), result["path"].as_str().unwrap().into());
    active.insert("hdr_correction_sha256".into(), result["sha256"].as_str().unwrap().into());
    assert!(c::matches(&active, Some(&corrected)).unwrap());
    let mut pending = active.clone();
    pending.insert("hdr_correction_pending".into(), "true".into());
    assert!(!c::matches(&pending, Some(&corrected)).unwrap());
    let mut failed = active.clone();
    failed.insert("hdr_correction_error".into(), "bad".into());
    assert!(!c::matches(&failed, Some(&corrected)).unwrap());
    assert!(!c::matches(&active, Some(&profile())).unwrap());
    let path = f.root.join("rc.xml");
    c::write_config(&path, "DP-1", Some(&corrected)).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains(result["sha256"].as_str().unwrap()));
    c::write_config(&path, "DP-1", Some(&profile())).unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("correction="));
    // Reset state: calibration inactive and white back at 203.
    let reset = section(&[("hdr_calibration_active", "false"), ("hdr_reference_white_nits", "203")]);
    assert!(c::matches(&reset, None).unwrap());
}

#[test]
fn mutated_import_cannot_apply() {
    let f = Fixture::new("mutated");
    let file = f.root.join("test.cube");
    std::fs::write(&file, default_cube()).unwrap();
    let result = correction::import_cube(&file, "0123456789abcdef", "HDR True Black").unwrap();
    let stored = PathBuf::from(result["path"].as_str().unwrap());
    std::fs::set_permissions(&stored, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut changed = default_cube();
    changed.extend_from_slice(b"# changed\n");
    std::fs::write(&stored, changed).unwrap();
    let mut corrected = profile();
    corrected.insert("correction".into(), Value::Object(result));
    let host = FakeHost::always(status(&[("hdr_correction_supported", "true")], monitor()), &f.path);
    let error = c::apply("DP-1", Some(&corrected), &host).unwrap_err();
    assert!(error.to_string().contains("changed"));
    assert!(!f.path.exists(), "write_config must not run");
    assert!(host.signals().is_empty());
}

#[test]
fn report_records_pending_measurements_and_mismatch() {
    let f = Fixture::new("report");
    let host = FakeHost::always(status(&[("hdr_correction_supported", "true")], monitor()), &f.path);
    let path = c::workspace_report("DP-1", &profile(), &host).unwrap();
    assert!(path.starts_with(f.root.join("state/hdr-calibration/reports")));
    let report: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(report["software_configuration_matches"], json!(false));
    assert_eq!(report["meter_verification"], json!("not performed"));
    assert!(report["measured_color_error"].is_null());
    assert_eq!(report["physical_color_accuracy"], json!("unverified"));
    assert!(report["correction_file_matches"].is_null());
}

#[test]
fn session_requires_updated_compositor() {
    struct Old;
    impl Host for Old {
        fn read_status(&self) -> Status { status(&[("pid", "1")], monitor()) }
        fn signal(&self, _: i32, _: i32) -> std::io::Result<()> { panic!("never signals") }
        fn cmdline(&self, _: i64) -> std::io::Result<Vec<u8>> { Ok(Vec::new()) }
        fn cwd(&self, _: i64) -> PathBuf { PathBuf::new() }
    }
    assert_eq!(Old.session().unwrap_err(), CalError::Value("Start a session with the updated labwc compositor to use HDR calibration.".into()));
    assert_eq!(c::g9(160.0), "160");
    assert_eq!(c::g9(1e-5), "1e-05");
    assert_eq!(c::g9(1234567890.5), "1.23456789e+09");
}

#[test]
fn argparse_texts_match_python() {
    use crate::args::*;
    assert_eq!(help("hdr_calibration.py"), "usage: hdr_calibration.py [-h] [--output OUTPUT] [--sdr-editing]\n                          [--smoke-test]\n\nDesktop HDR calibration: visual setup and recorded meter readings.\n\noptions:\n  -h, --help       show this help message and exit\n  --output OUTPUT\n  --sdr-editing\n  --smoke-test\n");
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(parse(&args(&["--output"])), Outcome::Error("argument --output: expected one argument".into()));
    assert_eq!(parse(&args(&["--bogus"])), Outcome::Error("unrecognized arguments: --bogus".into()));
    assert_eq!(parse(&args(&["--out=DP-1", "--sdr"])), Outcome::Run(Args { output: Some("DP-1".into()), sdr_editing: true, smoke_test: false }));
}
