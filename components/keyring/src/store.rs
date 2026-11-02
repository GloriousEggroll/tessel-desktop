//! The keyring on disk, `~/.local/share/tessel/keyring/` (0700):
//!
//! * `master.json`: the master key wrapped with the login password ([`crate::crypto::Wrapped`]),
//! * `collections/<id>.json`: label, times, item ids with their attribute index
//!   in clear; labels, attributes and secrets sealed with the master key,
//! * `aliases.json`: `{"default": "login"}`.
//!
//! Every file is written 0600 through a temporary file and rename.

use crate::crypto::{self, attribute_hash, b64, unb64, Kdf, MasterKey, Wrapped};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub const LOGIN: &str = "login";
pub const SESSION: &str = "session";

pub fn default_dir() -> PathBuf {
    tessel_core::paths::suite_data_dir().join("keyring")
}

/// Where the keyring lived before Tessel (`~/.local/share/desktop-suite/keyring`).
pub fn legacy_dir() -> PathBuf {
    tessel_core::paths::data_home().join("desktop-suite/keyring")
}

/// Copy the pre-Tessel keyring into `dir` when `dir` does not exist yet. The PAM
/// login runs before tessel-session-startup's migration, so the store moves
/// itself: copied into a temporary sibling (modes kept), then renamed into place.
/// The old directory is never changed. Errors leave `dir` missing (a fresh keyring).
pub fn migrate_legacy(legacy: &Path, dir: &Path) -> Result<bool, String> {
    if std::fs::symlink_metadata(dir).is_ok() || !legacy.is_dir() {
        return Ok(false);
    }
    let parent = dir.parent().ok_or("keyring directory has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = parent.join(format!(".{name}.migrating-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporary);
    let result = copy_tree(legacy, &temporary).and_then(|()| match std::fs::rename(&temporary, dir) {
        Ok(()) => Ok(true),
        // Another process finished first: keep its copy.
        Err(_) if dir.exists() => Ok(false),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    });
    let _ = std::fs::remove_dir_all(&temporary);
    result
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let error = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    let mode = std::fs::metadata(from).map_err(|e| error(from, e))?.permissions().mode() & 0o7777;
    std::fs::create_dir(to).map_err(|e| error(to, e))?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700)).map_err(|e| error(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| error(from, e))? {
        let entry = entry.map_err(|e| error(from, e))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| error(&source, e))?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() {
            // std::fs::copy keeps the permission bits (0600).
            std::fs::copy(&source, &target).map_err(|e| error(&source, e))?;
        }
    }
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode)).map_err(|e| error(to, e))
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: Zeroizing<Vec<u8>>,
    pub content_type: String,
    pub created: u64,
    pub modified: u64,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Item").field("id", &self.id).field("label", &self.label).field("attributes", &self.attributes)
            .finish_non_exhaustive()
    }
}

impl Item {
    fn to_json(&self) -> Value {
        json!({"id": self.id, "label": self.label, "attributes": self.attributes, "secret": b64(&self.secret),
               "content_type": self.content_type, "created": self.created, "modified": self.modified})
    }

    fn from_json(value: &Value) -> Option<Item> {
        let attributes = value.get("attributes")?.as_object()?.iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect();
        Some(Item {
            id: value.get("id")?.as_u64()?,
            label: value.get("label")?.as_str()?.to_owned(),
            attributes,
            secret: Zeroizing::new(unb64(value.get("secret")?.as_str()?)?),
            content_type: value.get("content_type").and_then(Value::as_str).unwrap_or("text/plain").to_owned(),
            created: value.get("created").and_then(Value::as_u64).unwrap_or(0),
            modified: value.get("modified").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    pub fn matches(&self, query: &BTreeMap<String, String>) -> bool {
        query.iter().all(|(k, v)| self.attributes.get(k) == Some(v))
    }
}

#[derive(Debug)]
pub struct Collection {
    pub id: String,
    pub label: String,
    pub created: u64,
    pub modified: u64,
    salt: Vec<u8>,
    next_id: u64,
    /// false for the in-memory session collection.
    pub persistent: bool,
    /// Item id -> attribute hashes (known while locked).
    index: BTreeMap<u64, Vec<String>>,
    /// `Some` while unlocked.
    items: Option<Vec<Item>>,
    sealed: Option<(Vec<u8>, Vec<u8>)>,
}

fn collection_aad(id: &str) -> Vec<u8> {
    // On-disk format constant: keeps the pre-Tessel name (existing keyrings).
    format!("waykeyring v1 collection {id}").into_bytes()
}

impl Collection {
    pub fn new(id: &str, label: &str, persistent: bool) -> Result<Self, String> {
        let t = now();
        Ok(Collection {
            id: id.to_owned(), label: label.to_owned(), created: t, modified: t, salt: crypto::random_vec(16)?,
            next_id: 1, persistent, index: BTreeMap::new(), items: Some(Vec::new()), sealed: None,
        })
    }

    pub fn locked(&self) -> bool {
        self.items.is_none()
    }

    pub fn item_ids(&self) -> Vec<u64> {
        self.index.keys().copied().collect()
    }

    pub fn items(&self) -> Option<&[Item]> {
        self.items.as_deref()
    }

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.as_ref()?.iter().find(|i| i.id == id)
    }

    fn hashes(&self, attributes: &BTreeMap<String, String>) -> Vec<String> {
        attributes.iter().map(|(k, v)| attribute_hash(&self.salt, k, v)).collect()
    }

    /// Ids whose attributes include every pair of `query` (works while locked).
    pub fn search(&self, query: &BTreeMap<String, String>) -> Vec<u64> {
        let wanted = self.hashes(query);
        self.index.iter().filter(|(_, hashes)| wanted.iter().all(|h| hashes.contains(h))).map(|(id, _)| *id).collect()
    }

    /// Add or (with `replace`, same attributes) update an item; returns its id.
    pub fn store(&mut self, label: &str, attributes: BTreeMap<String, String>, secret: Zeroizing<Vec<u8>>,
                 content_type: &str, replace: bool) -> Option<u64> {
        let hashes = self.hashes(&attributes);
        let items = self.items.as_mut()?;
        let t = now();
        if replace {
            if let Some(item) = items.iter_mut().find(|i| i.attributes == attributes) {
                item.label = label.to_owned();
                item.secret = secret;
                item.content_type = content_type.to_owned();
                item.modified = t;
                self.modified = t;
                return Some(item.id);
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        items.push(Item { id, label: label.to_owned(), attributes, secret, content_type: content_type.to_owned(),
                          created: t, modified: t });
        self.index.insert(id, hashes);
        self.modified = t;
        Some(id)
    }

    /// Change an unlocked item.
    pub fn update(&mut self, id: u64, edit: impl FnOnce(&mut Item)) -> bool {
        let Some(items) = self.items.as_mut() else { return false };
        let Some(item) = items.iter_mut().find(|i| i.id == id) else { return false };
        edit(item);
        item.modified = now();
        let hashes = item.attributes.iter().map(|(k, v)| attribute_hash(&self.salt, k, v)).collect();
        self.index.insert(id, hashes);
        self.modified = now();
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let Some(items) = self.items.as_mut() else { return false };
        let before = items.len();
        items.retain(|i| i.id != id);
        self.index.remove(&id);
        self.modified = now();
        items.len() != before
    }

    pub fn lock(&mut self) {
        if self.persistent && self.sealed.is_some() {
            self.items = None;
        }
    }

    /// Decrypt with the master key; false if it does not open (wrong key or damaged file).
    pub fn unlock(&mut self, master: &MasterKey) -> bool {
        if self.items.is_some() {
            return true;
        }
        let Some((nonce, sealed)) = &self.sealed else { return false };
        let Some(plain) = crypto::open(master.bytes(), &collection_aad(&self.id), nonce, sealed) else { return false };
        let Ok(value) = serde_json::from_slice::<Value>(&plain) else { return false };
        let items: Vec<Item> = value.get("items").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Item::from_json).collect()).unwrap_or_default();
        if let Some(label) = value.get("label").and_then(Value::as_str) {
            self.label = label.to_owned();
        }
        self.index = items.iter().map(|i| (i.id, self.hashes(&i.attributes))).collect();
        self.next_id = self.next_id.max(items.iter().map(|i| i.id + 1).max().unwrap_or(1));
        self.items = Some(items);
        true
    }

    fn to_json(&mut self, master: &MasterKey) -> Result<Value, String> {
        let items = self.items.as_ref().ok_or("the collection is locked")?;
        let plain = Zeroizing::new(serde_json::to_vec(&json!({
            "label": self.label, "items": items.iter().map(Item::to_json).collect::<Vec<_>>()
        })).map_err(|e| e.to_string())?);
        let (nonce, sealed) = crypto::seal(master.bytes(), &collection_aad(&self.id), &plain)?;
        let index: Map<String, Value> = self.index.iter().map(|(id, h)| (id.to_string(), json!(h))).collect();
        let value = json!({
            "version": 1, "id": self.id, "label": self.label, "created": self.created, "modified": self.modified,
            "salt": b64(&self.salt), "next_id": self.next_id, "index": index,
            "nonce": b64(&nonce), "sealed": b64(&sealed),
        });
        self.sealed = Some((nonce, sealed));
        Ok(value)
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != 1 {
            return None;
        }
        let index = value.get("index")?.as_object()?.iter().filter_map(|(id, hashes)| {
            Some((id.parse().ok()?, hashes.as_array()?.iter().filter_map(|h| h.as_str().map(str::to_owned)).collect()))
        }).collect();
        Some(Collection {
            id: value.get("id")?.as_str()?.to_owned(),
            label: value.get("label")?.as_str()?.to_owned(),
            created: value.get("created")?.as_u64()?,
            modified: value.get("modified")?.as_u64()?,
            salt: unb64(value.get("salt")?.as_str()?)?,
            next_id: value.get("next_id")?.as_u64()?,
            persistent: true,
            index,
            items: None,
            sealed: Some((unb64(value.get("nonce")?.as_str()?)?, unb64(value.get("sealed")?.as_str()?)?)),
        })
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    let temp = parent.join(format!(".{}.tmp-{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("f"),
                                   std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e| format!("{}: {e}", path.display()))
}

/// A collection id from a label: lowercase ASCII letters, digits and "_".
pub fn collection_id(label: &str, taken: &[&str]) -> String {
    let mut base: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>().trim_matches('_').to_owned();
    if base.is_empty() {
        base = "collection".into();
    }
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains(&id.as_str()) || id == SESSION {
        id = format!("{base}{n}");
        n += 1;
    }
    id
}

pub struct Keyring {
    pub dir: PathBuf,
    master: Option<MasterKey>,
    wrapped: Option<Wrapped>,
    pub collections: Vec<Collection>,
    pub aliases: BTreeMap<String, String>,
}

impl Keyring {
    /// Read the keyring (locked). A missing directory is an empty, not set up keyring.
    /// The default directory first takes over a pre-Tessel keyring ([`migrate_legacy`]).
    pub fn open(dir: PathBuf) -> Self {
        if dir == default_dir() {
            if let Err(message) = migrate_legacy(&legacy_dir(), &dir) {
                eprintln!("tessel-keyring: cannot copy the old keyring: {message}");
            }
        }
        let wrapped = std::fs::read(dir.join("master.json")).ok()
            .and_then(|d| serde_json::from_slice::<Value>(&d).ok()).and_then(|v| Wrapped::from_json(&v));
        let mut collections = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir.join("collections")) {
            let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
            paths.sort();
            for path in paths {
                if let Some(c) = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice::<Value>(&d).ok())
                    .and_then(|v| Collection::from_json(&v)) {
                    if c.id != SESSION && !collections.iter().any(|o: &Collection| o.id == c.id) {
                        collections.push(c);
                    }
                }
            }
        }
        let mut aliases: BTreeMap<String, String> = std::fs::read(dir.join("aliases.json")).ok()
            .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
            .and_then(|v| v.as_object().cloned()).unwrap_or_default()
            .into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_owned()))).collect();
        if let Ok(session) = Collection::new(SESSION, "Session", false) {
            collections.push(session);
        }
        aliases.insert(SESSION.into(), SESSION.into());
        Keyring { dir, master: None, wrapped, collections, aliases }
    }

    pub fn is_set_up(&self) -> bool {
        self.wrapped.is_some()
    }

    pub fn unlocked(&self) -> bool {
        self.master.is_some()
    }

    pub fn master(&self) -> Option<&MasterKey> {
        self.master.as_ref()
    }

    pub fn collection(&self, id: &str) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    pub fn collection_mut(&mut self, id: &str) -> Option<&mut Collection> {
        self.collections.iter_mut().find(|c| c.id == id)
    }

    /// Collection id an alias points to (`default` falls back to `login`).
    pub fn resolve_alias(&self, alias: &str) -> Option<String> {
        let id = self.aliases.get(alias).cloned().or_else(|| (alias == "default").then(|| LOGIN.to_owned()))?;
        self.collection(&id).map(|c| c.id.clone())
    }

    /// First use: a new master key wrapped with `password`, the login collection and the default alias.
    pub fn set_up(&mut self, password: &[u8]) -> Result<(), String> {
        self.set_up_with(password, Kdf::new()?)
    }

    pub fn set_up_with(&mut self, password: &[u8], kdf: Kdf) -> Result<(), String> {
        if self.is_set_up() {
            return Err("the keyring is already set up".into());
        }
        let master = MasterKey::generate()?;
        let wrapped = Wrapped::wrap(&master, password, kdf)?;
        write_private(&self.dir.join("master.json"), wrapped.to_json().to_string().as_bytes())?;
        self.wrapped = Some(wrapped);
        self.master = Some(master);
        if self.collection(LOGIN).is_none() {
            self.collections.insert(0, Collection::new(LOGIN, "Login", true)?);
        }
        self.save(LOGIN)?;
        if !self.aliases.contains_key("default") {
            self.aliases.insert("default".into(), LOGIN.into());
            self.save_aliases()?;
        }
        Ok(())
    }

    /// Unlock every collection with a master key (from the kernel keyring or a password).
    pub fn unlock_with(&mut self, master: MasterKey) {
        for collection in &mut self.collections {
            collection.unlock(&master);
        }
        self.master = Some(master);
    }

    /// Ok(false) for a wrong password.
    pub fn unlock_with_password(&mut self, password: &[u8]) -> Result<bool, String> {
        let wrapped = self.wrapped.as_ref().ok_or("the keyring is not set up")?;
        match wrapped.unwrap_key(password)? {
            Some(master) => {
                self.unlock_with(master);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The master key for a password (does not unlock).
    pub fn master_for(&self, password: &[u8]) -> Result<Option<MasterKey>, String> {
        self.wrapped.as_ref().ok_or("the keyring is not set up")?.unwrap_key(password)
    }

    /// Re-wrap the master key with a new password. Ok(false) if `old` is wrong.
    pub fn change_password(&mut self, old: &[u8], new: &[u8]) -> Result<bool, String> {
        let Some(master) = self.master_for(old)? else { return Ok(false) };
        let wrapped = Wrapped::wrap(&master, new, Kdf::new()?)?;
        write_private(&self.dir.join("master.json"), wrapped.to_json().to_string().as_bytes())?;
        self.wrapped = Some(wrapped);
        Ok(true)
    }

    pub fn lock(&mut self) {
        self.master = None;
        for collection in &mut self.collections {
            collection.lock();
        }
    }

    pub fn save(&mut self, id: &str) -> Result<(), String> {
        let master = self.master.clone().ok_or("the keyring is locked")?;
        let dir = self.dir.clone();
        let collection = self.collection_mut(id).ok_or("no such collection")?;
        if !collection.persistent {
            return Ok(());
        }
        let value = collection.to_json(&master)?;
        write_private(&dir.join("collections").join(format!("{id}.json")), value.to_string().as_bytes())
    }

    pub fn save_aliases(&self) -> Result<(), String> {
        let map: Map<String, Value> = self.aliases.iter().filter(|(k, _)| k.as_str() != SESSION)
            .map(|(k, v)| (k.clone(), json!(v))).collect();
        write_private(&self.dir.join("aliases.json"), Value::Object(map).to_string().as_bytes())
    }

    pub fn create_collection(&mut self, label: &str) -> Result<String, String> {
        if !self.unlocked() {
            return Err("the keyring is locked".into());
        }
        let taken: Vec<&str> = self.collections.iter().map(|c| c.id.as_str()).collect();
        let id = collection_id(label, &taken);
        self.collections.push(Collection::new(&id, label, true)?);
        self.save(&id)?;
        Ok(id)
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<(), String> {
        if id == SESSION {
            return Err("the session collection cannot be deleted".into());
        }
        self.collections.retain(|c| c.id != id);
        let path = self.dir.join("collections").join(format!("{id}.json"));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        let before = self.aliases.len();
        self.aliases.retain(|_, v| v != id);
        if before != self.aliases.len() {
            self.save_aliases()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tessel-keyring-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn set_up_store_reopen_unlock() {
        let dir = temp("roundtrip");
        let mut keyring = Keyring::open(dir.clone());
        assert!(!keyring.is_set_up());
        keyring.set_up_with(b"pw", Kdf::for_tests()).unwrap();
        let login = keyring.collection_mut(LOGIN).unwrap();
        let id = login.store("GitHub", attrs(&[("service", "github"), ("user", "me")]),
                             Zeroizing::new(b"token".to_vec()), "text/plain", false).unwrap();
        keyring.save(LOGIN).unwrap();
        let mode = std::fs::metadata(dir.join("collections/login.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let text = std::fs::read_to_string(dir.join("collections/login.json")).unwrap();
        assert!(!text.contains("token") && !text.contains("github") && !text.contains("GitHub"));

        let mut again = Keyring::open(dir.clone());
        assert!(again.is_set_up() && !again.unlocked());
        let login = again.collection(LOGIN).unwrap();
        assert!(login.locked());
        assert_eq!(login.search(&attrs(&[("service", "github")])), vec![id], "locked search uses the index");
        assert!(login.search(&attrs(&[("service", "gitlab")])).is_empty());
        assert!(!again.unlock_with_password(b"wrong").unwrap());
        assert!(again.unlock_with_password(b"pw").unwrap());
        let item = again.collection(LOGIN).unwrap().item(id).unwrap();
        assert_eq!(item.secret.as_slice(), b"token");
        assert_eq!(again.resolve_alias("default").as_deref(), Some(LOGIN));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replace_updates_items_with_equal_attributes() {
        let mut c = Collection::new("c", "C", true).unwrap();
        let a = attrs(&[("k", "v")]);
        let first = c.store("one", a.clone(), Zeroizing::new(b"1".to_vec()), "text/plain", true).unwrap();
        let second = c.store("two", a.clone(), Zeroizing::new(b"2".to_vec()), "text/plain", true).unwrap();
        assert_eq!(first, second);
        let third = c.store("three", a, Zeroizing::new(b"3".to_vec()), "text/plain", false).unwrap();
        assert_ne!(third, first);
        assert_eq!(c.item(first).unwrap().secret.as_slice(), b"2");
        assert!(c.remove(first));
        assert_eq!(c.item_ids(), vec![third]);
    }

    #[test]
    fn change_password_keeps_the_data() {
        let dir = temp("passwd");
        let mut keyring = Keyring::open(dir.clone());
        keyring.set_up_with(b"old", Kdf::for_tests()).unwrap();
        keyring.collection_mut(LOGIN).unwrap()
            .store("x", attrs(&[("a", "b")]), Zeroizing::new(b"s".to_vec()), "text/plain", false);
        keyring.save(LOGIN).unwrap();
        assert!(!keyring.change_password(b"bad", b"new").unwrap());
        assert!(keyring.change_password(b"old", b"new").unwrap());
        let mut again = Keyring::open(dir.clone());
        assert!(!again.unlock_with_password(b"old").unwrap());
        assert!(again.unlock_with_password(b"new").unwrap());
        assert_eq!(again.collection(LOGIN).unwrap().items().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn collection_ids_are_unique_and_safe() {
        assert_eq!(collection_id("My Keys!", &[]), "my_keys");
        assert_eq!(collection_id("Login", &["login"]), "login2");
        assert_eq!(collection_id("Session", &[]), "session2");
        assert_eq!(collection_id("..", &[]), "collection");
    }

    #[test]
    fn on_disk_associated_data_keeps_the_pre_tessel_names() {
        assert_eq!(collection_aad("login"), b"waykeyring v1 collection login");
    }

    #[test]
    fn legacy_keyring_is_copied_once_with_its_modes() {
        // Under the target directory (next to this test binary), not TMPDIR.
        let root = std::env::current_exe().unwrap().parent().unwrap()
            .join(format!("tessel-keyring-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (legacy, dir) = (root.join("desktop-suite/keyring"), root.join("tessel/keyring"));
        let mut old = Keyring::open(legacy.clone());
        old.set_up_with(b"pw", Kdf::for_tests()).unwrap();
        let id = old.collection_mut(LOGIN).unwrap().store("GitHub", attrs(&[("service", "github")]),
                                                           Zeroizing::new(b"token".to_vec()), "text/plain", false).unwrap();
        old.save(LOGIN).unwrap();
        assert_eq!(std::fs::metadata(&legacy).unwrap().permissions().mode() & 0o777, 0o700);

        assert_eq!(migrate_legacy(&legacy, &dir), Ok(true));
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("collections")), 0o700);
        assert_eq!(mode(&dir.join("master.json")), 0o600);
        assert_eq!(mode(&dir.join("collections/login.json")), 0o600);
        assert!(legacy.join("master.json").is_file(), "the old keyring is kept");
        assert!(std::fs::read_dir(&root.join("tessel")).unwrap().flatten().all(|e| e.file_name() == "keyring"),
                "no temporary directory left");
        let mut new = Keyring::open(dir.clone());
        assert!(new.unlock_with_password(b"pw").unwrap());
        assert_eq!(new.collection(LOGIN).unwrap().item(id).unwrap().secret.as_slice(), b"token");

        // An existing new keyring is never replaced; no old keyring is no copy.
        std::fs::remove_file(dir.join("master.json")).unwrap();
        assert_eq!(migrate_legacy(&legacy, &dir), Ok(false));
        assert!(!dir.join("master.json").exists());
        assert_eq!(migrate_legacy(&root.join("none"), &root.join("other")), Ok(false));
        assert!(!root.join("other").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
