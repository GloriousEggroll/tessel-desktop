//! location_completion.LocationCompletion: folder suggestions below the
//! location entry (debounced 100 ms, at most 128, keyboard and pointer).

use files_model::completion::{folder_query, FolderQuery};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const LIMIT: usize = 128;

pub struct Inner {
    pub entry: gtk::Entry,
    current: RefCell<Option<Rc<dyn Fn() -> gio::File>>>,
    show_hidden: RefCell<Option<Rc<dyn Fn() -> bool>>>,
    source: RefCell<Option<glib::SourceId>>,
    cancellable: RefCell<Option<gio::Cancellable>>,
    generation: Cell<u64>,
    muted: Cell<bool>,
    closed: Cell<bool>,
    tab_requested: Cell<bool>,
    pub items: RefCell<Vec<String>>,
    pub popover: gtk::Popover,
    pub rows: gtk::ListBox,
    pub scroll: gtk::ScrolledWindow,
    pub hint: gtk::Label,
    signals: RefCell<Vec<glib::SignalHandlerId>>,
    keys: gtk::EventControllerKey,
}

#[derive(Clone)]
pub struct LocationCompletion(pub Rc<Inner>);

impl std::ops::Deref for LocationCompletion {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl LocationCompletion {
    pub fn new(entry: &gtk::Entry, current: Rc<dyn Fn() -> gio::File>, show_hidden: Rc<dyn Fn() -> bool>) -> Self {
        let popover = gtk::Popover::builder().autohide(false).has_arrow(false).position(gtk::PositionType::Bottom).build();
        popover.set_can_focus(false);
        popover.set_halign(gtk::Align::Start);
        popover.add_css_class("files-completion");
        popover.set_parent(entry);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let rows = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).activate_on_single_click(true).build();
        rows.add_css_class("navigation-sidebar");
        rows.set_can_focus(false);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vscrollbar_policy(gtk::PolicyType::Automatic)
            .max_content_height(288).propagate_natural_height(true).build();
        scroll.set_child(Some(&rows));
        column.append(&scroll);
        let hint = gtk::Label::builder().label("Keep typing to narrow the list").xalign(0.).build();
        hint.add_css_class("dim-label");
        column.append(&hint);
        popover.set_child(Some(&column));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let completion = LocationCompletion(Rc::new(Inner {
            entry: entry.clone(), current: RefCell::new(Some(current)), show_hidden: RefCell::new(Some(show_hidden)),
            source: RefCell::new(None), cancellable: RefCell::new(None), generation: Cell::new(0), muted: Cell::new(false),
            closed: Cell::new(false), tab_requested: Cell::new(false), items: RefCell::new(Vec::new()), popover, rows, scroll, hint,
            signals: RefCell::new(Vec::new()), keys: keys.clone(),
        }));
        let weak = completion.downgrade();
        completion.rows.connect_row_activated(move |_, row| {
            if let Some(c) = upgrade(&weak) {
                c.accept(row.index(), true)
            }
        });
        let weak = completion.downgrade();
        let changed = entry.connect_changed(move |_| {
            if let Some(c) = upgrade(&weak) {
                c.changed()
            }
        });
        let weak = completion.downgrade();
        let unmap = entry.connect_unmap(move |_| {
            if let Some(c) = upgrade(&weak) {
                c.dismiss()
            }
        });
        *completion.signals.borrow_mut() = vec![changed, unmap];
        let weak = completion.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| match upgrade(&weak) {
            Some(c) if c.key(key, state) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        entry.add_controller(keys);
        completion
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn focused(&self) -> bool {
        let Some(root) = self.entry.root() else { return false };
        let focus = root.focus();
        self.entry.is_mapped() && focus.is_some_and(|f| f == *self.entry.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.entry))
    }

    pub fn reset(&self, text: &str) {
        self.dismiss();
        self.muted.set(true);
        self.entry.set_text(text);
        self.muted.set(false);
    }

    pub fn dismiss(&self) {
        self.generation.set(self.generation.get() + 1);
        self.tab_requested.set(false);
        if let Some(source) = self.source.borrow_mut().take() {
            source.remove();
        }
        if let Some(cancellable) = self.cancellable.borrow_mut().take() {
            cancellable.cancel();
        }
        self.popover.popdown();
        self.items.borrow_mut().clear();
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
    }

    fn changed(&self) {
        if self.muted.get() || self.closed.get() {
            return;
        }
        self.dismiss();
        if self.focused() && !self.entry.text().is_empty() {
            let weak = self.downgrade();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                if let Some(c) = upgrade(&weak) {
                    *c.source.borrow_mut() = None;
                    c.lookup();
                }
            });
            *self.source.borrow_mut() = Some(source);
        }
    }

    fn lookup(&self) {
        *self.source.borrow_mut() = None;
        let Some(current) = self.current.borrow().clone() else { return };
        let query = folder_query(&self.entry.text(), &current());
        let Some(query) = query else { return };
        if self.closed.get() || !self.focused() {
            return;
        }
        let generation = self.generation.get();
        let cancellable = gio::Cancellable::new();
        *self.cancellable.borrow_mut() = Some(cancellable.clone());
        let include_hidden = self.show_hidden.borrow().as_ref().is_some_and(|f| f()) || query.fragment.starts_with('.');
        let weak = self.downgrade();
        let query = Rc::new(query);
        let c = cancellable.clone();
        let parent = query.parent.clone();
        parent.enumerate_children_async("standard::name,standard::type,standard::is-hidden", gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT, Some(&cancellable), move |result| {
                let Some(this) = upgrade(&weak) else { return };
                match result {
                    Err(_) => this.failed(generation, &c), // Missing/inaccessible paths are ordinary while typing.
                    Ok(enumerator) => {
                        if this.current(generation, &c) {
                            this.next_batch(enumerator, generation, c, query, include_hidden, Vec::new());
                        } else {
                            close(&enumerator);
                        }
                    }
                }
            });
    }

    fn current(&self, generation: u64, cancellable: &gio::Cancellable) -> bool {
        generation == self.generation.get() && !cancellable.is_cancelled() && !self.closed.get()
    }

    fn failed(&self, generation: u64, cancellable: &gio::Cancellable) {
        if self.current(generation, cancellable) {
            *self.cancellable.borrow_mut() = None;
            self.tab_requested.set(false);
        }
    }

    fn next_batch(&self, enumerator: gio::FileEnumerator, generation: u64, cancellable: gio::Cancellable, query: Rc<FolderQuery>,
                  include_hidden: bool, mut matches: Vec<String>) {
        let weak = self.downgrade();
        let c = cancellable.clone();
        enumerator.clone().next_files_async(128, glib::Priority::DEFAULT, Some(&cancellable), move |result| {
            let Some(this) = upgrade(&weak) else { return };
            let infos = match result {
                Ok(infos) => infos,
                Err(_) => {
                    close(&enumerator);
                    this.failed(generation, &c);
                    return;
                }
            };
            if !this.current(generation, &c) {
                close(&enumerator);
                return;
            }
            for info in &infos {
                let name = info.name().to_string_lossy().into_owned();
                if info.file_type() == gio::FileType::Directory && name.starts_with(&query.fragment)
                    && (include_hidden || !(info.is_hidden() || name.starts_with('.'))) {
                    matches.push(name);
                    if matches.len() >= LIMIT {
                        break;
                    }
                }
            }
            if infos.is_empty() || matches.len() >= LIMIT {
                close(&enumerator);
                *this.cancellable.borrow_mut() = None;
                if this.focused() {
                    this.show(&query, matches);
                }
            } else {
                this.next_batch(enumerator, generation, c, query, include_hidden, matches);
            }
        });
    }

    fn show(&self, query: &FolderQuery, mut names: Vec<String>) {
        names.sort_by_key(|n| (suite_core::pyjson::casefold(n), n.clone()));
        for name in &names {
            self.items.borrow_mut().push(query.complete(name));
            let row = gtk::ListBoxRow::builder().focusable(false).build();
            row.set_tooltip_text(Some(&query.parent.child(name).parse_name()));
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            line.append(&suite_gtk::utils::image("folder-symbolic", 18));
            line.append(&gtk::Label::builder().label(format!("{name}/")).xalign(0.).hexpand(true).ellipsize(pango::EllipsizeMode::Middle).build());
            row.set_child(Some(&line));
            self.rows.append(&row);
        }
        self.rows.unselect_all();
        if !self.items.borrow().is_empty() {
            self.hint.set_visible(names.len() >= LIMIT);
            self.popover.set_size_request(760.min(260.max(self.entry.width())), -1);
            self.popover.popup();
            if self.tab_requested.get() {
                self.tab_requested.set(false);
                self.accept(0, false);
            }
        } else {
            self.tab_requested.set(false);
        }
    }

    pub fn accept(&self, index: i32, navigate: bool) {
        let text = {
            let items = self.items.borrow();
            if index < 0 || index as usize >= items.len() {
                return;
            }
            items[index as usize].clone()
        };
        self.reset(&text);
        self.entry.grab_focus();
        self.entry.set_position(-1);
        if navigate {
            self.entry.emit_activate();
        } else {
            self.changed();
        }
    }

    /// `_key` (capture phase on the entry).
    pub fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return false;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        if key == gdk::Key::Tab && !shift && (self.source.borrow().is_some() || self.cancellable.borrow().is_some()) {
            // A fast Tab must still complete instead of moving focus away while
            // the debounce timer or a remote directory lookup is pending.
            self.tab_requested.set(true);
            let pending = self.source.borrow_mut().take();
            if let Some(source) = pending {
                source.remove();
                self.lookup();
            }
            return true;
        }
        let count = self.items.borrow().len() as i32;
        if !self.popover.is_visible() || count == 0 {
            return false;
        }
        let mut index = self.rows.selected_row().map(|r| r.index()).unwrap_or(-1);
        if key == gdk::Key::Escape {
            self.dismiss();
            return true;
        }
        if key == gdk::Key::Down || key == gdk::Key::Up {
            index = if key == gdk::Key::Down {
                (index + 1).rem_euclid(count)
            } else {
                (if index >= 0 { index - 1 } else { count - 1 }).rem_euclid(count)
            };
            if let Some(row) = self.rows.row_at_index(index) {
                self.rows.select_row(Some(&row));
                if let Some(bounds) = row.compute_bounds(&self.rows) {
                    let adjustment = self.scroll.vadjustment();
                    adjustment.clamp_page(bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
                }
            }
            return true;
        }
        if key == gdk::Key::Tab && !shift {
            self.accept(index.max(0), false);
            return true;
        }
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && index >= 0 {
            self.accept(index, true);
            return true;
        }
        false
    }

    pub fn close(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.dismiss();
        for id in self.signals.borrow_mut().drain(..) {
            self.entry.disconnect(id);
        }
        self.entry.remove_controller(&self.keys);
        self.popover.unparent();
        *self.current.borrow_mut() = None;
        *self.show_hidden.borrow_mut() = None;
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
    pub fn source_pending(&self) -> bool {
        self.source.borrow().is_some()
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<LocationCompletion> {
    weak.upgrade().map(LocationCompletion)
}

fn close(enumerator: &gio::FileEnumerator) {
    enumerator.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
}
