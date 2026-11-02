//! Ark's cliunarchiver plugin: `lsar -j` lists and `unar` extracts RAR
//! (also RAR5, encrypted, header-encrypted and multi-volume), ARJ and LHA.

use super::process::{self, Output};
use super::Backend;
use crate::archive::{clean_path, Columns, Ctx, Entry, Error, Listing, Result};
use tessel_archive_settings::plugins::Kind;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct Unarchiver;

fn absolute(path: &Path) -> String {
    if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
        .to_string_lossy().into_owned()
}

/// "2026-10-29 22:43:50 -0600" → epoch seconds.
pub fn parse_date(value: &str) -> Option<i64> {
    let mut parts = value.split_whitespace();
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next().unwrap_or("+0000"));
    let zone = if zone.len() == 5 { format!("{}:{}", &zone[..3], &zone[3..]) } else { zone.to_string() };
    glib::DateTime::from_iso8601(&format!("{date}T{time}{zone}"), None).ok().map(|d| d.to_unix())
}

fn password_error(text: &str, ctx: &Ctx) -> Option<Error> {
    let lower = text.to_lowercase();
    (lower.contains("requires a password") || lower.contains("password") && (lower.contains("wrong") || lower.contains("incorrect")))
        .then(|| if ctx.password.is_none() { Error::PasswordNeeded } else { Error::WrongPassword })
}

/// Parse `lsar -j` output; entries carry their XADIndex in `indexes`.
pub fn parse_lsar(text: &str, ctx: &Ctx) -> Result<(Listing, Vec<(String, u64)>)> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            return Err(password_error(text, ctx).unwrap_or_else(|| Error::Failed(format!("lsar: {}", text.trim()))));
        }
    };
    let error = value.get("lsarError").and_then(Value::as_i64).unwrap_or(0);
    let contents = value.get("lsarContents").and_then(Value::as_array).cloned().unwrap_or_default();
    if error != 0 && contents.is_empty() {
        // XADPasswordException is 9 in XADException.h.
        if error == 9 {
            return Err(if ctx.password.is_none() { Error::PasswordNeeded } else { Error::WrongPassword });
        }
        return Err(Error::Failed(format!("lsar: error {error}")));
    }
    let properties = value.get("lsarProperties").cloned().unwrap_or(Value::Null);
    let mut listing = Listing {
        comment: properties.get("XADComment").and_then(Value::as_str).map(str::to_string).filter(|c| !c.is_empty()),
        volumes: properties.get("XADVolumes").and_then(Value::as_array).map(|v| v.len() as u32).unwrap_or(1).max(1),
        ..Default::default()
    };
    let mut indexes = Vec::new();
    for item in contents {
        let Some(name) = item.get("XADFileName").and_then(Value::as_str) else { continue };
        let Some(path) = clean_path(name) else { continue };
        let flag = |key: &str| item.get(key).and_then(Value::as_i64).unwrap_or(0) != 0;
        let number = |key: &str| item.get(key).and_then(Value::as_u64);
        let crc = ["RARCRC32", "7zCRC32", "ZipCRC32", "CRC32"].iter().find_map(|k| number(k)).map(|c| format!("{c:08X}"));
        let is_dir = flag("XADIsDirectory");
        let entry = Entry {
            path: path.clone(),
            is_dir,
            size: if is_dir { 0 } else { number("XADFileSize").unwrap_or(0) },
            compressed: if is_dir { None } else { number("XADCompressedSize") },
            mtime: item.get("XADLastModificationDate").and_then(Value::as_str).and_then(parse_date),
            mode: number("XADPosixPermissions").map(|m| m as u32 & 0o7777),
            owner: item.get("XADPosixUserName").and_then(Value::as_str).map(str::to_string),
            group: item.get("XADPosixGroupName").and_then(Value::as_str).map(str::to_string),
            crc,
            method: item.get("XADCompressionName").and_then(Value::as_str).map(str::to_string),
            version: number("RARCompressionVersion").map(|v| v.to_string()),
            encrypted: flag("XADIsEncrypted"),
            link: item.get("XADLinkDestination").and_then(Value::as_str).map(str::to_string),
        };
        if let Some(index) = number("XADIndex") {
            indexes.push((path, index));
        }
        listing.entries.push(entry);
    }
    listing.columns = Columns { compressed: true, permissions: true, crc: true, method: true, owner: false, version: false };
    Ok((listing.finish(), indexes))
}

/// lsar prints the password notice outside the JSON (on stderr).
fn listing_of(output: &Output, ctx: &Ctx) -> Result<(Listing, Vec<(String, u64)>)> {
    match parse_lsar(&output.stdout, ctx) {
        Err(Error::Failed(message)) => Err(password_error(&output.text(), ctx).unwrap_or(Error::Failed(message))),
        other => other,
    }
}

fn lsar(archive: &Path, ctx: &Ctx, test: bool) -> Result<Output> {
    let mut args = vec!["-j".to_string(), "-nr".into()];
    if test {
        args = vec!["-t".into(), "-nr".into()];
    }
    if let Some(p) = &ctx.password {
        args.push("-p".into());
        args.push(p.clone());
    }
    args.push("--".into());
    args.push(absolute(archive));
    process::run("lsar", &args, None, ctx, None)
}

impl Backend for Unarchiver {
    fn id(&self) -> &'static str {
        "cliunarchiver"
    }

    fn list(&self, _archive: &Path, _kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<Listing> {
        let output = lsar(&volumes[0], ctx, false)?;
        listing_of(&output, ctx).map(|(listing, _)| listing)
    }

    fn extract(&self, _archive: &Path, _kind: Kind, volumes: &[PathBuf], entries: Option<&[String]>, dest: &Path, ctx: &Ctx) -> Result<()> {
        let mut args = vec!["-o".to_string(), absolute(dest), "-f".into(), "-D".into(), "-nr".into()];
        if let Some(p) = &ctx.password {
            args.push("-p".into());
            args.push(p.clone());
        }
        let mut total = 0usize;
        if let Some(entries) = entries {
            if entries.is_empty() {
                return Ok(());
            }
            let wanted: HashSet<&str> = entries.iter().map(String::as_str).collect();
            let (_, indexes) = listing_of(&lsar(&volumes[0], ctx, false)?, ctx)?;
            args.push("-i".into());
            args.push("--".into());
            args.push(absolute(&volumes[0]));
            for (path, index) in indexes {
                if wanted.contains(path.as_str()) {
                    args.push(index.to_string());
                    total += 1;
                }
            }
        } else {
            args.push("--".into());
            args.push(absolute(&volumes[0]));
        }
        let seen = std::sync::atomic::AtomicUsize::new(0);
        let progress = |chunk: &str| {
            let n = seen.fetch_add(chunk.matches("... ").count(), std::sync::atomic::Ordering::Relaxed);
            if total > 0 {
                ctx.report(n as f64 / total as f64, "");
            }
        };
        let output = process::run("unar", &args, None, ctx, Some(&progress))?;
        let text = output.text();
        if let Some(error) = password_error(&text, ctx) {
            return Err(error);
        }
        if output.status != 0 || text.contains("Failed!") || text.contains("failed!") {
            return Err(Error::Failed(output.error("unar")));
        }
        Ok(())
    }

    fn test(&self, _archive: &Path, _kind: Kind, volumes: &[PathBuf], ctx: &Ctx) -> Result<()> {
        let output = lsar(&volumes[0], ctx, true)?;
        let text = output.text();
        if let Some(error) = password_error(&text, ctx) {
            return Err(error);
        }
        if output.status != 0 || text.lines().any(|l| l.contains("... Failed") || l.trim_end().ends_with("Failed!")) {
            return Err(Error::Failed(output.error("lsar")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsar_json() {
        let text = r#"{"lsarFormatVersion":2,"lsarContents":[
            {"XADIsDirectory":1,"XADFileName":"dir","XADIndex":0,"XADLastModificationDate":"2026-10-29 22:43:50 -0600"},
            {"XADFileName":"dir/a.txt","XADFileSize":6,"XADCompressedSize":11,"XADIndex":1,"XADIsEncrypted":1,
             "RARCRC32":909783072,"XADCompressionName":"RAR5","XADPosixPermissions":33188}],
            "lsarFormatName":"RAR 5","lsarProperties":{"XADComment":"note","XADVolumes":["a.part1.rar","a.part2.rar"]}}"#;
        let (l, idx) = parse_lsar(text, &Ctx::new()).unwrap();
        assert_eq!(l.comment.as_deref(), Some("note"));
        assert_eq!(l.volumes, 2);
        let a = l.find("dir/a.txt").unwrap();
        assert_eq!((a.size, a.compressed, a.mode), (6, Some(11), Some(0o644)));
        assert_eq!(a.crc.as_deref(), Some("363A3020"));
        assert!(l.encrypted);
        assert_eq!(idx, vec![("dir".to_string(), 0), ("dir/a.txt".to_string(), 1)]);
        assert_eq!(parse_date("2026-10-29 22:43:50 -0600"), parse_date("2026-10-30 04:43:50 +0000"));
    }

    #[test]
    fn password_errors() {
        let none = Ctx::new();
        let some = Ctx::new().with_password(Some("x".into()));
        let text = "This archive requires a password to unpack. Use the -p option to provide one.";
        assert_eq!(parse_lsar(text, &none).err(), Some(Error::PasswordNeeded));
        let bad = r#"{"lsarContents":[],"lsarError":9}"#;
        assert_eq!(parse_lsar(bad, &some).err(), Some(Error::WrongPassword));
    }
}
