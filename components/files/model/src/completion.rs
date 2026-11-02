//! `location_completion.py` logic (the `LocationCompletion` popover widget is
//! the lead's): resolving typed addresses and the folder query behind
//! non-blocking folder completion.

use gio::prelude::*;

use crate::util::{expanduser, has_uri_scheme, quote, starts_with_scheme, unquote};

/// `LocationCompletion.LIMIT`: at most this many suggestions.
pub const LIMIT: usize = 128;
/// Attributes the completion enumerates.
pub const COMPLETION_ATTRIBUTES: &str = "standard::name,standard::type,standard::is-hidden";
/// Debounce (ms) between typing and the lookup.
pub const LOOKUP_DELAY_MS: u32 = 100;

/// `location_file(text, current)`: relative addresses resolve against the
/// viewed folder, not the process cwd; `~` is expanded.
pub fn location_file(text: &str, current: &gio::File) -> gio::File {
    let expanded = expanduser(text);
    if expanded.starts_with('/') || has_uri_scheme(&expanded) {
        gio::File::for_commandline_arg(&expanded)
    } else {
        current.resolve_relative_path(&expanded)
    }
}

/// `FolderQuery(parent, prefix, fragment, uri)`.
#[derive(Clone, Debug)]
pub struct FolderQuery {
    pub parent: gio::File,
    pub prefix: String,
    pub fragment: String,
    pub uri: bool,
}

impl PartialEq for FolderQuery {
    fn eq(&self, other: &Self) -> bool {
        self.parent.equal(&other.parent) && self.prefix == other.prefix && self.fragment == other.fragment && self.uri == other.uri
    }
}

impl FolderQuery {
    /// The completed address for child folder `name` (escaped inside URIs).
    pub fn complete(&self, name: &str) -> String {
        let name = if self.uri { quote(name, "") } else { name.to_owned() };
        format!("{}{}/", self.prefix, name)
    }
}

/// `folder_query(text, current)`: `None` for empty text or an incomplete URI
/// authority (`smb://server`).
pub fn folder_query(text: &str, current: &gio::File) -> Option<FolderQuery> {
    if text.is_empty() {
        return None;
    }
    let text = if text == "~" { "~/" } else { text };
    let uri = starts_with_scheme(text, "://");
    if uri {
        let rest = text.split_once("://").map(|(_, rest)| rest).unwrap_or("");
        if !rest.contains('/') {
            return None;
        }
    }
    let (prefix, fragment) = match text.rfind('/') {
        Some(index) => (&text[..=index], &text[index + 1..]),
        None => ("", text),
    };
    let parent = if prefix.is_empty() { current.clone() } else { location_file(prefix, current) };
    let fragment = if uri { unquote(fragment) } else { fragment.to_owned() };
    Some(FolderQuery { parent, prefix: prefix.to_owned(), fragment, uri })
}

/// Whether an enumerated child is offered: a directory (links resolved) whose
/// name starts with `fragment`, hidden only when `include_hidden`.
/// `include_hidden` should be `show_hidden || fragment.starts_with('.')`.
pub fn matches_fragment(info: &gio::FileInfo, fragment: &str, include_hidden: bool) -> bool {
    let name = info.name();
    let name = name.to_string_lossy();
    info.file_type() == gio::FileType::Directory
        && name.starts_with(fragment)
        && (include_hidden || !(info.is_hidden() || name.starts_with('.')))
}

/// `sorted(names, key=lambda name: (name.casefold(), name))`.
pub fn sort_names(names: &mut [String]) {
    names.sort_by(|a, b| tessel_core::pyjson::casefold(a).cmp(&tessel_core::pyjson::casefold(b)).then_with(|| a.cmp(b)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("files-model-completion-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // test_location_completion.test_relative_paths_and_uri_escaping_navigate_to_the_suggested_folder (pure part)
    #[test]
    fn queries_and_relative_locations() {
        crate::test_env();
        let source = temp("q");
        std::fs::create_dir_all(source.join("Documents")).unwrap();
        let current = gio::File::for_path(&source);
        let query = folder_query("~/Doc", &current).unwrap();
        assert_eq!(query.parent.path().unwrap(), tessel_core::paths::home());
        assert_eq!(query.complete("Documents"), "~/Documents/");
        assert_eq!(query.fragment, "Doc");
        assert_eq!(location_file("../Documents", &gio::File::for_path(source.join("Documents"))).path().unwrap(), source.join("Documents"));
        assert!(folder_query("smb://server", &current).is_none());
        assert!(folder_query("", &current).is_none());
        let tilde = folder_query("~", &current).unwrap();
        assert_eq!((tilde.prefix.as_str(), tilde.fragment.as_str()), ("~/", ""));
        // Relative text completes against the viewed folder.
        let relative = folder_query("My f", &current).unwrap();
        assert!(relative.parent.equal(&current));
        assert_eq!(relative.complete("My folder #1"), "My folder #1/");
        // URIs: the fragment is unescaped, completions are escaped.
        let uri = format!("{}/My%20f", current.uri());
        let query = folder_query(&uri, &current).unwrap();
        assert!(query.uri);
        assert_eq!(query.fragment, "My f");
        assert!(query.parent.equal(&current));
        assert_eq!(query.complete("My folder #1"), format!("{}/My%20folder%20%231/", current.uri()));
        assert_eq!(location_file(&format!("{}/Documents", source.display()), &current).path().unwrap(), source.join("Documents"));
        std::fs::remove_dir_all(&source).unwrap();
    }

    // test_hidden_prefix_offers_only_real_directories_including_symlinks (model part)
    #[test]
    fn hidden_prefix_offers_only_directories() {
        crate::test_env();
        let source = temp("h");
        let target = temp("h-target");
        std::fs::create_dir(source.join(".steam")).unwrap();
        std::os::unix::fs::symlink(&target, source.join(".steam library")).unwrap();
        std::fs::write(source.join(".steps.txt"), "not a directory").unwrap();
        std::os::unix::fs::symlink(source.join("absent"), source.join(".steam-broken")).unwrap();
        std::fs::create_dir(source.join(".something")).unwrap();
        std::fs::create_dir(source.join("steam")).unwrap();
        let current = gio::File::for_path(&source);
        let text = format!("{}/.ste", source.display());
        let query = folder_query(&text, &current).unwrap();
        let include_hidden = query.fragment.starts_with('.');
        let enumerator = query
            .parent
            .enumerate_children(COMPLETION_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)
            .unwrap();
        let mut names: Vec<String> = enumerator
            .filter_map(Result::ok)
            .filter(|info| matches_fragment(info, &query.fragment, include_hidden))
            .map(|info| info.name().to_string_lossy().into_owned())
            .collect();
        sort_names(&mut names);
        let items: Vec<String> = names.iter().map(|n| query.complete(n)).collect();
        assert_eq!(items, [format!("{}/.steam/", source.display()), format!("{}/.steam library/", source.display())]);
        // Without a dot fragment hidden folders are excluded.
        let plain = folder_query(&format!("{}/", source.display()), &current).unwrap();
        let enumerator = plain
            .parent
            .enumerate_children(COMPLETION_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)
            .unwrap();
        let names: Vec<String> = enumerator
            .filter_map(Result::ok)
            .filter(|info| matches_fragment(info, &plain.fragment, false))
            .map(|info| info.name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["steam"]);
        let mut sorted = vec!["b".to_owned(), "B".to_owned(), "a".to_owned()];
        sort_names(&mut sorted);
        assert_eq!(sorted, ["a", "B", "b"]);
        std::fs::remove_dir_all(&source).unwrap();
        std::fs::remove_dir_all(&target).unwrap();
    }
}
