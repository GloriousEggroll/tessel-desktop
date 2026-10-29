//! `OrderedMap<V>`: a small insertion-ordered string map with Python `dict`
//! semantics (assigning an existing key keeps its position; equality ignores
//! order). Used for every dict-valued `DockConfig` field so saved JSON keeps
//! Python's key order byte-for-byte.

use std::fmt;

#[derive(Clone, Default)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> OrderedMap<V> {
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    /// `d[key] = value`: replaces in place, else appends. Returns the old value.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        if let Some(slot) = self.get_mut(&key) {
            return Some(std::mem::replace(slot, value));
        }
        self.entries.push((key, value));
        None
    }
    /// `d.setdefault(key, value)`.
    pub fn setdefault(&mut self, key: impl Into<String>, value: V) -> &mut V {
        let key = key.into();
        let index = match self.entries.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                self.entries.push((key, value));
                self.entries.len() - 1
            }
        };
        &mut self.entries[index].1
    }
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(index).1)
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_str(), v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }
    pub fn into_entries(self) -> Vec<(String, V)> {
        self.entries
    }
}

impl<V: PartialEq> PartialEq for OrderedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<V: fmt::Debug> fmt::Debug for OrderedMap<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Into<String>, V> FromIterator<(K, V)> for OrderedMap<V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

impl<V> std::ops::Index<&str> for OrderedMap<V> {
    type Output = V;
    fn index(&self, key: &str) -> &V {
        self.get(key).unwrap_or_else(|| panic!("KeyError: {key:?}"))
    }
}

impl<V> std::ops::IndexMut<&str> for OrderedMap<V> {
    fn index_mut(&mut self, key: &str) -> &mut V {
        self.get_mut(key).unwrap_or_else(|| panic!("KeyError: {key:?}"))
    }
}

impl<V> IntoIterator for OrderedMap<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}
