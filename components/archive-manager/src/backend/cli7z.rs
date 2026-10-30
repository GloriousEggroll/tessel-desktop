//! Ark's cli7z plugin: the `7z` program (7-Zip) for 7z and zip archives
//! (passwords, AES/ZipCrypto, encrypted headers, split volumes) and for
//! reading disc, package and filesystem images.

use super::process::{self, last_percent, list_file, Output, TempDir};
use super::{plain_groups, stage_links, Backend};
use crate::archive::{clean_path, AddItem, Columns, CreateOptions, Ctx, Entry, Error, Listing, Result};
use archive_settings::plugins::Kind;
use std::path::{Path, PathBuf};

pub struct SevenZip;

/// Used for read operations when no password is known: 7-Zip must never
/// prompt; a wrong-password answer then means "a password is needed".
const NO_PASSWORD: &str = "-pwayarchive-no-password";

fn password_arg(ctx: &Ctx) -> String {
    match &ctx.password {
        Some(p) => format!("-p{p}"),
        None => NO_PASSWORD.to_string(),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

pub fn check(output: &Output, ctx: &Ctx) -> Result<()> {
    let text = output.text();
    if text.contains("Wrong password") || text.contains("Cannot open encrypted archive") || text.contains("Data Error in encrypted file") {
        return Err(if ctx.password.is_none() { Error::PasswordNeeded } else { Error::WrongPassword });
    }
    if output.status >= 2 {
        return Err(Error::Failed(output.error("7z")));
    }
    Ok(())
}

/// `Attributes = A -rw-r--r--` / `drwxr-xr-x` → (is_dir, mode, is_link).
fn posix_attributes(value: &str) -> (bool, Option<u32>, bool) {
    let windows_dir = value.split_whitespace().next().is_some_and(|w| w.contains('D') && !w.contains('-'));
    let Some(posix) = value.split_whitespace().find(|w| w.len() == 10 && w[1..].chars().all(|c| "rwxsStT-".contains(c))) else {
        return (windows_dir, None, false);
    };
    let chars: Vec<char> = posix.chars().collect();
    let mut mode = 0u32;
    let bits = [(1, 'r', 0o400), (2, 'w', 0o200), (3, 'x', 0o100), (4, 'r', 0o040), (5, 'w', 0o020), (6, 'x', 0o010),
                (7, 'r', 0o004), (8, 'w', 0o002), (9, 'x', 0o001)];
    for (i, c, bit) in bits {
        if chars[i] == c || (c == 'x' && "sSt".contains(chars[i]) && chars[i].is_lowercase()) {
            mode |= bit;
        }
    }
    if "sS".contains(chars[3]) {
        mode |= 0o4000;
    }
    if "sS".contains(chars[6]) {
        mode |= 0o2000;
    }
    if "tT".contains(chars[9]) {
        mode |= 0o1000;
    }
    (chars[0] == 'd' || windows_dir, Some(mode), chars[0] == 'l')
}

/// "2026-10-29 22:43:50.0533986" (local time) → epoch seconds.
pub fn parse_time(value: &str) -> Option<i64> {
    let value = value.trim();
    let (date, time) = value.split_once(' ')?;
    let d: Vec<i32> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    let t: Vec<f64> = time.split(':').filter_map(|p| p.parse().ok()).collect();
    if d.len() != 3 || t.len() != 3 {
        return None;
    }
    let dt = glib::DateTime::from_local(d[0], d[1], d[2], t[0] as i32, t[1] as i32, t[2].floor()).ok()?;
    Some(dt.to_unix())
}

/// Parse `7z l -slt` output.
pub fn parse_listing(text: &str) -> Listing {
    let mut listing = Listing { volumes: 1, ..Default::default() };
    let mut sections = text.splitn(2, "\n----------\n");
    let header = sections.next().unwrap_or("");
    let body = sections.next().unwrap_or("");
    let mut kind = String::new();
    for line in header.lines() {
        if let Some((key, value)) = line.split_once(" = ") {
            match key {
                "Type" => kind = value.to_string(),
                "Method" => listing.method = Some(value.to_string()),
                "Volumes" => listing.volumes = value.parse().unwrap_or(1),
                "Comment" => listing.comment = Some(value.to_string()).filter(|c| !c.is_empty()),
                _ => {}
            }
        }
    }
    let zip = kind == "zip";
    let mut columns = Columns { compressed: true, crc: true, method: true, version: zip, ..Default::default() };
    for block in body.split("\n\n") {
        let mut entry = Entry::default();
        let mut path = None;
        let mut folder = false;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(" = ").or_else(|| line.strip_suffix(" =").map(|k| (k, ""))) else { continue };
            match key {
                "Path" => path = Some(value.to_string()),
                "Folder" => folder = value == "+",
                "Size" => entry.size = value.parse().unwrap_or(0),
                "Packed Size" => entry.compressed = value.parse().ok(),
                "Modified" => entry.mtime = parse_time(value),
                "Attributes" => {
                    let (is_dir, mode, _) = posix_attributes(value);
                    folder |= is_dir;
                    if mode.is_some() {
                        columns.permissions = true;
                    }
                    entry.mode = mode;
                }
                "CRC" => entry.crc = Some(value.to_string()).filter(|c| !c.is_empty()),
                "Encrypted" => entry.encrypted = value == "+",
                "Method" => entry.method = Some(value.to_string()).filter(|c| !c.is_empty()),
                "Version" => entry.version = Some(value.to_string()).filter(|c| !c.is_empty()),
                "Symbolic Link" => entry.link = Some(value.to_string()).filter(|c| !c.is_empty()),
                "User" => {
                    entry.owner = Some(value.to_string()).filter(|c| !c.is_empty());
                    columns.owner |= entry.owner.is_some();
                }
                "Group" => entry.group = Some(value.to_string()).filter(|c| !c.is_empty()),
                _ => {}
            }
        }
        let Some(path) = path.as_deref().and_then(clean_path) else { continue };
        entry.path = path;
        entry.is_dir = folder;
        if folder {
            entry.size = 0;
            entry.compressed = None;
        }
        listing.entries.push(entry);
    }
    listing.columns = columns;
    listing.finish()
}

fn common_args(ctx: &Ctx, read: bool) -> Vec<String> {
    let mut args = vec!["-y".into(), "-bb0".into(), "-bsp1".into(), "-sccUTF-8".into(), "-scsUTF-8".into(), "-spd".into()];
    if read {
        args.push(password_arg(ctx));
    } else if let Some(p) = &ctx.password {
        args.push(format!("-p{p}"));
    }
    args
}

fn progress(ctx: &Ctx) -> impl Fn(&str) + Sync + '_ {
    move |chunk: &str| {
        if let Some(f) = last_percent(chunk) {
            ctx.report(f, "");
        }
    }
}

pub fn type_switch(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::SevenZip => Some("-t7z"),
        Kind::Zip => Some("-tzip"),
        _ => None,
    }
}

impl Backend for SevenZip {
    fn id(&self) -> &'static str {
        "cli7z"
    }

    fn list(&self, archive: &Path, _kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<Listing> {
        let mut args = vec!["l".to_string(), "-slt".into(), "-sccUTF-8".into(), password_arg(ctx), "--".into()];
        args.push(absolute(&volumes[0]).to_string_lossy().into_owned());
        let output = process::run("7z", &args, None, ctx, None)?;
        check(&output, ctx)?;
        let mut listing = parse_listing(&output.stdout);
        listing.volumes = listing.volumes.max(volumes.len() as u32);
        if Kind::from_name(&archive.to_string_lossy()) == Some(Kind::Zip) || listing.columns.version {
            if let Ok(comment) = super::zipcomment::read(archive) {
                listing.comment = comment;
            }
        }
        Ok(listing)
    }

    fn extract(&self, _archive: &Path, _kind: Kind, volumes: &[PathBuf], entries: Option<&[String]>, dest: &Path, ctx: &Ctx) -> Result<()> {
        let scratch = TempDir::private("list-")?;
        let mut args = vec!["x".to_string()];
        args.extend(common_args(ctx, true));
        args.push(format!("-o{}", absolute(dest).display()));
        if let Some(entries) = entries {
            if entries.is_empty() {
                return Ok(());
            }
            let list = list_file(scratch.path(), entries)?;
            args.push(format!("-i@{}", list.display()));
        }
        args.push("--".into());
        args.push(absolute(&volumes[0]).to_string_lossy().into_owned());
        let output = process::run("7z", &args, None, ctx, Some(&progress(ctx)))?;
        check(&output, ctx)
    }

    fn test(&self, _archive: &Path, _kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<()> {
        let mut args = vec!["t".to_string()];
        args.extend(common_args(ctx, true));
        args.push("--".into());
        args.push(absolute(&volumes[0]).to_string_lossy().into_owned());
        let output = process::run("7z", &args, None, ctx, Some(&progress(ctx)))?;
        check(&output, ctx)
    }

    fn add(&self, archive: &Path, kind: Kind, items: &[AddItem], options: &CreateOptions, ctx: &Ctx) -> Result<()> {
        let archive = absolute(archive);
        let mut base = vec!["a".to_string()];
        if let Some(t) = type_switch(kind) {
            base.push(t.into());
        }
        base.extend(["-y", "-bb0", "-bsp1", "-sccUTF-8", "-scsUTF-8", "-spd"].map(String::from));
        if let Some(level) = options.level {
            base.push(format!("-mx={}", level.min(9)));
        }
        if let Some(method) = options.method.as_deref().filter(|m| !m.is_empty()) {
            base.push(if kind == Kind::Zip { format!("-mm={method}") } else { format!("-m0={method}") });
        }
        let password = options.password.clone().or_else(|| ctx.password.clone()).filter(|p| !p.is_empty());
        if let Some(password) = &password {
            base.push(format!("-p{password}"));
            if kind == Kind::SevenZip && options.encrypt_header {
                base.push("-mhe=on".into());
            }
            if kind == Kind::Zip {
                base.push(format!("-mem={}", options.encryption.as_deref().unwrap_or("AES256")));
            }
        }
        if let Some(size) = options.volume_size.filter(|s| *s > 0) {
            base.push(format!("-v{size}b"));
        }
        let run = |cwd: &Path, names: &[String]| -> Result<()> {
            let mut args = base.clone();
            args.push("--".into());
            args.push(archive.to_string_lossy().into_owned());
            args.extend(names.iter().cloned());
            let mut ctx = ctx.clone();
            ctx.password = password.clone();
            let output = process::run("7z", &args, Some(cwd), &ctx, Some(&progress(&ctx)))?;
            check(&output, &ctx)
        };
        // Split archives are written in one go; everything else may be added in groups.
        match plain_groups(items).filter(|g| g.len() == 1 || options.volume_size.is_none()) {
            Some(groups) => {
                for (folder, names) in groups {
                    run(&folder, &names)?;
                }
            }
            None => {
                let parent = archive.parent().map(Path::to_path_buf).unwrap_or_default();
                let stage = TempDir::new_in(&parent, ".wayarchive-add-")?;
                let tops = stage_links(stage.path(), items)?;
                run(stage.path(), &tops)?;
            }
        }
        if kind == Kind::Zip && options.volume_size.is_none() {
            if let Some(comment) = options.comment.as_deref().filter(|c| !c.is_empty()) {
                super::zipcomment::write(&archive, comment)?;
            }
        }
        Ok(())
    }

    fn delete(&self, archive: &Path, _kind: Kind, entries: &[String], ctx: &Ctx) -> Result<()> {
        let scratch = TempDir::private("list-")?;
        let list = list_file(scratch.path(), entries)?;
        let mut args = vec!["d".to_string()];
        args.extend(common_args(ctx, false));
        args.push(format!("-i@{}", list.display()));
        args.push("--".into());
        args.push(absolute(archive).to_string_lossy().into_owned());
        let output = process::run("7z", &args, None, ctx, Some(&progress(ctx)))?;
        check(&output, ctx)
    }

    fn rename(&self, archive: &Path, _kind: Kind, renames: &[(String, String)], ctx: &Ctx) -> Result<()> {
        let mut args = vec!["rn".to_string()];
        args.extend(common_args(ctx, false));
        args.push("--".into());
        args.push(absolute(archive).to_string_lossy().into_owned());
        for (old, new) in renames {
            args.push(old.clone());
            args.push(new.clone());
        }
        let output = process::run("7z", &args, None, ctx, Some(&progress(ctx)))?;
        check(&output, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_slt_listing() {
        let text = "Listing archive: x.zip\n--\nPath = x.zip\nType = zip\nPhysical Size = 711\nComment = hi\n\n----------\n\
Path = dir\nFolder = +\nSize = 0\nPacked Size = 0\nModified = 2026-10-29 22:43:50\nAttributes = D drwxr-xr-x\nEncrypted = -\nMethod = Store\nVersion = 10\n\n\
Path = dir/b.txt\nFolder = -\nSize = 6\nPacked Size = 8\nModified = 2026-10-29 22:43:50.12\nAttributes = A -rw-r-----\nCRC = 363A3020\nEncrypted = +\nMethod = AES-256 Deflate\nVersion = 20\n\n";
        let listing = parse_listing(text);
        assert_eq!(listing.comment.as_deref(), Some("hi"));
        assert_eq!(listing.entries.len(), 2);
        let dir = listing.find("dir").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.mode, Some(0o755));
        let file = listing.find("dir/b.txt").unwrap();
        assert_eq!((file.size, file.compressed, file.mode), (6, Some(8), Some(0o640)));
        assert_eq!(file.crc.as_deref(), Some("363A3020"));
        assert!(file.encrypted && listing.encrypted);
        assert_eq!(file.version.as_deref(), Some("20"));
        assert!(listing.columns.version && listing.columns.crc);
        assert!(file.mtime.is_some());
    }

    #[test]
    fn attributes() {
        assert_eq!(posix_attributes("D drwxr-xr-x"), (true, Some(0o755), false));
        assert_eq!(posix_attributes("A -rwsr-xr-x"), (false, Some(0o4755), false));
        assert_eq!(posix_attributes("A lrwxrwxrwx"), (false, Some(0o777), true));
        assert_eq!(posix_attributes("D"), (true, None, false));
        assert_eq!(posix_attributes("A"), (false, None, false));
    }
}
