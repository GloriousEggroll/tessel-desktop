//! Port of test_file_preferences.py.

use super::*;
use crate::testutil::{names, read, write, TempDir};
use std::os::unix::fs::{symlink, PermissionsExt};

fn map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("object expected"),
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    path: PathBuf,
}

fn fixture() -> Fixture {
    let temp = TempDir::new("files-preferences-");
    let root = temp.path().to_path_buf();
    let path = root.join("config/preferences.json");
    Fixture { _temp: temp, root, path }
}

#[test]
fn test_read_only_defaults_create_no_files_and_use_xdg() {
    let f = fixture();
    assert_eq!(config_home_from(Some(f.root.clone().into_os_string()), Path::new("/home/x")), f.root);
    assert_eq!(config_home_from(Some("relative".into()), Path::new("/home/x")), PathBuf::from("/home/x/.config"));
    assert_eq!(config_home_from(None, Path::new("/home/x")), PathBuf::from("/home/x/.config"));
    let prefs = FilePreferences::new(Some(f.root.join("desktop-suite/file-manager.json")));
    assert_eq!(prefs.data, *DEFAULTS);
    assert!(GTKBookmarks::new(Some(f.root.join("gtk-3.0/bookmarks"))).list().is_empty());
    assert!(names(&f.root).is_empty());
}

#[test]
fn test_invalid_config_is_safely_normalized() {
    let data = validate(&json!({"view": [], "sort": "nonsense", "icon_size": 900,
                                "show_hidden": "true", "window_size": [1, 9000],
                                "tabs": ["file:///home/me", "javascript\n:bad", "sftp://host/path"],
                                "tab_splits": ["", "file:///discard", "trash:///"],
                                "active_tab": 99, "evil": 42}));
    assert_eq!(data["view"], "icons");
    assert_eq!(data["sort"], "name");
    assert_eq!(data["icon_size"], 256);
    assert_eq!(data["show_hidden"], false);
    assert_eq!(data["window_size"], json!([640, 4320]));
    assert_eq!(data["tabs"], json!(["file:///home/me", "sftp://host/path"]));
    assert_eq!(data["tab_splits"], json!(["", "trash:///"]));
    assert_eq!(data["active_tab"], 1);
    assert!(!data.contains_key("evil"));
    assert_eq!(validate(&json!({"icon_size": 48.0, "active_tab": true}))["icon_size"], 64);
    assert_eq!(validate(&json!("not a dict")), *DEFAULTS);
    let keys: Vec<&String> = data.keys().collect();
    let default_keys: Vec<&String> = DEFAULTS.keys().collect();
    assert_eq!(keys, default_keys, "keys keep the Python order");
}

#[test]
fn test_atomic_update_persists_and_instances_do_not_clobber() {
    let f = fixture();
    let mut first = FilePreferences::new(Some(f.path.clone()));
    let mut second = FilePreferences::new(Some(f.path.clone()));
    first.set("show_hidden", json!(true)).unwrap();
    second.update(map(json!({"view": "details", "icon_size": 48}))).unwrap();
    let loaded = FilePreferences::new(Some(f.path.clone()));
    assert_eq!(loaded.get("show_hidden"), true);
    assert_eq!(loaded.get("view"), "details");
    assert_eq!(loaded.get("icon_size"), 48);
    assert_eq!(std::fs::metadata(&f.path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(names(f.path.parent().unwrap()), ["preferences.json", "preferences.json.lock"]);
    let error = first.set("no-such-setting", json!(true)).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Key);
    assert_eq!(error.to_string(), "'Unknown file manager preferences: no-such-setting'");
    let text = read(&f.path);
    assert!(text.starts_with("{\n  \"view\": \"details\",\n  \"icon_size\": 48,\n  \"sort\": \"name\","), "{text}");
    assert!(text.contains("\"tabs\": [],\n"));
    assert!(text.contains("\"window_size\": [\n    1100,\n    760\n  ],\n"));
    assert!(text.ends_with("\"vim_keys\": false\n}\n"));
}

#[test]
fn test_invalid_json_defaults_and_defensive_get() {
    let f = fixture();
    std::fs::create_dir(f.path.parent().unwrap()).unwrap();
    write(&f.path, "{this is broken");
    let prefs = FilePreferences::new(Some(f.path.clone()));
    assert_eq!(prefs.get("view"), "icons");
    let mut copy = prefs.get("bookmarks");
    copy.as_array_mut().unwrap().push(json!({"uri": "file:///tmp", "label": "Temporary"}));
    assert_eq!(prefs.get("bookmarks"), json!([]));
    assert_eq!(prefs.get("missing"), Value::Null);
    assert_eq!(read(&f.path), "{this is broken");
}

#[test]
fn test_pane_visibility_and_width_are_persistent_and_validated() {
    let f = fixture();
    let mut prefs = FilePreferences::new(Some(f.path.clone()));
    prefs.update(map(json!({"show_sidebar": false, "show_information": true, "information_width": 460}))).unwrap();
    let loaded = FilePreferences::new(Some(f.path.clone()));
    assert_eq!(loaded.get("show_sidebar"), false);
    assert_eq!(loaded.get("show_information"), true);
    assert_eq!(loaded.get("information_width"), 460);
    assert_eq!(validate(&json!({"information_width": -20}))["information_width"], 300);
    assert_eq!(validate(&json!({"information_width": 10000}))["information_width"], 800);
    assert_eq!(validate(&json!({"information_width": "wide"}))["information_width"], 360);
}

#[test]
fn test_folder_overrides_and_session_and_saved_searches() {
    let f = fixture();
    let mut prefs = FilePreferences::new(Some(f.path.clone()));
    prefs.set_folder_options("sftp://host/work", map(json!({"view": "details", "show_hidden": true}))).unwrap();
    prefs.update(map(json!({"tabs": ["file:///tmp", "trash:///"], "tab_splits": ["file:///home", ""],
                            "active_tab": 1, "search_saved": [{"name": "Notes", "uri": "file:///tmp",
                                                                 "query": "*.txt", "contents": false}]}))).unwrap();
    let mut prefs = FilePreferences::new(Some(f.path.clone()));
    assert_eq!(prefs.folder_options("sftp://host/work")["view"], "details");
    assert_eq!(prefs.folder_options("sftp://host/work")["show_hidden"], true);
    assert_eq!(prefs.folder_options("file:///other")["view"], "icons");
    assert_eq!(prefs.folder_options("file:///other").len(), FOLDER_KEYS.len());
    assert_eq!(prefs.get("tab_splits"), json!(["file:///home", ""]));
    assert_eq!(prefs.get("active_tab"), 1);
    assert_eq!(prefs.get("search_saved")[0]["query"], "*.txt");
    let error = prefs.set_folder_options("not a uri", map(json!({"view": "compact"}))).unwrap_err();
    assert!(error.is_value_error());
    assert_eq!(error.to_string(), "A folder URI is required");
    let error = prefs.set_folder_options("file:///x", map(json!({"tabs": []}))).unwrap_err();
    assert_eq!(error.to_string(), "'Unknown folder options: tabs'");
    let saved = validate(&json!({"search_saved": [{"uri": "file:///", "query": "x".repeat(200)}]}));
    assert_eq!(saved["search_saved"][0]["name"].as_str().unwrap().len(), 128);
    assert_eq!(saved["search_saved"][0]["contents"], false);
}

#[test]
fn test_save_writes_explicit_edits() {
    let f = fixture();
    let mut prefs = FilePreferences::new(Some(f.path.clone()));
    prefs.data.insert("view".into(), json!("columns"));
    prefs.data.insert("icon_size".into(), json!(1));
    prefs.save().unwrap();
    let loaded = FilePreferences::new(Some(f.path.clone()));
    assert_eq!(loaded.get("view"), "columns");
    assert_eq!(loaded.get("icon_size"), 16);
}

#[test]
fn test_preferences_honor_symlink_target() {
    let f = fixture();
    let target = f.root.join("linked.json");
    write(&target, "{}");
    std::fs::create_dir(f.path.parent().unwrap()).unwrap();
    symlink(&target, &f.path).unwrap();
    FilePreferences::new(Some(f.path.clone())).set("view", json!("compact")).unwrap();
    assert!(std::fs::symlink_metadata(&f.path).unwrap().file_type().is_symlink());
    let data: Value = serde_json::from_str(&read(&target)).unwrap();
    assert_eq!(data["view"], "compact");
}

#[test]
fn test_gtk_bookmarks_preserve_unrelated_lines_and_labels() {
    let f = fixture();
    let path = f.root.join("bookmarks");
    let original = "# local comment\nfile:///home/me/My%20Files My files\nsftp://server/work Work\n";
    write(&path, original);
    let bookmarks = GTKBookmarks::new(Some(path.clone()));
    assert_eq!(bookmarks.list(), vec![
        Bookmark { uri: "file:///home/me/My%20Files".into(), label: "My files".into() },
        Bookmark { uri: "sftp://server/work".into(), label: "Work".into() },
    ]);
    bookmarks.add("sftp://server/work", "ignored duplicate").unwrap();
    assert_eq!(read(&path), original);
    bookmarks.add("file:///tmp", "Temporary\nfiles").unwrap();
    bookmarks.rename("sftp://server/work", "Remote").unwrap();
    bookmarks.remove("file:///home/me/My%20Files").unwrap();
    assert_eq!(read(&path), "# local comment\nsftp://server/work Remote\nfile:///tmp Temporary files\n");
    assert!(!valid_uri("file:///tmp\nfile:///etc"));
    assert!(!valid_uri("file:///space in name"));
    assert!(!valid_uri("file:///a\u{1c}b"));
    assert!(valid_uri("trash:"));
    let error = bookmarks.add("/tmp", "").unwrap_err();
    assert_eq!(error.to_string(), "A valid bookmark URI is required");
    bookmarks.remove("sftp://server/work").unwrap();
    bookmarks.remove("file:///tmp").unwrap();
    bookmarks.remove("# local comment").unwrap();
    assert_eq!(read(&path), "# local comment\n");
}
