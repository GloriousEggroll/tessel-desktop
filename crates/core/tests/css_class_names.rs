//! GTK style classes are `tessel-<part>`. The Tessel rename once turned CSS selectors
//! into `.tessel_<part>` while the windows kept `tessel-<part>`, and every rule in
//! those style sheets silently stopped applying (no padding, square corners).

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs" || e == "css") {
            out.push(path);
        }
    }
}

/// `.tessel_x` that is a CSS class selector, not Rust (`a::tessel_x`, `..tessel_x`, `self.tessel_x`).
fn snake_selectors(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut start = 0;
    while let Some(offset) = text[start..].find(".tessel_") {
        let at = start + offset;
        start = at + 1;
        let before = text[..at].trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let word = &text[before.len()..at];
        let rust = at > 0 && matches!(bytes[at - 1], b'.' | b':')
            || (!word.is_empty() && !matches!(word, "window" | "box" | "label" | "button" | "entry" | "popover"));
        if !rust {
            let name: String = text[at + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            found.push(name);
        }
    }
    found
}

#[test]
fn css_selectors_use_the_hyphenated_class_names() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rust_files(&root.join("components"), &mut files);
    rust_files(&root.join("crates"), &mut files);
    let mut bad = Vec::new();
    for file in files {
        if file.ends_with("core/tests/css_class_names.rs") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else { continue };
        for name in snake_selectors(&text) {
            bad.push(format!("{}: .{name}", file.display()));
        }
    }
    assert!(bad.is_empty(), "snake_case style selectors never match a widget's tessel-* class:\n{}", bad.join("\n"));
}

#[test]
fn the_scanner_tells_css_from_rust() {
    assert_eq!(snake_selectors("window.tessel_lock { } .tessel_media .x {}"), ["tessel_lock", "tessel_media"]);
    assert!(snake_selectors("tessel_core::x; ..tessel_core::y; self.tessel_clipboard.kill()").is_empty());
}
