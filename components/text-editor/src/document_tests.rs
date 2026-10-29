//! Port of test_document.py (DocumentTests).
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Temp {
    root: PathBuf,
    path: PathBuf,
}

impl Temp {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("text-editor-doc-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let path = root.join("sample.txt");
        Self { root, path }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn target(path: &Path) -> Target {
    Target::Path(path.to_owned())
}

fn set_mtime_ns(path: &Path, seconds: i64, nanos: i64) {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let times = [libc::timespec { tv_sec: seconds, tv_nsec: nanos }, libc::timespec { tv_sec: seconds, tv_nsec: nanos }];
    assert_eq!(unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) }, 0);
}

#[test]
fn test_bom_and_crlf_preserved_after_edit() {
    let t = Temp::new("bom");
    for (codec, bom) in [(Codec::Utf8, BOM_UTF8), (Codec::Utf16Le, BOM_UTF16_LE), (Codec::Utf16Be, BOM_UTF16_BE), (Codec::Utf32Le, BOM_UTF32_LE)] {
        let mut data = bom.to_vec();
        data.extend(codec.encode("One\r\nαβ\r\n").unwrap());
        fs::write(&t.path, &data).unwrap();
        let mut document = Document::load(&target(&t.path), None).unwrap();
        assert_eq!(document.text, "One\nαβ\n", "{codec:?}");
        let text = document.text.clone() + "Three\n";
        document.save(&text, None, false, None, None).unwrap();
        let mut expected = bom.to_vec();
        expected.extend(codec.encode("One\r\nαβ\r\nThree\r\n").unwrap());
        assert_eq!(fs::read(&t.path).unwrap(), expected, "{codec:?}");
    }
}

#[test]
fn test_no_edit_preserves_mixed_endings_byte_for_byte() {
    let t = Temp::new("mixed");
    let raw = b"one\r\ntwo\nthree\rfour";
    fs::write(&t.path, raw).unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    assert!(document.mixed);
    let text = document.text.clone();
    document.save(&text, None, false, None, None).unwrap();
    assert_eq!(fs::read(&t.path).unwrap(), raw);
}

#[test]
fn test_non_utf8_is_not_silently_replaced() {
    let t = Temp::new("cp1252");
    fs::write(&t.path, b"caf\xe9").unwrap();
    assert!(matches!(Document::load(&target(&t.path), None), Err(DocError::Decode(_))));
    let mut document = Document::load(&target(&t.path), Some("windows-1252")).unwrap();
    assert_eq!(document.text, "café");
    assert_eq!(document.encoding, "cp1252");
    document.save("café!", None, false, Some("utf-8"), None).unwrap();
    assert_eq!(fs::read(&t.path).unwrap(), "café!".as_bytes());
}

#[test]
fn test_unrepresentable_encoding_does_not_touch_file() {
    let t = Temp::new("unrepresentable");
    fs::write(&t.path, b"original").unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    let error = document.save("🖥", None, false, Some("iso-8859-1"), None).unwrap_err();
    assert!(matches!(error, DocError::Encode(_)), "{error:?}");
    assert_eq!(fs::read(&t.path).unwrap(), b"original");
}

#[test]
fn test_conflict_with_modified_timestamp_restored() {
    let t = Temp::new("timestamp");
    fs::write(&t.path, b"original").unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    let meta = fs::metadata(&t.path).unwrap();
    use std::os::unix::fs::MetadataExt;
    let (seconds, nanos) = (meta.mtime(), meta.mtime_nsec());
    fs::write(&t.path, b"external").unwrap();
    set_mtime_ns(&t.path, seconds, nanos);
    assert!(matches!(document.save("my edits", None, false, None, None), Err(DocError::Conflict(_))));
    assert_eq!(fs::read(&t.path).unwrap(), b"external");
}

#[test]
fn test_deleted_file_is_not_recreated_over_external_change() {
    let t = Temp::new("deleted");
    fs::write(&t.path, "original").unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    fs::remove_file(&t.path).unwrap();
    assert!(matches!(document.save("replacement", None, false, None, None), Err(DocError::Conflict(_))));
    assert!(!t.path.exists());
}

#[test]
fn test_symlink_is_preserved_and_retarget_is_conflict() {
    let t = Temp::new("symlink");
    let actual = t.root.join("actual");
    fs::write(&actual, "start").unwrap();
    std::os::unix::fs::symlink(&actual, &t.path).unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    document.save("edited", None, false, None, None).unwrap();
    assert!(fs::symlink_metadata(&t.path).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&actual).unwrap(), "edited");
    let alternative = t.root.join("alternative");
    fs::write(&alternative, "other").unwrap();
    fs::remove_file(&t.path).unwrap();
    std::os::unix::fs::symlink(&alternative, &t.path).unwrap();
    assert!(matches!(document.save("unsafe", None, false, None, None), Err(DocError::Conflict(_))));
    assert_eq!(fs::read_to_string(&alternative).unwrap(), "other");
}

#[test]
fn test_existing_save_as_requires_overwrite() {
    let t = Temp::new("overwrite");
    fs::write(&t.path, "external").unwrap();
    let mut document = Document::default();
    assert_eq!(document.save("new", Some(&target(&t.path)), false, None, None), Err(DocError::Exists(EXISTS.into())));
    document.save("new", Some(&target(&t.path)), true, None, None).unwrap();
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "new");
}

#[test]
fn test_file_mode_and_missing_final_newline_preserved() {
    let t = Temp::new("mode");
    fs::write(&t.path, "start").unwrap();
    fs::set_permissions(&t.path, fs::Permissions::from_mode(0o640)).unwrap();
    let mut document = Document::load(&target(&t.path), None).unwrap();
    document.save("finish", None, false, None, None).unwrap();
    assert_eq!(fs::metadata(&t.path).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(fs::read(&t.path).unwrap(), b"finish");
}

#[test]
fn test_binary_input_refused() {
    assert!(matches!(decode(b"binary\x00text", None), Err(DocError::Value(_))));
}

#[test]
fn test_new_file_and_line_ending_conversion() {
    let t = Temp::new("endings");
    let mut document = Document::default();
    document.save("line\nnext\n", Some(&target(&t.path)), false, None, Some("CRLF")).unwrap();
    assert_eq!(fs::read(&t.path).unwrap(), b"line\r\nnext\r\n");
    let text = document.text.clone();
    document.save(&text, None, false, None, Some("LF")).unwrap();
    assert_eq!(fs::read(&t.path).unwrap(), b"line\nnext\n");
}

#[test]
fn test_conversion_to_utf16_is_detectable_on_next_open() {
    let t = Temp::new("utf16");
    let mut document = Document::default();
    document.save("Hello α", Some(&target(&t.path)), false, Some("utf-16-le"), None).unwrap();
    assert!(fs::read(&t.path).unwrap().starts_with(BOM_UTF16_LE));
    let reopened = Document::load(&target(&t.path), None).unwrap();
    assert_eq!(reopened.text, "Hello α");
    assert_eq!(Document::load(&target(&t.path), Some(&reopened.encoding)).unwrap().text, "Hello α");
}

#[test]
fn test_new_command_line_path_cannot_overwrite_file_created_later() {
    let t = Temp::new("later");
    let mut document = Document::with_path(t.path.clone());
    fs::write(&t.path, "created by another program").unwrap();
    assert!(matches!(document.save("my draft", None, false, None, None), Err(DocError::Exists(_))));
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "created by another program");
}

#[test]
fn test_new_file_creation_race_never_replaces_other_writer() {
    let t = Temp::new("race");
    LINK_HOOK.with(|hook| {
        hook.replace(Some(Box::new(|source: &Path, destination: &Path| {
            fs::write(destination, "another writer won").unwrap();
            fs::hard_link(source, destination)
        })))
    });
    let result = Document::with_path(t.path.clone()).save("my draft", None, false, None, None);
    LINK_HOOK.with(|hook| hook.replace(None));
    assert!(matches!(result, Err(DocError::Exists(ref m)) if m.starts_with("[Errno 17] File exists: '")), "{result:?}");
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "another writer won");
    let entries: Vec<PathBuf> = fs::read_dir(&t.root).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(entries, vec![t.path.clone()]);
}

#[test]
fn test_new_local_file_is_private_like_mkstemp() {
    // os.link() publishes the mkstemp file, so new files are 0600.
    let t = Temp::new("private");
    Document::default().save("x", Some(&target(&t.path)), false, None, None).unwrap();
    assert_eq!(fs::metadata(&t.path).unwrap().permissions().mode() & 0o777, 0o600);
    let missing = t.root.join("missing/file.txt");
    let error = Document::with_path(missing).save("x", None, false, None, None).unwrap_err();
    assert!(matches!(error, DocError::Os(ref m) if m.starts_with("[Errno 2] No such file or directory: '")), "{error:?}");
}

#[test]
fn test_gio_location_load_save_and_etag_conflict() {
    // The generic GIO path serves GVfs SFTP/SMB files; a local Gio.File
    // exercises the same calls without a server.
    let t = Temp::new("gio");
    fs::write(&t.path, "remote original\n").unwrap();
    let file = gio::File::for_path(&t.path);
    let mut document = Document::load_gio(&file, None).unwrap();
    assert_eq!(document.text, "remote original\n");
    assert!(document.path.is_none());
    assert!(document.remote.as_ref().unwrap().equal(&file));
    assert_eq!(document.name().as_deref(), Some("sample.txt"));
    assert_eq!(document.identifier(), Some(file.uri().to_string()));
    assert!(document.same_location(&file));
    assert!(!document.changed_on_disk());
    document.save("remote edit\n", None, false, None, None).unwrap();
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "remote edit\n");
    set_mtime_ns(&t.path, 0, 1); // Change the etag as another writer would.
    fs::write(&t.path, "server side change\n").unwrap();
    assert!(document.changed_on_disk());
    assert!(matches!(document.save("my second edit\n", None, false, None, None), Err(DocError::Conflict(_))));
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "server side change\n");
}

#[test]
fn test_gio_new_file_is_created_exclusively_and_save_as_needs_overwrite() {
    let t = Temp::new("gio-new");
    let created = gio::File::for_path(t.root.join("new.txt"));
    Document::default().save_gio("first\n", &created, false, None, None).unwrap();
    assert_eq!(fs::read_to_string(t.root.join("new.txt")).unwrap(), "first\n");
    fs::write(&t.path, "other").unwrap();
    assert!(matches!(Document::default().save_gio("mine", &gio::File::for_path(&t.path), false, None, None), Err(DocError::Exists(_))));
    assert_eq!(fs::read_to_string(&t.path).unwrap(), "other");
    CREATE_HOOK.with(|hook| hook.replace(Some(DocError::Exists("raced".into()))));
    let raced = Document::default().save_gio("mine", &gio::File::for_path(t.root.join("raced.txt")), false, None, None);
    CREATE_HOOK.with(|hook| hook.replace(None));
    assert_eq!(raced, Err(DocError::Exists("raced".into())));
}

#[test]
fn test_gio_deleted_file_is_conflict_and_uris_resolve() {
    let t = Temp::new("gio-deleted");
    fs::write(&t.path, "x").unwrap();
    let mut document = Document::load_gio(&gio::File::for_path(&t.path), None).unwrap();
    fs::remove_file(&t.path).unwrap();
    assert!(matches!(document.save("y", None, false, None, None), Err(DocError::Conflict(_))));
    assert!(!t.path.exists());
    let uri = gio::File::for_path(&t.path).uri().to_string();
    let (local, _) = location(&Target::Arg(uri));
    assert_eq!(local, Some(t.path.clone()));
    let (remote, file) = location(&Target::Arg("sftp://example.invalid/home/me/notes.txt".into()));
    if remote.is_none() {
        // Without GVfs FUSE the URI stays a non-native location.
        assert_eq!(file.uri_scheme().as_deref(), Some("sftp"));
    }
}

#[test]
fn test_decode_endings_and_bom_choice() {
    let d = decode(b"a\r\nb\rc\r\n", None).unwrap();
    assert_eq!((d.newline.as_str(), d.mixed, d.text.as_str()), ("CRLF", true, "a\nb\nc\n"));
    // Ties prefer CRLF, then CR, then LF (Python dict order).
    assert_eq!(decode(b"a\nb\r", None).unwrap().newline, "CR");
    assert_eq!(decode(b"", None).unwrap().newline, "LF");
    let d = decode(b"\xef\xbb\xbfhi", Some("utf-8-sig")).unwrap();
    assert_eq!((d.encoding.as_str(), d.bom.as_slice(), d.text.as_str()), ("utf-8-sig", BOM_UTF8, "hi"));
    // A UTF-16 BOM is not stripped when another encoding is requested.
    assert!(decode(b"\xff\xfea\x00", Some("utf-8")).is_err());
    assert!(matches!(decode(b"x", Some("klingon")), Err(DocError::Lookup(_))));
    assert_eq!(Document::default().encode("a\nb", None, Some("CR")).unwrap(), b"a\rb");
    assert!(matches!(Document::default().encode("a", None, Some("NL")), Err(DocError::Value(_))));
    assert!(matches!(Document::default().save("x", None, false, None, None), Err(DocError::Value(_))));
}
