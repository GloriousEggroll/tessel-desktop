//! Command line: Ark's options (same long and short names).

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub files: Vec<String>,
    pub dialog: bool,
    pub destination: Option<String>,
    pub open_destination: bool,
    pub add: bool,
    pub add_to: Option<String>,
    pub change_to_first_path: bool,
    pub auto_filename: Option<String>,
    pub batch: bool,
    pub auto_destination: bool,
    pub auto_subfolder: bool,
    pub mimetypes: bool,
    pub check: bool,
}

impl Options {
    /// Runs without a main window and exits when done (own process, own exit code).
    pub fn is_job(&self) -> bool {
        self.batch || self.add || self.add_to.is_some() || (self.dialog && !self.files.is_empty())
    }
    pub fn is_add(&self) -> bool {
        self.add || self.add_to.is_some()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub const USAGE: &str = "Usage: tessel-archive [options] [files...]\n";

pub fn help() -> String {
    format!("{USAGE}
Archive Manager: open, create, extract and edit archives.

Options:
  -h, --help                     Show this help.
  -v, --version                  Show the version.
  -d, --dialog                   Show a dialog for the options of the operation (extract or add).
  -o, --destination <directory>  Destination folder to extract to.
  -O, --opendestination          Open the destination folder after extraction.
  -c, --add                      Ask for an archive file name and add the given files to it; quit when done.
  -t, --add-to <filename>        Add the given files to <filename>; create it if it does not exist; quit when done.
  -p, --changetofirstpath        Change the current folder to the first entry and add all others relative to it.
  -f, --autofilename <suffix>    Choose the archive name from the first file, with the given suffix (zip, tar.gz, 7z...).
  -b, --batch                    Extract each archive, then quit (with a progress window).
  -e, --autodestination          Extract next to the archive (batch mode).
  -a, --autosubfolder            Extract into a subfolder named after the archive unless it has a single top-level folder.
  -m, --mimetypes                List the supported archive mime types.
      --check                    Print the available plugins and formats as JSON.

Arguments:
  files                          Archives to open or extract, or files to add (with --add/--add-to).
")
}

fn error(message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{USAGE}tessel-archive: error: {message}\n") }
}

pub fn parse(argv: &[String]) -> Result<Options, Exit> {
    let mut o = Options::default();
    let mut i = 0;
    let mut positional_only = false;
    while i < argv.len() {
        let arg = &argv[i];
        i += 1;
        if positional_only || arg == "-" || !arg.starts_with('-') {
            o.files.push(arg.clone());
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }
        let (name, inline): (String, Option<String>) = match arg.strip_prefix("--") {
            Some(long) => match long.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (long.to_string(), None),
            },
            None => {
                // -abc (flags) / -o dir / -odir
                let chars: Vec<char> = arg[1..].chars().collect();
                let mut j = 0;
                while j < chars.len() {
                    let long = match chars[j] {
                        'h' => "help", 'v' => "version", 'd' => "dialog", 'o' => "destination", 'O' => "opendestination",
                        'c' => "add", 't' => "add-to", 'p' => "changetofirstpath", 'f' => "autofilename", 'b' => "batch",
                        'e' => "autodestination", 'a' => "autosubfolder", 'm' => "mimetypes",
                        other => return Err(error(&format!("unknown option: -{other}"))),
                    };
                    if matches!(long, "destination" | "add-to" | "autofilename") {
                        let rest: String = chars[j + 1..].iter().collect();
                        let value = if !rest.is_empty() {
                            rest
                        } else if i < argv.len() {
                            i += 1;
                            argv[i - 1].clone()
                        } else {
                            return Err(error(&format!("option -{} needs a value", chars[j])));
                        };
                        apply(&mut o, long, Some(value))?;
                        break;
                    }
                    apply(&mut o, long, None)?;
                    j += 1;
                }
                continue;
            }
        };
        let takes_value = matches!(name.as_str(), "destination" | "add-to" | "autofilename");
        let value = if takes_value {
            match inline {
                Some(v) => Some(v),
                None if i < argv.len() => {
                    i += 1;
                    Some(argv[i - 1].clone())
                }
                None => return Err(error(&format!("option --{name} needs a value"))),
            }
        } else {
            if inline.is_some() {
                return Err(error(&format!("option --{name} does not take a value")));
            }
            None
        };
        apply(&mut o, &name, value)?;
    }
    if o.auto_filename.is_some() && !o.add {
        o.add = true;
    }
    if (o.batch || o.is_add()) && o.files.is_empty() {
        return Err(error("no files given"));
    }
    if o.batch && o.is_add() {
        return Err(error("--batch cannot be combined with --add or --add-to"));
    }
    Ok(o)
}

fn apply(o: &mut Options, name: &str, value: Option<String>) -> Result<(), Exit> {
    match name {
        "help" => return Err(Exit { code: 0, stdout: help(), stderr: String::new() }),
        "version" => return Err(Exit { code: 0, stdout: format!("tessel-archive {}\n", env!("CARGO_PKG_VERSION")), stderr: String::new() }),
        "dialog" => o.dialog = true,
        "destination" => o.destination = value,
        "opendestination" => o.open_destination = true,
        "add" => o.add = true,
        "add-to" => o.add_to = value,
        "changetofirstpath" => o.change_to_first_path = true,
        "autofilename" => o.auto_filename = value,
        "batch" => o.batch = true,
        "autodestination" => o.auto_destination = true,
        "autosubfolder" => o.auto_subfolder = true,
        "mimetypes" => o.mimetypes = true,
        "check" => o.check = true,
        other => return Err(error(&format!("unknown option: --{other}"))),
    }
    Ok(())
}

/// Archive name for `--autofilename <suffix>`: the first file's name (without
/// its extension for files) plus the suffix, next to it, made unique Ark-style (`name_1.zip`).
pub fn auto_archive_path(first: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let suffix = suffix.trim_start_matches('.');
    let folder = first.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into());
    let base = if first.is_dir() {
        name
    } else {
        match name.rfind('.') {
            Some(i) if i > 0 => name[..i].to_string(),
            _ => name,
        }
    };
    let mut candidate = folder.join(format!("{base}.{suffix}"));
    let mut n = 1;
    while std::fs::symlink_metadata(&candidate).is_ok() {
        candidate = folder.join(format!("{base}_{n}.{suffix}"));
        n += 1;
    }
    candidate
}

/// Archive paths for `--changetofirstpath`: relative to the first file's folder
/// (files elsewhere keep just their name).
pub fn add_items(files: &[std::path::PathBuf], change_to_first_path: bool) -> Vec<crate::archive::AddItem> {
    let base = files.first().and_then(|f| f.parent()).map(std::path::Path::to_path_buf);
    files.iter().filter_map(|file| {
        let name = file.file_name()?.to_string_lossy().into_owned();
        let dest = match (&base, change_to_first_path) {
            (Some(base), true) => file.strip_prefix(base).ok().map(|r| r.to_string_lossy().into_owned()).unwrap_or(name),
            _ => name,
        };
        Some(crate::archive::AddItem { source: file.clone(), dest })
    }).collect()
}

/// `--check`: the plugins and what they can do here.
pub fn check_json() -> String {
    use tessel_archive_settings::plugins::{Access, PLUGINS};
    let plugins: Vec<serde_json::Value> = PLUGINS.iter().map(|p| serde_json::json!({
        "id": p.id,
        "available": p.available(),
        "read_write": p.formats.iter().filter(|f| f.1 == Access::ReadWrite).map(|f| f.0.extension()).collect::<Vec<_>>(),
        "read_only": p.formats.iter().filter(|f| f.1 == Access::ReadOnly).map(|f| f.0.extension()).collect::<Vec<_>>(),
    })).collect();
    serde_json::json!({"ready": true, "application_id": crate::APPLICATION_ID, "libarchive": crate::ffi::version(), "plugins": plugins}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ark_options() {
        let o = parse(&args(&["--batch", "--autosubfolder", "--destination", "/tmp/out", "--opendestination", "a.zip", "b.7z"])).unwrap();
        assert!(o.batch && o.auto_subfolder && o.open_destination && o.is_job());
        assert_eq!(o.destination.as_deref(), Some("/tmp/out"));
        assert_eq!(o.files, ["a.zip", "b.7z"]);
        let o = parse(&args(&["--add-to=/x/out.zip", "--changetofirstpath", "/x/a", "/x/b"])).unwrap();
        assert_eq!(o.add_to.as_deref(), Some("/x/out.zip"));
        assert!(o.change_to_first_path && o.is_add());
        let o = parse(&args(&["-c", "-f", "tar.gz", "/x/a"])).unwrap();
        assert_eq!(o.auto_filename.as_deref(), Some("tar.gz"));
        assert!(o.add);
        let o = parse(&args(&["-baO", "-o/dest", "z.zip"])).unwrap();
        assert!(o.batch && o.auto_subfolder && o.open_destination);
        assert_eq!(o.destination.as_deref(), Some("/dest"));
        let o = parse(&args(&["--dialog", "a.zip"])).unwrap();
        assert!(o.dialog && o.is_job());
        let o = parse(&args(&["a.zip", "--", "--weird-name.zip"])).unwrap();
        assert_eq!(o.files, ["a.zip", "--weird-name.zip"]);
        assert!(!o.is_job());
    }

    #[test]
    fn errors_and_help() {
        assert_eq!(parse(&args(&["--bogus"])).unwrap_err().code, 2);
        assert_eq!(parse(&args(&["--batch"])).unwrap_err().code, 2);
        assert_eq!(parse(&args(&["--destination"])).unwrap_err().code, 2);
        assert_eq!(parse(&args(&["--batch", "--add", "x"])).unwrap_err().code, 2);
        let help = parse(&args(&["--help"])).unwrap_err();
        assert_eq!(help.code, 0);
        assert!(help.stdout.contains("--autosubfolder"));
    }

    #[test]
    fn auto_names_and_relative_paths() {
        let dir = std::env::temp_dir().join(format!("tessel-archive-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("photos/sub")).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(auto_archive_path(&dir.join("notes.txt"), "zip"), dir.join("notes.zip"));
        assert_eq!(auto_archive_path(&dir.join("photos"), ".tar.gz"), dir.join("photos.tar.gz"));
        std::fs::write(dir.join("photos.tar.gz"), "").unwrap();
        assert_eq!(auto_archive_path(&dir.join("photos"), "tar.gz"), dir.join("photos_1.tar.gz"));
        let items = add_items(&[dir.join("notes.txt"), dir.join("photos/sub")], true);
        assert_eq!(items.iter().map(|i| i.dest.as_str()).collect::<Vec<_>>(), ["notes.txt", "photos/sub"]);
        let items = add_items(&[dir.join("notes.txt"), dir.join("photos/sub")], false);
        assert_eq!(items.iter().map(|i| i.dest.as_str()).collect::<Vec<_>>(), ["notes.txt", "sub"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod desktop_tests {
    #[test]
    fn desktop_file_lists_every_archive_type() {
        let text = include_str!("../data/org.tessel.ArchiveManager.desktop");
        let line = text.lines().find(|l| l.starts_with("MimeType=")).unwrap();
        let listed: Vec<&str> = line["MimeType=".len()..].split(';').filter(|m| !m.is_empty()).collect();
        assert_eq!(listed, tessel_archive_settings::plugins::Kind::all_mimes());
        assert!(text.contains("\nExec=tessel-archive %U\n"));
        assert!(text.contains("\nStartupWMClass=org.tessel.ArchiveManager\n"));
    }
}
