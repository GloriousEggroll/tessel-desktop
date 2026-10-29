//! file_goto.GoAnywhere: the Ctrl+K keyboard palette (fuzzy names/paths over
//! Home, open tabs and bookmarks, typed paths and URIs).

use files_model::goto::{self, Entry, FolderIndex, Ranked, RESULT_LIMIT};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

pub type OnOpen = Rc<dyn Fn(gio::File, bool)>;

pub struct Inner {
    pub window: gtk::Window,
    pub entry: gtk::SearchEntry,
    pub list: gtk::ListBox,
    pub status: gtk::Label,
    pub index: RefCell<Option<FolderIndex>>,
    places: Vec<Entry>,
    on_open: OnOpen,
    generation: Cell<u64>,
    timer: RefCell<Option<glib::SourceId>>,
    pub closed: Cell<bool>,
    rows: RefCell<Vec<(gtk::ListBoxRow, gio::File, bool)>>,
}

#[derive(Clone)]
pub struct GoAnywhere(pub Rc<Inner>);

impl std::ops::Deref for GoAnywhere {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl GoAnywhere {
    pub fn new(parent: &gtk::Window, places: &[String], index: FolderIndex, on_open: OnOpen) -> Self {
        let width = 720.min(480.max(parent.width() - 160));
        let window = gtk::Window::builder().transient_for(parent).modal(true).decorated(false).title("Go Anywhere")
            .default_width(width).default_height(460).build();
        window.add_css_class("files-goto");
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.add_css_class("files-goto-content");
        let entry = gtk::SearchEntry::builder().placeholder_text("Go to a file or folder…").hexpand(true).build();
        column.append(&entry);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Browse).build();
        list.add_css_class("files-goto-results");
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();
        column.append(&scroll);
        let status = gtk::Label::builder().xalign(0.).build();
        status.add_css_class("muted");
        column.append(&status);
        window.set_child(Some(&column));
        let palette = GoAnywhere(Rc::new(Inner {
            window, entry, list, status, index: RefCell::new(Some(index)), places: goto::place_entries(places), on_open,
            generation: Cell::new(0), timer: RefCell::new(None), closed: Cell::new(false), rows: RefCell::new(Vec::new()),
        }));
        let weak = palette.downgrade();
        palette.entry.connect_search_changed(move |_| {
            if let Some(p) = upgrade(&weak) {
                p.schedule()
            }
        });
        let weak = palette.downgrade();
        palette.entry.connect_activate(move |_| {
            if let Some(p) = upgrade(&weak) {
                let row = p.list.selected_row().or_else(|| p.list.row_at_index(0));
                p.choose(row.as_ref());
            }
        });
        let weak = palette.downgrade();
        palette.entry.connect_stop_search(move |_| {
            if let Some(p) = upgrade(&weak) {
                p.window.close()
            }
        });
        let weak = palette.downgrade();
        palette.list.connect_row_activated(move |_, row| {
            if let Some(p) = upgrade(&weak) {
                p.choose(Some(row))
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = palette.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| match upgrade(&weak) {
            Some(p) if p.key_pressed(key) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        palette.window.add_controller(keys);
        let weak = palette.downgrade();
        palette.window.connect_close_request(move |_| {
            if let Some(p) = upgrade(&weak) {
                p.closing()
            }
            glib::Propagation::Proceed
        });
        let initial: Vec<Ranked> = palette.places.iter().take(RESULT_LIMIT).map(|(path, ..)| (0, path.clone(), true)).collect();
        palette.show_results(&initial, None);
        palette.status.set_text("Bookmarks and open folders. Type to search.");
        palette
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn key_pressed(&self, key: gdk::Key) -> bool {
        if key == gdk::Key::Escape {
            self.window.close();
            return true;
        }
        if key == gdk::Key::Down || key == gdk::Key::Up {
            let index = self.list.selected_row().map(|r| r.index()).unwrap_or(-1) + if key == gdk::Key::Down { 1 } else { -1 };
            if let Some(target) = self.list.row_at_index(index.max(0)) {
                self.list.select_row(Some(&target));
                target.grab_focus();
                if let Some(text) = self.entry.delegate().and_downcast::<gtk::Text>() {
                    text.grab_focus_without_selecting();
                }
            }
            return true;
        }
        false
    }

    pub fn schedule(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = self.downgrade();
        let timer = glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            if let Some(p) = upgrade(&weak) {
                *p.timer.borrow_mut() = None;
                p.search();
            }
        });
        *self.timer.borrow_mut() = Some(timer);
    }

    pub fn search(&self) {
        *self.timer.borrow_mut() = None;
        if self.closed.get() {
            return;
        }
        let query = self.entry.text().trim().to_string();
        self.generation.set(self.generation.get() + 1);
        let generation = self.generation.get();
        let typed = typed_location(&query);
        let (mut entries, done) = {
            let index = self.index.borrow();
            let index = index.as_ref();
            (index.map(|i| i.entries()).unwrap_or_default(), index.is_none_or(|i| i.done()))
        };
        let mut all = self.places.clone();
        all.append(&mut entries);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let q = query.clone();
            let results = gio::spawn_blocking(move || goto::rank(&q, &all, RESULT_LIMIT)).await.unwrap_or_default();
            if let Some(p) = upgrade(&weak) {
                p.finished(generation, typed, results, done);
            }
        });
    }

    fn finished(&self, generation: u64, typed: Option<gio::File>, results: Vec<Ranked>, done: bool) {
        if self.closed.get() || generation != self.generation.get() {
            return;
        }
        self.show_results(&results, typed.as_ref());
        let (count, truncated) = {
            let index = self.index.borrow();
            index.as_ref().map(|i| (i.len(), i.truncated())).unwrap_or((0, false))
        };
        let noun = if results.len() == 1 { "match" } else { "matches" };
        let mut text = format!("{} {noun} in {} items", results.len(), thousands(count));
        if !done {
            text.push_str(" — still indexing…");
        }
        if truncated {
            text.push_str(" (index limit reached)");
        }
        self.status.set_text(&text);
        if !done && !self.entry.text().trim().is_empty() {
            // Results improve while the folder walk continues.
            let weak = self.downgrade();
            let timer = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                if let Some(p) = upgrade(&weak) {
                    *p.timer.borrow_mut() = None;
                    p.search();
                }
            });
            *self.timer.borrow_mut() = Some(timer);
        }
    }

    pub fn show_results(&self, results: &[Ranked], typed: Option<&gio::File>) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.rows.borrow_mut().clear();
        if let Some(typed) = typed {
            let path = typed.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| typed.uri().to_string());
            let is_dir = typed.query_file_type(gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>) != gio::FileType::Regular;
            self.append(&path, is_dir, Some(&format!("Go to {}", typed.parse_name())), Some(typed.clone()));
        }
        for (_, path, is_dir) in results {
            self.append(path, *is_dir, None, None);
        }
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
    }

    fn append(&self, path: &str, is_dir: bool, title: Option<&str>, location: Option<gio::File>) {
        let row = gtk::ListBoxRow::new();
        let location = location.unwrap_or_else(|| gio::File::for_path(path));
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let kind = if is_dir { "inode/directory".to_string() } else { gio::content_type_guess(Some(path), None).0.to_string() };
        let icon = gtk::Image::from_gicon(&gio::content_type_get_symbolic_icon(&kind));
        icon.set_pixel_size(16);
        line.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let p = PathBuf::from(path);
        let base = p.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string());
        let name = gtk::Label::builder().label(title.map(str::to_string).unwrap_or(base)).xalign(0.).ellipsize(pango::EllipsizeMode::End).build();
        name.add_css_class("heading");
        text.append(&name);
        if title.is_none() {
            let parent = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            let parent = if parent.is_empty() { ".".to_string() } else { parent };
            let where_ = gtk::Label::builder().label(goto::display_path(&parent)).xalign(0.).ellipsize(pango::EllipsizeMode::Start).build();
            where_.add_css_class("muted");
            text.append(&where_);
        }
        line.append(&text);
        row.set_child(Some(&line));
        self.list.append(&row);
        self.rows.borrow_mut().push((row, location, is_dir));
    }

    /// `row._location`, `row._is_dir`.
    pub fn row_target(&self, row: &gtk::ListBoxRow) -> Option<(gio::File, bool)> {
        self.rows.borrow().iter().find(|(r, ..)| r == row).map(|(_, f, d)| (f.clone(), *d))
    }

    pub fn choose(&self, row: Option<&gtk::ListBoxRow>) {
        let Some((location, is_dir)) = row.and_then(|r| self.row_target(r)) else { return };
        self.window.close();
        (self.on_open)(location, is_dir);
    }

    fn closing(&self) {
        self.closed.set(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(index) = self.index.borrow().as_ref() {
            index.stop();
            index.clear();
        }
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<GoAnywhere> {
    weak.upgrade().map(GoAnywhere)
}

/// `_typed_location(query)`.
pub fn typed_location(query: &str) -> Option<gio::File> {
    if query.starts_with('/') || query.starts_with('~') || query.contains("://") {
        let path = if query.starts_with('~') { crate::pane::expanduser(query) } else { query.to_string() };
        let location = gio::File::for_commandline_arg(path);
        if query.contains("://") || location.query_exists(None::<&gio::Cancellable>) {
            return Some(location);
        }
    }
    None
}

/// `f'{count:,}'`.
pub fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
