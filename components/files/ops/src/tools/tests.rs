//! Port of test_file_tools.py: archive safety, metadata and rename planning in
//! private fixtures.

use super::*;
use crate::testutil::{names, read, write, TempDir};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    source: PathBuf,
}

fn fixture() -> Fixture {
    let temp = TempDir::new("files-tools-");
    let root = temp.path().to_path_buf();
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    write(&source.join("document.txt"), "hello\n");
    std::fs::create_dir(source.join("nested")).unwrap();
    write(&source.join("nested/résumé.txt"), "Résumé");
    std::fs::create_dir(source.join("empty")).unwrap();
    Fixture { _temp: temp, root, source }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
    let mut archive = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, data) in entries {
        archive.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        archive.write_all(data).unwrap();
    }
    archive.finish().unwrap();
}

fn tar_with(path: &Path, entries: &[(&[u8], tar::EntryType, u32, &[u8])]) {
    let mut builder = tar::Builder::new(File::create(path).unwrap());
    for (name, kind, mode, data) in entries {
        let mut header = tar::Header::new_ustar();
        let ustar = header.as_ustar_mut().unwrap();
        ustar.name[..name.len()].copy_from_slice(name);
        header.set_entry_type(*kind);
        header.set_mode(*mode);
        header.set_size(data.len() as u64);
        if matches!(kind, tar::EntryType::Symlink | tar::EntryType::Link) {
            header.set_link_name("../outside").unwrap();
        }
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.finish().unwrap();
}

#[test]
fn test_archive_roundtrip_formats() {
    let f = fixture();
    for extension in ["zip", "tar", "tar.gz", "tar.bz2", "tar.xz", "tgz", "tbz2", "txz"] {
        let archive = f.root.join(format!("test.{extension}"));
        assert_eq!(create_archive(&[f.source.clone()], &archive, None, None).unwrap(), archive);
        let destination = f.root.join(format!("out-{extension}"));
        let created = extract_archive(&archive, &destination, None, None).unwrap();
        assert_eq!(read(&destination.join("source/document.txt")), "hello\n");
        assert_eq!(read(&destination.join("source/nested/résumé.txt")), "Résumé");
        assert!(destination.join("source/empty").is_dir());
        assert!(created.contains(&destination.join("source/nested/résumé.txt")));
        assert_eq!(created[0], destination.join("source"));
    }
    // Python-compatible tar layout: padded to 10240-byte records, directories end in "/".
    let plain = std::fs::read(f.root.join("test.tar")).unwrap();
    assert_eq!(plain.len() % 10240, 0);
    let mut archive = tar::Archive::new(File::open(f.root.join("test.tar")).unwrap());
    let listed: Vec<String> = archive.entries().unwrap()
        .map(|e| String::from_utf8(e.unwrap().path_bytes().into_owned()).unwrap()).collect();
    assert_eq!(listed, ["source/", "source/document.txt", "source/empty/", "source/nested/", "source/nested/résumé.txt"]);
    let rejected = create_archive(&[f.source.clone()], &f.root.join("test.rar"), None, None).unwrap_err();
    assert_eq!(rejected.to_string(), "Use .zip, .tar, .tar.gz, .tar.bz2, or .tar.xz");
}

#[test]
fn test_create_never_overwrites_and_rejects_inside_sources() {
    let f = fixture();
    let archive = f.root.join("test.zip");
    std::fs::write(&archive, b"keep").unwrap();
    let error = create_archive(&[f.source.clone()], &archive, None, None).unwrap_err();
    assert_eq!(error.kind, ErrorKind::FileExists);
    assert_eq!(std::fs::read(&archive).unwrap(), b"keep");
    let error = create_archive(&[f.source.clone()], &f.source.join("output.zip"), None, None).unwrap_err();
    assert!(error.is_value_error());
    assert_eq!(error.to_string(), "Choose an archive destination outside the selected folders");
    assert!(!f.source.join("output.zip").exists());
    let error = create_archive(&[f.source.clone(), f.root.join("other/source")], &f.root.join("x.zip"), None, None)
        .unwrap_err();
    assert_eq!(error.to_string(), "Archive source names must be unique");
}

#[test]
fn test_cancelled_create_removes_partial_archive() {
    let f = fixture();
    for name in ["test.zip", "test.tar.gz"] {
        let archive = f.root.join(name);
        let cancel = AtomicBool::new(false);
        let mut progress = |_: u64, _: u64| cancel.store(true, Ordering::SeqCst);
        let error = create_archive(&[f.source.clone()], &archive, Some(&cancel), Some(&mut progress)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert!(!archive.exists());
    }
}

#[test]
fn test_zip_traversal_rejected_before_any_writes() {
    let f = fixture();
    for name in ["../escaped", "/absolute", "safe/../../escape", "C:/escape", "..\\escape"] {
        let archive = f.root.join("evil.zip");
        let _ = std::fs::remove_file(&archive);
        zip_with(&archive, &[("legitimate.txt", b"contents"), (name, b"escaped")]);
        let error = extract_archive(&archive, &f.root.join("out"), None, None).unwrap_err();
        assert!(error.is_value_error(), "{name}: {error}");
        assert_eq!(error.to_string(), format!("Unsafe archive member: {name}"));
        assert!(!f.root.join("out").exists());
    }
}

#[test]
fn test_archive_links_rejected() {
    let f = fixture();
    let zip_path = f.root.join("links.zip");
    let mut archive = zip::ZipWriter::new(File::create(&zip_path).unwrap());
    archive.add_symlink("link", "../outside", zip::write::SimpleFileOptions::default()).unwrap();
    archive.finish().unwrap();
    let error = extract_archive(&zip_path, &f.root.join("zip-out"), None, None).unwrap_err();
    assert_eq!(error.to_string(), "Unsupported archive link, special file or encrypted entry: link");
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link, tar::EntryType::Fifo] {
        let path = f.root.join("links.tar");
        tar_with(&path, &[(b"link", kind, 0o644, b"")]);
        let error = extract_archive(&path, &f.root.join("tar-out"), None, None).unwrap_err();
        assert_eq!(error.to_string(), "Archive links and special files are not extracted: link");
    }
    assert!(!f.root.join("tar-out").exists());
    symlink(&f.root, f.source.join("link")).unwrap();
    let error = create_archive(&[f.source.clone()], &f.root.join("create-links.zip"), None, None).unwrap_err();
    assert!(error.is_value_error());
    assert!(error.to_string().starts_with("Archive creation does not follow symbolic links: "));
    assert!(!f.root.join("create-links.zip").exists());
}

#[test]
fn test_existing_symlink_directory_is_not_followed() {
    let f = fixture();
    let archive = f.root.join("test.zip");
    zip_with(&archive, &[("folder/new.txt", b"do not escape")]);
    let destination = f.root.join("out");
    std::fs::create_dir(&destination).unwrap();
    symlink(&f.source, destination.join("folder")).unwrap();
    let error = extract_archive(&archive, &destination, None, None).unwrap_err();
    assert!(error.is_os_error(), "{error}");
    assert_eq!(error.to_string(), "[Errno 20] Not a directory: 'folder'");
    assert!(!f.source.join("new.txt").exists());
    assert!(std::fs::symlink_metadata(destination.join("folder")).unwrap().file_type().is_symlink());
}

#[test]
fn test_existing_file_preserved_and_new_files_rolled_back() {
    let f = fixture();
    let archive = f.root.join("test.zip");
    zip_with(&archive, &[("new/sub.txt", b"temporary"), ("existing.txt", b"replace")]);
    let destination = f.root.join("out");
    std::fs::create_dir(&destination).unwrap();
    write(&destination.join("existing.txt"), "keep me");
    let error = extract_archive(&archive, &destination, None, None).unwrap_err();
    assert_eq!(error.kind, ErrorKind::FileExists);
    assert_eq!(error.to_string(), "[Errno 17] File exists: 'existing.txt'");
    assert_eq!(read(&destination.join("existing.txt")), "keep me");
    assert!(!destination.join("new").exists());
}

#[test]
fn test_cancelled_extract_removes_only_new_files() {
    let f = fixture();
    for name in ["test.zip", "test.tar.xz"] {
        let archive = f.root.join(name);
        create_archive(&[f.source.clone()], &archive, None, None).unwrap();
        let destination = f.root.join(format!("out-{name}"));
        std::fs::create_dir(&destination).unwrap();
        write(&destination.join("keep.txt"), "keep");
        let cancel = AtomicBool::new(false);
        let mut progress = |_: u64, _: u64| cancel.store(true, Ordering::SeqCst);
        let error = extract_archive(&archive, &destination, Some(&cancel), Some(&mut progress)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.to_string(), "Operation cancelled");
        assert_eq!(names(&destination), ["keep.txt"]);
    }
}

#[test]
fn test_tar_dot_directory_and_strips_privilege_bits() {
    let f = fixture();
    let archive = f.root.join("test.tar");
    tar_with(&archive, &[(b".", tar::EntryType::Directory, 0o755, b""),
                         (b"./executable", tar::EntryType::Regular, 0o6755, b"data")]);
    let destination = f.root.join("out");
    extract_archive(&archive, &destination, None, None).unwrap();
    assert_eq!(mode(&destination.join("executable")), 0o755);
    // ZIP entries without Unix attributes become 0644.
    let zipped = f.root.join("plain.zip");
    let mut writer = zip::ZipWriter::new(File::create(&zipped).unwrap());
    writer.start_file("dos.txt", zip::write::SimpleFileOptions::default().system(zip::System::Dos)).unwrap();
    writer.write_all(b"x").unwrap();
    writer.finish().unwrap();
    extract_archive(&zipped, &f.root.join("zip-out"), None, None).unwrap();
    assert_eq!(mode(&f.root.join("zip-out/dos.txt")), 0o644);
}

#[test]
fn test_duplicate_members_and_file_parents_rejected() {
    let f = fixture();
    for (second, message) in [(&b"file"[..], "Duplicate archive member: file"),
                              (&b"file/child"[..], "An archive file is also used as a folder")] {
        let archive = f.root.join("test.tar");
        tar_with(&archive, &[(b"file", tar::EntryType::Regular, 0o644, b"x"), (second, tar::EntryType::Regular, 0o644, b"x")]);
        let error = extract_archive(&archive, &f.root.join("out"), None, None).unwrap_err();
        assert!(error.is_value_error());
        assert_eq!(error.to_string(), message);
        assert!(!f.root.join("out").exists());
    }
}

#[test]
fn test_batch_rename_numbering_and_no_mutation() {
    let f = fixture();
    let first = f.source.join("document.txt");
    let second = f.source.join("nested/résumé.txt");
    let plan = plan_batch_rename(&[first.clone(), second.clone()], "Page ###", 3).unwrap();
    assert_eq!(plan, vec![(first.clone(), first.with_file_name("Page 003.txt")),
                          (second.clone(), second.with_file_name("Page 004.txt"))]);
    assert!(first.exists());
    assert!(!plan[0].1.exists());
    let folder = plan_batch_rename(&[f.source.join("empty")], "Dir #", 0).unwrap();
    assert_eq!(folder[0].1, f.source.join("Dir 0"));
    let error = plan_batch_rename(&[first.clone(), f.source.join("nested/../document.txt")], "Same", 1).unwrap_err();
    assert_eq!(error.to_string(), "Include # in the name to number multiple files");
    assert_eq!(plan_batch_rename(&[first.clone()], "x", -1).unwrap_err().to_string(),
               "The starting number must not be negative");
    assert_eq!(plan_batch_rename(&[first.clone(), first.clone()], "x#", 1).unwrap_err().to_string(),
               "Select one or more distinct files");
}

#[test]
fn test_batch_rename_collision_and_invalid_names() {
    let f = fixture();
    let path = f.source.join("document.txt");
    write(&f.source.join("Page 1.txt"), "keep");
    let error = plan_batch_rename(&[path.clone()], "Page #", 1).unwrap_err();
    assert_eq!(error.kind, ErrorKind::FileExists);
    assert_eq!(error.to_string(), format!("A file already exists at {}", f.source.join("Page 1.txt").display()));
    for name in ["../escape", "a/b", "a\\b", "", ".."] {
        let error = plan_batch_rename(&[path.clone()], name, 1).unwrap_err();
        assert!(error.is_value_error(), "{name}");
        assert_eq!(error.to_string(), "Enter a file name without path separators");
    }
}

#[test]
fn test_metadata_checksums_and_no_symlink_permission_changes() {
    use sha2::Digest;
    let f = fixture();
    let path = f.source.join("document.txt");
    let data = std::fs::read(&path).unwrap();
    let digest = checksums(&path, &["sha256", "md5"], None).unwrap();
    let expected: String = sha2::Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest[0], ("sha256".to_string(), expected));
    assert_eq!(digest[1], ("md5".to_string(), "b1946ac92492d2347c6235b4d2611184".to_string()));
    assert_eq!(checksums(&path, &["nope"], None).unwrap_err().to_string(), "unsupported hash type nope");
    set_permissions(&path, 0).unwrap();
    set_permissions(&path, 0o640).unwrap();
    assert_eq!(file_metadata(&path).unwrap().mode, 0o640);
    assert_eq!(file_metadata(&path).unwrap().mode_text, "-rw-r-----");
    assert!(set_permissions(&path, 0o10000).is_err());
    let link = f.source.join("link");
    symlink(&path, &link).unwrap();
    let error = set_permissions(&link, 0o777).unwrap_err();
    assert!(error.is_value_error());
    assert_eq!(error.to_string(), "Permissions can only be edited for regular files and folders");
    assert_eq!(file_metadata(&path).unwrap().mode, 0o640);
    let meta = file_metadata(&link).unwrap();
    assert_eq!(meta.link_target, Some(path.clone()));
    assert!(meta.is_symlink && !meta.is_directory);
    assert_eq!(meta.name, "link");
    let error = checksums(&link, &["sha256"], None).unwrap_err();
    assert!(error.is_os_error());
    assert!(error.to_string().starts_with("[Errno 40] Too many levels of symbolic links: '"));
    // Owner changes to the current ids are always permitted.
    let me = file_metadata(&path).unwrap();
    set_owner(&path, Principal::Id(-1), Principal::Name(me.gid.to_string())).unwrap();
    let error = set_owner(&path, Principal::from("no-such-user-for-files-tests"), Principal::Id(-1)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Key);
    assert_eq!(error.to_string(), "\"getpwnam(): name not found: 'no-such-user-for-files-tests'\"");
}

#[test]
fn test_filemode_matches_python() {
    assert_eq!(filemode(0o100644), "-rw-r--r--");
    assert_eq!(filemode(0o040755), "drwxr-xr-x");
    assert_eq!(filemode(0o120777), "lrwxrwxrwx");
    assert_eq!(filemode(0o104755), "-rwsr-xr-x");
    assert_eq!(filemode(0o102644), "-rw-r-Sr--");
    assert_eq!(filemode(0o041777), "drwxrwxrwt");
    assert_eq!(filemode(0o140600), "srw-------");
    assert_eq!(filemode(0o010600), "prw-------");
}

#[test]
fn test_recursive_size_does_not_follow_links_or_double_count_hardlinks() {
    let f = fixture();
    let first = f.source.join("document.txt");
    std::fs::hard_link(&first, f.source.join("hardlink")).unwrap();
    symlink(&f.source, f.source.join("loop")).unwrap();
    let result = recursive_size(&[f.source.clone()], None).unwrap();
    let size = |p: &Path| std::fs::symlink_metadata(p).unwrap().len();
    let expected = size(&first) + size(&f.source.join("nested/résumé.txt")) + size(&f.source.join("loop"));
    assert_eq!(result.bytes, expected);
    assert_eq!(result.directories, 3);
    assert_eq!(result.unreadable, 0);
    let missing = recursive_size(&[f.root.join("missing")], None).unwrap();
    assert_eq!(missing.unreadable, 1);
    let cancel = AtomicBool::new(true);
    assert_eq!(recursive_size(&[f.source.clone()], Some(&cancel)).unwrap_err().kind, ErrorKind::Cancelled);
}

#[test]
fn test_acl_roundtrip_and_invalid_input() {
    if !acl_available() {
        eprintln!("acl utilities are unavailable; skipping");
        return;
    }
    let f = fixture();
    let path = f.source.join("document.txt");
    write_acl(&path, "user::rw-\ngroup::r--\nother::---\n").unwrap();
    let text = read_acl(&path).unwrap();
    assert!(text.contains("user::rw-"), "{text}");
    assert_eq!(mode(&path), 0o640);
    let error = write_acl(&path, "not a valid acl").unwrap_err();
    assert!(error.is_os_error());
    assert_eq!(mode(&path), 0o640);
    assert_eq!(write_acl(&path, "  \n").unwrap_err().to_string(), "Enter a valid access control list");
}
