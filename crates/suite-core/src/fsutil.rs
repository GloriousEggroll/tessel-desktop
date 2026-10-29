//! File helpers with Python error text.

use crate::pyerr::os_error_path;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// Write `directory/name` like `tempfile.mkstemp(prefix=…)` + `os.replace`:
/// the directory is created 0700, the file is 0600, the temporary is removed
/// on failure. Errors are Python `OSError` strings.
pub fn write_private(directory: &Path, name: &str, prefix: &str, data: &[u8]) -> Result<(), String> {
    if !directory.is_dir() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)
            .map_err(|e| os_error_path(&e, directory))?;
    }
    let mut temporary = None;
    let mut last_error = None;
    for attempt in 0..100u32 {
        let suffix: String = {
            let seed = glib::random_int() ^ attempt.wrapping_mul(2654435761);
            (0..8).map(|i| b"abcdefghijklmnopqrstuvwxyz0123456789_"[((seed >> (i * 4)) as usize + i as usize * 7) % 37] as char).collect()
        };
        let path = directory.join(format!("{prefix}{suffix}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => {
                temporary = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                last_error = Some(os_error_path(&error, &path));
                break;
            }
        }
    }
    let Some((path, mut file)) = temporary else {
        return Err(last_error.unwrap_or_else(|| "No usable temporary file name found".into()));
    };
    let target = directory.join(name);
    let result = file.write_all(data).map_err(|e| os_error_path(&e, &path))
        .and_then(|()| std::fs::rename(&path, &target).map_err(|e| os_error_path(&e, &path)));
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_write_is_0600_in_0700_directory_and_reports_errno() {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir)
            .join(format!("suite-core-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let directory = base.join("state");
        write_private(&directory, "state.json", ".state-", b"{}").unwrap();
        assert_eq!(std::fs::read(directory.join("state.json")).unwrap(), b"{}");
        assert_eq!(std::fs::metadata(directory.join("state.json")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1, "no temporary left behind");
        let blocked = base.join("file");
        std::fs::write(&blocked, "").unwrap();
        let error = write_private(&blocked.join("sub"), "x", ".x-", b"").unwrap_err();
        assert!(error.starts_with("[Errno 20] Not a directory: '"), "{error}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
