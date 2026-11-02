//! Bookkeeping shared by the importers: which files were imported
//! (`imported.json`), adding imported items without duplicates, and
//! "pending" imports: the login helper decrypts a KDE wallet with the login
//! password and leaves its items sealed with the master key for the daemon.
use crate::crypto::{self, b64, unb64, MasterKey};
use crate::gnome_import::Imported;
use crate::store::{write_private, Collection};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const RECORD: &str = "imported.json";
/// On-disk format constant (keeps the pre-Tessel name, like the other AADs).
const PENDING_AAD: &[u8] = b"waykeyring v1 pending import";

pub fn record(dir: &Path) -> Value {
    std::fs::read(dir.join(RECORD)).ok().and_then(|d| serde_json::from_slice(&d).ok())
        .filter(Value::is_object).unwrap_or_else(|| json!({}))
}

pub fn is_marked(record: &Value, kind: &str, name: &str) -> bool {
    record.get(kind).and_then(|k| k.get(name)).is_some()
}

pub fn mark(dir: &Path, kind: &str, name: &str, items: u32) -> Result<(), String> {
    let mut value = record(dir);
    if !value.get(kind).is_some_and(Value::is_object) {
        value[kind] = json!({});
    }
    value[kind][name] = json!({"items": items, "at": crate::store::now()});
    write_private(&dir.join(RECORD), value.to_string().as_bytes())
}

/// Items imported from every source.
pub fn total(record: &Value) -> u32 {
    record.as_object().into_iter().flat_map(|o| o.values()).filter_map(Value::as_object)
        .flat_map(|o| o.values()).filter_map(|v| v.get("items").and_then(Value::as_u64)).sum::<u64>() as u32
}

/// Add items to `collection`; an item is skipped when one with the same
/// (non-empty) attributes exists, or the same attributes and secret.
/// Returns (added, skipped).
pub fn add_items(collection: &mut Collection, items: Vec<Imported>) -> (u32, u32) {
    let (mut added, mut skipped) = (0, 0);
    for item in items {
        let duplicate = collection.items().unwrap_or(&[]).iter().any(|i| {
            i.attributes == item.attributes && (!item.attributes.is_empty() || i.secret == item.secret)
        });
        if duplicate {
            skipped += 1;
        } else if collection.store(&item.label, item.attributes, item.secret, "text/plain", false).is_some() {
            added += 1;
        }
    }
    (added, skipped)
}

fn pending_dir(dir: &Path) -> PathBuf {
    dir.join("pending")
}

pub fn write_pending(dir: &Path, master: &MasterKey, name: &str, items: &[Imported]) -> Result<(), String> {
    let list: Vec<Value> = items.iter().map(|i| json!({
        "label": i.label, "attributes": i.attributes, "secret": b64(&i.secret)
    })).collect();
    let plain = Zeroizing::new(serde_json::to_vec(&json!({"name": name, "items": list})).map_err(|e| e.to_string())?);
    let (nonce, sealed) = crypto::seal(master.bytes(), PENDING_AAD, &plain)?;
    let file = pending_dir(dir).join(format!("{}.json", name.replace(['/', ':'], "_")));
    write_private(&file, json!({"version": 1, "nonce": b64(&nonce), "sealed": b64(&sealed)}).to_string().as_bytes())
}

pub fn has_pending(dir: &Path, name: &str) -> bool {
    pending_dir(dir).join(format!("{}.json", name.replace(['/', ':'], "_"))).exists()
}

/// Every pending import (name, items); the files are removed.
pub fn take_pending(dir: &Path, master: &MasterKey) -> Vec<(String, Vec<Imported>)> {
    let mut out = Vec::new();
    for path in std::fs::read_dir(pending_dir(dir)).into_iter().flatten().flatten().map(|e| e.path()) {
        let opened = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice::<Value>(&d).ok()).and_then(|v| {
            crypto::open(master.bytes(), PENDING_AAD, &unb64(v.get("nonce")?.as_str()?)?, &unb64(v.get("sealed")?.as_str()?)?)
        });
        let Some(plain) = opened else { continue };
        let Ok(value) = serde_json::from_slice::<Value>(&plain) else { continue };
        let name = value.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        let items = value.get("items").and_then(Value::as_array).into_iter().flatten().filter_map(|i| {
            Some(Imported {
                label: i.get("label")?.as_str()?.to_owned(),
                attributes: i.get("attributes")?.as_object()?.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect::<BTreeMap<_, _>>(),
                secret: Zeroizing::new(unb64(i.get("secret")?.as_str()?)?),
            })
        }).collect();
        out.push((name, items));
        let _ = std::fs::remove_file(&path);
    }
    out
}

/// At login: decrypt KDE wallets not imported yet with the login password and
/// leave them pending for the daemon. Wallets with another password are left
/// for Global Settings > Passwords > Import.
pub fn kwallets_at_login(dir: &Path, kwallet_dir: &Path, password: &[u8], master: &MasterKey) {
    let done = record(dir);
    for wallet in crate::kwallet_import::wallets(kwallet_dir) {
        let name = wallet.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if is_marked(&done, "kwallet", &name) || has_pending(dir, &format!("kwallet:{name}")) {
            continue;
        }
        if let Ok(items) = crate::kwallet_import::import_file(&wallet, password) {
            let _ = write_pending(dir, master, &format!("kwallet:{name}"), &items);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_round_trip_and_duplicates() {
        let dir = std::env::temp_dir().join(format!("tessel-keyring-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let master = MasterKey::generate().unwrap();
        let item = |label: &str, attrs: &[(&str, &str)], secret: &[u8]| Imported {
            label: label.into(), attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            secret: Zeroizing::new(secret.to_vec()),
        };
        write_pending(&dir, &master, "kwallet:k.kwl", &[item("a", &[("x", "1")], b"s")]).unwrap();
        assert!(has_pending(&dir, "kwallet:k.kwl"));
        let text = std::fs::read_to_string(pending_dir(&dir).join("kwallet_k.kwl.json")).unwrap();
        assert!(!text.contains("\"a\""));
        assert!(take_pending(&dir, &MasterKey::generate().unwrap()).is_empty(), "another key opens nothing");
        let pending = take_pending(&dir, &master);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "kwallet:k.kwl");
        assert!(!has_pending(&dir, "kwallet:k.kwl"));
        let mut c = Collection::new("login", "Login", true).unwrap();
        assert_eq!(add_items(&mut c, pending.into_iter().next().unwrap().1), (1, 0));
        assert_eq!(add_items(&mut c, vec![item("b", &[("x", "1")], b"other"), item("n", &[], b"note"),
                                          item("n2", &[], b"note")]), (1, 2));
        mark(&dir, "kwallet", "k.kwl", 1).unwrap();
        mark(&dir, "gnome", "g.keyring", 3).unwrap();
        let r = record(&dir);
        assert!(is_marked(&r, "kwallet", "k.kwl") && !is_marked(&r, "kwallet", "x.kwl"));
        assert_eq!(total(&r), 4);
        let _ = std::fs::remove_dir_all(dir);
    }
}
