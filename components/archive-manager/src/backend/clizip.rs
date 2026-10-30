//! Ark's clizip plugin: Info-ZIP's `zip`, `unzip`, `zipinfo` and `zipnote`
//! (ZipCrypto passwords; renames through zipnote).

use super::process::{self, Output, TempDir};
use super::{plain_groups, stage_links, Backend};
use crate::archive::{clean_path, AddItem, Columns, CreateOptions, Ctx, Entry, Error, Listing, Result};
use archive_settings::plugins::Kind;
use std::path::{Path, PathBuf};

pub struct InfoZip;

fn absolute(path: &Path) -> String {
    if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
        .to_string_lossy().into_owned()
}

/// unzip treats member names as wildcard patterns: escape them.
pub fn escape_pattern(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        match c {
            '[' => out.push_str("[[]"),
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            other => out.push(other),
        }
    }
    out
}

fn unzip_check(output: &Output, ctx: &Ctx) -> Result<()> {
    let text = output.text();
    if output.status == 82 || text.contains("incorrect password") || text.contains("unable to get password") {
        return Err(if ctx.password.is_none() { Error::PasswordNeeded } else { Error::WrongPassword });
    }
    // 1: warnings only; 11: no matching files (empty selection).
    if output.status > 1 && output.status != 11 {
        return Err(Error::Failed(output.error("unzip")));
    }
    Ok(())
}

fn zip_check(output: &Output) -> Result<()> {
    if output.status != 0 && output.status != 12 {
        return Err(Error::Failed(output.error("zip")));
    }
    Ok(())
}

/// "20261029.224352" → epoch seconds (local time).
fn parse_stamp(value: &str) -> Option<i64> {
    let (date, time) = value.split_once('.')?;
    if date.len() != 8 || time.len() != 6 {
        return None;
    }
    let n = |s: &str| s.parse::<i32>().ok();
    let dt = glib::DateTime::from_local(n(&date[..4])?, n(&date[4..6])?, n(&date[6..])?, n(&time[..2])?, n(&time[2..4])?,
                                         f64::from(n(&time[4..])?)).ok()?;
    Some(dt.to_unix())
}

fn mode_of(text: &str) -> Option<u32> {
    if text.len() != 10 {
        return None;
    }
    let c: Vec<char> = text.chars().collect();
    let mut mode = 0;
    for (i, bit) in [(1, 0o400), (2, 0o200), (3, 0o100), (4, 0o040), (5, 0o020), (6, 0o010), (7, 0o004), (8, 0o002), (9, 0o001)] {
        if c[i] != '-' {
            mode |= bit;
        }
    }
    Some(mode)
}

/// Parse `zipinfo -l -T` output.
pub fn parse_zipinfo(text: &str) -> Listing {
    let mut listing = Listing { volumes: 1, ..Default::default() };
    for line in text.lines() {
        let mut fields = Vec::new();
        let mut rest = line;
        for _ in 0..8 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let name = rest.strip_prefix(' ').unwrap_or(rest);
        if fields.len() < 8 || name.is_empty() || fields[0].len() != 10 || fields[4].len() != 2 {
            continue;
        }
        let (Ok(size), Ok(compressed)) = (fields[3].parse::<u64>(), fields[5].parse::<u64>()) else { continue };
        let Some(path) = clean_path(name) else { continue };
        let is_dir = name.ends_with('/') || fields[0].starts_with('d');
        let encrypted = fields[4].chars().next().is_some_and(|c| c.is_ascii_uppercase());
        listing.entries.push(Entry {
            path, is_dir, size, compressed: (!is_dir).then_some(compressed), mtime: parse_stamp(fields[7]),
            mode: mode_of(fields[0]), method: Some(fields[6].to_string()), version: Some(fields[1].to_string()),
            encrypted, ..Default::default()
        });
    }
    listing.columns = Columns { compressed: true, permissions: true, method: true, version: true, ..Default::default() };
    listing.finish()
}

/// zipnote's comment file with `@=new` lines for renamed entries.
pub fn rename_note(note: &str, renames: &[(String, String)], listing: &Listing) -> String {
    let mut out = String::new();
    for line in note.lines() {
        out.push_str(line);
        out.push('\n');
        if let Some(name) = line.strip_prefix("@ ") {
            if name.starts_with('(') {
                continue;
            }
            let Some(clean) = clean_path(name) else { continue };
            let renamed = super::apply_renames(&clean, renames);
            if renamed != clean {
                let is_dir = listing.find(&clean).is_some_and(|e| e.is_dir) || name.ends_with('/');
                out.push_str(&format!("@={}{}\n", renamed, if is_dir { "/" } else { "" }));
            }
        }
    }
    out
}

impl Backend for InfoZip {
    fn id(&self) -> &'static str {
        "clizip"
    }

    fn list(&self, archive: &Path, _kind: Kind, _volumes: &[PathBuf], ctx: &Ctx) -> Result<Listing> {
        let output = process::run("zipinfo", &["-l".into(), "-T".into(), absolute(archive)], None, ctx, None)?;
        if output.status > 1 {
            return Err(Error::Failed(output.error("zipinfo")));
        }
        let mut listing = parse_zipinfo(&output.stdout);
        listing.comment = super::zipcomment::read(archive).ok().flatten();
        Ok(listing)
    }

    fn extract(&self, archive: &Path, _kind: Kind, _volumes: &[PathBuf], entries: Option<&[String]>, dest: &Path, ctx: &Ctx) -> Result<()> {
        let mut args = vec!["-o".to_string(), "-qq".into()];
        if let Some(p) = &ctx.password {
            args.push("-P".into());
            args.push(p.clone());
        }
        args.push(absolute(archive));
        if let Some(entries) = entries {
            if entries.is_empty() {
                return Ok(());
            }
            let listing_dirs: Vec<&String> = entries.iter().collect();
            // Folders are recreated from their files; unzip wants directory names with a slash.
            args.extend(listing_dirs.iter().map(|e| escape_pattern(e)));
            args.extend(listing_dirs.iter().map(|e| escape_pattern(&format!("{e}/"))));
        }
        args.push("-d".into());
        args.push(absolute(dest));
        let output = process::run("unzip", &args, None, ctx, None)?;
        unzip_check(&output, ctx)
    }

    fn test(&self, archive: &Path, _kind: Kind, _volumes: &[PathBuf], ctx: &Ctx) -> Result<()> {
        let mut args = vec!["-t".to_string(), "-qq".into()];
        if let Some(p) = &ctx.password {
            args.push("-P".into());
            args.push(p.clone());
        }
        args.push(absolute(archive));
        let output = process::run("unzip", &args, None, ctx, None)?;
        unzip_check(&output, ctx)
    }

    fn add(&self, archive: &Path, _kind: Kind, items: &[AddItem], options: &CreateOptions, ctx: &Ctx) -> Result<()> {
        let archive_path = PathBuf::from(absolute(archive));
        let mut base = vec!["-r".to_string(), "-q".into()];
        if let Some(level) = options.level {
            base.push(format!("-{}", level.min(9)));
        }
        match options.method.as_deref() {
            Some("BZip2") => base.extend(["-Z".into(), "bzip2".into()]),
            Some("Copy") => base.extend(["-Z".into(), "store".into()]),
            _ => {}
        }
        let password = options.password.clone().or_else(|| ctx.password.clone()).filter(|p| !p.is_empty());
        if let Some(password) = &password {
            base.push("-P".into());
            base.push(password.clone());
        }
        if let Some(size) = options.volume_size.filter(|s| *s > 0) {
            base.push("-s".into());
            base.push(format!("{}k", size.div_ceil(1024).max(64)));
        }
        let run = |cwd: &Path, names: &[String], links: bool| -> Result<()> {
            let mut args = base.clone();
            if links {
                args.push("-y".into());
            }
            args.push(archive_path.to_string_lossy().into_owned());
            args.push("--".into());
            args.extend(names.iter().cloned());
            zip_check(&process::run("zip", &args, Some(cwd), ctx, None)?)
        };
        match plain_groups(items).filter(|g| g.len() == 1 || options.volume_size.is_none()) {
            Some(groups) => {
                for (folder, names) in groups {
                    run(&folder, &names, true)?;
                }
            }
            None => {
                let parent = archive_path.parent().map(Path::to_path_buf).unwrap_or_default();
                let stage = TempDir::new_in(&parent, ".wayarchive-add-")?;
                let tops = stage_links(stage.path(), items)?;
                run(stage.path(), &tops, false)?;
            }
        }
        if let Some(comment) = options.comment.as_deref().filter(|c| !c.is_empty() && options.volume_size.is_none()) {
            super::zipcomment::write(&archive_path, comment)?;
        }
        Ok(())
    }

    fn delete(&self, archive: &Path, _kind: Kind, entries: &[String], ctx: &Ctx) -> Result<()> {
        let mut args = vec!["-d".to_string(), "-q".into(), "-nw".into(), absolute(archive), "--".into()];
        for entry in entries {
            args.push(entry.clone());
            args.push(format!("{entry}/"));
        }
        zip_check(&process::run("zip", &args, None, ctx, None)?)
    }

    fn rename(&self, archive: &Path, kind: Kind, renames: &[(String, String)], ctx: &Ctx) -> Result<()> {
        let listing = self.list(archive, kind, &[archive.to_path_buf()], ctx)?;
        let note = process::run("zipnote", &[absolute(archive)], None, ctx, None)?;
        if note.status != 0 {
            return Err(Error::Failed(note.error("zipnote")));
        }
        let edited = rename_note(&note.stdout, renames, &listing);
        let output = process::run_input("zipnote", &["-w".into(), absolute(archive)], ctx, edited.as_bytes())?;
        if output.status != 0 {
            return Err(Error::Failed(output.error("zipnote")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "Archive:  zc.zip\nZip file size: 536 bytes, number of entries: 3\n\
-rw-r--r--  3.0 unx        2 TX       14 defN 20261029.225334 plain file.txt\n\
drwxr-xr-x  3.0 unx        0 bx        0 stor 20261029.225334 we ird[1]/\n\
-rw-------  3.0 unx        2 bx        4 stor 20261029.225334 we ird[1]/f*.txt\n\
3 files, 4 bytes uncompressed, 4 bytes compressed:  0.0%\n";

    #[test]
    fn parses_zipinfo() {
        let l = parse_zipinfo(INFO);
        assert_eq!(l.entries.len(), 3);
        let f = l.find("plain file.txt").unwrap();
        assert!(f.encrypted && l.encrypted);
        assert_eq!((f.size, f.compressed, f.method.as_deref(), f.version.as_deref()), (2, Some(14), Some("defN"), Some("3.0")));
        assert!(l.find("we ird[1]").unwrap().is_dir);
        assert_eq!(l.find("we ird[1]/f*.txt").unwrap().mode, Some(0o600));
    }

    #[test]
    fn escapes_and_notes() {
        assert_eq!(escape_pattern("a[1]*?.txt"), "a[[]1][*][?].txt");
        let l = parse_zipinfo(INFO);
        let note = "@ plain file.txt\n@ (comment above this line)\n@ we ird[1]/\n@ (comment above this line)\n@ we ird[1]/f*.txt\n@ (comment above this line)\n@ (zip file comment below this line)\n";
        let edited = rename_note(note, &[("we ird[1]".into(), "renamed".into())], &l);
        assert!(edited.contains("@ we ird[1]/\n@=renamed/\n"));
        assert!(edited.contains("@ we ird[1]/f*.txt\n@=renamed/f*.txt\n"));
        assert!(!edited.contains("@=plain"));
    }
}
