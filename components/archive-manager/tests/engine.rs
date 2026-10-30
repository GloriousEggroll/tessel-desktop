//! Engine checks against the real tools and libarchive: every plugin, format
//! and operation on fixtures under `std::env::temp_dir()` (run with
//! TMPDIR=/run/user/1000/cbt/rust-archive-manager so they stay private).
//! Formats whose tool is missing are skipped with a note.

use archive_manager::archive::{CreateOptions, Ctx, Error};
use archive_manager::ops::{self, ExtractOptions, Resolution};
use archive_manager::settings::plugins::Kind;
use archive_manager::settings::Settings;
use std::path::{Path, PathBuf};

fn root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wayarchive-engine-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn have(tool: &str) -> bool {
    suite_core::shell::which(tool).is_some()
}

/// src/a.txt, src/dir/b.txt, src/dir/sub/c.txt, src/odd name [1].txt
fn sources(dir: &Path) -> Vec<PathBuf> {
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("dir/sub")).unwrap();
    std::fs::write(src.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(src.join("dir/b.txt"), "bravo\n".repeat(100)).unwrap();
    std::fs::write(src.join("dir/sub/c.txt"), "charlie\n").unwrap();
    std::fs::write(src.join("odd name [1].txt"), "odd\n").unwrap();
    vec![src.join("a.txt"), src.join("dir"), src.join("odd name [1].txt")]
}

fn only(plugin: &str) -> Settings {
    let mut s = Settings::default();
    for p in archive_manager::settings::plugins::PLUGINS {
        if p.id != plugin {
            s.disabled_plugins.push(p.id.to_string());
        }
    }
    s
}

fn paths(archive: &ops::Archive) -> Vec<String> {
    let mut p: Vec<String> = archive.listing.entries.iter().map(|e| format!("{}{}", e.path, if e.is_dir { "/" } else { "" })).collect();
    p.sort();
    p
}

fn never(_: &ops::Conflict) -> Resolution {
    panic!("unexpected conflict")
}

fn extract_all(archive: &ops::Archive, dest: &Path) {
    ops::extract(archive, None, dest, &ExtractOptions { preserve_paths: true, ..Default::default() }, &Ctx::new(), &mut never).unwrap();
}

const ALL: [&str; 6] = ["a.txt", "dir/", "dir/b.txt", "dir/sub/", "dir/sub/c.txt", "odd name [1].txt"];

/// Create → list → extract → edit round trip for one format/plugin.
fn round_trip(kind: Kind, plugin: &str, ext: &str, options: CreateOptions) {
    let dir = root(&format!("{plugin}-{}", ext.replace('.', "_")));
    let src = sources(&dir);
    let settings = only(plugin);
    let path = dir.join(format!("test.{ext}"));
    ops::create(&path, kind, &src, &options, &settings, &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: create: {e}"));
    let ctx = Ctx::new().with_password(options.password.clone());
    let archive = ops::open(&path, &settings, &ctx).unwrap_or_else(|e| panic!("{plugin} {ext}: open: {e}"));
    assert_eq!(archive.kind, kind, "{ext}");
    assert_eq!(archive.reader, plugin);
    assert_eq!(paths(&archive), ALL, "{plugin} {ext}");
    assert_eq!(archive.listing.find("dir/b.txt").unwrap().size, 600);
    let out = dir.join("out");
    extract_all(&archive, &out);
    assert_eq!(std::fs::read_to_string(out.join("dir/sub/c.txt")).unwrap(), "charlie\n");
    assert_eq!(std::fs::read_to_string(out.join("odd name [1].txt")).unwrap(), "odd\n");
    assert!(std::fs::read_dir(&out).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".wayarchive")));
    ops::test(&archive, &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: test: {e}"));
    // selected entry, without paths
    let flat = dir.join("flat");
    ops::extract(&archive, Some(&["dir".to_string()]), &flat, &ExtractOptions { preserve_paths: false, ..Default::default() }, &Ctx::new(), &mut never).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&flat).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["b.txt", "c.txt"], "{plugin} {ext}");
    // edits
    let open = || ops::open(&path, &settings, &ctx).unwrap();
    ops::rename(&open(), "a.txt", "renamed.txt", &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: rename: {e}"));
    ops::move_into_folder(&open(), &["odd name [1].txt".to_string()], "dir/sub", &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: move: {e}"));
    ops::create_folder(&open(), "dir", "new folder", &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: mkdir: {e}"));
    ops::copy_into_folder(&open(), &["renamed.txt".to_string()], "dir", &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: copy: {e}"));
    let extra = dir.join("extra.txt");
    std::fs::write(&extra, "extra").unwrap();
    ops::add(&open(), &[extra], "dir/new folder", &CreateOptions::default(), &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: add: {e}"));
    ops::delete(&open(), &["dir/b.txt".to_string()], &Ctx::new()).unwrap_or_else(|e| panic!("{plugin} {ext}: delete: {e}"));
    let after = open();
    assert_eq!(paths(&after), ["dir/", "dir/new folder/", "dir/new folder/extra.txt", "dir/renamed.txt", "dir/sub/", "dir/sub/c.txt",
                               "dir/sub/odd name [1].txt", "renamed.txt"], "{plugin} {ext}");
    let out2 = dir.join("out2");
    extract_all(&after, &out2);
    assert_eq!(std::fs::read_to_string(out2.join("dir/new folder/extra.txt")).unwrap(), "extra");
    assert_eq!(std::fs::read_to_string(out2.join("dir/renamed.txt")).unwrap(), "alpha\n");
    if options.password.is_some() {
        assert!(after.listing.encrypted, "{plugin} {ext}: encrypted");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn libarchive_tar_family() {
    let mut kinds = vec![(Kind::Tar, "tar"), (Kind::TarGz, "tar.gz"), (Kind::TarBz2, "tar.bz2"), (Kind::TarXz, "tar.xz"),
                         (Kind::TarZst, "tar.zst"), (Kind::TarLz4, "tar.lz4"), (Kind::TarLzma, "tar.lzma"), (Kind::TarZ, "tar.Z")];
    if have("lzop") {
        kinds.push((Kind::TarLzo, "tar.lzo"));
    }
    if have("lzip") {
        kinds.push((Kind::TarLz, "tar.lz"));
    }
    for (kind, ext) in kinds {
        round_trip(kind, "libarchive", ext, CreateOptions { level: Some(6), ..Default::default() });
    }
}

#[test]
fn sevenzip_formats_and_passwords() {
    if !have("7z") {
        eprintln!("skipped: no 7z");
        return;
    }
    round_trip(Kind::SevenZip, "cli7z", "7z", CreateOptions { level: Some(5), method: Some("LZMA2".into()), ..Default::default() });
    round_trip(Kind::Zip, "cli7z", "zip", CreateOptions { method: Some("Deflate".into()), comment: Some("a comment".into()), ..Default::default() });
    round_trip(Kind::SevenZip, "cli7z", "7z", CreateOptions { password: Some("sésame".into()), encrypt_header: true, ..Default::default() });
    round_trip(Kind::Zip, "cli7z", "zip", CreateOptions { password: Some("pw".into()), encryption: Some("AES256".into()), ..Default::default() });
    round_trip(Kind::Zip, "cli7z", "zip", CreateOptions { password: Some("pw".into()), encryption: Some("ZipCrypto".into()), ..Default::default() });
}

#[test]
fn infozip() {
    if !have("zip") || !have("unzip") {
        eprintln!("skipped: no Info-ZIP");
        return;
    }
    round_trip(Kind::Zip, "clizip", "zip", CreateOptions { level: Some(9), ..Default::default() });
    round_trip(Kind::Zip, "clizip", "zip", CreateOptions { password: Some("pw".into()), ..Default::default() });
}

#[test]
fn header_encryption_needs_password_and_rejects_wrong_one() {
    if !have("7z") {
        return;
    }
    let dir = root("hdr");
    let src = sources(&dir);
    let path = dir.join("locked.7z");
    let settings = Settings::default();
    ops::create(&path, Kind::SevenZip, &src, &CreateOptions { password: Some("right".into()), encrypt_header: true, ..Default::default() }, &settings, &Ctx::new()).unwrap();
    assert_eq!(ops::open(&path, &settings, &Ctx::new()).err(), Some(Error::PasswordNeeded));
    assert_eq!(ops::open(&path, &settings, &Ctx::new().with_password(Some("wrong".into()))).err(), Some(Error::WrongPassword));
    assert!(ops::open(&path, &settings, &Ctx::new().with_password(Some("right".into()))).is_ok());
    // file-level encryption: listing works, extraction asks
    let file_only = dir.join("files.zip");
    ops::create(&file_only, Kind::Zip, &src, &CreateOptions { password: Some("right".into()), ..Default::default() }, &settings, &Ctx::new()).unwrap();
    let archive = ops::open(&file_only, &settings, &Ctx::new()).unwrap();
    assert!(archive.listing.encrypted);
    let err = ops::extract(&archive, None, &dir.join("o"), &ExtractOptions { preserve_paths: true, ..Default::default() }, &Ctx::new(), &mut never).err();
    assert_eq!(err, Some(Error::PasswordNeeded));
    let err = ops::extract(&archive, None, &dir.join("o"), &ExtractOptions { preserve_paths: true, ..Default::default() },
                           &Ctx::new().with_password(Some("bad".into())), &mut never).err();
    assert_eq!(err, Some(Error::WrongPassword));
    // unar reads 7z with header encryption too
    if have("lsar") {
        let listing = archive_manager::backend::get("cliunarchiver").unwrap()
            .list(&path, Kind::SevenZip, &[path.clone()], &Ctx::new());
        assert_eq!(listing.err(), Some(Error::PasswordNeeded));
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn split_volumes_open_read_only() {
    if !have("7z") {
        return;
    }
    let dir = root("split");
    let src = dir.join("big.bin");
    let data: Vec<u8> = (0..200_000u32).flat_map(|i| (i.wrapping_mul(2654435761)).to_le_bytes()).collect();
    std::fs::write(&src, &data).unwrap();
    let settings = Settings::default();
    for (kind, ext) in [(Kind::SevenZip, "7z"), (Kind::Zip, "zip")] {
        let path = dir.join(format!("split.{ext}"));
        ops::create(&path, kind, std::slice::from_ref(&src), &CreateOptions { level: Some(0), volume_size: Some(300_000), ..Default::default() }, &settings, &Ctx::new()).unwrap();
        let first = dir.join(format!("split.{ext}.001"));
        assert!(first.exists() && dir.join(format!("split.{ext}.002")).exists(), "{ext} volumes");
        let archive = ops::open(&first, &settings, &Ctx::new()).unwrap();
        assert_eq!(archive.kind, kind);
        assert!(archive.volumes.len() >= 3);
        assert!(archive.read_only());
        let out = dir.join(format!("out-{ext}"));
        extract_all(&archive, &out);
        assert_eq!(std::fs::read(out.join("big.bin")).unwrap(), data);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn single_file_compressors() {
    let dir = root("single");
    let file = dir.join("notes.txt");
    std::fs::write(&file, "single file contents\n").unwrap();
    let settings = Settings::default();
    for (kind, ext) in [(Kind::Gz, "gz"), (Kind::Bz2, "bz2"), (Kind::Xz, "xz"), (Kind::Zst, "zst"), (Kind::Lz4, "lz4"), (Kind::Lzma, "lzma")] {
        let path = dir.join(format!("notes.txt.{ext}"));
        ops::create(&path, kind, std::slice::from_ref(&file), &CreateOptions::default(), &settings, &Ctx::new()).unwrap_or_else(|e| panic!("{ext}: {e}"));
        let archive = ops::open(&path, &settings, &Ctx::new()).unwrap();
        assert_eq!(archive.kind, kind);
        assert_eq!(archive.reader, "libsinglefile");
        assert!(archive.read_only());
        assert_eq!(paths(&archive), ["notes.txt"]);
        assert_eq!(archive.listing.entries[0].size, 21);
        let out = dir.join(format!("out-{ext}"));
        extract_all(&archive, &out);
        assert_eq!(std::fs::read_to_string(out.join("notes.txt")).unwrap(), "single file contents\n");
    }
    // a gzip file made by gzip(1)
    let other = dir.join("plain.log");
    std::fs::write(&other, "log\n").unwrap();
    assert!(std::process::Command::new("gzip").arg("-k").arg(&other).status().unwrap().success());
    let archive = ops::open(&dir.join("plain.log.gz"), &settings, &Ctx::new()).unwrap();
    assert_eq!(paths(&archive), ["plain.log"]);
    let err = ops::create(&dir.join("two.gz"), Kind::Gz, &[file.clone(), other.clone()], &CreateOptions::default(), &settings, &Ctx::new());
    assert!(matches!(err, Err(Error::Unsupported(_))));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn conflicts_overwrite_skip_rename_all_cancel() {
    let dir = root("conflicts");
    let src = sources(&dir);
    let settings = Settings::default();
    let path = dir.join("c.tar.gz");
    ops::create(&path, Kind::TarGz, &src, &CreateOptions::default(), &settings, &Ctx::new()).unwrap();
    let archive = ops::open(&path, &settings, &Ctx::new()).unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(out.join("dir/sub")).unwrap();
    let preserve = ExtractOptions { preserve_paths: true, ..Default::default() };
    let reset = || {
        std::fs::write(out.join("a.txt"), "old a").unwrap();
        std::fs::write(out.join("dir/b.txt"), "old b").unwrap();
    };
    // Skip one, overwrite the other
    reset();
    let mut asked = Vec::new();
    ops::extract(&archive, None, &out, &preserve, &Ctx::new(), &mut |c| {
        asked.push(c.target.file_name().unwrap().to_string_lossy().into_owned());
        if asked.len() == 1 { Resolution::Skip } else { Resolution::Overwrite }
    }).unwrap();
    asked.sort();
    assert_eq!(asked, ["a.txt", "b.txt"]);
    let a = std::fs::read_to_string(out.join("a.txt")).unwrap();
    let b = std::fs::read_to_string(out.join("dir/b.txt")).unwrap();
    assert!((a == "old a") != (b == "old b"), "one skipped, one overwritten");
    // Rename
    reset();
    let report = ops::extract(&archive, Some(&["a.txt".to_string()]), &out, &preserve, &Ctx::new(), &mut |_| Resolution::Rename("a-new.txt".into())).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("a-new.txt")).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "old a");
    assert_eq!(report.top, [out.join("a-new.txt")]);
    // Overwrite all asks once
    reset();
    let mut count = 0;
    ops::extract(&archive, None, &out, &preserve, &Ctx::new(), &mut |_| { count += 1; Resolution::OverwriteAll }).unwrap();
    assert_eq!(count, 1);
    assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "alpha\n");
    // Skip all
    reset();
    let mut count = 0;
    let report = ops::extract(&archive, None, &out, &preserve, &Ctx::new(), &mut |_| { count += 1; Resolution::SkipAll }).unwrap();
    assert_eq!(count, 1);
    assert_eq!(report.skipped, 4);
    // Cancel
    reset();
    let err = ops::extract(&archive, None, &out, &preserve, &Ctx::new(), &mut |_| Resolution::Cancel).err();
    assert_eq!(err, Some(Error::Cancelled));
    assert!(std::fs::read_dir(&out).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".wayarchive")), "staging removed");
    // Subfolder + strip (drag and drop from inside dir/)
    let dnd = dir.join("dnd");
    ops::extract(&archive, Some(&["dir/sub".to_string()]), &dnd, &ExtractOptions { preserve_paths: true, strip: Some("dir".into()), subfolder: None },
                 &Ctx::new(), &mut never).unwrap();
    assert_eq!(std::fs::read_to_string(dnd.join("sub/c.txt")).unwrap(), "charlie\n");
    assert!(!dnd.join("dir").exists());
    let sub = dir.join("sub");
    let report = ops::extract(&archive, None, &sub, &ExtractOptions { preserve_paths: true, subfolder: Some("c".into()), ..Default::default() }, &Ctx::new(), &mut never).unwrap();
    assert_eq!(report.folder, sub.join("c"));
    assert!(sub.join("c/dir/sub/c.txt").exists());
    assert!(ops::needs_subfolder(&archive.listing));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancel_stops_a_job() {
    let dir = root("cancel");
    let big = dir.join("big");
    std::fs::write(&big, vec![7u8; 8 << 20]).unwrap();
    let ctx = Ctx::new();
    ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let err = ops::create(&dir.join("x.tar.xz"), Kind::TarXz, &[big], &CreateOptions::default(), &Settings::default(), &ctx).err();
    assert_eq!(err, Some(Error::Cancelled));
    assert!(!dir.join("x.tar.xz").exists());
    let _ = std::fs::remove_dir_all(dir);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A stored (uncompressed) RAR 4 archive.
fn rar4(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = b"Rar!\x1a\x07\x00".to_vec();
    let block = |body: &[u8]| -> Vec<u8> {
        let crc = (crc32(body) & 0xffff) as u16;
        let mut v = crc.to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    };
    out.extend(block(&[0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0]));
    for (name, data, dir) in files {
        let mut body = vec![0x74u8];
        let flags: u16 = 0x8000 | if *dir { 0x00e0 } else { 0 };
        body.extend_from_slice(&flags.to_le_bytes());
        // RAR 4 stores '\\' separators (WinRAR and rar(1) alike).
        let name = name.replace('/', "\\");
        body.extend_from_slice(&((32 + name.len()) as u16).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.push(3);
        body.extend_from_slice(&crc32(data).to_le_bytes());
        body.extend_from_slice(&0x5d5d_6000u32.to_le_bytes());
        body.push(29);
        body.push(0x30);
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        let attr: u32 = if *dir { 0o40755 } else { 0o100644 };
        body.extend_from_slice(&attr.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        out.extend(block(&body));
        out.extend_from_slice(data);
    }
    out.extend(block(&[0x7b, 0x00, 0x40, 7, 0]));
    out
}

#[test]
fn rar_read_only_with_unar_and_libarchive() {
    let dir = root("rar");
    let path = dir.join("comic.rar");
    std::fs::write(&path, rar4(&[("pages", b"", true), ("pages/01.txt", b"page one\n", false), ("cover.txt", b"cover\n", false)])).unwrap();
    for plugin in ["cliunarchiver", "libarchive"] {
        if plugin == "cliunarchiver" && !have("lsar") {
            continue;
        }
        let settings = only(plugin);
        let archive = ops::open(&path, &settings, &Ctx::new()).unwrap_or_else(|e| panic!("{plugin}: {e}"));
        assert_eq!(archive.kind, Kind::Rar);
        assert_eq!(archive.reader, plugin);
        assert!(archive.read_only());
        assert_eq!(paths(&archive), ["cover.txt", "pages/", "pages/01.txt"], "{plugin}");
        let out = dir.join(format!("out-{plugin}"));
        ops::extract(&archive, Some(&["pages".to_string()]), &out, &ExtractOptions { preserve_paths: true, ..Default::default() }, &Ctx::new(), &mut never).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("pages/01.txt")).unwrap(), "page one\n");
        assert!(!out.join("cover.txt").exists());
        ops::test(&archive, &Ctx::new()).unwrap();
        assert!(matches!(ops::delete(&archive, &["cover.txt".into()], &Ctx::new()), Err(Error::Unsupported(_))));
    }
    // cbr is a rar
    std::fs::copy(&path, dir.join("comic.cbr")).unwrap();
    assert_eq!(ops::open(&dir.join("comic.cbr"), &Settings::default(), &Ctx::new()).unwrap().kind, Kind::Rar);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn read_only_images_and_packages() {
    let dir = root("images");
    let src = dir.join("tree");
    std::fs::create_dir_all(src.join("docs")).unwrap();
    std::fs::write(src.join("docs/readme.txt"), "read me\n").unwrap();
    let settings = Settings::default();
    let mut checked = Vec::new();
    let check = |path: &Path, kind: Kind, entry: &str| {
        let archive = ops::open(path, &settings, &Ctx::new()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(archive.kind, kind, "{}", path.display());
        assert!(archive.read_only(), "{}", path.display());
        assert!(archive.listing.find(entry).is_some(), "{}: {:?}", path.display(), paths(&archive));
        let out = path.with_extension("out");
        extract_all(&archive, &out);
        assert!(out.join(entry).exists(), "{} extracted", path.display());
    };
    if have("xorriso") {
        let iso = dir.join("disc.iso");
        let ok = std::process::Command::new("xorriso").args(["-as", "mkisofs", "-quiet", "-R", "-o"]).arg(&iso).arg(&src)
            .stderr(std::process::Stdio::null()).status().unwrap().success();
        assert!(ok);
        check(&iso, Kind::Iso, "docs/readme.txt");
        checked.push("iso");
    }
    if have("cpio") {
        let cpio = dir.join("tree.cpio");
        let file = std::fs::File::create(&cpio).unwrap();
        let mut child = std::process::Command::new("cpio").args(["-o", "-H", "newc", "--quiet"]).current_dir(&src)
            .stdin(std::process::Stdio::piped()).stdout(file).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"docs\ndocs/readme.txt\n").unwrap();
        assert!(child.wait().unwrap().success());
        check(&cpio, Kind::Cpio, "docs/readme.txt");
        checked.push("cpio");
    }
    if have("ar") {
        let deb = dir.join("pkg.deb");
        std::fs::write(dir.join("debian-binary"), "2.0\n").unwrap();
        std::fs::write(dir.join("control.tar.gz"), "").unwrap();
        assert!(std::process::Command::new("ar").arg("rc").arg(&deb).arg("debian-binary").arg("control.tar.gz").current_dir(&dir).status().unwrap().success());
        check(&deb, Kind::Deb, "debian-binary");
        let lib = dir.join("lib.a");
        assert!(std::process::Command::new("ar").arg("rc").arg(&lib).arg("debian-binary").current_dir(&dir).status().unwrap().success());
        check(&lib, Kind::Ar, "debian-binary");
        checked.push("deb/ar");
    }
    if have("mksquashfs") && have("7z") {
        let sq = dir.join("fs.squashfs");
        assert!(std::process::Command::new("mksquashfs").arg(&src).arg(&sq).args(["-quiet", "-no-progress"]).stdout(std::process::Stdio::null()).status().unwrap().success());
        check(&sq, Kind::Squashfs, "docs/readme.txt");
        checked.push("squashfs");
    }
    eprintln!("read-only formats checked: {checked:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn checksums_and_comment() {
    let dir = root("sums");
    let path = dir.join("f");
    std::fs::write(&path, "abc").unwrap();
    let sums = ops::checksums(&path, &Ctx::new()).unwrap();
    assert_eq!(sums[0], "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(sums[1], "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(sums[2], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    if have("7z") {
        let src = sources(&dir);
        let zip = dir.join("c.zip");
        ops::create(&zip, Kind::Zip, &src, &CreateOptions::default(), &Settings::default(), &Ctx::new()).unwrap();
        let archive = ops::open(&zip, &Settings::default(), &Ctx::new()).unwrap();
        assert_eq!(archive.listing.comment, None);
        ops::set_comment(&archive, "Ünïcode\ncomment").unwrap();
        let archive = ops::open(&zip, &Settings::default(), &Ctx::new()).unwrap();
        assert_eq!(archive.listing.comment.as_deref(), Some("Ünïcode\ncomment"));
        ops::test(&archive, &Ctx::new()).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn plugins_can_be_disabled() {
    let dir = root("plugins");
    let src = sources(&dir);
    let zip = dir.join("p.zip");
    ops::create(&zip, Kind::Zip, &src, &CreateOptions::default(), &Settings::default(), &Ctx::new()).unwrap();
    let mut s = Settings::default();
    s.set_plugin_enabled("cli7z", false);
    s.set_plugin_enabled("clizip", false);
    let archive = ops::open(&zip, &s, &Ctx::new()).unwrap();
    assert_eq!(archive.reader, "libarchive");
    assert!(archive.read_only(), "libarchive reads zip only");
    s.set_plugin_enabled("libarchive", false);
    assert!(matches!(ops::open(&zip, &s, &Ctx::new()), Err(Error::NoBackend(_))));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn corrupt_archive_fails_test() {
    let dir = root("corrupt");
    let src = sources(&dir);
    let path = dir.join("x.tar.gz");
    ops::create(&path, Kind::TarGz, &src, &CreateOptions::default(), &Settings::default(), &Ctx::new()).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    for b in &mut bytes[n / 2..n - 8] {
        *b ^= 0x5a;
    }
    std::fs::write(&path, bytes).unwrap();
    let result = ops::open(&path, &Settings::default(), &Ctx::new()).and_then(|a| ops::test(&a, &Ctx::new()));
    assert!(result.is_err());
    let _ = std::fs::remove_dir_all(dir);
}
