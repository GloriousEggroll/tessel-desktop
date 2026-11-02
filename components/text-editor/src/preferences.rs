//! `$XDG_CONFIG_HOME/tessel/tessel-text-editor.json` (editor_ui.Preferences).

use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub fn config_path() -> PathBuf {
    tessel_core::paths::config_home().join("tessel/tessel-text-editor.json")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub wrap: bool,
    pub line_numbers: bool,
    pub whitespace: bool,
    pub auto_indent: bool,
    pub spaces: bool,
    pub tab_width: i64,
    pub font: String,
    pub vim: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { wrap: true, line_numbers: true, whitespace: false, auto_indent: true, spaces: true, tab_width: 4,
               font: "Monospace 11".into(), vim: false }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Preferences {
    pub options: Options,
    pub recent: Vec<String>,
}

impl Preferences {
    /// Load like Python: each key is taken only when `type(candidate) is type(default)`.
    pub fn load() -> Self {
        Self::from_text(std::fs::read_to_string(config_path()).ok().as_deref())
    }

    pub fn from_text(text: Option<&str>) -> Self {
        let mut result = Self::default();
        let Some(value) = text.and_then(|t| serde_json::from_str::<Value>(t).ok()) else { return result };
        let Some(object) = value.as_object() else { return result };
        let flag = |key: &str, slot: &mut bool| {
            if let Some(Value::Bool(b)) = object.get(key) {
                *slot = *b;
            }
        };
        let o = &mut result.options;
        flag("wrap", &mut o.wrap);
        flag("line_numbers", &mut o.line_numbers);
        flag("whitespace", &mut o.whitespace);
        flag("auto_indent", &mut o.auto_indent);
        flag("spaces", &mut o.spaces);
        if let Some(Value::Number(n)) = object.get("tab_width") {
            if let Some(width) = n.as_i64().or_else(|| n.as_u64().map(|_| i64::MAX)) {
                o.tab_width = width;
            }
        }
        if let Some(Value::String(font)) = object.get("font") {
            o.font = font.clone();
        }
        flag("vim", &mut o.vim);
        if let Some(Value::Array(items)) = object.get("recent") {
            result.recent = items.iter().filter_map(Value::as_str).map(str::to_owned).take(12).collect();
        }
        o.tab_width = o.tab_width.clamp(1, 16);
        result
    }

    /// `json.dumps(values, indent=2) + "\n"` in DEFAULTS key order.
    pub fn to_text(&self) -> String {
        let o = &self.options;
        let mut map = Map::new();
        map.insert("wrap".into(), json!(o.wrap));
        map.insert("line_numbers".into(), json!(o.line_numbers));
        map.insert("whitespace".into(), json!(o.whitespace));
        map.insert("auto_indent".into(), json!(o.auto_indent));
        map.insert("spaces".into(), json!(o.spaces));
        map.insert("tab_width".into(), json!(o.tab_width));
        map.insert("font".into(), json!(o.font));
        map.insert("vim".into(), json!(o.vim));
        map.insert("recent".into(), json!(self.recent));
        let style = JsonStyle { indent: Some(2), item_separator: ",", ..JsonStyle::DEFAULT };
        json_dumps(&Value::Object(map), &style) + "\n"
    }

    /// Atomic `<name>.<pid>.tmp` + chmod 0600 + rename; errors are ignored.
    pub fn save(&self) {
        let path = config_path();
        let _ = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().expect("config dir"))?;
            let temporary = path.with_file_name(format!("{}.{}.tmp", path.file_name().unwrap().to_string_lossy(), std::process::id()));
            std::fs::write(&temporary, self.to_text())?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&temporary, &path)
        })();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_types_and_clamps_follow_python() {
        assert_eq!(Preferences::from_text(None), Preferences::default());
        assert_eq!(Preferences::from_text(Some("[1, 2]")), Preferences::default());
        let p = Preferences::from_text(Some(r#"{"wrap": 0, "line_numbers": false, "tab_width": 99, "font": "Mono 9",
            "vim": true, "spaces": "yes", "recent": ["a", 3, "b", "c","d","e","f","g","h","i","j","k","l","m"]}"#));
        assert!(p.options.wrap, "int is not bool");
        assert!(!p.options.line_numbers);
        assert!(p.options.spaces);
        assert_eq!((p.options.tab_width, p.options.font.as_str(), p.options.vim), (16, "Mono 9", true));
        assert_eq!(p.recent.len(), 12);
        assert_eq!(&p.recent[..3], &["a", "b", "c"]);
        assert_eq!(Preferences::from_text(Some(r#"{"tab_width": 4.0}"#)).options.tab_width, 4);
        assert_eq!(Preferences::from_text(Some(r#"{"tab_width": true}"#)).options.tab_width, 4);
        assert_eq!(Preferences::from_text(Some(r#"{"tab_width": -3}"#)).options.tab_width, 1);
    }

    #[test]
    fn text_matches_python_json_dumps_indent_2() {
        let mut p = Preferences::default();
        assert_eq!(p.to_text(), "{\n  \"wrap\": true,\n  \"line_numbers\": true,\n  \"whitespace\": false,\n  \"auto_indent\": true,\n  \"spaces\": true,\n  \"tab_width\": 4,\n  \"font\": \"Monospace 11\",\n  \"vim\": false,\n  \"recent\": []\n}\n");
        p.recent = vec!["/tmp/é".into()];
        assert!(p.to_text().ends_with("\"recent\": [\n    \"/tmp/\\u00e9\"\n  ]\n}\n"));
    }
}
