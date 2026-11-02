//! The archive's entries as a GTK tree: `EntryObject` (one row) and the
//! folder tree built from a listing, optionally filtered by a search text
//! (matches plus the folders leading to them).

use crate::archive::{Entry, Listing};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Node {
    pub entry: Entry,
    pub children: RefCell<Vec<Rc<Node>>>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct EntryObject {
        pub node: RefCell<Option<Rc<Node>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EntryObject {
        const NAME: &'static str = "WayarchiveEntry";
        type Type = super::EntryObject;
    }

    impl ObjectImpl for EntryObject {}
}

glib::wrapper! {
    pub struct EntryObject(ObjectSubclass<imp::EntryObject>);
}

impl EntryObject {
    pub fn new(node: Rc<Node>) -> Self {
        let object: Self = glib::Object::new();
        *object.imp().node.borrow_mut() = Some(node);
        object
    }
    pub fn node(&self) -> Rc<Node> {
        self.imp().node.borrow().clone().expect("EntryObject has a node")
    }
    pub fn entry(&self) -> Entry {
        self.node().entry.clone()
    }
    pub fn path(&self) -> String {
        self.node().entry.path.clone()
    }
    pub fn is_dir(&self) -> bool {
        self.node().entry.is_dir
    }
}

/// Folder tree of a listing, children sorted folders first then by name.
pub fn build_tree(listing: &Listing, filter: Option<&str>) -> Vec<Rc<Node>> {
    let needle = filter.map(str::to_lowercase).filter(|f| !f.is_empty());
    let keep: Option<std::collections::HashSet<String>> = needle.as_ref().map(|needle| {
        let mut keep = std::collections::HashSet::new();
        for entry in &listing.entries {
            if entry.name().to_lowercase().contains(needle.as_str()) {
                let mut path = entry.path.clone();
                keep.insert(path.clone());
                while let Some((parent, _)) = path.rsplit_once('/') {
                    path = parent.to_string();
                    keep.insert(path.clone());
                }
            }
        }
        keep
    });
    let mut by_parent: HashMap<&str, Vec<&Entry>> = HashMap::new();
    for entry in &listing.entries {
        if keep.as_ref().is_some_and(|k| !k.contains(&entry.path)) {
            continue;
        }
        by_parent.entry(entry.parent()).or_default().push(entry);
    }
    fn make(parent: &str, by_parent: &HashMap<&str, Vec<&Entry>>) -> Vec<Rc<Node>> {
        let mut list: Vec<&Entry> = by_parent.get(parent).cloned().unwrap_or_default();
        list.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(a.name(), b.name())));
        list.into_iter().map(|e| {
            let children = if e.is_dir { make(&e.path, by_parent) } else { Vec::new() };
            Rc::new(Node { entry: e.clone(), children: RefCell::new(children) })
        }).collect()
    }
    make("", &by_parent)
}

/// Case-insensitive natural order ("file2" < "file10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (n2, m2) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let order = n2.len().cmp(&m2.len()).then_with(|| n2.cmp(m2));
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                let order = c.to_lowercase().cmp(d.to_lowercase());
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

pub fn store(nodes: &[Rc<Node>]) -> gtk::gio::ListStore {
    let store = gtk::gio::ListStore::new::<EntryObject>();
    let objects: Vec<EntryObject> = nodes.iter().map(|n| EntryObject::new(n.clone())).collect();
    store.extend_from_slice(&objects);
    store
}

/// A TreeListModel over the tree (children created lazily on expand).
pub fn tree_model(nodes: &[Rc<Node>], expand: bool) -> gtk::TreeListModel {
    gtk::TreeListModel::new(store(nodes), false, expand, |item| {
        let object = item.downcast_ref::<EntryObject>()?;
        let node = object.node();
        if !node.entry.is_dir {
            return None;
        }
        let children = node.children.borrow();
        Some(store(&children).upcast())
    })
}

/// The EntryObject of a TreeListModel row (the item of a sorted/selection model).
pub fn row_entry(item: &glib::Object) -> Option<EntryObject> {
    let row = item.downcast_ref::<gtk::TreeListRow>()?;
    row.item().and_downcast::<EntryObject>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(paths: &[&str]) -> Listing {
        Listing {
            entries: paths.iter().map(|p| Entry { path: p.trim_end_matches('/').into(), is_dir: p.ends_with('/'), ..Default::default() }).collect(),
            ..Default::default()
        }.finish()
    }

    #[test]
    fn tree_and_filter() {
        let l = listing(&["b.txt", "a/", "a/x10.txt", "a/x9.txt", "c/d/e.png"]);
        let tree = build_tree(&l, None);
        let names: Vec<_> = tree.iter().map(|n| n.entry.path.clone()).collect();
        assert_eq!(names, ["a", "c", "b.txt"]);
        let a = tree[0].children.borrow();
        assert_eq!(a.iter().map(|n| n.entry.name().to_string()).collect::<Vec<_>>(), ["x9.txt", "x10.txt"]);
        let filtered = build_tree(&l, Some("PNG"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].entry.path, "c");
        assert_eq!(filtered[0].children.borrow()[0].children.borrow()[0].entry.path, "c/d/e.png");
        assert!(build_tree(&l, Some("zzz")).is_empty());
    }

    #[test]
    fn natural_order() {
        use std::cmp::Ordering::*;
        assert_eq!(natural_cmp("file2", "file10"), Less);
        assert_eq!(natural_cmp("File", "file"), Less);
        assert_eq!(natural_cmp("a", "B"), Less);
        assert_eq!(natural_cmp("x01", "x1"), Less);
    }
}
