//! Port of ~/tessel-bluetooth/shared/test_wallpaper_import.py. Never invokes real
//! authentication: "pkexec" is a fake script in $TMPDIR that logs its argv
//! and runs the rest of the command without elevation (Python's
//! `run(command[2:])`).
#![cfg(feature = "fileops")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tessel_shell::wallpaper_chooser::import::{self, ImportError, Importer, RemovalError, COPY_CHUNK_SIZE};

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp() -> Temp {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = std::env::temp_dir();
    assert!(!tmp.starts_with("/tmp"), "set TMPDIR to a scratch dir");
    let dir = tmp.join(format!("wallpaper-import-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Temp(dir)
}

struct Fx {
    _t: Temp,
    root: PathBuf,
    folder: PathBuf,
    source: PathBuf,
}

fn fx() -> Fx {
    let t = temp();
    let root = t.0.clone();
    let folder = root.join("collection");
    fs::create_dir(&folder).unwrap();
    let source = root.join("photo.png");
    fs::write(&source, b"test media contents").unwrap();
    Fx { _t: t, root, folder, source }
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    v.sort();
    v
}

/// A fake pkexec: logs argv (one per line, then `---`), runs `before`, then
/// either exits with `code` or runs the command without elevation.
fn fake_pkexec(root: &Path, before: &str, code: Option<i32>) -> (PathBuf, PathBuf) {
    let script = root.join("fake-pkexec");
    let log = root.join("pkexec.log");
    let tail = match code {
        Some(code) => format!("exit {code}"),
        None => "shift\nexec \"$@\"".to_string(),
    };
    fs::write(&script, format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\necho --- >> '{}'\n{before}\n{tail}\n", log.display(), log.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

fn invocations(log: &Path) -> Vec<Vec<String>> {
    let Ok(text) = fs::read_to_string(log) else { return vec![] };
    let mut calls = vec![vec![]];
    for line in text.lines() {
        if line == "---" {
            calls.push(vec![]);
        } else {
            calls.last_mut().unwrap().push(line.to_string());
        }
    }
    calls.pop();
    calls
}

fn protected(f: &Fx, before: &str, code: Option<i32>) -> (Importer, PathBuf) {
    let (pkexec, log) = fake_pkexec(&f.root, before, code);
    let importer = Importer { system_folders: vec![f.folder.clone()], pkexec, deny_unprivileged: true, ..Importer::default() };
    (importer, log)
}

fn plain() -> Importer {
    // Never authenticate in tests, even by mistake.
    Importer { pkexec: PathBuf::from("/nonexistent/pkexec"), ..Importer::default() }
}

#[test]
fn copy_keeps_original_and_existing_wallpapers() {
    let f = fx();
    let first = plain().import_wallpaper(&f.source, &f.folder, None).unwrap();
    fs::write(&f.source, b"a different wallpaper").unwrap();
    let second = plain().import_wallpaper(&f.source, &f.folder, None).unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), b"test media contents");
    assert_eq!(fs::read(&second).unwrap(), fs::read(&f.source).unwrap());
    assert_eq!(second.file_name().unwrap(), "photo (2).png");
}

#[test]
fn existing_symlink_is_never_followed_or_replaced() {
    let f = fx();
    let original = f.root.join("unrelated.png");
    fs::write(&original, b"keep me").unwrap();
    let link = f.folder.join("photo.png");
    std::os::unix::fs::symlink(&original, &link).unwrap();
    let result = plain().import_wallpaper(&f.source, &f.folder, None).unwrap();
    assert_ne!(result, link);
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(&original).unwrap(), b"keep me");
}

#[test]
fn file_already_in_collection_does_not_duplicate() {
    let f = fx();
    let result = plain().import_wallpaper(&f.source, &f.folder, None).unwrap();
    let again = plain().import_wallpaper(&result, &f.folder, None).unwrap();
    assert_eq!(result, again);
    assert_eq!(listing(&f.folder), vec![result]);
}

#[test]
fn cancelled_copy_removes_only_partial_destination() {
    let f = fx();
    fs::write(&f.source, vec![b'x'; COPY_CHUNK_SIZE + 1]).unwrap();
    let calls = std::cell::Cell::new(0);
    let cancelled = || {
        calls.set(calls.get() + 1);
        calls.get() >= 4
    };
    let error = plain().import_wallpaper(&f.source, &f.folder, Some(&cancelled)).unwrap_err();
    assert!(error.is_cancelled());
    assert!(listing(&f.folder).is_empty());
    assert_eq!(fs::metadata(&f.source).unwrap().len(), (COPY_CHUNK_SIZE + 1) as u64);
    // Cancelling after the first chunk was written also removes the partial file.
    let calls = std::cell::Cell::new(0);
    let later = || {
        calls.set(calls.get() + 1);
        calls.get() >= 5
    };
    assert!(plain().import_wallpaper(&f.source, &f.folder, Some(&later)).unwrap_err().is_cancelled());
    assert!(listing(&f.folder).is_empty());
}

#[test]
fn filename_is_a_visible_basename() {
    let f = fx();
    let source = f.root.join(".-test\nphoto.png");
    fs::write(&source, b"image").unwrap();
    let result = plain().import_wallpaper(&source, &f.folder, None).unwrap();
    assert_eq!(result.file_name().unwrap(), "-test_photo.png");
    assert_eq!(result.parent().unwrap(), fs::canonicalize(&f.folder).unwrap());
    assert_eq!(import::safe_filename("..."), "wallpaper");
    assert_eq!(import::safe_filename(&format!("{}.png", "é".repeat(200))).len(), 180 + 4);
}

#[test]
fn protected_arbitrary_directory_does_not_request_elevation() {
    let f = fx();
    let (pkexec, log) = fake_pkexec(&f.root, "", None);
    let importer = Importer { pkexec, deny_unprivileged: true, ..Importer::default() };
    let error = importer.import_wallpaper(&f.source, &f.folder, None).unwrap_err();
    assert!(error.message().contains("permission"), "{error}");
    assert!(invocations(&log).is_empty());
}

#[test]
fn protected_copy_uses_readable_snapshot_and_standard_utility() {
    let f = fx();
    let (importer, log) = protected(&f, &format!("printf 'changed after snapshot' > '{}'", f.source.display()), None);
    let result = importer.import_wallpaper(&f.source, &f.folder, None).unwrap();
    assert_eq!(fs::read(&result).unwrap(), b"test media contents");
    let calls = invocations(&log);
    assert_eq!(calls.len(), 1);
    let command = &calls[0];
    assert_eq!(command[..2], ["--disable-internal-agent".to_string(), "/usr/bin/cp".to_string()]);
    assert_eq!(command[2..5], ["--update=none-fail", "--no-preserve=mode,ownership", "--"].map(String::from));
    let snapshot = PathBuf::from(&command[command.len() - 2]);
    assert_ne!(snapshot, f.source);
    assert!(!snapshot.exists());
}

#[test]
fn protected_copy_race_does_not_clobber_destination() {
    let f = fx();
    let target = f.folder.join("photo.png");
    let (importer, _log) = protected(&f, &format!("printf 'another import won the race' > '{}'", target.display()), None);
    let error = importer.import_wallpaper(&f.source, &f.folder, None).unwrap_err();
    assert!(!error.is_cancelled());
    assert_eq!(fs::read(&target).unwrap(), b"another import won the race");
}

#[test]
fn authentication_cancellation_and_denial_are_distinct() {
    let f = fx();
    for (code, cancelled) in [(126, true), (127, false)] {
        let (importer, _) = protected(&f, "", Some(code));
        let error = importer.import_wallpaper(&f.source, &f.folder, None).unwrap_err();
        assert_eq!(error.is_cancelled(), cancelled, "{code}: {error}");
        if !cancelled {
            assert!(matches!(error, ImportError::Error(_)));
        }
    }
    assert!(listing(&f.folder).is_empty());
}

#[test]
fn protected_batch_authenticates_once_and_reserves_duplicate_names() {
    let f = fx();
    fs::write(f.folder.join("photo.png"), b"existing wallpaper").unwrap();
    let another = f.root.join("another");
    fs::create_dir(&another).unwrap();
    let second = another.join("photo.png");
    fs::write(&second, b"second wallpaper").unwrap();
    let third = f.root.join("--a movie with 'quotes'.mp4");
    fs::write(&third, b"video fixture").unwrap();
    let sources = vec![f.source.clone(), second.clone(), third.clone()];
    let contents: Vec<Vec<u8>> = sources.iter().map(|p| fs::read(p).unwrap()).collect();
    let (importer, log) = protected(&f, &format!("printf 'original changed after snapshot' > '{}'", f.source.display()), None);
    let (paths, errors, cancelled) = importer.import_wallpapers(&sources, &f.folder, None, None);
    let calls = invocations(&log);
    assert_eq!(calls.len(), 1, "authenticates once");
    let command = &calls[0];
    assert_eq!(command.last().unwrap(), &fs::canonicalize(&f.folder).unwrap().display().to_string());
    let dash = command.iter().position(|a| a == "--").unwrap();
    let snapshots: Vec<PathBuf> = command[dash + 1..command.len() - 1].iter().map(PathBuf::from).collect();
    assert_eq!(snapshots.len(), 3);
    let names: std::collections::HashSet<_> = snapshots.iter().map(|p| p.file_name().unwrap().to_owned()).collect();
    assert_eq!(names.len(), 3);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!cancelled);
    let got: Vec<String> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(got, vec!["photo (2).png".to_string(), "photo (3).png".into(), "--a movie with 'quotes'.mp4".into()]);
    assert_eq!(paths.iter().map(|p| fs::read(p).unwrap()).collect::<Vec<_>>(), contents);
    assert_eq!(fs::read(f.folder.join("photo.png")).unwrap(), b"existing wallpaper");
    assert!(sources.iter().all(|p| p.exists()));
    assert!(snapshots.iter().all(|p| !p.exists()));
}

#[test]
fn batch_authentication_cancel_or_denial_does_not_prompt_for_remaining_files() {
    let f = fx();
    let second = f.root.join("second.png");
    fs::write(&second, b"second").unwrap();
    for code in [126, 127] {
        let _ = fs::remove_file(f.root.join("pkexec.log"));
        let (importer, log) = protected(&f, "", Some(code));
        let (paths, errors, cancelled) = importer.import_wallpapers(&[f.source.clone(), second.clone()], &f.folder, None, None);
        let calls = invocations(&log);
        assert_eq!(calls.len(), 1);
        assert!(paths.is_empty());
        assert_eq!(cancelled, code == 126);
        assert_eq!(!errors.is_empty(), code == 127);
        assert!(listing(&f.folder).is_empty());
        let dash = calls[0].iter().position(|a| a == "--").unwrap();
        assert!(calls[0][dash + 1..calls[0].len() - 1].iter().all(|p| !Path::new(p).exists()));
    }
}

#[test]
fn batch_race_reports_partial_copy_without_overwriting_or_retrying_authentication() {
    let f = fx();
    let second = f.root.join("second.png");
    fs::write(&second, b"second").unwrap();
    let third = f.root.join("third.png");
    fs::write(&third, b"third").unwrap();
    let race_target = f.folder.join("second.png");
    let (importer, log) = protected(&f, &format!("printf 'KEEP!!' > '{}'", race_target.display()), None);
    let (paths, errors, cancelled) = importer.import_wallpapers(&[f.source.clone(), second, third], &f.folder, None, None);
    assert_eq!(invocations(&log).len(), 1);
    let folder = fs::canonicalize(&f.folder).unwrap();
    assert_eq!(paths, vec![folder.join("photo.png"), folder.join("third.png")]);
    assert!(!cancelled);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("second.png"));
    assert_eq!(fs::read(&race_target).unwrap(), b"KEEP!!");
}

#[test]
fn batch_cancellation_while_staging_never_requests_authentication() {
    let f = fx();
    let second = f.root.join("second.png");
    fs::write(&second, b"second").unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let (mut importer, log) = protected(&f, "", None);
    let set = flag.clone();
    importer.after_copy = Some(Arc::new(move || set.store(true, Ordering::SeqCst)));
    let cancelled = || flag.load(Ordering::SeqCst);
    let (paths, errors, was_cancelled) = importer.import_wallpapers(&[f.source.clone(), second], &f.folder, Some(&cancelled), None);
    assert!(invocations(&log).is_empty());
    assert!(was_cancelled);
    assert!(paths.is_empty() && errors.is_empty());
    assert!(listing(&f.folder).is_empty());
}

#[test]
fn writable_batch_copies_without_authentication() {
    let f = fx();
    let second = f.root.join("second.png");
    fs::write(&second, b"second").unwrap();
    let (pkexec, log) = fake_pkexec(&f.root, "", None);
    let importer = Importer { pkexec, system_folders: vec![f.folder.clone()], ..Importer::default() };
    let (paths, errors, cancelled) = importer.import_wallpapers(&[f.source.clone(), second.clone()], &f.folder, None, None);
    assert!(invocations(&log).is_empty());
    assert!(!cancelled);
    assert!(errors.is_empty());
    assert_eq!(paths.iter().map(|p| fs::read(p).unwrap()).collect::<Vec<_>>(), vec![fs::read(&f.source).unwrap(), fs::read(&second).unwrap()]);
}

#[test]
fn validation_errors_are_per_file() {
    let f = fx();
    let bad = f.root.join("Broken.png");
    fs::write(&bad, b"x").unwrap();
    let validate = |p: &Path| if p.ends_with("Broken.png") { Err("Source disappeared".to_string()) } else { Ok(()) };
    let (paths, errors, _) = plain().import_wallpapers(&[f.source.clone(), bad], &f.folder, None, Some(&validate));
    assert_eq!(paths.len(), 1);
    assert_eq!(errors, vec!["Broken.png: Source disappeared".to_string()]);
}

// ---------------------------------------------------------------- removal

struct Rx {
    f: Fx,
    wallpaper: PathBuf,
    identity: import::Identity,
}

fn rx() -> Rx {
    use std::os::unix::fs::MetadataExt;
    let f = fx();
    let wallpaper = f.folder.join("wallpaper.png");
    fs::write(&wallpaper, b"fixture").unwrap();
    let m = fs::symlink_metadata(&wallpaper).unwrap();
    Rx { f, wallpaper, identity: (m.dev(), m.ino()) }
}

#[test]
fn remove_only_selected_file() {
    let r = rx();
    let other = r.f.folder.join("keep.png");
    fs::write(&other, b"keep").unwrap();
    let result = plain().remove_wallpaper(&r.wallpaper, &r.f.folder, Some(r.identity), None).unwrap();
    assert_eq!(result, fs::canonicalize(&r.f.folder).unwrap().join("wallpaper.png"));
    assert!(!r.wallpaper.exists());
    assert_eq!(fs::read(&other).unwrap(), b"keep");
}

#[test]
fn symlink_removes_entry_without_deleting_target() {
    let r = rx();
    let link = r.f.folder.join("linked.png");
    std::os::unix::fs::symlink(&r.wallpaper, &link).unwrap();
    plain().remove_wallpaper(&link, &r.f.folder, None, None).unwrap();
    assert!(fs::symlink_metadata(&link).is_err());
    assert!(r.wallpaper.is_file());
}

#[test]
fn cancellation_preserves_wallpaper_without_authentication() {
    let r = rx();
    let (importer, log) = protected(&r.f, "", None);
    let error = importer.remove_wallpaper(&r.wallpaper, &r.f.folder, None, Some(&|| true)).unwrap_err();
    assert!(error.is_cancelled());
    assert!(invocations(&log).is_empty());
    assert!(r.wallpaper.is_file());
}

#[test]
fn replaced_file_during_confirmation_is_not_removed() {
    let r = rx();
    let original = r.f.folder.join("original.png");
    fs::rename(&r.wallpaper, &original).unwrap();
    fs::write(&r.wallpaper, b"replacement").unwrap();
    let error = plain().remove_wallpaper(&r.wallpaper, &r.f.folder, Some(r.identity), None).unwrap_err();
    assert!(error.message().contains("changed"), "{error}");
    assert_eq!(fs::read(&r.wallpaper).unwrap(), b"replacement");
    assert!(original.is_file());
}

#[test]
fn rejects_files_outside_collection_nonmedia_and_directories() {
    let r = rx();
    let outside = r.f.root.join("outside.png");
    fs::write(&outside, b"keep").unwrap();
    let nonmedia = r.f.folder.join("notes.txt");
    fs::write(&nonmedia, b"keep").unwrap();
    let directory = r.f.folder.join("folder.png");
    fs::create_dir(&directory).unwrap();
    for source in [&outside, &nonmedia, &directory] {
        assert!(matches!(plain().remove_wallpaper(source, &r.f.folder, None, None), Err(RemovalError::Error(_))), "{source:?}");
        assert!(source.exists());
    }
}

#[test]
fn protected_arbitrary_directory_never_authenticates() {
    let r = rx();
    let (pkexec, log) = fake_pkexec(&r.f.root, "", None);
    let importer = Importer { pkexec, deny_unprivileged: true, ..Importer::default() };
    let error = importer.remove_wallpaper(&r.wallpaper, &r.f.folder, None, None).unwrap_err();
    assert!(error.message().contains("permission"), "{error}");
    assert!(invocations(&log).is_empty());
    assert!(r.wallpaper.exists());
}

#[test]
fn system_removal_uses_standard_nonrecursive_unlink() {
    let r = rx();
    let (importer, log) = protected(&r.f, "", None);
    importer.remove_wallpaper(&r.wallpaper, &r.f.folder, Some(r.identity), None).unwrap();
    let target = fs::canonicalize(&r.f.folder).unwrap().join("wallpaper.png");
    assert_eq!(invocations(&log), vec![vec!["--disable-internal-agent".to_string(), "/usr/bin/unlink".into(), "--".into(), target.display().to_string()]]);
    assert!(!r.wallpaper.exists());
}

#[test]
fn system_auth_cancellation_denial_and_unlink_failure_preserve_file() {
    let r = rx();
    for (code, cancelled) in [(126, true), (127, false), (1, false)] {
        let (importer, _) = protected(&r.f, "echo failure >&2", Some(code));
        let error = importer.remove_wallpaper(&r.wallpaper, &r.f.folder, None, None).unwrap_err();
        assert_eq!(error.is_cancelled(), cancelled, "{code}");
        if code == 1 {
            assert_eq!(error.message(), "Could not remove the wallpaper. failure");
        }
        assert!(r.wallpaper.exists());
    }
}

#[test]
fn dash_filename_stays_positional_argument() {
    let r = rx();
    let target = r.f.folder.join("--not-an-option.png");
    fs::rename(&r.wallpaper, &target).unwrap();
    plain().remove_wallpaper(&target, &r.f.folder, None, None).unwrap();
    assert!(!target.exists());
}
