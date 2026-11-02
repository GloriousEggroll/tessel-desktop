//! The one place the application touches the sibling crates' APIs
//! (tessel-files-ops preferences/extras, tessel-files-model loaders), so their exact
//! signatures are adapted here only.

use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub use tessel_files_ops::preferences::{FilePreferences, GTKBookmarks};

pub type Prefs = Rc<RefCell<FilePreferences>>;
pub type Bookmarks = Rc<RefCell<GTKBookmarks>>;

pub fn pget(prefs: &Prefs, key: &str) -> Value {
    prefs.borrow().get(key)
}

pub fn pbool(prefs: &Prefs, key: &str) -> bool {
    tessel_core::pyjson::truthy(&pget(prefs, key))
}

pub fn pset(prefs: &Prefs, key: &str, value: Value) -> Result<(), String> {
    prefs.borrow_mut().set(key, value).map_err(|e| e.to_string())
}

pub fn pupdate(prefs: &Prefs, values: Map<String, Value>) -> Result<(), String> {
    prefs.borrow_mut().update(values).map_err(|e| e.to_string())
}

pub fn pdata(prefs: &Prefs) -> Map<String, Value> {
    prefs.borrow().data.clone()
}

pub fn pfolder_options(prefs: &Prefs, uri: &str) -> Map<String, Value> {
    prefs.borrow().folder_options(uri)
}

pub fn pset_folder_options(prefs: &Prefs, uri: &str, values: Map<String, Value>) -> Result<(), String> {
    prefs.borrow_mut().set_folder_options(uri, values).map_err(|e| e.to_string())
}

/// `file_extras.status_for_directory(path)` (blocking; run off the main loop).
pub fn status_for_directory(path: &str) -> Result<HashMap<String, String>, String> {
    let statuses = tessel_files_ops::extras::status_for_directory(std::path::Path::new(path), std::time::Duration::from_secs(4));
    Ok(statuses.into_iter().map(|(p, s)| (p.to_string_lossy().into_owned(), s)).collect())
}

/// `{"a": 1}` helper.
pub fn map(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}
