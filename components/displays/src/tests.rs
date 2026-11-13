//! Ports of test_primary_xinitrc.py and test_color_config.py, plus the
//! pure parts of test_brightness_integration.py (save_config CFG_SET /
//! CFG_DEL decisions) and the environment / HDR / IPC helpers.

use crate::color_config::{self as color, ColorSettings};
use crate::daemon::config::VrrMode;
use crate::model::{self, DesiredState, LayoutModel};
use crate::compat::fsx;
use crate::{ipc, labwc, primary};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let base = std::env::temp_dir().join(format!("wd-unit-{}", std::process::id()));
    let dir = base.join(format!("{name}-{}", COUNTER.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // write_colors backs up to $XDG_STATE_HOME: one shared private value.
    static STATE: std::sync::Once = std::sync::Once::new();
    STATE.call_once(|| std::env::set_var("XDG_STATE_HOME", base.join("state")));
    dir
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------- xinitrc

struct Startup {
    root: PathBuf,
    path: PathBuf,
}

impl Startup {
    fn new() -> Self {
        let root = scratch("xinitrc");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let xrandr = bin.join("xrandr");
        std::fs::write(&xrandr, "#!/bin/sh\nprintf \"%s\\n\" \"$@\" >> \"$XRANDR_TEST_LOG\"\n").unwrap();
        std::fs::set_permissions(&xrandr, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { path: root.join("labwc/xinitrc"), root }
    }

    fn write(&self, output: Option<&str>) -> primary::WriteResult {
        let result = primary::write_labwc_primary_xinitrc(&self.path, output);
        assert!(result.0, "{:?}", result.1);
        result
    }

    fn execute(&self) -> Vec<String> {
        let log = self.root.join("calls");
        std::fs::write(&log, "").unwrap();
        assert!(std::process::Command::new("sh").arg("-n").arg(&self.path).status().unwrap().success());
        let path = format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        assert!(std::process::Command::new("sh").arg(&self.path).env("PATH", path).env("XRANDR_TEST_LOG", &log)
            .status().unwrap().success());
        std::fs::read_to_string(&log).unwrap().lines().map(String::from).collect()
    }
}

#[test]
fn xinitrc_create_change_and_repeat_save() {
    let s = Startup::new();
    s.write(Some("HDMI-A-1"));
    assert_eq!(s.execute(), ["--output", "HDMI-A-1", "--primary"]);
    assert_eq!(mode(&s.path), 0o700);
    s.write(Some("DP-2"));
    let before = std::fs::metadata(&s.path).unwrap().modified().unwrap();
    assert_eq!(s.execute(), ["--output", "DP-2", "--primary"]);
    assert_eq!(s.write(Some("DP-2")), (true, None, false));
    assert_eq!(std::fs::metadata(&s.path).unwrap().modified().unwrap(), before);
}

#[test]
fn xinitrc_adopts_existing_command_and_preserves_other_commands() {
    let s = Startup::new();
    std::fs::create_dir_all(s.path.parent().unwrap()).unwrap();
    let before = "#!/bin/sh\n# Keep this comment\ntrue\n";
    let after = "xrandr --dpi 144\n# xrandr --output ignored --primary\ntrue\n";
    std::fs::write(&s.path, format!("{before}xrandr --output \"old-output\" --primary\n{after}")).unwrap();
    std::fs::set_permissions(&s.path, std::fs::Permissions::from_mode(0o750)).unwrap();
    s.write(Some("DP-2"));
    let text = std::fs::read_to_string(&s.path).unwrap();
    assert!(text.starts_with(before));
    assert!(text.ends_with(after));
    assert!(!text.contains("old-output"));
    assert_eq!(s.execute(), ["--output", "DP-2", "--primary", "--dpi", "144"]);
    assert_eq!(mode(&s.path), 0o750);
}

#[test]
fn xinitrc_removes_duplicate_primary_commands_and_clears_selection() {
    let s = Startup::new();
    s.write(Some("DP-1"));
    let mut text = std::fs::read_to_string(&s.path).unwrap();
    text.push_str("xrandr --output HDMI-A-1 --primary >/dev/null 2>&1\n");
    text.push_str(&(primary::compose_primary_callback("", Some("DP-3")) + "\ntrue\n"));
    std::fs::write(&s.path, text).unwrap();
    s.write(Some("DP-2"));
    assert_eq!(s.execute(), ["--output", "DP-2", "--primary"]);
    s.write(None);
    assert_eq!(s.execute(), Vec::<String>::new());
    assert!(std::fs::read_to_string(&s.path).unwrap().ends_with("true\n"));
}

#[test]
fn xinitrc_empty_selection_does_not_create_file() {
    let s = Startup::new();
    assert_eq!(s.write(None), (true, None, false));
    assert!(!s.path.exists());
}

#[test]
fn xinitrc_connector_is_a_literal_shell_argument() {
    let s = Startup::new();
    let output = format!("DP-1 $(touch {})", s.root.join("unexpected").display());
    s.write(Some(&output));
    assert_eq!(s.execute(), ["--output".to_owned(), output, "--primary".to_owned()]);
    assert!(!s.root.join("unexpected").exists());
}

#[test]
fn xinitrc_failed_atomic_replace_preserves_original() {
    let s = Startup::new();
    s.write(Some("DP-1"));
    let before = std::fs::read(&s.path).unwrap();
    let fail = |_: &Path, _: &Path| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read only"));
    let result = primary::write_labwc_primary_xinitrc_with(&s.path, Some("DP-2"), &fail);
    assert_eq!(result, (false, Some("read only".into()), false));
    assert_eq!(std::fs::read(&s.path).unwrap(), before);
    let temps = std::fs::read_dir(s.path.parent().unwrap()).unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".xinitrc.")).count();
    assert_eq!(temps, 0);
}

// ------------------------------------------------------- environment block

#[test]
fn environment_block_add_update_remove() {
    let dir = scratch("env");
    let path = dir.join("labwc/environment");
    assert_eq!(primary::write_labwc_primary_environment(&path, None), (true, None, false));
    assert!(!path.exists());
    assert_eq!(primary::write_labwc_primary_environment(&path, Some("DP-1")), (true, None, true));
    assert_eq!(mode(&path), 0o600);
    let block = primary::compose_labwc_primary_environment(Some("DP-1"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{block}\n"));
    std::fs::write(&path, "XKB_DEFAULT_LAYOUT=us").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    primary::write_labwc_primary_environment(&path, Some("HDMI-A-1\n"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "XKB_DEFAULT_LAYOUT=us\n\n# tessel-displays primary output environment begin\nSDL_VIDEO_DISPLAY_PRIORITY=HDMI-A-1\nWAYLANDDRV_PRIMARY_MONITOR=HDMI-A-1\nPROTON_WAYLAND_MONITOR=HDMI-A-1\n# tessel-displays primary output environment end\n");
    assert_eq!(mode(&path), 0o640);
    assert_eq!(primary::read_labwc_primary_environment(&path).as_deref(), Some("HDMI-A-1"));
    assert_eq!(primary::write_labwc_primary_environment(&path, Some("HDMI-A-1")), (true, None, false));
    std::fs::write(&path, format!("{text}OTHER=1\n")).unwrap();
    primary::write_labwc_primary_environment(&path, Some("DP-2"));
    assert!(std::fs::read_to_string(&path).unwrap().ends_with("PROTON_WAYLAND_MONITOR=DP-2\n# tessel-displays primary output environment end\nOTHER=1\n"));
    primary::write_labwc_primary_environment(&path, None);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "XKB_DEFAULT_LAYOUT=us\n\nOTHER=1\n");
}

#[test]
fn blocks_written_before_the_tessel_rename_are_still_managed() {
    let dir = scratch("legacy-markers");
    let path = dir.join("environment");
    std::fs::write(&path, "A=1\n# way-displays-gui primary output environment begin\nPROTON_WAYLAND_MONITOR=DP-1\n# way-displays-gui primary output environment end\n").unwrap();
    assert_eq!(primary::read_labwc_primary_environment(&path).as_deref(), Some("DP-1"));
    primary::write_labwc_primary_environment(&path, Some("DP-2"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("way-displays-gui") && text.matches("PROTON_WAYLAND_MONITOR=").count() == 1, "{text}");
    let rc = dir.join("rc.xml");
    std::fs::write(&rc, "<labwc_config>\n  <!-- way-displays-gui HDR overrides begin -->\n  <hdrOutputs>\n    <output name=\"DP-1\" enabled=\"no\" />\n  </hdrOutputs>\n  <!-- way-displays-gui HDR overrides end -->\n</labwc_config>\n").unwrap();
    assert_eq!(labwc::read_labwc_hdr_config(&rc).managed.get("DP-1"), Some(&false));
}

#[test]
fn environment_parse_prefers_first_nonempty_variable() {
    let text = "A=1\n# tessel-displays primary output environment begin\r\nSDL_VIDEO_DISPLAY_PRIORITY=\nPROTON_WAYLAND_MONITOR= DP-3 \n# tessel-displays primary output environment end  \n";
    assert_eq!(primary::parse_labwc_primary_environment(text).as_deref(), Some("DP-3"));
    assert_eq!(primary::parse_labwc_primary_environment("nothing"), None);
}

#[test]
fn callback_split_and_compose_round_trip() {
    let composed = primary::compose_primary_callback(" notify-send x ", Some("DP-1 'a'"));
    assert_eq!(composed, "# tessel-displays primary output begin\n# output: DP-1 'a'\nif command -v xrandr >/dev/null 2>&1; then\n  xrandr --output 'DP-1 '\"'\"'a'\"'\"'' --primary >/dev/null 2>&1 || true\nfi\n# tessel-displays primary output end\nnotify-send x");
    assert_eq!(primary::split_primary_callback(&composed), ("notify-send x".to_owned(), Some("DP-1 'a'".to_owned())));
    assert_eq!(primary::split_primary_callback("echo hi"), ("echo hi".to_owned(), None));
    assert_eq!(primary::compose_primary_callback("", None), "");
}

// ---------------------------------------------------------------- HDR

#[test]
fn hdr_overrides_read_and_write_only_managed_block() {
    let dir = scratch("hdr");
    let path = dir.join("rc.xml");
    let before = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><hdr>yes</hdr></core>\n  <hdrOutputs><output name=\"DP-9\" enabled=\"no\"/></hdrOutputs>\n</labwc_config>\n";
    std::fs::write(&path, before).unwrap();
    let config = labwc::read_labwc_hdr_config(&path);
    assert!(config.global);
    assert_eq!(config.overrides, BTreeMap::from([("DP-9".to_owned(), false)]));
    assert!(config.managed.is_empty());
    let overrides = BTreeMap::from([("HDMI-A-1".to_owned(), true), ("DP-1 & \"x\"".to_owned(), false)]);
    assert_eq!(labwc::write_labwc_hdr_overrides(&path, &overrides), (true, None));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><hdr>yes</hdr></core>\n  <hdrOutputs><output name=\"DP-9\" enabled=\"no\"/></hdrOutputs>\n\n  <!-- tessel-displays HDR overrides begin -->\n  <hdrOutputs>\n    <output name=\"DP-1 &amp; &quot;x&quot;\" enabled=\"no\" />\n    <output name=\"HDMI-A-1\" enabled=\"yes\" />\n  </hdrOutputs>\n  <!-- tessel-displays HDR overrides end -->\n</labwc_config>\n");
    let config = labwc::read_labwc_hdr_config(&path);
    assert_eq!(config.managed.get("HDMI-A-1"), Some(&true));
    assert_eq!(config.overrides.get("DP-1 & \"x\""), Some(&false));
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(labwc::write_labwc_hdr_overrides(&path, &overrides), (true, None));
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
    let single = BTreeMap::from([("DP-1".to_owned(), true)]);
    labwc::write_labwc_hdr_overrides(&path, &single);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches(labwc::LABWC_HDR_BEGIN).count(), 1);
    assert!(!text.contains("HDMI-A-1"));
    std::fs::write(&path, "<other/>").unwrap();
    assert_eq!(labwc::write_labwc_hdr_overrides(&path, &single), (false, Some("rc.xml has no closing labwc_config element".into())));
    let missing = dir.join("new/rc.xml");
    assert_eq!(labwc::write_labwc_hdr_overrides(&missing, &single), (true, None));
    assert_eq!(mode(&missing), 0o600);
    assert!(labwc::parse_labwc_bool(Some(" ON "), false));
    assert!(!labwc::parse_labwc_bool(Some("maybe"), false));
    assert_eq!(labwc::parse_labwc_hdr_xml("<labwc_config><hdrOutputs><output><name>X</name><enabled>yes</enabled></output></hdrOutputs></labwc_config>").1.get("X"), Some(&true));
}

// ------------------------------------------------------- adaptive sync

fn vrr_modes(items: &[(&str, VrrMode)]) -> BTreeMap<String, VrrMode> {
    items.iter().map(|(name, mode)| (name.to_string(), *mode)).collect()
}

#[test]
fn adaptive_sync_block_is_inserted_replaced_and_left_alone() {
    let dir = scratch("vrr");
    let path = dir.join("rc.xml");
    let before = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <!-- my own comment -->\n  <core><adaptiveSync>yes</adaptiveSync></core>\n  <!-- tessel-displays HDR overrides begin -->\n  <hdrOutputs>\n    <output name=\"DP-2\" enabled=\"yes\" />\n  </hdrOutputs>\n  <!-- tessel-displays HDR overrides end -->\n</labwc_config>\n";
    std::fs::write(&path, before).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let calls = std::cell::Cell::new(0);
    let reconfigure = || { calls.set(calls.get() + 1); (true, None) };
    let modes = vrr_modes(&[("HDMI-A-1", VrrMode::Off), ("DP-2", VrrMode::Auto), ("DP-3", VrrMode::Always)]);
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &reconfigure), Ok(labwc::BlockWrite::Written));
    assert_eq!(calls.get(), 1, "labwc reads the new block");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, format!("{}\n\n  <!-- tessel-displays adaptive sync begin -->\n  <adaptiveSyncOutputs>\n    <output name=\"DP-2\" mode=\"fullscreen\" />\n    <output name=\"DP-3\" mode=\"yes\" />\n    <output name=\"HDMI-A-1\" mode=\"no\" />\n  </adaptiveSyncOutputs>\n  <!-- tessel-displays adaptive sync end -->\n</labwc_config>\n",
                             before.trim_end().trim_end_matches("</labwc_config>").trim_end()));
    assert_eq!(mode(&path), 0o640);
    assert_eq!(labwc::parse_adaptive_sync_block(&text).get("DP-3").map(String::as_str), Some("yes"));
    assert_eq!(labwc::read_labwc_hdr_config(&path).managed.get("DP-2"), Some(&true), "the HDR block is kept");
    // The same choices: nothing written, labwc not asked.
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &reconfigure), Ok(labwc::BlockWrite::Unchanged));
    assert_eq!((calls.get(), std::fs::metadata(&path).unwrap().modified().unwrap()), (1, modified));
    // A change replaces only that block, once.
    let changed = vrr_modes(&[("DP-2", VrrMode::Always)]);
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &changed, &fsx::os_replace, &reconfigure), Ok(labwc::BlockWrite::Written));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches(labwc::LABWC_VRR_BEGIN).count(), 1);
    assert!(text.contains("<output name=\"DP-2\" mode=\"yes\" />") && !text.contains("HDMI-A-1"), "{text}");
    assert!(text.contains("<!-- my own comment -->") && text.contains("<core><adaptiveSync>yes</adaptiveSync></core>"));
    // The HDR writer keeps the adaptive sync block.
    labwc::write_labwc_hdr_overrides(&path, &BTreeMap::from([("DP-2".to_owned(), false)]));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(labwc::parse_adaptive_sync_block(&text).get("DP-2").map(String::as_str), Some("yes"));
    assert!(roxmltree::Document::parse(&text).is_ok(), "{text}");
    // A refused reload is reported.
    let refuse = || (false, Some("no labwc".to_owned()));
    let err = labwc::sync_adaptive_sync_with(&path, &vrr_modes(&[("DP-2", VrrMode::Off)]), &fsx::os_replace, &refuse);
    assert_eq!(err, Err("labwc did not reload rc.xml: no labwc".into()));
    // The lock file sits next to rc.xml; no temporary file is left.
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"rc.xml.tessel-lock".to_owned()));
}

#[test]
fn adaptive_sync_never_creates_an_rc_xml() {
    let dir = scratch("vrr-missing");
    let never = || -> (bool, Option<String>) { panic!("labwc must not be asked") };
    let modes = vrr_modes(&[("DP-1", VrrMode::Auto)]);
    for path in [dir.join("rc.xml"), dir.join("labwc/rc.xml")] {
        assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &never), Ok(labwc::BlockWrite::NoFile));
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "not even a lock file");
}

#[test]
fn adaptive_sync_opens_an_empty_root_and_upgrades_legacy_hdr_markers() {
    let dir = scratch("vrr-legacy");
    let path = dir.join("rc.xml");
    let ok = || (true, None);
    let modes = vrr_modes(&[("DP-1", VrrMode::Off)]);
    std::fs::write(&path, "<?xml version=\"1.0\"?>\n<labwc_config />\n").unwrap();
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &ok), Ok(labwc::BlockWrite::Written));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "<?xml version=\"1.0\"?>\n<labwc_config>\n\n  <!-- tessel-displays adaptive sync begin -->\n  <adaptiveSyncOutputs>\n    <output name=\"DP-1\" mode=\"no\" />\n  </adaptiveSyncOutputs>\n  <!-- tessel-displays adaptive sync end -->\n</labwc_config>\n");
    std::fs::write(&path, "<labwc_config>\n  <!-- way-displays-gui HDR overrides begin -->\n  <hdrOutputs>\n    <output name=\"DP-1\" enabled=\"no\" />\n  </hdrOutputs>\n  <!-- way-displays-gui HDR overrides end -->\n</labwc_config>\n").unwrap();
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &ok), Ok(labwc::BlockWrite::Written));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("way-displays-gui") && text.contains(labwc::LABWC_HDR_BEGIN) && text.contains(labwc::LABWC_VRR_END), "{text}");
    assert_eq!(labwc::read_labwc_hdr_config(&path).managed.get("DP-1"), Some(&false));
    std::fs::write(&path, "<other/>").unwrap();
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &ok), Err("rc.xml has no closing labwc_config element".into()));
    // A failed replace leaves rc.xml as it was and asks labwc nothing.
    std::fs::write(&path, "<labwc_config>\n</labwc_config>\n").unwrap();
    let fail = |_: &Path, _: &Path| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read only"));
    let never = || -> (bool, Option<String>) { panic!("labwc must not be asked") };
    assert_eq!(labwc::sync_adaptive_sync_with(&path, &modes, &fail, &never), Err("read only".into()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "<labwc_config>\n</labwc_config>\n");
}

#[test]
fn rc_xml_writers_wait_for_each_other() {
    let dir = scratch("vrr-lock");
    let path = dir.join("rc.xml");
    std::fs::write(&path, "<labwc_config>\n</labwc_config>\n").unwrap();
    let held = labwc::lock_rc(&path).unwrap();
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            let ok = || (true, None);
            labwc::sync_adaptive_sync_with(&path, &vrr_modes(&[("DP-1", VrrMode::Always)]), &fsx::os_replace, &ok)
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(!writer.is_finished(), "the writer waits for the lock");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "<labwc_config>\n</labwc_config>\n");
    // Another writer's change made while the lock was held is read, not lost.
    std::fs::write(&path, "<labwc_config>\n  <core/>\n</labwc_config>\n").unwrap();
    drop(held);
    assert_eq!(writer.join().unwrap(), Ok(labwc::BlockWrite::Written));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("<core/>") && text.contains("mode=\"yes\""), "{text}");
}

// ---------------------------------------------------------- colour config

fn depth(value: i64) -> ColorSettings {
    ColorSettings { sdr_bit_depth: value, ..Default::default() }
}

fn changes(items: &[(&str, ColorSettings)]) -> BTreeMap<String, ColorSettings> {
    items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn color_read_is_side_effect_free_and_last_monitor_override_wins() {
    let path = scratch("color").join("rc.xml");
    let text = "<labwc_config><colorManagement><output name=\"DP-1\" sdrBitDepth=\"8\"/></colorManagement><colorManagement><output name=\"DP-1\" sdrBitDepth=\"10\"/></colorManagement></labwc_config>";
    std::fs::write(&path, text).unwrap();
    assert_eq!(color::read_colors(&path).unwrap()["DP-1"].sdr_bit_depth, 10);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn color_apply_preserves_comments_hdr_and_unrelated_xml_then_rolls_back() {
    let path = scratch("color").join("rc.xml");
    let before = "<labwc_config>\n<!-- keep my comment -->\n<core><hdr>yes</hdr></core><keyboard/><colorManagement><output name=\"DP-1\" sdrBitDepth=\"8\"/></colorManagement>\n</labwc_config>\n";
    std::fs::write(&path, before).unwrap();
    let tx = color::write_colors(&path, &changes(&[("DP-1", depth(10))])).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("<!-- keep my comment -->"));
    assert!(text.contains("<core><hdr>yes</hdr></core><keyboard/>"));
    assert_eq!(color::read_colors(&path).unwrap()["DP-1"], depth(10));
    assert_eq!(std::fs::read_to_string(&tx.backup).unwrap(), before);
    assert_eq!(text, format!("{}{}\n<colorManagement>\n  <output name=\"DP-1\" profile=\"\" calibration=\"true\" sdrBitDepth=\"10\" />\n</colorManagement>\n{}\n</labwc_config>\n",
                             &before[..before.len() - "</labwc_config>\n".len()], color::BEGIN, color::END));
    color::rollback(&tx).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn color_second_editor_merges_other_monitors_and_escapes_names() {
    let path = scratch("color").join("rc.xml");
    std::fs::write(&path, "<labwc_config/>\n").unwrap();
    color::write_colors(&path, &changes(&[("DP-1 & \"A\"", depth(10))])).unwrap();
    color::write_colors(&path, &changes(&[("HDMI-A-1", ColorSettings { calibration: false, ..Default::default() })])).unwrap();
    let saved = color::read_colors(&path).unwrap();
    assert_eq!(saved["DP-1 & \"A\""].sdr_bit_depth, 10);
    assert!(!saved["HDMI-A-1"].calibration);
    assert_eq!(std::fs::read_to_string(&path).unwrap().matches(color::BEGIN).count(), 1);
}

#[test]
fn color_malformed_xml_and_missing_profiles_do_not_write() {
    let root = scratch("color");
    let path = root.join("rc.xml");
    for text in ["<labwc_config>".to_owned(), "<another/>".to_owned(), format!("<labwc_config>{}</labwc_config>", color::BEGIN)] {
        std::fs::write(&path, &text).unwrap();
        assert!(matches!(color::write_colors(&path, &changes(&[("DP-1", ColorSettings::default())])), Err(color::ColorError::Value(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    std::fs::write(&path, "<labwc_config/>").unwrap();
    let missing = ColorSettings { profile: root.join("missing.icc").to_string_lossy().into_owned(), ..Default::default() };
    assert!(matches!(color::write_colors(&path, &changes(&[("DP-1", missing)])), Err(color::ColorError::Os(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "<labwc_config/>");
}

#[test]
fn color_rollback_refuses_to_clobber_a_later_edit() {
    let path = scratch("color").join("rc.xml");
    let tx = color::write_colors(&path, &changes(&[("DP-1", ColorSettings::default())])).unwrap();
    assert!(!tx.existed);
    std::fs::write(&path, "<labwc_config><keyboard/></labwc_config>").unwrap();
    assert!(matches!(color::rollback(&tx), Err(color::ColorError::Runtime(_))));
    assert!(std::fs::read_to_string(&path).unwrap().contains("<keyboard/>"));
}

#[test]
fn color_validate_real_profile_and_reject_fifo_without_blocking() {
    let root = scratch("color");
    let path = root.join("sRGB & test.icc");
    unsafe {
        type Create = unsafe extern "C" fn() -> *mut libc::c_void;
        type Save = unsafe extern "C" fn(*mut libc::c_void, *const libc::c_char) -> libc::c_int;
        type Close = unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int;
        let create: Create = std::mem::transmute(color::lcms_symbol("cmsCreate_sRGBProfile").unwrap());
        let save: Save = std::mem::transmute(color::lcms_symbol("cmsSaveProfileToFile").unwrap());
        let close: Close = std::mem::transmute(color::lcms_symbol("cmsCloseProfile").unwrap());
        let profile = create();
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
        assert!(save(profile, c_path.as_ptr()) != 0);
        close(profile);
    }
    color::validate_profile(&path.to_string_lossy()).unwrap();
    let fifo = root.join("fifo");
    let c_fifo = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(color::validate_profile(&fifo.to_string_lossy()), Err(color::ColorError::Value(_))));
    assert!(matches!(color::validate_profile("relative.icc"), Err(color::ColorError::Value(_))));
    let short = root.join("short.icc");
    std::fs::write(&short, [0u8; 64]).unwrap();
    assert_eq!(color::validate_profile(&short.to_string_lossy()),
               Err(color::ColorError::Value("This is not a valid ICC profile (maximum size: 16 MiB).".into())));
}

#[test]
fn color_stale_status_is_unavailable() {
    let root = scratch("status");
    let path = root.join("labwc-color-wayland-color-test.ini");
    std::fs::write(&path, "[compositor]\npid=999999999\nversion=1\nicc_supported=true\n").unwrap();
    assert_eq!(color::read_status_file(Some(&path)), None);
    let parsed = color::parse_ini("[compositor]\nPID = 5\n[DP-1]\nprofile=/a\\sb\nhdr: true\n").unwrap();
    assert_eq!(parsed["compositor"]["pid"], "5");
    assert_eq!(parsed["DP-1"]["hdr"], "true");
    assert!(color::parse_ini("key=1\n").is_err());
    assert!(color::parse_ini("[a]\nx=1\nx=2\n").is_err());
}

#[test]
fn color_child_syntax_and_trailing_comments_are_preserved() {
    let path = scratch("color").join("rc.xml");
    let before = "<labwc_config><colorManagement><output><name>DP-1</name><sdrBitDepth>10</sdrBitDepth><calibration>false</calibration></output></colorManagement></labwc_config><!-- trailing comment -->";
    std::fs::write(&path, before).unwrap();
    assert_eq!(color::read_colors(&path).unwrap()["DP-1"], ColorSettings { sdr_bit_depth: 10, calibration: false, profile: String::new() });
    color::write_colors(&path, &changes(&[("HDMI-A-1", ColorSettings::default())])).unwrap();
    assert_eq!(color::read_colors(&path).unwrap()["DP-1"].sdr_bit_depth, 10);
    assert!(std::fs::read_to_string(&path).unwrap().ends_with("<!-- trailing comment -->"));
}

#[test]
fn color_self_closing_root_is_expanded() {
    let path = scratch("color").join("rc.xml");
    std::fs::write(&path, "<labwc_config/>\n").unwrap();
    color::write_colors(&path, &changes(&[("DP-1", ColorSettings::default())])).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), format!(
        "<labwc_config>\n{}\n<colorManagement>\n  <output name=\"DP-1\" profile=\"\" calibration=\"true\" sdrBitDepth=\"8\" />\n</colorManagement>\n{}\n</labwc_config>\n\n",
        color::BEGIN, color::END));
}

// ------------------------------------------------------------- HDR report

#[test]
fn hdr_report_distinguishes_metadata_white_target_and_measurement() {
    use crate::color_controls::{hdr_report, nits, status_messages};
    let mut active: BTreeMap<String, String> = [("hdr", "true"), ("enabled", "true"), ("hdr_supported", "true"),
        ("hdr_peak_nits", "426.9"), ("hdr_max_frame_average_nits", "265"), ("hdr_reference_white_nits", "203")]
        .iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let (visible, rows, _) = hdr_report(&active, false);
    let rows: BTreeMap<_, _> = rows.into_iter().collect();
    assert!(visible);
    assert_eq!(rows["Peak brightness"], "426.9 nits");
    assert_eq!(rows["Frame-average limit"], "265 nits");
    assert_eq!(rows["Reference white"], "203 nits");
    assert_eq!(rows["Output correction"], "None");
    active.insert("hdr_peak_nits".into(), "nan".into());
    active.insert("hdr_reference_white_nits".into(), "inf".into());
    let rows: BTreeMap<_, _> = hdr_report(&active, false).1.into_iter().collect();
    assert_eq!((rows["Peak brightness"].as_str(), rows["Reference white"].as_str()), ("Unavailable", "Unavailable"));
    active.insert("hdr".into(), "false".into());
    active.insert("hdr_reference_white_nits".into(), "203".into());
    let (visible, rows, _) = hdr_report(&active, false);
    assert!(!rows.iter().any(|(k, _)| k == "Reference white"));
    assert!(visible);
    active.insert("x".into(), "1234.5".into());
    assert_eq!(nits(&active, "x").as_deref(), Some("1,234.5"));
    let status: color::Status = BTreeMap::from([("compositor".to_owned(), BTreeMap::from([("icc_supported".to_owned(), "true".to_owned())])),
        ("DP-1".to_owned(), BTreeMap::from([("icc_active".to_owned(), "false".to_owned()), ("bit_depth".to_owned(), "8".to_owned()), ("hdr".to_owned(), "false".to_owned())]))]);
    let lines = status_messages(&status, "DP-1", "", false, "", true);
    assert_eq!(lines, ["Active color: built-in SDR", "Active output depth: 8-bit", "Color changes are pending until Apply."]);
    assert!(status_messages(&BTreeMap::new(), "DP-1", "", false, "", false)[0].contains("updated labwc"));
    // A renderer without color transforms (GLES2): HDR is named too.
    assert!(color::renderer_supports_color(&status) && color::renderer_supports_color(&BTreeMap::new()));
    let mut gles2 = status.clone();
    gles2.get_mut("compositor").unwrap().insert("icc_supported".into(), "false".into());
    assert!(!color::renderer_supports_color(&gles2));
    assert!(status_messages(&gles2, "DP-1", "", false, "", false)[0].starts_with("HDR and ICC profiles need the Vulkan renderer"));
}

// ------------------------------------------------------- save_config plan

fn state(desc: &str, name: &str) -> DesiredState {
    DesiredState { desc: desc.into(), name: Some(name.into()), width: json!(1920), height: json!(1080), refresh: json!(60000),
                   enabled: true, enabled_changed: false, primary: false, vrr: VrrMode::Auto, vrr_changed: false, hdr: false,
                   hdr_changed: false, scale: 1.0, scale_auto: false, transform: Some("STANDARD".into()) }
}

/// A fake daemon: CFG_SET adds VRR_OFF and VRR_ALWAYS members, CFG_DEL removes them.
#[derive(Default)]
struct VrrLists {
    off: Vec<String>,
    always: Vec<String>,
}

impl VrrLists {
    fn apply(&mut self, plan: &model::SavePlan) {
        for (key, list) in [("VRR_OFF", &mut self.off), ("VRR_ALWAYS", &mut self.always)] {
            if let Some(items) = plan.del_cfg.get(key).and_then(Value::as_array) {
                list.retain(|d| !items.iter().any(|i| i == d));
            }
            if let Some(items) = plan.cfg.get(key).and_then(Value::as_array) {
                for item in items {
                    let item = item.as_str().unwrap().to_owned();
                    if !list.contains(&item) {
                        list.push(item);
                    }
                }
            }
        }
    }

    /// What the daemon makes of the lists (`layout::vrr_mode`, exact names here).
    fn mode(&self, desc: &str) -> VrrMode {
        if self.off.iter().any(|d| d == desc) {
            VrrMode::Off
        } else if self.always.iter().any(|d| d == desc) {
            VrrMode::Always
        } else {
            VrrMode::Auto
        }
    }

    fn cfg(&self) -> Value {
        json!({"VRR_OFF": self.off, "VRR_ALWAYS": self.always})
    }
}

#[test]
fn every_adaptive_sync_change_saves_both_lists() {
    let (a, name) = ("Same model DP-1", "DP-1");
    let none = BTreeMap::new();
    for from in [VrrMode::Auto, VrrMode::Always, VrrMode::Off] {
        for to in [VrrMode::Auto, VrrMode::Always, VrrMode::Off] {
            let mut lists = VrrLists::default();
            match from {
                VrrMode::Off => lists.off = vec![a.into(), name.into()],
                VrrMode::Always => lists.always = vec![a.into(), name.into()],
                VrrMode::Auto => {}
            }
            let mut screen = state(a, name);
            screen.vrr = to;
            screen.vrr_changed = from != to;
            let plan = model::save_plan(Some(&lists.cfg()), &[Some(screen)], "", None, &none, &[]);
            lists.apply(&plan);
            assert_eq!(lists.mode(a), to, "{from:?} -> {to:?}: {plan:?}");
            assert!(!lists.off.iter().chain(&lists.always).any(|d| d == name) || from == to,
                    "{from:?} -> {to:?}: the connector-name entry is removed too");
            let expected = |mode: VrrMode| if to == mode && from != to { Some(json!([a])) } else { None };
            assert_eq!(plan.cfg.get("VRR_OFF").cloned(), expected(VrrMode::Off), "{from:?} -> {to:?}");
            assert_eq!(plan.cfg.get("VRR_ALWAYS").cloned(), expected(VrrMode::Always), "{from:?} -> {to:?}");
            if from == to {
                assert!(plan.del_cfg.get("VRR_OFF").is_none() && plan.del_cfg.get("VRR_ALWAYS").is_none());
            }
        }
    }
    let mut screen = state(a, name);
    (screen.vrr, screen.vrr_changed) = (VrrMode::Auto, true);
    let plan = model::save_plan(None, &[Some(screen)], "", None, &none, &[]);
    assert_eq!(plan.del_cfg["VRR_OFF"], json!([a, name]));
    assert_eq!(plan.del_cfg["VRR_ALWAYS"], json!([a, name]));
}

#[test]
fn vrr_changes_in_two_stale_editors_preserve_the_other_monitor() {
    let (a, b) = ("Same model DP-1", "Same model HDMI-A-1");
    let cfg = json!({"AUTO_SCALE": false, "CUSTOM_KEEP": {"untouched": true}, "VRR_OFF": [a, b]});
    let mut lists = VrrLists { off: vec![a.to_owned(), b.to_owned()], always: Vec::new() };
    let none = BTreeMap::new();
    // First editor sets the first monitor to Always.
    let mut first = [state(a, "DP-1"), state(b, "HDMI-A-1")];
    first[0].vrr = VrrMode::Always;
    first[0].vrr_changed = true;
    first[1].vrr = VrrMode::Off;
    let plan = model::save_plan(Some(&cfg), &first.clone().map(Some), "", None, &none, &[]);
    assert_eq!(plan.del_cfg["VRR_OFF"], json!([a, "DP-1"]));
    assert!(plan.cfg.get("VRR_OFF").is_none());
    assert_eq!(plan.cfg["VRR_ALWAYS"], json!([a]));
    assert_eq!(plan.cfg["CUSTOM_KEEP"], json!({"untouched": true}));
    lists.apply(&plan);
    assert_eq!((lists.mode(a), lists.mode(b)), (VrrMode::Always, VrrMode::Off));
    // Second (stale: both Off) editor sets the other monitor to Automatic.
    let mut second = [state(a, "DP-1"), state(b, "HDMI-A-1")];
    second[0].vrr = VrrMode::Off;
    second[1].vrr = VrrMode::Auto;
    second[1].vrr_changed = true;
    lists.apply(&model::save_plan(Some(&cfg), &second.clone().map(Some), "", None, &none, &[]));
    assert_eq!((lists.mode(a), lists.mode(b)), (VrrMode::Always, VrrMode::Auto), "the first editor's choice stays");
    // An unrelated Apply re-sends neither setting.
    first[0].vrr_changed = false;
    let plan = model::save_plan(Some(&cfg), &first.clone().map(Some), "", None, &none, &[]);
    for key in ["VRR_OFF", "VRR_ALWAYS"] {
        assert!(plan.cfg.get(key).is_none() && plan.del_cfg.get(key).is_none(), "{key}");
    }
    second[1].vrr = VrrMode::Off;
    lists.apply(&model::save_plan(Some(&cfg), &second.map(Some), "", None, &none, &[]));
    assert_eq!((lists.mode(a), lists.mode(b)), (VrrMode::Always, VrrMode::Off));
}

#[test]
fn the_window_reads_the_daemons_vrr_mode_then_the_lists() {
    let head = json!({"NAME": "DP-1", "DESCRIPTION": "Dell Inc. AW3225QF (DP-1)"});
    assert_eq!(model::vrr_mode_of(&head, &json!({}), &[]), VrrMode::Auto);
    assert_eq!(model::vrr_mode_of(&head, &json!({"VRR_ALWAYS": ["!^DP-"]}), &[]), VrrMode::Always);
    assert_eq!(model::vrr_mode_of(&head, &json!({"VRR_ALWAYS": ["DP-1"], "VRR_OFF": ["aw3225qf"]}), &[]), VrrMode::Off, "both lists is Off");
    let mut reported = head.clone();
    reported["VRR_MODE"] = json!("always");
    assert_eq!(model::vrr_mode_of(&reported, &json!({"VRR_OFF": ["DP-1"]}), &[]), VrrMode::Always, "the daemon's answer wins");
    assert!(model::vrr_capable_of(&head), "unknown is capable");
    reported["VRR_CAPABLE"] = json!(false);
    assert!(!model::vrr_capable_of(&reported));
}

#[test]
fn automatic_scaling_removes_only_that_displays_scale() {
    let (a, b) = ("Same model DP-1", "Same model HDMI-A-1");
    let cfg = json!({"AUTO_SCALE": true, "SCALE": {a: 1.5, b: 1.25}});
    let mut states = [state(a, "DP-1"), state(b, "HDMI-A-1")];
    states[0].scale_auto = true;
    states[1].scale = 1.25;
    let plan = model::save_plan(Some(&cfg), &states.map(Some), "", None, &BTreeMap::new(), &[]);
    assert_eq!(plan.del_cfg["SCALE"], json!({a: 1.5}));
    assert_eq!(plan.cfg["AUTO_SCALE"], json!(true));
    assert_eq!(plan.cfg["SCALE"], json!({b: 1.25}));
}

#[test]
fn transforms_disabled_callback_and_mode_plan() {
    let (a, b) = ("A", "B");
    let cfg = json!({"TRANSFORM": {a: "90"}, "DISABLED": {b: null}, "CALLBACK_CMD": "x"});
    let mut states = [state(a, "DP-1"), state(b, "DP-2")];
    states[1].enabled_changed = true;
    states[1].transform = Some("FLIPPED-STANDARD".into());
    states[0].hdr = true;
    states[0].hdr_changed = true;
    states[0].refresh = json!(59997);
    let plan = model::save_plan(Some(&cfg), &[Some(states[0].clone()), Some(states[1].clone()), None], "", None, &BTreeMap::new(), &[]);
    assert_eq!(plan.del_cfg, json!({"TRANSFORM": {a: "90"}, "DISABLED": {b: null}, "CALLBACK_CMD": ""}));
    assert_eq!(plan.cfg["TRANSFORM"], json!({b: "FLIPPED"}));
    assert!(plan.cfg.get("DISABLED").is_none());
    assert!(plan.cfg.get("CALLBACK_CMD").is_none());
    assert_eq!(plan.cfg["MODE"][a], json!({"WIDTH": 1920, "HEIGHT": 1080, "HZ": 59.997}));
    assert_eq!(plan.hdr_overrides, BTreeMap::from([("DP-1".to_owned(), true)]));
    assert_eq!((plan.hdr_changed.clone(), plan.enabled_changed.clone()), (vec![0], vec![1]));
    // Key order: existing keys keep their place, new ones follow Python's order.
    let keys: Vec<&String> = plan.cfg.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["TRANSFORM", "MODE", "SCALE"]);
    let mut disable = state(a, "DP-1");
    disable.enabled = false;
    disable.enabled_changed = true;
    let plan = model::save_plan(None, &[Some(disable)], "cb", None, &BTreeMap::new(), &[]);
    assert_eq!(plan.cfg["DISABLED"], json!({a: null}));
    assert_eq!(plan.cfg["CALLBACK_CMD"], json!("cb"));
}

fn heads() -> Vec<Value> {
    let monitor = |name: &str, width: i64| json!({"NAME": name, "DESCRIPTION": format!("Same model {name}"),
        "DESIRED": {"MODE": {"WIDTH": width, "HEIGHT": 1080}}, "CURRENT": {"MODE": {"WIDTH": width, "HEIGHT": 1080}}});
    vec![monitor("DP-1", 1920), monitor("HDMI-A-1", 1600)]
}

#[test]
fn layout_model_positions_snaps_and_arranges() {
    let layout = LayoutModel::new(&heads(), &json!({}), Some("TOP"), Some("ROW"), None, &Default::default());
    // 1920*0.08 = 153, 1600*0.08 = 128, 1080*0.08 = 86; group 281x86 → layout 640x280.
    assert_eq!(layout.size_request, (640, 280));
    let positions: Vec<_> = layout.entries.iter().map(|e| (e.x, e.y, e.width, e.height)).collect();
    assert_eq!(positions, [(179, 97, 153, 86), (332, 97, 128, 86)]);
    let (arrange, align, order) = layout.arrangement().unwrap();
    assert_eq!((arrange.as_str(), align.as_deref()), ("ROW", Some("MIDDLE")));
    assert_eq!(order, ["Same model DP-1", "Same model HDMI-A-1"]);
    // CFG order, column arrangement and a rotated, disabled monitor.
    let cfg = json!({"ARRANGE": "column", "ALIGN": "middle", "ORDER": ["Same model HDMI-A-1"], "DISABLED": {"Same model DP-1": null}});
    let mut overrides = serde_json::Map::new();
    overrides.insert("Same model HDMI-A-1".into(), json!("90"));
    let layout = LayoutModel::new(&heads(), &cfg, None, None, None, &overrides);
    assert_eq!((layout.arrange.as_str(), layout.align.as_str()), ("COLUMN", "MIDDLE"));
    assert_eq!(layout.entries[0].desc, "Same model HDMI-A-1");
    assert_eq!(layout.entries[0].transform, "90");
    assert_eq!((layout.entries[0].width, layout.entries[0].height), (86, 128));
    // Snapping also uses the untracked (disabled) shape at (0, 0): 76 → 86, as in Python.
    assert_eq!((layout.entries[0].x, layout.entries[0].y), (277, 86));
    assert!(!layout.entries[1].placed);
    let mut recentered = layout.clone();
    recentered.recenter(640, 280, &json!({"ARRANGE": "ROW", "ALIGN": "MIDDLE"}));
    assert_eq!((recentered.entries[0].x, recentered.entries[0].y), (200, 76));
    assert_eq!(model::snap_to_edges(175, 0, &[(0, 0, 153, 86)], 100, 50), (153, 0));
    assert_eq!(model::parse_transform("FLIPPED-270"), (270, true));
    assert_eq!(model::parse_transform("flipped"), (0, true));
    assert_eq!(model::parse_transform("NORMAL"), (0, false));
    assert_eq!(model::rotated_dimensions(1, 2, "270"), (2, 1));
}

#[test]
fn a_turned_display_keeps_its_turned_size_and_alignment() {
    // The daemon reports every head's DESIRED transform.
    let head = |name: &str, transform: &str| json!({"NAME": name, "DESCRIPTION": name,
        "DESIRED": {"MODE": {"WIDTH": 1920, "HEIGHT": 1080}, "TRANSFORM": transform}, "CURRENT": {"MODE": {"WIDTH": 1920, "HEIGHT": 1080}}});
    let mut layout = LayoutModel::new(&[head("A", "NORMAL"), head("B", "90")], &json!({}), None, None, None, &Default::default());
    assert_eq!((layout.entries[1].width, layout.entries[1].height), (86, 153));
    // Dragged next to A with their bottom edges lined up (A is 153x86).
    (layout.entries[0].x, layout.entries[0].y) = (100, 150);
    (layout.entries[1].x, layout.entries[1].y) = (253, 83);
    assert_eq!(layout.arrangement().unwrap(), ("ROW".into(), Some("BOTTOM".into()), vec!["A".into(), "B".into()]));
    (layout.entries[1].x, layout.entries[1].y) = (253, 116);
    assert_eq!(layout.arrangement().unwrap().1.as_deref(), Some("MIDDLE"));
    // Turning it back, or a new mode, gives the size of the mode as it is shown.
    layout.entries[1].set_mode(1920, 1080, "NORMAL");
    assert_eq!((layout.entries[1].width, layout.entries[1].height), (153, 86));
    layout.entries[1].set_mode(2560, 1440, "FLIPPED-270");
    assert_eq!((layout.entries[1].width, layout.entries[1].height, layout.entries[1].orig_width), (115, 204, 2560));
}

// ------------------------------------------------------------------- IPC

#[test]
fn ipc_round_trip_with_private_fake_daemon() {
    use std::io::{Read, Write};
    let dir = scratch("ipc");
    let socket = dir.join("tessel-displays.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = vec![0u8; 65536];
            let count = stream.read(&mut buffer).unwrap();
            requests.push(String::from_utf8_lossy(&buffer[..count]).into_owned());
            stream.write_all(b"CFG:\n  ARRANGE: ROW\nSTATE:\n  HEADS:\n    - NAME: DP-1\n      DESCRIPTION: First\n").unwrap();
        }
        requests
    });
    let reply = ipc::ipc_operation_at(&socket, "GET", "").unwrap();
    assert!(ipc::get_config_data_from(|| Ok(reply)));
    assert_eq!(ipc::config_data()["STATE"]["HEADS"][0]["NAME"], json!("DP-1"));
    ipc::ipc_operation_at(&socket, "CFG_SET", &ipc::config_payload(&json!({"SCALE": {"First": 1.5}}))).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests, ["OP: GET\n", "OP: CFG_SET\nCFG:\n  SCALE:\n    First: 1.5"]);
    assert_eq!(ipc::ipc_operation_at(&dir.join("missing.sock"), "GET", "").unwrap(), "");
    assert!(!ipc::get_config_data_from(|| Ok("- a list".into())));
    assert_eq!(ipc::config_data(), json!({}));
}

// ------------------------------------------------------- reverting an Apply

#[test]
fn revert_restores_values_and_removes_what_apply_added() {
    let before = json!({"AUTO_SCALE": true, "ARRANGE": "ROW", "SCALE": {"A": 2.0}, "MODE": {"A": {"WIDTH": 1920}},
                        "TRANSFORM": null, "VRR_OFF": ["A"], "ORDER": [], "DISABLED": {"!^eDP-[0-9]": {"IF": [{"LID": "CLOSED"}]}}});
    let after = json!({"AUTO_SCALE": false, "ARRANGE": "COLUMN", "SCALE": {"A": 1.25, "B": 1.5}, "MODE": {"A": {"WIDTH": 2880}, "B": {"WIDTH": 800}},
                       "TRANSFORM": {"A": "90"}, "VRR_OFF": ["A", "B"], "ORDER": ["B", "A"], "CALLBACK_CMD": "x",
                       "DISABLED": {"!^eDP-[0-9]": {"IF": [{"LID": "CLOSED"}]}, "B": null}});
    let (set, del) = crate::model::revert_plan(&before, &after, &[]);
    assert_eq!(set["AUTO_SCALE"], json!(true));
    assert_eq!(set["ARRANGE"], "ROW");
    assert_eq!(set["SCALE"], json!({"A": 2.0}));
    assert_eq!(set["MODE"], json!({"A": {"WIDTH": 1920}}));
    assert!(set.get("TRANSFORM").is_none() && set.get("ORDER").is_none(), "nothing empty is sent");
    assert_eq!(del, json!({"SCALE": {"B": 1.5}, "MODE": {"B": {"WIDTH": 800}}, "TRANSFORM": {"A": "90"}, "VRR_OFF": ["B"],
                           "ORDER": ["B", "A"], "CALLBACK_CMD": "", "DISABLED": {"B": null}}));
}

#[test]
fn revert_restores_each_screens_adaptive_sync_mode() {
    // Before: A Off, B Always, C Automatic. Apply: A Always, B Automatic, C Off.
    let before = json!({"VRR_OFF": ["A"], "VRR_ALWAYS": ["B"]});
    let after = json!({"VRR_OFF": ["C"], "VRR_ALWAYS": ["A"]});
    let (set, del) = crate::model::revert_plan(&before, &after, &[]);
    assert_eq!(del["VRR_OFF"], json!(["C"]));
    assert_eq!(del["VRR_ALWAYS"], json!(["A"]));
    let mut lists = VrrLists { off: vec!["C".into()], always: vec!["A".into()] };
    lists.apply(&model::SavePlan { cfg: set, del_cfg: del, hdr_overrides: BTreeMap::new(), hdr_changed: vec![], enabled_changed: vec![] });
    assert_eq!((lists.mode("A"), lists.mode("B"), lists.mode("C")), (VrrMode::Off, VrrMode::Always, VrrMode::Auto));
}

#[test]
fn revert_keeps_a_false_setting() {
    let (set, _) = crate::model::revert_plan(&json!({"AUTO_SCALE": false}), &json!({"AUTO_SCALE": true}), &[]);
    assert_eq!(set["AUTO_SCALE"], json!(false));
}

#[test]
fn revert_switches_a_display_under_the_lid_rule_back_by_name() {
    let rule = json!({"!^eDP-[0-9]": {"IF": [{"LID": "CLOSED"}]}});
    let config = json!({"DISABLED": rule});
    let (_, del) = crate::model::revert_plan(&config, &config, &[("Panel".to_owned(), false)]);
    assert_eq!(del["DISABLED"], json!({"Panel": null}), "turned off by Apply: back on");
    let (set, del) = crate::model::revert_plan(&config, &config, &[("Panel".to_owned(), true)]);
    assert!(set["DISABLED"].as_object().unwrap().contains_key("Panel"), "turned on by Apply: back off");
    assert!(del.get("DISABLED").is_none());
    assert!(set["DISABLED"].as_object().unwrap().contains_key("!^eDP-[0-9]"), "the rule is kept");
}

#[test]
fn adaptive_sync_strings_are_translated_into_every_language() {
    let strings = ["Adaptive Sync (VRR)", "Automatic", "Always", "Off", "On only while an app is fullscreen on this screen.",
                   "Adaptive Sync is not available on this screen."];
    let source = include_str!("monitor_widget.rs");
    for text in strings {
        assert!(source.contains(&format!("tr!(\"{text}\")")), "{text} is not used");
    }
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../i18n/locale");
    let mut languages = 0;
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.file_stem().is_some_and(|stem| stem == "en") {
            continue;
        }
        languages += 1;
        let catalog: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for text in strings {
            let translated = catalog[text].as_str().unwrap_or_else(|| panic!("{}: {text}", path.display()));
            assert!(!translated.is_empty() && translated != text, "{}: {text}", path.display());
        }
    }
    assert_eq!(languages, 30);
}

// ------------------------------------------- a monitor on another connector (issue #9)

const MOVED_FILE: &str = "ORDER:
- UGD Artist 12     0x20210910 (HDMI-A-2)
- Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2)
MODE:
  UGD Artist 12     0x20210910 (HDMI-A-3): {WIDTH: 1920, HEIGHT: 1080, HZ: 60.003}
  Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-4): {WIDTH: 2560, HEIGHT: 1440, HZ: 239.97}
  UGD Artist 12     0x20210910 (HDMI-A-2): {WIDTH: 1920, HEIGHT: 1080, HZ: 60.003}
  Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2): {WIDTH: 2560, HEIGHT: 1440, HZ: 239.97}
VRR_OFF:
- Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2)
";
const MSI: &str = "Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101";
const UGD: &str = "UGD Artist 12     0x20210910";

fn on(identity: &str, connector: &str) -> String {
    format!("{identity} ({connector})")
}

/// The daemon's state for the two monitors of the issue on these connectors.
fn moved_heads(dp: &str, hdmi: &str) -> Vec<Value> {
    let head = |make: &str, model: &str, serial: &str, connector: &str, width: i64, height: i64| json!({
        "NAME": connector, "DESCRIPTION": format!("{make} {model} {serial} ({connector})"), "MAKE": make, "MODEL": model, "SERIAL_NUMBER": serial,
        "CURRENT": {"ENABLED": true, "MODE": {"WIDTH": width, "HEIGHT": height, "REFRESH_MHZ": 60000}},
        "DESIRED": {"ENABLED": true, "MODE": {"WIDTH": width, "HEIGHT": height, "REFRESH_MHZ": 60000}}});
    vec![head("Micro-Star Int'l Co., Ltd.", "MAG 274Q X24", "0x01010101", dp, 2560, 1440), head("UGD", "Artist 12    ", "0x20210910", hdmi, 1920, 1080)]
}

/// The issue's file with more per-screen settings, an unplugged monitor, a short name, a regex and a partial name.
fn moved_cfg() -> Value {
    let mut cfg = crate::compat::yaml::load(MOVED_FILE).unwrap();
    cfg["SCALE"] = json!({on(MSI, "DP-4"): 1.25, on(MSI, "DP-2"): 1.5, "DP-9": 2.0, "!^HDMI-A-9$": 1.75, "Artist 12     0x20210910 (HDMI-A-9)": 1.125,
                          "Other Screen 1 (DP-9)": 1.25});
    cfg["TRANSFORM"] = json!({on(UGD, "HDMI-A-3"): "90"});
    cfg["DISABLED"] = json!({"Other Screen 1 (DP-9)": null});
    cfg
}

#[test]
fn the_window_shows_the_settings_a_moved_monitor_gets() {
    let cfg = moved_cfg();
    let heads = moved_heads("DP-7", "HDMI-A-7");
    let shown = model::resolved_cfg(&cfg, &heads);
    assert_eq!(shown["SCALE"][on(MSI, "DP-7")], json!(1.5), "the last of the monitor's entries");
    assert_eq!(shown["TRANSFORM"][on(UGD, "HDMI-A-7")], json!("90"));
    assert_eq!(shown["MODE"][on(MSI, "DP-7")], json!({"WIDTH": 2560, "HEIGHT": 1440, "HZ": 239.97}));
    assert_eq!(shown["ORDER"], json!([on(UGD, "HDMI-A-7"), on(MSI, "DP-7")]));
    assert!(shown["SCALE"].get(on(UGD, "HDMI-A-7")).is_none() && shown["DISABLED"].as_object().unwrap().len() == 1);
    for key in ["SCALE", "MODE", "TRANSFORM", "DISABLED"] {
        for (name, value) in cfg[key].as_object().unwrap() {
            assert_eq!(&shown[key][name], value, "{key} {name} is kept");
        }
    }
    let monitors: Vec<Value> = heads.iter().map(|head| { let mut head = head.clone(); head.as_object_mut().unwrap().remove("DESIRED"); head }).collect();
    let layout = LayoutModel::new(&monitors, &shown, None, None, None, shown["TRANSFORM"].as_object().unwrap());
    let entries: Vec<(&str, &str)> = layout.entries.iter().map(|e| (e.name.as_str(), e.transform.as_str())).collect();
    assert_eq!(entries, [("HDMI-A-7", "90"), ("DP-7", "0")], "the saved order and rotation");
    assert_eq!(model::vrr_mode_of(&heads[0], &cfg, &heads), VrrMode::Off, "an older daemon's state: the lists");
    assert_eq!(model::vrr_mode_of(&heads[1], &cfg, &heads), VrrMode::Auto);
    // On a connector of its own the monitor's own entries are shown; a regex or name that matches wins too.
    let heads = moved_heads("DP-2", "HDMI-A-2");
    let shown = model::resolved_cfg(&cfg, &heads);
    assert_eq!((shown["SCALE"].clone(), shown["ORDER"].clone(), shown["MODE"].clone()), (cfg["SCALE"].clone(), cfg["ORDER"].clone(), cfg["MODE"].clone()));
    assert_eq!(shown["TRANSFORM"][on(UGD, "HDMI-A-2")], json!("90"));
    let heads = moved_heads("DP-9", "HDMI-A-9");
    let shown = model::resolved_cfg(&cfg, &heads);
    assert!(shown["SCALE"].get(on(MSI, "DP-9")).is_none() && shown["SCALE"].get(on(UGD, "HDMI-A-9")).is_none(), "{}", shown["SCALE"]);
    assert_eq!(model::resolved_cfg(&cfg, &[]), cfg);
}

/// The daemon's CFG_DEL then CFG_SET of a plan, as `save_config` sends them.
fn daemon_after(cfg: &Value, plan: &model::SavePlan) -> crate::daemon::config::Cfg {
    use crate::daemon::config::{Cfg, Partial};
    Cfg::from_value(cfg).merged_del(&Partial::from_value(&plan.del_cfg)).merged_set(&Partial::from_value(&plan.cfg))
}

#[test]
fn saving_moves_only_that_monitors_entries_to_its_connector() {
    let cfg = moved_cfg();
    let heads = moved_heads("DP-7", "HDMI-A-7");
    let (mut first, mut second) = (state(&on(MSI, "DP-7"), "DP-7"), state(&on(UGD, "HDMI-A-7"), "HDMI-A-7"));
    (first.width, first.height, first.refresh, first.scale) = (json!(2560), json!(1440), json!(239_970), 1.5);
    second.transform = Some("90".into());
    let shown = model::resolved_cfg(&cfg, &heads);
    let layout = LayoutModel::new(&heads, &shown, None, None, None, shown["TRANSFORM"].as_object().unwrap());
    let plan = model::save_plan(Some(&cfg), &[Some(first.clone()), Some(second.clone())], "", Some(&layout), &BTreeMap::new(), &heads);
    assert_eq!(plan.cfg["ORDER"], json!([on(UGD, "HDMI-A-7"), on(MSI, "DP-7")]), "the arrangement as it was");
    let keys = |value: &Value| -> Vec<String> { value.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default() };
    assert_eq!(keys(&plan.cfg["MODE"]), [on(MSI, "DP-7"), on(UGD, "HDMI-A-7")]);
    assert_eq!(keys(&plan.del_cfg["MODE"]), [on(MSI, "DP-4"), on(MSI, "DP-2"), on(UGD, "HDMI-A-3"), on(UGD, "HDMI-A-2")]);
    assert_eq!(plan.cfg["SCALE"], json!({"DP-9": 2.0, "!^HDMI-A-9$": 1.75, "Artist 12     0x20210910 (HDMI-A-9)": 1.125,
                                         "Other Screen 1 (DP-9)": 1.25, on(MSI, "DP-7"): 1.5, on(UGD, "HDMI-A-7"): 1.0}));
    assert_eq!(plan.del_cfg["SCALE"], json!({on(MSI, "DP-4"): 1.25, on(MSI, "DP-2"): 1.5}));
    assert_eq!((plan.cfg["TRANSFORM"].clone(), plan.del_cfg["TRANSFORM"].clone()), (json!({on(UGD, "HDMI-A-7"): "90"}), json!({on(UGD, "HDMI-A-3"): "90"})));
    assert_eq!(plan.cfg["DISABLED"], json!({"Other Screen 1 (DP-9)": null}));
    assert!(plan.del_cfg.get("DISABLED").is_none() && plan.cfg.get("VRR_OFF").is_none() && plan.del_cfg.get("VRR_OFF").is_none(),
            "adaptive sync is saved only when it changes");
    let saved = daemon_after(&cfg, &plan);
    let text = saved.file_text();
    for old in ["(DP-4)", "(DP-2): ", "(HDMI-A-2)", "(HDMI-A-3)"] {
        assert!(!text.contains(old), "{old} is gone:\n{text}");
    }
    assert!(text.contains("Other Screen 1 (DP-9)") && text.contains("'!^HDMI-A-9$'") && text.contains("DP-9: 2") && text.contains("(HDMI-A-9)"), "{text}");
    assert_eq!(saved.vrr_off, [on(MSI, "DP-2")], "unchanged, still used by the moved rule");
    // The daemon reads the saved file the same way on DP-7, and on DP-2 next boot.
    for (dp, hdmi) in [("DP-7", "HDMI-A-7"), ("DP-2", "HDMI-A-2")] {
        let screens = model::screens(&moved_heads(dp, hdmi));
        let scale = crate::daemon::layout::entry_for(&screens[0], &screens, &saved.scales).map(|e| e.1);
        let transform = crate::daemon::layout::entry_for(&screens[1], &screens, &saved.transforms).map(|e| e.1);
        assert_eq!((scale, transform), (Some(1.5), Some(1)), "{dp}");
        assert_eq!(crate::daemon::layout::vrr_mode(&saved, &screens[0], &screens), VrrMode::Off, "{dp}");
    }
    // Monitors without entries elsewhere save exactly as before.
    let plain = json!({"TRANSFORM": {"A": "90"}, "SCALE": {on(MSI, "DP-7"): 1.25}, "VRR_OFF": ["DP-7"]});
    let states = [Some(first), Some(second)];
    assert_eq!(model::save_plan(Some(&plain), &states, "", None, &BTreeMap::new(), &heads),
               model::save_plan(Some(&plain), &states, "", None, &BTreeMap::new(), &[]));
}

#[test]
fn identical_monitors_keep_every_entry_when_saved() {
    let twin = |connector: &str| json!({"NAME": connector, "DESCRIPTION": format!("Dell Inc. U2720Q  ({connector})"), "MAKE": "Dell Inc.", "MODEL": "U2720Q", "SERIAL_NUMBER": ""});
    let heads = [twin("DP-1"), twin("DP-3")];
    let cfg = json!({"SCALE": {"Dell Inc. U2720Q  (DP-2)": 2.0}, "VRR_OFF": ["Dell Inc. U2720Q  (DP-2)"]});
    let mut states = [state("Dell Inc. U2720Q  (DP-1)", "DP-1"), state("Dell Inc. U2720Q  (DP-3)", "DP-3")];
    states[0].vrr_changed = true;
    let plan = model::save_plan(Some(&cfg), &states.map(Some), "", None, &BTreeMap::new(), &heads);
    assert_eq!(plan.cfg["SCALE"]["Dell Inc. U2720Q  (DP-2)"], json!(2.0));
    assert!(plan.del_cfg.get("SCALE").is_none() && plan.del_cfg.get("MODE").is_none());
    assert_eq!(plan.del_cfg["VRR_OFF"], json!(["Dell Inc. U2720Q  (DP-1)", "DP-1"]));
    assert_eq!(model::resolved_cfg(&cfg, &heads), cfg);
}

#[test]
fn adaptive_sync_set_back_to_automatic_leaves_no_entry_for_the_monitor() {
    let heads = moved_heads("DP-4", "HDMI-A-3");
    let desc = on(MSI, "DP-4");
    for to in [VrrMode::Auto, VrrMode::Always, VrrMode::Off] {
        let mut lists = VrrLists { off: vec![on(MSI, "DP-2"), "Other Screen 1 (DP-9)".into()], always: vec![on(MSI, "DP-1")] };
        let shown = model::vrr_mode_of(&heads[0], &lists.cfg(), &heads);
        assert_eq!(shown, VrrMode::Off, "the stale DP-2 entry applies, as the daemon reports");
        let mut screen = state(&desc, "DP-4");
        (screen.vrr, screen.vrr_changed) = (to, to != shown);
        let plan = model::save_plan(Some(&lists.cfg()), &[Some(screen)], "", None, &BTreeMap::new(), &heads);
        lists.apply(&plan);
        let expected = |mode: VrrMode| if mode == to && to != VrrMode::Auto { vec![desc.clone()] } else { Vec::new() };
        let others = |list: &[String]| list.iter().filter(|d| d.starts_with(MSI)).cloned().collect::<Vec<_>>();
        if to != VrrMode::Off {
            assert_eq!((others(&lists.off), others(&lists.always)), (expected(VrrMode::Off), expected(VrrMode::Always)), "{to:?}: {plan:?}");
        }
        assert_eq!(model::vrr_mode_of(&heads[0], &lists.cfg(), &heads), to, "{to:?}");
        assert!(lists.off.contains(&"Other Screen 1 (DP-9)".to_owned()), "another monitor's entry stays");
    }
}

#[test]
fn the_adaptive_sync_block_names_the_new_connector() {
    use crate::daemon::{config::Cfg, layout};
    let dir = scratch("vrr-moved");
    let path = dir.join("rc.xml");
    std::fs::write(&path, "<labwc_config>\n</labwc_config>\n").unwrap();
    let cfg = Cfg::load(MOVED_FILE).unwrap();
    for (dp, hdmi) in [("DP-2", "HDMI-A-2"), ("DP-7", "HDMI-A-7")] {
        let screens = model::screens(&moved_heads(dp, hdmi));
        let modes = layout::vrr_modes(&cfg, &screens);
        labwc::sync_adaptive_sync_with(&path, &modes, &fsx::os_replace, &|| (true, None)).unwrap();
        let block = labwc::parse_adaptive_sync_block(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(block.into_iter().collect::<Vec<_>>(), [(dp.to_owned(), "no".to_owned()), (hdmi.to_owned(), "fullscreen".to_owned())]);
    }
}
