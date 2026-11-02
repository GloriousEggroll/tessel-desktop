//! Ark-style Dolphin "service menu" archive actions: when the suite's archive
//! manager (`tessel-archive`, org.tessel.ArchiveManager) is installed, the file
//! context menu offers an "Extract" submenu for archives and a "Compress"
//! submenu for any local selection, each running `tessel-archive` with the same
//! command line Ark's service menus run `ark` with. Without `tessel-archive` the
//! window keeps its internal Compress…/Extract… items.
//!
//! Everything here is pure (argv building, mime matching, menu planning)
//! except [`archive_manager`], a cached program lookup.

use gtk::gio;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use tessel_core::tr;

/// The archive manager's executable name.
pub const ARCHIVE_MANAGER: &str = "tessel-archive";

/// Mime types the archive manager opens (Ark's plugin set: libarchive,
/// libzip, 7z, rar/unrar, unarchiver and the single-file compressors).
/// Matched exactly (modulo shared-mime-info aliases), never by
/// inheritance: ODF/OOXML documents and other zip-based formats are not
/// "archives" to the user.
pub const ARCHIVE_MIME_TYPES: &[&str] = &[
    // zip and zip-based containers Ark lists explicitly
    "application/zip",
    "application/x-zip-compressed",
    "application/vnd.comicbook+zip",
    "application/x-cbz",
    "application/java-archive",
    "application/x-java-archive",
    "application/vnd.android.package-archive",
    "application/x-xpinstall",
    // 7-Zip, RAR, other archivers
    "application/x-7z-compressed",
    "application/x-cb7",
    "application/vnd.rar",
    "application/x-rar",
    "application/x-rar-compressed",
    "application/vnd.comicbook-rar",
    "application/x-cbr",
    "application/x-arj",
    "application/arj",
    "application/x-lha",
    "application/x-lzh-compressed",
    "application/x-ace",
    "application/x-stuffit",
    "application/x-xar",
    "application/vnd.ms-cab-compressed",
    // tar and compressed tars
    "application/x-tar",
    "application/x-cbt",
    "application/x-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-bzip3-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-lzma-compressed-tar",
    "application/x-lz4-compressed-tar",
    "application/x-zstd-compressed-tar",
    "application/x-lzip-compressed-tar",
    "application/x-lrzip-compressed-tar",
    "application/x-tzo",
    "application/x-tarz",
    // single compressed files
    "application/gzip",
    "application/x-gzip",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-bzip3",
    "application/x-xz",
    "application/x-lzma",
    "application/x-lz4",
    "application/x-lzip",
    "application/x-lrzip",
    "application/x-lzop",
    "application/zstd",
    "application/x-zstd",
    "application/x-compress",
    // images and packages
    "application/x-cd-image",
    "application/x-iso9660-image",
    "application/vnd.efi.iso",
    "application/x-cpio",
    "application/x-sv4cpio",
    "application/x-sv4crc",
    "application/vnd.debian.binary-package",
    "application/x-deb",
    "application/x-rpm",
    "application/x-source-rpm",
    "application/x-archive",
];

/// Is `content_type` one of [`ARCHIVE_MIME_TYPES`] (aliases included)?
pub fn is_archive_type(content_type: &str) -> bool {
    !content_type.is_empty()
        && ARCHIVE_MIME_TYPES.iter().any(|known| *known == content_type || gio::content_type_equals(known, content_type))
}

/// An item's archive-ness from its content type, falling back to a name
/// guess when the sniffed type is unknown (`application/octet-stream`).
pub fn is_archive(content_type: &str, name: &str) -> bool {
    if is_archive_type(content_type) {
        return true;
    }
    if content_type.is_empty() || gio::content_type_is_unknown(content_type) {
        let (guessed, _) = gio::content_type_guess(Some(name), None);
        return is_archive_type(&guessed);
    }
    false
}

/// One service-menu entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    ExtractHere,
    ExtractTo,
    ExtractSubfolder,
    CompressTarGz,
    CompressZip,
    CompressTo,
}

impl ServiceAction {
    pub const EXTRACT: [ServiceAction; 3] = [ServiceAction::ExtractHere, ServiceAction::ExtractTo, ServiceAction::ExtractSubfolder];
    pub const COMPRESS: [ServiceAction; 3] = [ServiceAction::CompressTarGz, ServiceAction::CompressZip, ServiceAction::CompressTo];

    /// The window action name (`win.<name>`).
    pub fn action_name(self) -> &'static str {
        match self {
            ServiceAction::ExtractHere => "archive-extract-here",
            ServiceAction::ExtractTo => "archive-extract-to",
            ServiceAction::ExtractSubfolder => "archive-extract-subfolder",
            ServiceAction::CompressTarGz => "archive-compress-tar-gz",
            ServiceAction::CompressZip => "archive-compress-zip",
            ServiceAction::CompressTo => "archive-compress-to",
        }
    }

    /// The English label (Ark's wording).
    pub fn english_label(self) -> &'static str {
        match self {
            ServiceAction::ExtractHere => "Extract archive here",
            ServiceAction::ExtractTo => "Extract archive to…",
            ServiceAction::ExtractSubfolder => "Extract archive here, autodetect subfolder",
            ServiceAction::CompressTarGz => "Here (as TAR.GZ)",
            ServiceAction::CompressZip => "Here (as ZIP)",
            ServiceAction::CompressTo => "Compress to…",
        }
    }

    pub fn label(self) -> String {
        tr!(self.english_label())
    }

    /// Whether the action writes into a folder chosen without a dialog
    /// (disabled when that folder is not writable, as in Ark).
    pub fn writes_here(self) -> bool {
        !matches!(self, ServiceAction::ExtractTo | ServiceAction::CompressTo)
    }

    /// The folder [`Self::writes_here`] writes into: the current folder for
    /// extraction, the first item's folder for compression
    /// (`--changetofirstpath`).
    pub fn output_folder(self, paths: &[PathBuf], current: Option<&Path>) -> Option<PathBuf> {
        match self {
            ServiceAction::ExtractHere | ServiceAction::ExtractSubfolder | ServiceAction::ExtractTo => current.map(Path::to_path_buf),
            _ => paths.first().and_then(|p| p.parent()).map(Path::to_path_buf),
        }
    }

    /// The exact argv: `program` is the resolved `tessel-archive`, `paths` the
    /// selected local items in order, `destination` the current folder
    /// (needed by the "here" extract actions only). `None` when the action
    /// cannot run with these arguments.
    pub fn argv(self, program: &str, paths: &[PathBuf], destination: Option<&Path>) -> Option<Vec<String>> {
        if paths.is_empty() {
            return None;
        }
        let mut argv: Vec<String> = vec![program.to_owned()];
        let push = |argv: &mut Vec<String>, items: &[&str]| argv.extend(items.iter().map(|s| (*s).to_owned()));
        match self {
            ServiceAction::ExtractHere | ServiceAction::ExtractSubfolder => {
                let destination = destination?;
                push(&mut argv, &["--batch"]);
                if self == ServiceAction::ExtractSubfolder {
                    push(&mut argv, &["--autosubfolder"]);
                }
                argv.push("--destination".into());
                argv.push(destination.to_string_lossy().into_owned());
            }
            ServiceAction::ExtractTo => push(&mut argv, &["--batch", "--dialog"]),
            ServiceAction::CompressTarGz => push(&mut argv, &["--add", "--changetofirstpath", "--autofilename", "tar.gz"]),
            ServiceAction::CompressZip => push(&mut argv, &["--add", "--changetofirstpath", "--autofilename", "zip"]),
            ServiceAction::CompressTo => push(&mut argv, &["--add", "--changetofirstpath", "--dialog"]),
        }
        argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        Some(argv)
    }
}

/// What the archive part of a selection's context menu shows.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MenuPlan {
    /// Extract submenu (every selected item is a local archive).
    pub extract: bool,
    /// Compress submenu (every selected item is local).
    pub compress: bool,
}

/// One selected item for [`plan`]: (has a local path, is an archive).
pub fn plan(items: &[(bool, bool)]) -> MenuPlan {
    if items.is_empty() || items.iter().any(|(local, _)| !local) {
        return MenuPlan::default();
    }
    MenuPlan { extract: items.iter().all(|(_, archive)| *archive), compress: true }
}

thread_local! {
    static CACHED: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// The absolute path of `tessel-archive` (Tessel's program directories, then
/// PATH), looked up once per process.
pub fn archive_manager() -> Option<String> {
    CACHED.with(|cached| {
        cached.borrow_mut().get_or_insert_with(|| {
            let program = tessel_core::paths::program(ARCHIVE_MANAGER);
            match program.is_absolute() {
                true => Some(program.to_string_lossy().into_owned()),
                false => tessel_files_model::util::which(ARCHIVE_MANAGER, None),
            }
        }).clone()
    })
}

/// Forget the cached lookup (tests that install/remove a fake `tessel-archive`).
pub fn forget_archive_manager() {
    CACHED.with(|cached| cached.borrow_mut().take());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn archive_types_match_arks_set_and_aliases_but_not_zip_documents() {
        for kind in ["application/zip", "application/x-7z-compressed", "application/vnd.rar", "application/x-tar",
                     "application/x-compressed-tar", "application/x-bzip2-compressed-tar", "application/x-xz-compressed-tar",
                     "application/x-zstd-compressed-tar", "application/gzip", "application/x-bzip2", "application/x-xz",
                     "application/zstd", "application/x-lz4", "application/x-lzma", "application/x-cd-image", "application/x-cpio",
                     "application/vnd.debian.binary-package", "application/x-rpm", "application/x-archive", "application/x-xar",
                     "application/vnd.ms-cab-compressed", "application/vnd.comicbook+zip", "application/vnd.comicbook-rar",
                     "application/x-cb7", "application/x-java-archive", "application/vnd.android.package-archive"] {
            assert!(is_archive_type(kind), "{kind}");
        }
        for kind in ["", "text/plain", "inode/directory", "application/pdf", "application/octet-stream",
                     "application/vnd.oasis.opendocument.text",
                     "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "image/png"] {
            assert!(!is_archive_type(kind), "{kind}");
        }
    }

    #[test]
    fn unknown_types_fall_back_to_the_name() {
        for name in ["a.zip", "b.tar.gz", "c.tgz", "d.tar.zst", "e.7z", "f.rar", "g.iso", "h.deb", "i.rpm", "j.xz", "k.cbz", "l.jar"] {
            assert!(is_archive("application/octet-stream", name), "{name}");
            assert!(is_archive("", name), "{name}");
        }
        assert!(!is_archive("application/octet-stream", "notes.txt"));
        // A sniffed known type wins over the name.
        assert!(!is_archive("text/plain", "really-text.zip"));
        assert!(is_archive("application/zip", "no-extension"));
    }

    #[test]
    fn extract_argv_matches_arks_service_menu() {
        let archives = paths(&["/data/a b.zip", "/data/c.tar.gz"]);
        let here = Path::new("/data/two words");
        let program = "/usr/bin/tessel-archive";
        assert_eq!(ServiceAction::ExtractHere.argv(program, &archives, Some(here)).unwrap(),
                   ["/usr/bin/tessel-archive", "--batch", "--destination", "/data/two words", "/data/a b.zip", "/data/c.tar.gz"]);
        assert_eq!(ServiceAction::ExtractSubfolder.argv(program, &archives, Some(here)).unwrap(),
                   ["/usr/bin/tessel-archive", "--batch", "--autosubfolder", "--destination", "/data/two words", "/data/a b.zip",
                    "/data/c.tar.gz"]);
        assert_eq!(ServiceAction::ExtractTo.argv(program, &archives[..1], Some(here)).unwrap(),
                   ["/usr/bin/tessel-archive", "--batch", "--dialog", "/data/a b.zip"]);
        // The dialog variant needs no destination; the "here" ones do.
        assert!(ServiceAction::ExtractTo.argv(program, &archives, None).is_some());
        assert!(ServiceAction::ExtractHere.argv(program, &archives, None).is_none());
        assert!(ServiceAction::ExtractHere.argv(program, &[], Some(here)).is_none());
    }

    #[test]
    fn compress_argv_matches_arks_service_menu() {
        let items = paths(&["/data/Folder", "/data/-dash.txt", "/data/$(x).txt"]);
        let program = "/opt/bin/tessel-archive";
        let tail = ["/data/Folder", "/data/-dash.txt", "/data/$(x).txt"];
        let expect = |head: &[&str]| head.iter().chain(tail.iter()).map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(ServiceAction::CompressTarGz.argv(program, &items, None).unwrap(),
                   expect(&[program, "--add", "--changetofirstpath", "--autofilename", "tar.gz"]));
        assert_eq!(ServiceAction::CompressZip.argv(program, &items, Some(Path::new("/elsewhere"))).unwrap(),
                   expect(&[program, "--add", "--changetofirstpath", "--autofilename", "zip"]));
        assert_eq!(ServiceAction::CompressTo.argv(program, &items, None).unwrap(),
                   expect(&[program, "--add", "--changetofirstpath", "--dialog"]));
        assert!(ServiceAction::CompressZip.argv(program, &[], None).is_none());
    }

    #[test]
    fn output_folders_and_names() {
        let items = paths(&["/a/b/one.zip", "/c/two.zip"]);
        let current = Path::new("/view");
        assert_eq!(ServiceAction::ExtractHere.output_folder(&items, Some(current)), Some(PathBuf::from("/view")));
        assert_eq!(ServiceAction::CompressZip.output_folder(&items, Some(current)), Some(PathBuf::from("/a/b")));
        assert!(ServiceAction::ExtractHere.writes_here() && ServiceAction::CompressTarGz.writes_here());
        assert!(!ServiceAction::ExtractTo.writes_here() && !ServiceAction::CompressTo.writes_here());
        let names: Vec<&str> = ServiceAction::EXTRACT.iter().chain(ServiceAction::COMPRESS.iter()).map(|a| a.action_name()).collect();
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), 6);
        assert!(names.iter().all(|n| crate::window::ACTIONS.split_whitespace().all(|existing| existing != *n)));
    }

    #[test]
    fn menu_plan_mirrors_ark() {
        assert_eq!(plan(&[]), MenuPlan::default());
        assert_eq!(plan(&[(true, true)]), MenuPlan { extract: true, compress: true });
        assert_eq!(plan(&[(true, true), (true, true)]), MenuPlan { extract: true, compress: true });
        // Mixed selections can be compressed but not extracted.
        assert_eq!(plan(&[(true, true), (true, false)]), MenuPlan { extract: false, compress: true });
        assert_eq!(plan(&[(true, false)]), MenuPlan { extract: false, compress: true });
        // Anything without a local path hides both.
        assert_eq!(plan(&[(true, true), (false, true)]), MenuPlan::default());
        assert_eq!(plan(&[(false, false)]), MenuPlan::default());
    }

    #[test]
    fn lookup_is_cached_until_forgotten() {
        forget_archive_manager();
        let first = archive_manager();
        assert_eq!(archive_manager(), first);
        CACHED.with(|cached| *cached.borrow_mut() = Some(Some("/fake/tessel-archive".into())));
        assert_eq!(archive_manager().as_deref(), Some("/fake/tessel-archive"));
        forget_archive_manager();
        assert_eq!(archive_manager(), first);
    }
}
