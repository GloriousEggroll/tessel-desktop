//! `network_preferences.py`: notification preferences in
//! `$XDG_CONFIG_HOME/waybar/network-notifications.json` (re-read on every
//! get, atomic writes; only a literal `true` counts).

use serde_json::{Map, Value};
use std::path::PathBuf;

pub const KEYS: [&str; 4] = ["disable-connected-notifications", "disable-disconnected-notifications",
                             "disable-vpn-notifications", "disable-portal-notifications"];

pub struct NotificationPreferences {
    pub path: PathBuf,
    values: std::cell::RefCell<Map<String, Value>>,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationPreferences {
    pub fn new() -> Self {
        Self::at(tessel_core::paths::config_home().join("waybar/network-notifications.json"))
    }

    pub fn at(path: PathBuf) -> Self {
        let prefs = Self { path, values: Default::default() };
        prefs.reload();
        prefs
    }

    fn reload(&self) {
        let values = std::fs::read_to_string(&self.path).ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| if let Value::Object(map) = value { Some(map) } else { None })
            .unwrap_or_default();
        self.values.replace(values);
    }

    /// Global Settings and the popup can edit this file independently.
    pub fn get_boolean(&self, name: &str) -> bool {
        self.reload();
        self.values.borrow().get(name) == Some(&Value::Bool(true))
    }

    pub fn set_boolean(&self, name: &str, enabled: bool) -> std::io::Result<()> {
        self.reload();
        let mut values = self.values.borrow().clone();
        values.insert(name.to_owned(), Value::Bool(enabled));
        let parent = self.path.parent().map(PathBuf::from).unwrap_or_default();
        std::fs::create_dir_all(&parent)?;
        let text = tessel_core::pyfmt::json_dumps(&Value::Object(values.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT);
        let temporary = parent.join(format!("tmp{}", tessel_core::secrets::token_urlsafe(6).replace(['-', '_'], "x")));
        let result = std::fs::write(&temporary, text).and_then(|_| std::fs::rename(&temporary, &self.path));
        let _ = std::fs::remove_file(&temporary);
        result?;
        self.values.replace(values);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_true_only_and_merged_atomic_writes() {
        let dir = std::env::temp_dir().join(format!("netprefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prefs = NotificationPreferences::at(dir.join("waybar/network-notifications.json"));
        assert!(!prefs.get_boolean("disable-vpn-notifications"));
        std::fs::create_dir_all(dir.join("waybar")).unwrap();
        std::fs::write(&prefs.path, r#"{"disable-vpn-notifications": 1, "other": "kept"}"#).unwrap();
        assert!(!prefs.get_boolean("disable-vpn-notifications"));
        prefs.set_boolean("disable-portal-notifications", true).unwrap();
        assert_eq!(std::fs::read_to_string(&prefs.path).unwrap(),
                   r#"{"disable-vpn-notifications": 1, "other": "kept", "disable-portal-notifications": true}"#);
        assert!(prefs.get_boolean("disable-portal-notifications"));
        std::fs::write(&prefs.path, "[]").unwrap();
        assert!(!prefs.get_boolean("disable-portal-notifications"));
        assert_eq!(std::fs::read_dir(dir.join("waybar")).unwrap().count(), 1, "no temporary left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
