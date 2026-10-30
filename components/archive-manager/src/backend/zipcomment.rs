//! Zip archive comments, read and written directly in the end of central
//! directory record (the comment is the last field of the file), so every
//! zip backend supports "Edit comment" like Ark's libzip plugin.

use crate::archive::{Error, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const EOCD_LEN: usize = 22;

/// (offset of the EOCD record, current comment length).
fn locate(file: &mut std::fs::File) -> Result<(u64, usize)> {
    let len = file.seek(SeekFrom::End(0))?;
    let tail = len.min((EOCD_LEN + 65535) as u64);
    file.seek(SeekFrom::Start(len - tail))?;
    let mut buffer = vec![0u8; tail as usize];
    file.read_exact(&mut buffer)?;
    let mut i = buffer.len().saturating_sub(EOCD_LEN) as isize;
    while i >= 0 {
        let at = i as usize;
        if buffer[at..at + 4] == EOCD {
            let comment_len = u16::from_le_bytes([buffer[at + 20], buffer[at + 21]]) as usize;
            if at + EOCD_LEN + comment_len == buffer.len() {
                return Ok((len - tail + at as u64, comment_len));
            }
        }
        i -= 1;
    }
    Err(Error::Failed(suite_core::tr!("This file is not a valid zip archive.")))
}

pub fn read(path: &Path) -> Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let (offset, len) = locate(&mut file)?;
    if len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset + EOCD_LEN as u64))?;
    let mut bytes = vec![0u8; len];
    file.read_exact(&mut bytes)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

pub fn write(path: &Path, comment: &str) -> Result<()> {
    let bytes = comment.as_bytes();
    if bytes.len() > 65535 {
        return Err(Error::Failed(suite_core::tr!("The comment is too long.")));
    }
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let (offset, _) = locate(&mut file)?;
    file.seek(SeekFrom::Start(offset + 20))?;
    file.write_all(&(bytes.len() as u16).to_le_bytes())?;
    file.write_all(bytes)?;
    file.set_len(offset + EOCD_LEN as u64 + bytes.len() as u64)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_zip() -> Vec<u8> {
        let mut v = EOCD.to_vec();
        v.extend_from_slice(&[0u8; 18]);
        v
    }

    #[test]
    fn round_trip() {
        let path = std::env::temp_dir().join(format!("wayarchive-comment-{}.zip", std::process::id()));
        std::fs::write(&path, empty_zip()).unwrap();
        assert_eq!(read(&path).unwrap(), None);
        write(&path, "héllo\nworld").unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("héllo\nworld"));
        write(&path, "x").unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("x"));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 23);
        write(&path, "").unwrap();
        assert_eq!(read(&path).unwrap(), None);
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(read(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
