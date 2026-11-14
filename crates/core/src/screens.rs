//! Names for screens, shared by everything that lists them.

use crate::pyjson::{casefold, is_py_space};

/// `monitor_label(name, description, model)`: compositor metadata rather
/// than GTK's placeholder model names.
pub fn monitor_label(name: &str, description: &str, model: &str) -> String {
    fn known(value: &str) -> String {
        let value = value.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        if ["unknown", "(null)", "none", "n/a"].contains(&casefold(&value).as_str()) { String::new() } else { value }
    }
    let mut detail = known(description);
    let suffix = format!(" ({name})");
    if detail.ends_with(&suffix) {
        detail = known(&detail[..detail.len() - suffix.len()]);
    }
    if detail.is_empty() {
        detail = known(model);
    }
    if !detail.is_empty() && detail != name { format!("{name} · {detail}") } else { name.to_owned() }
}
