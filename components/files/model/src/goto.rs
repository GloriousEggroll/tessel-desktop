//! `file_goto.py` logic: fuzzy name/path ranking and the capped background
//! folder index of Ctrl+K "Go anywhere" (the `GoAnywhere` window is the
//! lead's). The index is built on demand in one worker thread, capped, and
//! dropped when the palette closes.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const INDEX_LIMIT: usize = 80_000;
pub const INDEX_DEPTH: usize = 10;
pub const RESULT_LIMIT: usize = 60;
pub const SKIP: &[&str] = &[
    "node_modules", "__pycache__", ".git", ".hg", ".svn", ".cache", ".npm", ".cargo", ".rustup", "proc", "sys", "dev",
    ".steam", "steamapps", "compatdata", "shadercache", ".var", ".wine",
];
const SEPARATORS: &[char] = &[' ', '-', '_', '.', '/'];

/// One indexed entry: `(path, name, is_dir, bonus)`.
pub type Entry = (String, String, bool, i64);
/// One ranked result: `(score, path, is_dir)`.
pub type Ranked = (i64, String, bool);

fn find_from(haystack: &[char], needle: &[char], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(haystack.len()));
    }
    if needle.len() > haystack.len() {
        return None;
    }
    (start..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// `fuzzy_score(query, text)` for an already case-folded `query`: substrings
/// beat scattered letters; `None` if absent. Indexes count characters.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = tessel_core::pyjson::casefold(text).chars().collect();
    let query: Vec<char> = query.chars().collect();
    let length = text.len() as i64;
    if let Some(found) = find_from(&text, &query, 0) {
        let boundary = found == 0 || SEPARATORS.contains(&text[found - 1]);
        let bonus = if found == 0 { 600 } else if boundary { 250 } else { 0 };
        return Some(2000 + bonus - found as i64 - length);
    }
    let (mut score, mut position, mut last) = (0i64, 0usize, -2i64);
    for character in query {
        let index = (position..text.len()).find(|&i| text[i] == character)?;
        score += if index as i64 == last + 1 {
            12
        } else if index == 0 || SEPARATORS.contains(&text[index - 1]) {
            6
        } else {
            1
        };
        last = index as i64;
        position = index + 1;
    }
    Some(score * 10 - length)
}

/// `rank(query, entries, limit)`: best `(score, path, is_dir)` matches. The
/// last word matches names; the other words must appear in the path.
pub fn rank(query: &str, entries: &[Entry], limit: usize) -> Vec<Ranked> {
    let folded = tessel_core::pyjson::casefold(query);
    let words: Vec<&str> = folded.split(crate::util::is_py_space).filter(|w| !w.is_empty()).collect();
    let Some((name_query, path_words)) = words.split_last() else { return Vec::new() };
    let mut results = Vec::new();
    for (path, name, is_dir, bonus) in entries {
        let score = match fuzzy_score(name_query, name) {
            Some(score) => score,
            None => {
                if !name_query.contains('/') {
                    continue;
                }
                match fuzzy_score(name_query, path) {
                    Some(score) => score - 500,
                    None => continue,
                }
            }
        };
        if !path_words.is_empty() {
            let folded_path = tessel_core::pyjson::casefold(path);
            if !path_words.iter().all(|word| folded_path.contains(word)) {
                continue;
            }
        }
        let depth = path.matches('/').count() as i64;
        results.push((score + bonus + if *is_dir { 40 } else { 0 } - depth * 4, path.clone(), *is_dir));
    }
    results.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    results.truncate(limit);
    results
}

/// `GoAnywhere.places`: `(path, Path(path).name or path, True, 400)` for each
/// unique non-empty place, in order.
pub fn place_entries(places: &[String]) -> Vec<Entry> {
    let mut seen = HashSet::new();
    places
        .iter()
        .filter(|p| !p.is_empty() && seen.insert(p.as_str()))
        .map(|path| {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| path.clone());
            (path.clone(), name, true, 400)
        })
        .collect()
}

#[derive(Default)]
struct IndexState {
    entries: Mutex<Vec<Entry>>,
    done: AtomicBool,
    truncated: AtomicBool,
    stopped: AtomicBool,
}

/// `FolderIndex(roots, include_hidden)`: walks local folders breadth-first in
/// one background thread ("files-goto-index") into a plain list of entries
/// (at most [`INDEX_LIMIT`], depth < [`INDEX_DEPTH`], [`SKIP`] names and —
/// unless `include_hidden` — dot names skipped, symlinks never followed).
pub struct FolderIndex {
    state: Arc<IndexState>,
    pub include_hidden: bool,
    thread: Option<JoinHandle<()>>,
}

impl FolderIndex {
    pub fn new(roots: &[std::path::PathBuf], include_hidden: bool) -> Self {
        let state = Arc::new(IndexState::default());
        let roots: Vec<String> = roots.iter().map(|r| r.to_string_lossy().into_owned()).collect();
        let worker = state.clone();
        let thread = std::thread::Builder::new()
            .name("files-goto-index".into())
            .spawn(move || {
                walk(&worker, roots, include_hidden);
                worker.done.store(true, Ordering::SeqCst);
            })
            .ok();
        if thread.is_none() {
            state.done.store(true, Ordering::SeqCst);
        }
        Self { state, include_hidden, thread }
    }

    /// A snapshot (`index.entries[:]`).
    pub fn entries(&self) -> Vec<Entry> {
        self.state.entries.lock().map(|e| e.clone()).unwrap_or_default()
    }
    /// `len(index.entries)`.
    pub fn len(&self) -> usize {
        self.state.entries.lock().map(|e| e.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn done(&self) -> bool {
        self.state.done.load(Ordering::SeqCst)
    }
    pub fn truncated(&self) -> bool {
        self.state.truncated.load(Ordering::SeqCst)
    }
    pub fn stop(&self) {
        self.state.stopped.store(true, Ordering::SeqCst);
    }
    /// `index.entries = []` (the palette drops the index on close).
    pub fn clear(&self) {
        if let Ok(mut entries) = self.state.entries.lock() {
            *entries = Vec::new();
        }
    }
    /// `index.thread.join(timeout)`; returns whether the walk finished.
    pub fn join(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.done() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
        self.done()
    }
}

impl Drop for FolderIndex {
    fn drop(&mut self) {
        // The thread is detached like Python's daemon thread; ask it to stop.
        self.stop();
    }
}

fn walk(state: &IndexState, roots: Vec<String>, include_hidden: bool) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending: VecDeque<(String, usize)> = roots.into_iter().map(|r| (r, 0)).collect();
    while let Some((folder, depth)) = pending.pop_front() {
        if state.stopped.load(Ordering::SeqCst) {
            return;
        }
        if !seen.insert(folder.clone()) {
            continue;
        }
        let Ok(listing) = std::fs::read_dir(&folder) else { continue };
        let children: Vec<_> = listing.filter_map(Result::ok).collect();
        for entry in children {
            let name = entry.file_name().to_string_lossy().into_owned();
            if (name.starts_with('.') && !include_hidden) || SKIP.contains(&name.as_str()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            let is_dir = file_type.is_dir();
            // os.path.join(folder, name)
            let path = if folder.ends_with('/') { format!("{folder}{name}") } else { format!("{folder}/{name}") };
            let count = {
                let Ok(mut entries) = state.entries.lock() else { return };
                entries.push((path.clone(), name, is_dir, 0));
                entries.len()
            };
            if count >= INDEX_LIMIT {
                state.truncated.store(true, Ordering::SeqCst);
                return;
            }
            if is_dir && depth + 1 < INDEX_DEPTH {
                pending.push_back((path, depth + 1));
            }
        }
    }
}

/// `display_path(path)`: Home becomes `~`.
pub fn display_path(path: &str) -> String {
    let home = tessel_core::paths::home().to_string_lossy().into_owned();
    if path == home || path.starts_with(&format!("{home}/")) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            ("/a/Projects/report-final.pdf".into(), "report-final.pdf".into(), false, 0),
            ("/a/Old/report.pdf".into(), "report.pdf".into(), false, 0),
            ("/a/Projects".into(), "Projects".into(), true, 0),
        ]
    }

    // test_files_browsing.test_fuzzy_ranking_and_go_anywhere (pure part)
    #[test]
    fn fuzzy_ranking() {
        assert_eq!(fuzzy_score("xyz", "Alpha.txt"), None);
        assert!(fuzzy_score("alp", "Alpha.txt").unwrap() > fuzzy_score("apt", "Alpha.txt").unwrap());
        assert_eq!(fuzzy_score("", "x"), Some(0));
        // Exact Python values.
        assert_eq!(fuzzy_score("alp", "Alpha.txt"), Some(2000 + 600 - 9));
        assert_eq!(fuzzy_score("txt", "Alpha.txt"), Some(2000 + 250 - 6 - 9));
        assert_eq!(fuzzy_score("apt", "Alpha.txt"), Some((6 + 1 + 6) * 10 - 9));
        assert_eq!(rank("report", &entries(), RESULT_LIMIT)[0].1, "/a/Old/report.pdf");
        let paths: Vec<String> = rank("projects rpf", &entries(), RESULT_LIMIT).into_iter().map(|r| r.1).collect();
        assert_eq!(paths, ["/a/Projects/report-final.pdf"]);
        assert!(rank("   ", &entries(), RESULT_LIMIT).is_empty());
        // A '/' in the name query falls back to the path at -500.
        let by_path = rank("old/rep", &entries(), RESULT_LIMIT);
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].1, "/a/Old/report.pdf");
        assert_eq!(rank("p", &entries(), 1).len(), 1);
    }

    #[test]
    fn folder_index_walks_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("files-model-goto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("Documents/deep/deeper");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("needle-notes.md"), "x").unwrap();
        std::fs::write(root.join(".secret"), "hidden").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let mut index = FolderIndex::new(&[root.clone()], false);
        assert!(index.join(Duration::from_secs(5)));
        let entries = index.entries();
        let paths: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
        assert!(paths.contains(&nested.join("needle-notes.md").to_str().unwrap()));
        assert!(!entries.iter().any(|e| e.1 == ".secret" || e.1 == "node_modules" || e.1 == "pkg"));
        let link = entries.iter().find(|e| e.1 == "loop").unwrap();
        assert!(!link.2, "symlinks are not followed");
        assert!(!index.truncated());
        let ranked = rank("needle", &index.entries(), RESULT_LIMIT);
        assert_eq!(ranked[0].1, nested.join("needle-notes.md").to_string_lossy());
        index.clear();
        assert!(index.entries().is_empty());
        let mut hidden = FolderIndex::new(&[root.clone()], true);
        assert!(hidden.join(Duration::from_secs(5)));
        assert!(hidden.entries().iter().any(|e| e.1 == ".secret"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn display_path_and_places() {
        let home = tessel_core::paths::home().to_string_lossy().into_owned();
        assert_eq!(display_path(&home), "~");
        assert_eq!(display_path(&format!("{home}/Documents")), "~/Documents");
        assert_eq!(display_path(&format!("{home}x/Documents")), format!("{home}x/Documents"));
        assert_eq!(display_path("/etc"), "/etc");
        let places = place_entries(&["/".into(), "/home/a".into(), "".into(), "/home/a".into()]);
        assert_eq!(places, vec![("/".into(), "/".into(), true, 400), ("/home/a".into(), "a".into(), true, 400)]);
    }
}
