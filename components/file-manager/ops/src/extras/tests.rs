//! Port of test_file_extras.py (metadata store, Git status, custom actions).

use super::*;
use crate::operations::{Callbacks, Operations};
use crate::testutil::{read, write, TempDir};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

fn map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("object expected"),
    }
}

fn set_raw_xattr(path: &Path, value: &[u8]) -> std::result::Result<(), i32> {
    let name = std::ffi::CString::new(METADATA_ATTRIBUTE).unwrap();
    let target = fsx::cstring(path).unwrap();
    let status = unsafe { libc::lsetxattr(target.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
    if status == 0 { Ok(()) } else { Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)) }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    file: PathBuf,
    store: MetadataStore,
}

fn fixture() -> Fixture {
    let temp = TempDir::new("files-extras-");
    let root = temp.path().to_path_buf();
    let file = root.join("example.txt");
    std::fs::write(&file, b"Original contents\x00unchanged\n").unwrap();
    let store = MetadataStore::new(Some(root.join("state/metadata.json")));
    Fixture { _temp: temp, root, file, store }
}

#[test]
fn test_constructor_and_reads_do_not_create_files() {
    let f = fixture();
    assert_eq!(f.store.read(&f.file), EMPTY_METADATA);
    assert_eq!(f.store.read("sftp://example.invalid/path"), EMPTY_METADATA);
    assert!(!f.store.path.parent().unwrap().exists());
    let error = f.store.write("sftp://example.invalid/path", &map(json!({"rating": 2}))).unwrap_err();
    assert!(error.is_value_error());
    assert_eq!(error.to_string(), "File annotations are available for local files");
    assert_eq!(local_path(&Target::from("file:///a?b")).unwrap_err().to_string(),
               "A file URI cannot contain a query or fragment");
    assert_eq!(local_path(&Target::from("file://localhost/a%20b")).unwrap(), PathBuf::from("/a b"));
}

#[test]
fn test_metadata_roundtrip_partial_updates_preserve_file_and_mode() {
    let f = fixture();
    let stat = |p: &Path| std::fs::metadata(p).unwrap();
    let before = (std::fs::read(&f.file).unwrap(), stat(&f.file).mode(), stat(&f.file).mtime_nsec(), stat(&f.file).mtime());
    let uri = pyos::as_uri(&f.file);
    let result = f.store.write(uri.as_str(), &map(json!({"tags": ["Work", "work", "  Notes  "], "rating": 4,
                                                          "comment": "Some notes\non two lines"}))).unwrap();
    assert_eq!(result.tags, ["Work", "Notes"]);
    f.store.write(&f.file, &map(json!({"rating": 2}))).unwrap();
    let read_back = f.store.read(&f.file);
    assert_eq!(read_back.rating, 2);
    assert_eq!(read_back.comment, "Some notes\non two lines");
    assert_eq!(read_back.tags, ["Work", "Notes"]);
    assert_eq!(before, (std::fs::read(&f.file).unwrap(), stat(&f.file).mode(), stat(&f.file).mtime_nsec(),
                        stat(&f.file).mtime()));
    f.store.write(&f.file, &map(json!({"tags": [], "rating": 0, "comment": ""}))).unwrap();
    assert_eq!(f.store.read(&f.file), EMPTY_METADATA);
}

#[test]
fn test_symlink_and_fifo_are_never_followed_or_modified() {
    let f = fixture();
    let link = f.root.join("link");
    symlink(&f.file, &link).unwrap();
    assert_eq!(f.store.read(&link), EMPTY_METADATA);
    let error = f.store.write(&link, &map(json!({"comment": "Not the target"}))).unwrap_err();
    assert!(error.is_os_error());
    assert_eq!(f.store.read(&f.file), EMPTY_METADATA);
    let fifo = f.root.join("fifo");
    let name = fsx::cstring(&fifo).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = f.store.write(&fifo, &map(json!({"rating": 3}))).unwrap_err();
    assert!(error.is_value_error());
    assert_eq!(error.to_string(), "File annotations require a regular file or folder");
}

#[test]
fn test_fallback_for_unsupported_xattrs_is_atomic_and_inode_bound() {
    let f = fixture();
    let mut unsupported = f.store.clone();
    unsupported.fail_get = Some(libc::ENOTSUP);
    unsupported.fail_set = Some(libc::ENOTSUP);
    unsupported.write(&f.file, &map(json!({"tags": ["Offline"], "rating": 5}))).unwrap();
    assert_eq!(unsupported.read(&f.file).rating, 5);
    unsupported.write(&f.file, &map(json!({"comment": "Persisted"}))).unwrap();
    assert_eq!(f.store.read(&f.file).comment, "Persisted");
    assert_eq!(std::fs::metadata(&f.store.path).unwrap().permissions().mode() & 0o777, 0o600);
    let data: Value = serde_json::from_str(&read(&f.store.path)).unwrap();
    assert!(data.get(pyos::as_uri(&f.file)).is_some());
    assert!(read(&f.store.path).starts_with("{\n  \"file://"));
    let replacement = f.root.join("replacement");
    write(&replacement, "A different file");
    std::fs::rename(&replacement, &f.file).unwrap();
    assert_eq!(f.store.read(&f.file), EMPTY_METADATA);
}

#[test]
fn test_fallback_overrides_old_xattr_and_clears_after_successful_write() {
    let f = fixture();
    if let Err(errno) = set_raw_xattr(&f.file, b"{\"rating\":1}") {
        eprintln!("Test filesystem lacks xattrs: errno {errno}; skipping");
        return;
    }
    let mut full = f.store.clone();
    full.fail_set = Some(libc::ENOSPC);
    full.write(&f.file, &map(json!({"rating": 5}))).unwrap();
    assert_eq!(f.store.read(&f.file).rating, 5);
    f.store.write(&f.file, &map(json!({"rating": 4}))).unwrap();
    assert_eq!(f.store.read(&f.file).rating, 4);
    let data: Value = serde_json::from_str(&read(&f.store.path)).unwrap();
    assert!(data.get(pyos::as_uri(&f.file)).is_none());
}

#[test]
fn test_permission_errors_are_not_silently_fallback_writes() {
    let f = fixture();
    let mut denied = f.store.clone();
    denied.fail_set = Some(libc::EACCES);
    let error = denied.write(&f.file, &map(json!({"rating": 3}))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Permission);
    assert!(!f.store.path.exists());
}

#[test]
fn test_untrusted_metadata_values_are_bounded_and_unknown_fields_rejected() {
    let f = fixture();
    let comment = format!("\0{}", "a".repeat(9000));
    f.store.write(&f.file, &map(json!({"tags": ["\n a\u{0}", null], "rating": 99, "comment": comment}))).unwrap();
    let data = f.store.read(&f.file);
    assert_eq!(data.tags, ["a"]);
    assert_eq!(data.rating, 5);
    assert_eq!(data.comment.chars().count(), 8192);
    let error = f.store.write(&f.file, &map(json!({"unknown": "no"}))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.to_string(), "Unknown annotation fields: unknown");
    assert_eq!(normal_metadata(&json!({"rating": true, "tags": "x"})), EMPTY_METADATA);
    assert_eq!(normal_metadata(&json!({"rating": -3})).rating, 0);
}

#[test]
fn test_xattr_payload_is_compact_json() {
    let f = fixture();
    if set_raw_xattr(&f.file, b"{}").is_err() {
        eprintln!("Test filesystem lacks xattrs; skipping");
        return;
    }
    f.store.write(&f.file, &map(json!({"tags": ["é"], "rating": 1}))).unwrap();
    let (fd, _) = open_item(&f.file).unwrap();
    let raw = f.store.get_xattr(&fd).unwrap();
    assert_eq!(String::from_utf8(raw).unwrap(), "{\"tags\":[\"é\"],\"rating\":1,\"comment\":\"\"}");
}

#[test]
fn test_annotations_follow_real_file_operation_copy() {
    let f = fixture();
    if set_raw_xattr(&f.file, b"{\"tags\":[\"Copy me\"],\"rating\":4}").is_err() {
        eprintln!("Test filesystem lacks xattrs; skipping");
        return;
    }
    let destination = f.root.join("copied");
    std::fs::create_dir(&destination).unwrap();
    let operations = Operations::new();
    let job = operations.start("copy", &[gio::File::for_path(&f.file)], Some(&gio::File::for_path(&destination)),
                               Callbacks::new());
    let result = job.wait(Some(Duration::from_secs(5))).expect("copy finished");
    assert!(result.success(), "{:?}", result.failures);
    let copied = destination.join("example.txt");
    assert_eq!(f.store.read(&copied).tags, ["Copy me"]);
    assert_eq!(f.store.read(&copied).rating, 4);
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(root).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"])
        .args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

fn git_fixture() -> Option<(TempDir, PathBuf)> {
    if suite_core::shell::which("git").is_none() {
        eprintln!("Git is not installed; skipping");
        return None;
    }
    let temp = TempDir::new("files-git-");
    let root = pyos::realpath(temp.path());
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Files tests"]);
    git(&root, &["config", "user.email", "files@example.invalid"]);
    Some((temp, root))
}

#[test]
fn test_changed_files_subdirectories_and_rename_nul_parsing() {
    let Some((_temp, root)) = git_fixture() else { return };
    std::fs::create_dir(root.join("nested")).unwrap();
    write(&root.join("nested/old name.txt"), "Original\n");
    let modified = root.join("notes.txt");
    write(&modified, "Before\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "Initial", "--no-verify"]);
    let renamed = root.join("nested/new\nname.txt");
    git(&root, &["mv", "nested/old name.txt", "nested/new\nname.txt"]);
    write(&modified, "After\n");
    write(&root.join("untracked file"), "");
    let index = || std::fs::metadata(root.join(".git/index")).unwrap();
    let before = (index().mtime(), index().mtime_nsec());
    let statuses = status_for_directory(root.as_path(), GIT_TIMEOUT);
    assert_eq!(statuses[&renamed], "Renamed (staged)");
    assert_eq!(statuses[&modified], "Modified");
    assert_eq!(statuses[&root.join("untracked file")], "Untracked");
    assert_eq!(statuses[&root.join("nested")], "Contains changes");
    assert_eq!(before, (index().mtime(), index().mtime_nsec()));
    let nested = status_for_directory(root.join("nested"), GIT_TIMEOUT);
    assert_eq!(nested, HashMap::from([(renamed.clone(), "Renamed (staged)".to_string())]));
}

#[test]
fn test_fsmonitor_hook_never_runs_and_errors_return_empty() {
    let Some((_temp, root)) = git_fixture() else { return };
    let marker = root.join("hook-was-run");
    let hook = root.join("hook");
    write(&hook, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&root, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    write(&root.join("file"), "x");
    let statuses = status_for_directory(root.as_path(), GIT_TIMEOUT);
    assert_eq!(statuses[&root.join("file")], "Untracked");
    assert!(!marker.exists());
    assert!(status_for_directory(root.as_path(), Duration::ZERO).is_empty(), "a timeout returns {{}}");
    assert!(status_for_directory("sftp://remote.invalid/work", GIT_TIMEOUT).is_empty());
    let plain = TempDir::new("files-nogit-");
    assert!(status_for_directory(plain.path(), GIT_TIMEOUT).is_empty());
}

#[test]
fn test_git_labels() {
    assert_eq!(git_label("UU"), "Conflict");
    assert_eq!(git_label("??"), "Untracked");
    assert_eq!(git_label("!!"), "Ignored");
    assert_eq!(git_label("M "), "Modified (staged)");
    assert_eq!(git_label(" M"), "Modified");
    assert_eq!(git_label("MM"), "Modified");
    assert_eq!(git_label("A "), "Added (staged)");
    assert_eq!(git_label(" T"), "Type changed");
    assert_eq!(git_label("RM"), "Renamed");
}

#[test]
fn test_configuration_and_expansion_preserve_literal_shell_characters() {
    let temp = TempDir::new("files-actions-");
    let root = temp.path().to_path_buf();
    let config = root.join("actions.json");
    write(&config, &json!([
        {"name": "Inspect", "argv": ["program", "%F", "uri=%u", "100%%"]},
        {"name": "Invalid", "argv": "program %F"},
        {"name": "  ", "argv": ["program"]},
        {"name": "Percent", "argv": ["pro%gram"]},
        {"name": " Folders ", "argv": ["x"], "directories": false, "files": 1},
    ]).to_string());
    let actions = CustomActions::new(Some(config)).list();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[1], CustomAction { name: "Folders".into(), argv: vec!["x".into()], directories: false, files: false });
    let first = root.join("$(not-a-command); one.txt");
    let second = root.join("two.txt");
    let argv = CustomActions::expand(&actions[0], &[first.clone().into(), second.clone().into()]).unwrap();
    let expected: Vec<OsString> = vec!["program".into(), first.clone().into(), second.clone().into(),
                                       format!("uri={}", pyos::as_uri(&first)).into(), "100%".into()];
    assert_eq!(argv, expected);
    let error = CustomActions::expand(&actions[0], &["sftp://host/path".into()]).unwrap_err();
    assert_eq!(error.to_string(), "This action requires local files");
    let bad = CustomAction { argv: vec!["program".into(), "bad%F".into()], ..Default::default() };
    let error = CustomActions::expand(&bad, &[first.clone().into()]).unwrap_err();
    assert_eq!(error.to_string(), "Unsupported action placeholder %F");
    assert_eq!(CustomActions::expand(&bad, &[]).unwrap_err().to_string(), "Select at least one file");
    let uris = CustomAction { argv: vec!["p".into(), "%U".into()], ..Default::default() };
    assert_eq!(CustomActions::expand(&uris, &["sftp://host/path".into()]).unwrap(),
               vec![OsString::from("p"), OsString::from("sftp://host/path")]);
    assert!(CustomActions::new(Some(root.join("missing.json"))).list().is_empty());
}

#[test]
fn test_actions_launch_detached_without_shell() {
    let temp = TempDir::new("files-actions-");
    let root = temp.path().to_path_buf();
    let output = root.join("result");
    let selected = root.join("semi;$(literal)");
    // `%%s` expands to a literal `%s` for printf; the selected path stays one argument.
    let script = "printf '%%s\\n' \"$@\" > \"$0.args\"; cut -d' ' -f1,6 /proc/$$/stat > \"$0.ids\"";
    let action = CustomAction {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into(), output.to_string_lossy().into_owned(), "%F".into()],
        ..Default::default()
    };
    let mut child = CustomActions::invoke(&action, &[selected.clone().into()], Some(&root.as_path().into())).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(read(&root.join("result.args")), format!("{}\n", selected.display()));
    let ids = read(&root.join("result.ids"));
    let ids: Vec<&str> = ids.split_whitespace().collect();
    assert_eq!(ids[0], ids[1], "the action leads its own session");
}
