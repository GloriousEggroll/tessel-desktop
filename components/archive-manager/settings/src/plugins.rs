//! Formats and backends ("plugins", as in Ark's kerfuffle plugins): which
//! backend handles which format, read-only or read-write, at what priority.

use crate::n_;

/// Archive formats (`Kind::label()` is shown to users).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Tar, TarGz, TarBz2, TarXz, TarLzma, TarZst, TarLz, TarLzo, TarLz4, TarLrz, TarZ,
    Zip, SevenZip, Rar,
    Gz, Bz2, Xz, Lzma, Zst, Lz, Lzo, Lz4, Lrz, Z,
    Iso, Cpio, Deb, Rpm, Ar, Xar, Cab, Lha, Arj, Squashfs, AppImage, Wim,
}

pub struct FormatInfo {
    pub kind: Kind,
    pub label: &'static str,
    /// Main mime type first.
    pub mimes: &'static [&'static str],
    /// Lower-case extensions, longest first, without the dot. The first is used for new archives.
    pub extensions: &'static [&'static str],
}

pub const FORMATS: &[FormatInfo] = &[
    FormatInfo { kind: Kind::Tar, label: n_!("Tar archive"), mimes: &["application/x-tar", "application/x-cbt"], extensions: &["tar", "cbt"] },
    FormatInfo { kind: Kind::TarGz, label: n_!("Tar archive (gzip)"), mimes: &["application/x-compressed-tar"], extensions: &["tar.gz", "tgz"] },
    FormatInfo { kind: Kind::TarBz2, label: n_!("Tar archive (bzip2)"), mimes: &["application/x-bzip2-compressed-tar", "application/x-bzip-compressed-tar"], extensions: &["tar.bz2", "tbz2", "tbz", "tb2"] },
    FormatInfo { kind: Kind::TarXz, label: n_!("Tar archive (xz)"), mimes: &["application/x-xz-compressed-tar"], extensions: &["tar.xz", "txz"] },
    FormatInfo { kind: Kind::TarLzma, label: n_!("Tar archive (lzma)"), mimes: &["application/x-lzma-compressed-tar"], extensions: &["tar.lzma", "tlz"] },
    FormatInfo { kind: Kind::TarZst, label: n_!("Tar archive (zstd)"), mimes: &["application/x-zstd-compressed-tar"], extensions: &["tar.zst", "tzst"] },
    FormatInfo { kind: Kind::TarLz, label: n_!("Tar archive (lzip)"), mimes: &["application/x-lzip-compressed-tar"], extensions: &["tar.lz"] },
    FormatInfo { kind: Kind::TarLzo, label: n_!("Tar archive (lzop)"), mimes: &["application/x-tzo"], extensions: &["tar.lzo", "tzo"] },
    FormatInfo { kind: Kind::TarLz4, label: n_!("Tar archive (lz4)"), mimes: &["application/x-lz4-compressed-tar"], extensions: &["tar.lz4"] },
    FormatInfo { kind: Kind::TarLrz, label: n_!("Tar archive (lrzip)"), mimes: &["application/x-lrzip-compressed-tar"], extensions: &["tar.lrz", "tlrz"] },
    FormatInfo { kind: Kind::TarZ, label: n_!("Tar archive (compress)"), mimes: &["application/x-tarz"], extensions: &["tar.z", "taz"] },
    FormatInfo { kind: Kind::Zip, label: n_!("Zip archive"), mimes: &["application/zip", "application/x-zip-compressed", "application/vnd.comicbook+zip", "application/x-cbz", "application/x-java-archive"], extensions: &["zip", "cbz", "jar"] },
    FormatInfo { kind: Kind::SevenZip, label: n_!("7-Zip archive"), mimes: &["application/x-7z-compressed", "application/x-cb7"], extensions: &["7z", "cb7"] },
    FormatInfo { kind: Kind::Rar, label: n_!("RAR archive"), mimes: &["application/vnd.rar", "application/x-rar", "application/x-rar-compressed", "application/vnd.comicbook-rar", "application/x-cbr"], extensions: &["rar", "cbr"] },
    FormatInfo { kind: Kind::Gz, label: n_!("Gzip-compressed file"), mimes: &["application/gzip", "application/x-gzip"], extensions: &["gz"] },
    FormatInfo { kind: Kind::Bz2, label: n_!("Bzip2-compressed file"), mimes: &["application/x-bzip2", "application/x-bzip"], extensions: &["bz2"] },
    FormatInfo { kind: Kind::Xz, label: n_!("XZ-compressed file"), mimes: &["application/x-xz"], extensions: &["xz"] },
    FormatInfo { kind: Kind::Lzma, label: n_!("LZMA-compressed file"), mimes: &["application/x-lzma"], extensions: &["lzma"] },
    FormatInfo { kind: Kind::Zst, label: n_!("Zstandard-compressed file"), mimes: &["application/zstd", "application/x-zstd"], extensions: &["zst"] },
    FormatInfo { kind: Kind::Lz, label: n_!("Lzip-compressed file"), mimes: &["application/x-lzip"], extensions: &["lz"] },
    FormatInfo { kind: Kind::Lzo, label: n_!("LZO-compressed file"), mimes: &["application/x-lzop"], extensions: &["lzo"] },
    FormatInfo { kind: Kind::Lz4, label: n_!("LZ4-compressed file"), mimes: &["application/x-lz4"], extensions: &["lz4"] },
    FormatInfo { kind: Kind::Lrz, label: n_!("Lrzip-compressed file"), mimes: &["application/x-lrzip"], extensions: &["lrz"] },
    FormatInfo { kind: Kind::Z, label: n_!("Compress-compressed file"), mimes: &["application/x-compress"], extensions: &["z"] },
    FormatInfo { kind: Kind::Iso, label: n_!("ISO disc image"), mimes: &["application/x-cd-image", "application/x-iso9660-image"], extensions: &["iso"] },
    FormatInfo { kind: Kind::Cpio, label: n_!("CPIO archive"), mimes: &["application/x-cpio", "application/x-sv4cpio", "application/x-sv4crc", "application/x-bcpio"], extensions: &["cpio"] },
    FormatInfo { kind: Kind::Deb, label: n_!("Debian package"), mimes: &["application/vnd.debian.binary-package", "application/x-deb"], extensions: &["deb", "udeb"] },
    FormatInfo { kind: Kind::Rpm, label: n_!("RPM package"), mimes: &["application/x-rpm", "application/x-source-rpm", "application/x-redhat-package-manager"], extensions: &["rpm"] },
    FormatInfo { kind: Kind::Ar, label: n_!("Unix archive"), mimes: &["application/x-archive"], extensions: &["a", "ar"] },
    FormatInfo { kind: Kind::Xar, label: n_!("XAR archive"), mimes: &["application/x-xar"], extensions: &["xar", "pkg"] },
    FormatInfo { kind: Kind::Cab, label: n_!("Cabinet archive"), mimes: &["application/vnd.ms-cab-compressed"], extensions: &["cab"] },
    FormatInfo { kind: Kind::Lha, label: n_!("LHA archive"), mimes: &["application/x-lha", "application/x-lzh-compressed"], extensions: &["lha", "lzh"] },
    FormatInfo { kind: Kind::Arj, label: n_!("ARJ archive"), mimes: &["application/x-arj"], extensions: &["arj"] },
    FormatInfo { kind: Kind::Squashfs, label: n_!("SquashFS image"), mimes: &["application/vnd.squashfs"], extensions: &["squashfs", "sqsh", "sfs", "snap"] },
    FormatInfo { kind: Kind::AppImage, label: n_!("AppImage"), mimes: &["application/vnd.appimage", "application/x-iso9660-appimage"], extensions: &["appimage"] },
    FormatInfo { kind: Kind::Wim, label: n_!("Windows image"), mimes: &["application/x-ms-wim"], extensions: &["wim", "swm", "esd"] },
];

impl Kind {
    pub fn info(self) -> &'static FormatInfo {
        FORMATS.iter().find(|f| f.kind == self).expect("every kind has a FormatInfo")
    }
    pub fn label(self) -> &'static str {
        self.info().label
    }
    pub fn mime(self) -> &'static str {
        self.info().mimes[0]
    }
    pub fn extension(self) -> &'static str {
        self.info().extensions[0]
    }
    pub fn is_tar(self) -> bool {
        matches!(self, Kind::Tar | Kind::TarGz | Kind::TarBz2 | Kind::TarXz | Kind::TarLzma | Kind::TarZst | Kind::TarLz
            | Kind::TarLzo | Kind::TarLz4 | Kind::TarLrz | Kind::TarZ)
    }
    /// A compressor of one file (no archive structure).
    pub fn is_single_file(self) -> bool {
        matches!(self, Kind::Gz | Kind::Bz2 | Kind::Xz | Kind::Lzma | Kind::Zst | Kind::Lz | Kind::Lzo | Kind::Lz4
            | Kind::Lrz | Kind::Z)
    }
    pub fn from_mime(mime: &str) -> Option<Kind> {
        FORMATS.iter().find(|f| f.mimes.contains(&mime)).map(|f| f.kind)
    }
    /// By file name: the longest matching extension wins (`x.tar.gz` is TarGz, not Gz).
    pub fn from_name(name: &str) -> Option<Kind> {
        let lower = name.to_lowercase();
        let mut best: Option<(usize, Kind)> = None;
        for format in FORMATS {
            for extension in format.extensions {
                if lower.len() > extension.len() + 1 && lower.ends_with(extension)
                    && lower.as_bytes()[lower.len() - extension.len() - 1] == b'.'
                    && best.is_none_or(|(len, _)| extension.len() > len) {
                    best = Some((extension.len(), format.kind));
                }
            }
        }
        best.map(|(_, kind)| kind)
    }
    /// Every archive mime type (for the .desktop file and mimeapps defaults).
    pub fn all_mimes() -> Vec<&'static str> {
        let mut all: Vec<&str> = FORMATS.iter().flat_map(|f| f.mimes.iter().copied()).collect();
        all.dedup();
        all
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

pub struct Plugin {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Executables that must all be on PATH (empty: built in).
    pub tools: &'static [&'static str],
    /// (format, access, priority): the highest enabled, available priority wins.
    pub formats: &'static [(Kind, Access, u32)],
}

use Access::{ReadOnly as R, ReadWrite as W};
use Kind::*;

pub const PLUGINS: &[Plugin] = &[
    Plugin {
        id: "libarchive", name: "libarchive",
        description: n_!("Tar archives with every compression filter; reads many other formats."),
        tools: &[],
        formats: &[(Tar, W, 100), (TarGz, W, 100), (TarBz2, W, 100), (TarXz, W, 100), (TarLzma, W, 100), (TarZst, W, 100),
                   (TarLz, W, 100), (TarLzo, W, 100), (TarLz4, W, 100), (TarLrz, R, 100), (TarZ, W, 100),
                   (Zip, R, 50), (SevenZip, R, 50), (Rar, R, 50), (Iso, R, 100), (Cpio, R, 100), (Deb, R, 100), (Rpm, R, 100),
                   (Ar, R, 100), (Xar, R, 100), (Cab, R, 100), (Lha, R, 100)],
    },
    Plugin {
        id: "libsinglefile", name: n_!("Single-file compressors"),
        description: n_!("Files compressed on their own with gzip, bzip2, xz, zstd, lz4 and others."),
        tools: &[],
        formats: &[(Gz, W, 100), (Bz2, W, 100), (Xz, W, 100), (Lzma, W, 100), (Zst, W, 100), (Lz, W, 100), (Lzo, W, 100),
                   (Lz4, W, 100), (Lrz, R, 100), (Z, W, 100)],
    },
    Plugin {
        id: "cli7z", name: "7-Zip",
        description: n_!("7z and zip archives with passwords, encrypted headers and split volumes; reads disc and package images."),
        tools: &["7z"],
        formats: &[(SevenZip, W, 180), (Zip, W, 180), (Rar, R, 90), (Iso, R, 80), (Cab, R, 80), (Cpio, R, 80), (Deb, R, 80),
                   (Ar, R, 80), (Rpm, R, 80), (Xar, R, 80), (Lha, R, 80), (Arj, R, 80), (Wim, R, 80), (Squashfs, R, 80),
                   (AppImage, R, 80)],
    },
    Plugin {
        id: "clizip", name: "Info-ZIP",
        description: n_!("Zip archives with the zip and unzip programs."),
        tools: &["zip", "unzip"],
        formats: &[(Zip, W, 160)],
    },
    Plugin {
        id: "cliunarchiver", name: "The Unarchiver",
        description: n_!("RAR archives (also encrypted and multi-volume) with unar and lsar."),
        tools: &["unar", "lsar"],
        formats: &[(Rar, R, 120), (Arj, R, 60), (Lha, R, 60)],
    },
];

pub fn find(id: &str) -> Option<&'static Plugin> {
    PLUGINS.iter().find(|p| p.id == id)
}

impl Plugin {
    pub fn available(&self) -> bool {
        self.tools.iter().all(|tool| suite_core::shell::which(tool).is_some())
    }
    pub fn access(&self, kind: Kind) -> Option<(Access, u32)> {
        self.formats.iter().find(|(k, _, _)| *k == kind).map(|(_, a, p)| (*a, *p))
    }
}

/// Plugins able to `access` this format, best first, skipping disabled and
/// unavailable ones (`enabled(id)`, `available(plugin)` injectable for tests).
pub fn ranked(kind: Kind, write: bool, enabled: &dyn Fn(&str) -> bool, available: &dyn Fn(&Plugin) -> bool) -> Vec<&'static Plugin> {
    let mut list: Vec<(&Plugin, u32)> = PLUGINS.iter()
        .filter(|p| enabled(p.id) && available(p))
        .filter_map(|p| p.access(kind).filter(|(a, _)| !write || *a == Access::ReadWrite).map(|(_, prio)| (p, prio)))
        .collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    list.into_iter().map(|(p, _)| p).collect()
}

/// Formats new archives can be created in with the given plugins (create dialog).
pub fn writable_kinds(enabled: &dyn Fn(&str) -> bool, available: &dyn Fn(&Plugin) -> bool) -> Vec<Kind> {
    FORMATS.iter().map(|f| f.kind).filter(|k| !ranked(*k, true, enabled, available).is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_extension_wins() {
        assert_eq!(Kind::from_name("a.TAR.GZ"), Some(TarGz));
        assert_eq!(Kind::from_name("a.gz"), Some(Gz));
        assert_eq!(Kind::from_name("a.tar.zst"), Some(TarZst));
        assert_eq!(Kind::from_name("comic.cbr"), Some(Rar));
        assert_eq!(Kind::from_name("gz"), None);
        assert_eq!(Kind::from_name("notes.txt"), None);
    }

    #[test]
    fn every_kind_has_info_and_unique_mimes() {
        let mut seen = std::collections::HashSet::new();
        for f in FORMATS {
            assert_eq!(f.kind.info().kind, f.kind);
            for m in f.mimes {
                assert!(seen.insert(*m), "duplicate mime {m}");
            }
        }
        assert_eq!(Kind::from_mime("application/vnd.comicbook+zip"), Some(Zip));
    }

    #[test]
    fn priorities_follow_ark() {
        let all = |_: &str| true;
        let avail = |_: &Plugin| true;
        assert_eq!(ranked(Zip, false, &all, &avail).iter().map(|p| p.id).collect::<Vec<_>>(), ["cli7z", "clizip", "libarchive"]);
        assert_eq!(ranked(Zip, true, &all, &avail).iter().map(|p| p.id).collect::<Vec<_>>(), ["cli7z", "clizip"]);
        assert_eq!(ranked(Rar, false, &all, &avail)[0].id, "cliunarchiver");
        assert!(ranked(Rar, true, &all, &avail).is_empty());
        assert_eq!(ranked(TarXz, true, &all, &avail)[0].id, "libarchive");
        let no7z = |id: &str| id != "cli7z";
        assert_eq!(ranked(SevenZip, false, &no7z, &avail)[0].id, "libarchive");
        assert!(ranked(SevenZip, true, &no7z, &avail).is_empty());
        let kinds = writable_kinds(&all, &avail);
        assert!(kinds.contains(&SevenZip) && kinds.contains(&TarGz) && kinds.contains(&Gz) && !kinds.contains(&Iso));
    }
}
