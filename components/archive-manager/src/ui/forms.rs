//! Ark's bigger dialogs: Create (new archive / add with --add), Extract,
//! Properties.

use super::dialogs::{button, dialog, heading, once};
use crate::archive::{methods, supports_comment, supports_header_encryption, supports_level, supports_password, supports_volumes,
                     CreateOptions};
use crate::ops::{self, Archive};
use archive_settings::plugins::Kind;
use archive_settings::Settings;
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use suite_core::tr;

#[derive(Clone, Debug)]
pub struct CreateRequest {
    pub path: PathBuf,
    pub kind: Kind,
    pub options: CreateOptions,
}

/// The widgets of the create dialog (tests drive them).
pub struct CreateDialog {
    pub window: gtk::Window,
    pub folder: gtk::Entry,
    pub name: gtk::Entry,
    pub kind: gtk::DropDown,
    pub kinds: Vec<Kind>,
    pub level: gtk::Scale,
    pub method: gtk::DropDown,
    pub password: gtk::PasswordEntry,
    pub confirm: gtk::PasswordEntry,
    pub encrypt_header: gtk::CheckButton,
    pub encryption: gtk::DropDown,
    pub split: gtk::CheckButton,
    pub volume_mb: gtk::SpinButton,
    pub comment: gtk::TextView,
    pub accept: gtk::Button,
    pub problem: gtk::Label,
}

fn labeled(grid: &gtk::Grid, row: i32, label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Label {
    let l = gtk::Label::builder().label(label).xalign(1.0).build();
    l.set_mnemonic_widget(Some(widget));
    l.add_css_class("dim-label");
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
    l
}

/// Strip any known archive extension and append `kind`'s.
pub fn with_extension(name: &str, kind: Kind) -> String {
    let base = match Kind::from_name(name) {
        Some(old) => {
            let lower = name.to_lowercase();
            old.info().extensions.iter().find(|e| lower.ends_with(&format!(".{e}")))
                .map(|e| name[..name.len() - e.len() - 1].to_string()).unwrap_or_else(|| name.to_string())
        }
        None => name.to_string(),
    };
    let ext = if kind == Kind::TarZ { "tar.Z" } else if kind == Kind::Z { "Z" } else { kind.extension() };
    format!("{base}.{ext}")
}

pub fn choose_folder(parent: Option<&gtk::Window>, initial: &Path, done: impl FnOnce(PathBuf) + 'static) {
    let chooser = gtk::FileDialog::builder().title(tr!("Choose a Folder")).modal(true).initial_folder(&gio::File::for_path(initial)).build();
    chooser.select_folder(parent, None::<&gio::Cancellable>, move |result| {
        if let Ok(path) = result.map(|f| f.path()) {
            if let Some(path) = path {
                done(path);
            }
        }
    });
}

fn folder_row(parent_window: &gtk::Window, initial: &Path) -> (gtk::Box, gtk::Entry) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let entry = gtk::Entry::builder().text(initial.display().to_string()).hexpand(true).build();
    let browse = gtk::Button::from_icon_name("folder-open-symbolic");
    browse.set_tooltip_text(Some(&tr!("Choose a Folder")));
    row.append(&entry);
    row.append(&browse);
    let (w, e) = (parent_window.downgrade(), entry.clone());
    browse.connect_clicked(move |_| {
        let current = PathBuf::from(e.text().as_str());
        let e = e.clone();
        choose_folder(w.upgrade().as_ref(), &current, move |path| e.set_text(&path.display().to_string()));
    });
    (row, entry)
}

/// Ark's Create dialog. `kinds`: the formats that can be written now.
pub fn create_dialog(parent: Option<&gtk::Window>, folder: &Path, name: &str, kinds: Vec<Kind>, preferred: Option<Kind>,
                     callback: impl FnOnce(Option<CreateRequest>) + 'static) -> Rc<CreateDialog> {
    let (window, outer, buttons) = dialog(parent, &tr!("Create New Archive"), 560);
    window.set_widget_name("create-dialog");
    // Scroll when every section is expanded on a short screen.
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let scroll = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true)
        .max_content_height(520).vexpand(true).build();
    outer.append(&scroll);
    let grid = gtk::Grid::builder().column_spacing(12).row_spacing(8).build();
    body.append(&grid);
    let (folder_box, folder_entry) = folder_row(&window, folder);
    labeled(&grid, 0, &tr!("Folder:"), &folder_box);
    let name_entry = gtk::Entry::builder().hexpand(true).activates_default(true).build();
    name_entry.set_widget_name("create-name");
    labeled(&grid, 1, &tr!("File name:"), &name_entry);
    let labels: Vec<String> = kinds.iter().map(|k| format!("{} (.{})", tr!(k.label()), k.extension())).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let kind_drop = gtk::DropDown::from_strings(&label_refs);
    kind_drop.set_widget_name("create-type");
    labeled(&grid, 2, &tr!("Type:"), &kind_drop);
    let start = preferred.or(Some(Kind::Zip)).and_then(|k| kinds.iter().position(|x| *x == k)).unwrap_or(0);
    kind_drop.set_selected(start as u32);
    let initial_kind = kinds.get(start).copied().unwrap_or(Kind::Zip);
    name_entry.set_text(&with_extension(name, initial_kind));

    // Compression
    let compression = gtk::Expander::new(Some(&tr!("Compression")));
    compression.set_expanded(true);
    let cgrid = gtk::Grid::builder().column_spacing(12).row_spacing(8).margin_top(6).build();
    compression.set_child(Some(&cgrid));
    body.append(&compression);
    let level = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 9.0, 1.0);
    level.set_digits(0);
    level.set_draw_value(true);
    level.set_value(5.0);
    level.set_hexpand(true);
    level.set_margin_start(12);
    level.set_margin_end(24);
    for (v, l) in [(0.0, tr!("Minimum")), (5.0, tr!("Normal")), (9.0, tr!("Maximum"))] {
        level.add_mark(v, gtk::PositionType::Bottom, Some(&l));
    }
    level.set_widget_name("create-level");
    labeled(&cgrid, 0, &tr!("Compression level:"), &level);
    let method = gtk::DropDown::from_strings(&[]);
    method.set_widget_name("create-method");
    let method_label = labeled(&cgrid, 1, &tr!("Compression method:"), &method);

    // Password
    let protection = gtk::Expander::new(Some(&tr!("Password Protection")));
    protection.set_expanded(false);
    let pgrid = gtk::Grid::builder().column_spacing(12).row_spacing(8).margin_top(6).build();
    protection.set_child(Some(&pgrid));
    body.append(&protection);
    let password = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).build();
    password.set_widget_name("create-password");
    labeled(&pgrid, 0, &tr!("Password:"), &password);
    let confirm = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).build();
    confirm.set_widget_name("create-confirm");
    labeled(&pgrid, 1, &tr!("Confirm password:"), &confirm);
    let encryption = gtk::DropDown::from_strings(&["AES-256", "ZipCrypto"]);
    encryption.set_widget_name("create-encryption");
    let encryption_label = labeled(&pgrid, 2, &tr!("Encryption method:"), &encryption);
    let encrypt_header = gtk::CheckButton::with_label(&tr!("Ask for the password before showing the list of files"));
    encrypt_header.set_widget_name("create-encrypt-header");
    pgrid.attach(&encrypt_header, 1, 3, 1, 1);

    // Volumes
    let volumes = gtk::Expander::new(Some(&tr!("Multi-volume Archive")));
    let vbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    vbox.set_margin_top(6);
    let split = gtk::CheckButton::with_label(&tr!("Create a multi-volume archive"));
    split.set_widget_name("create-split");
    let volume_mb = gtk::SpinButton::with_range(1.0, 1_000_000.0, 1.0);
    volume_mb.set_value(100.0);
    let unit = gtk::Label::new(Some(&tr!("MB per volume")));
    vbox.append(&split);
    vbox.append(&volume_mb);
    vbox.append(&unit);
    volumes.set_child(Some(&vbox));
    body.append(&volumes);

    // Comment
    let comment_expander = gtk::Expander::new(Some(&tr!("Comment")));
    let comment = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).build();
    comment.set_widget_name("create-comment");
    let scroll = gtk::ScrolledWindow::builder().child(&comment).min_content_height(70).margin_top(6).build();
    scroll.add_css_class("frame");
    comment_expander.set_child(Some(&scroll));
    body.append(&comment_expander);

    let problem = gtk::Label::builder().xalign(0.0).wrap(true).build();
    problem.add_css_class("error");
    problem.set_widget_name("create-problem");
    body.append(&problem);
    let cancel = button(&buttons, &tr!("Cancel"), "create-cancel", None);
    let accept = button(&buttons, &tr!("Create"), "create-accept", Some("suggested-action"));
    window.set_default_widget(Some(&accept));

    let this = Rc::new(CreateDialog {
        window: window.clone(), folder: folder_entry, name: name_entry, kind: kind_drop, kinds, level, method, password, confirm,
        encrypt_header, encryption, split, volume_mb, comment, accept: accept.clone(), problem,
    });
    let method_names: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let refresh_kind = {
        let (t, method_names) = (Rc::downgrade(&this), method_names.clone());
        let (compression, protection, volumes, comment_expander) = (compression.clone(), protection.clone(), volumes.clone(), comment_expander.clone());
        Rc::new(move |rename: bool| {
            let Some(t) = t.upgrade() else { return };
            let kind = t.selected_kind();
            if rename {
                t.name.set_text(&with_extension(&t.name.text(), kind));
            }
            let m = methods(kind);
            *method_names.borrow_mut() = m.to_vec();
            t.method.set_model(Some(&gtk::StringList::new(m)));
            t.method.set_visible(!m.is_empty());
            method_label.set_visible(!m.is_empty());
            t.level.set_sensitive(supports_level(kind));
            compression.set_visible(supports_level(kind) || !m.is_empty());
            protection.set_visible(supports_password(kind));
            t.encrypt_header.set_visible(supports_header_encryption(kind));
            t.encryption.set_visible(kind == Kind::Zip);
            encryption_label.set_visible(kind == Kind::Zip);
            volumes.set_visible(supports_volumes(kind));
            comment_expander.set_visible(supports_comment(kind));
            t.validate();
        })
    };
    refresh_kind(false);
    let r = refresh_kind.clone();
    this.kind.connect_selected_notify(move |_| r(true));
    for entry in [this.password.upcast_ref::<gtk::Editable>(), this.confirm.upcast_ref(), this.name.upcast_ref(), this.folder.upcast_ref()] {
        let t = Rc::downgrade(&this);
        entry.connect_changed(move |_| {
            if let Some(t) = t.upgrade() {
                t.validate();
            }
        });
    }
    let done = once(callback);
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(None);
        w.close();
    });
    let (d, w, t) = (done.clone(), window.clone(), Rc::downgrade(&this));
    accept.connect_clicked(move |_| {
        let Some(t) = t.upgrade() else { return };
        let Some(request) = t.request(&method_names.borrow()) else { return };
        let (d, w) = (d.clone(), w.clone());
        if std::fs::symlink_metadata(&request.path).is_ok() {
            let name = request.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            super::dialogs::confirm(Some(&w.clone()), &tr!("File Already Exists"),
                                    &tr!("{name} already exists. Do you want to replace it?", name = name),
                                    &tr!("Replace"), true, move |yes| {
                if yes && std::fs::remove_file(&request.path).is_ok() {
                    d(Some(request));
                    w.close();
                }
            });
            return;
        }
        d(Some(request));
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(None);
        glib::Propagation::Proceed
    });
    // The dialog owns its controller until it is destroyed (callbacks hold weak references).
    let keep = RefCell::new(Some(this.clone()));
    window.connect_destroy(move |_| {
        keep.borrow_mut().take();
    });
    window.present();
    this.name.grab_focus();
    let stem = this.name.text().find('.').unwrap_or(0) as i32;
    this.name.select_region(0, stem);
    this
}

impl CreateDialog {
    pub fn selected_kind(&self) -> Kind {
        self.kinds.get(self.kind.selected() as usize).copied().unwrap_or(Kind::Zip)
    }

    pub fn validate(&self) -> bool {
        let name = self.name.text();
        let problem = if name.trim().is_empty() || name.contains('/') {
            tr!("Enter a file name.")
        } else if !Path::new(self.folder.text().as_str()).is_dir() {
            tr!("The folder does not exist.")
        } else if self.password.text() != self.confirm.text() {
            tr!("The passwords do not match.")
        } else {
            String::new()
        };
        self.problem.set_label(&problem);
        self.problem.set_visible(!problem.is_empty());
        self.accept.set_sensitive(problem.is_empty());
        problem.is_empty()
    }

    pub fn request(&self, method_names: &[&str]) -> Option<CreateRequest> {
        if !self.validate() {
            return None;
        }
        let kind = self.selected_kind();
        let password = self.password.text().to_string();
        let buffer = self.comment.buffer();
        let comment = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let options = CreateOptions {
            level: supports_level(kind).then(|| self.level.value().round() as u32),
            method: method_names.get(self.method.selected() as usize).map(|m| m.to_string()),
            password: (supports_password(kind) && !password.is_empty()).then_some(password.clone()),
            encrypt_header: supports_header_encryption(kind) && !password.is_empty() && self.encrypt_header.is_active(),
            encryption: (kind == Kind::Zip && !password.is_empty())
                .then(|| if self.encryption.selected() == 1 { "ZipCrypto".to_string() } else { "AES256".to_string() }),
            volume_size: (supports_volumes(kind) && self.split.is_active()).then(|| (self.volume_mb.value() * 1024.0 * 1024.0) as u64),
            comment: (supports_comment(kind) && !comment.is_empty()).then_some(comment),
        };
        Some(CreateRequest { path: Path::new(self.folder.text().as_str()).join(self.name.text().trim()), kind, options })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtractRequest {
    pub destination: PathBuf,
    pub subfolder: Option<String>,
    pub preserve_paths: bool,
    pub selected_only: bool,
    pub open_destination: bool,
    pub close_after: bool,
}

pub struct ExtractDialog {
    pub window: gtk::Window,
    pub destination: gtk::Entry,
    pub use_subfolder: gtk::CheckButton,
    pub subfolder: gtk::Entry,
    pub selected_only: gtk::CheckButton,
    pub all: gtk::CheckButton,
    pub preserve: gtk::CheckButton,
    pub open_destination: gtk::CheckButton,
    pub close_after: gtk::CheckButton,
    pub accept: gtk::Button,
}

/// Ark's Extract dialog.
pub fn extract_dialog(parent: Option<&gtk::Window>, archive: &Archive, selection: usize, destination: &Path, settings: &Settings,
                      show_close: bool, callback: impl FnOnce(Option<ExtractRequest>) + 'static) -> Rc<ExtractDialog> {
    let (window, body, buttons) = dialog(parent, &tr!("Extract"), 560);
    window.set_widget_name("extract-dialog");
    heading(&body, &tr!("Extract {name}", name = archive.name()));
    let grid = gtk::Grid::builder().column_spacing(12).row_spacing(8).build();
    body.append(&grid);
    let (dest_box, dest_entry) = folder_row(&window, destination);
    dest_entry.set_widget_name("extract-destination");
    labeled(&grid, 0, &tr!("Extract to:"), &dest_box);
    let use_subfolder = gtk::CheckButton::with_label(&tr!("Extract into subfolder:"));
    use_subfolder.set_widget_name("extract-use-subfolder");
    let subfolder = gtk::Entry::builder().text(ops::subfolder_name(&archive.path)).hexpand(true).build();
    subfolder.set_widget_name("extract-subfolder");
    let sub_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    sub_row.append(&use_subfolder);
    sub_row.append(&subfolder);
    grid.attach(&sub_row, 1, 1, 1, 1);
    use_subfolder.set_active(settings.extract_subfolder && ops::needs_subfolder(&archive.listing));
    subfolder.set_sensitive(use_subfolder.is_active());
    let s = subfolder.clone();
    use_subfolder.connect_toggled(move |c| s.set_sensitive(c.is_active()));

    let all = gtk::CheckButton::with_label(&tr!("All files"));
    all.set_widget_name("extract-all");
    let selected_only = gtk::CheckButton::with_label(&tr!("Selected files only ({count})", count = selection as i64));
    selected_only.set_widget_name("extract-selected");
    selected_only.set_group(Some(&all));
    if selection > 0 {
        selected_only.set_active(true);
    } else {
        all.set_active(true);
        selected_only.set_sensitive(false);
    }
    let which = gtk::Box::new(gtk::Orientation::Vertical, 4);
    which.append(&all);
    which.append(&selected_only);
    labeled(&grid, 2, &tr!("Extract:"), &which);
    let options = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let preserve = gtk::CheckButton::with_label(&tr!("Preserve paths when extracting"));
    preserve.set_widget_name("extract-preserve");
    preserve.set_active(settings.preserve_paths);
    let open_destination = gtk::CheckButton::with_label(&tr!("Open destination folder after extracting"));
    open_destination.set_widget_name("extract-open");
    open_destination.set_active(settings.open_destination);
    let close_after = gtk::CheckButton::with_label(&tr!("Close Archive Manager after extracting"));
    close_after.set_widget_name("extract-close");
    close_after.set_active(settings.close_after_extraction);
    close_after.set_visible(show_close);
    options.append(&preserve);
    options.append(&open_destination);
    options.append(&close_after);
    labeled(&grid, 3, &tr!("Options:"), &options);
    let cancel = button(&buttons, &tr!("Cancel"), "extract-cancel", None);
    let accept = button(&buttons, &tr!("Extract"), "extract-accept", Some("suggested-action"));
    window.set_default_widget(Some(&accept));
    let this = Rc::new(ExtractDialog {
        window: window.clone(), destination: dest_entry, use_subfolder, subfolder, selected_only, all, preserve, open_destination, close_after,
        accept: accept.clone(),
    });
    let done = once(callback);
    let (d, w) = (done.clone(), window.clone());
    cancel.connect_clicked(move |_| {
        d(None);
        w.close();
    });
    let (d, w, t) = (done.clone(), window.clone(), Rc::downgrade(&this));
    accept.connect_clicked(move |_| {
        let Some(t) = t.upgrade() else { return };
        d(Some(t.request()));
        w.close();
    });
    let d = done.clone();
    window.connect_close_request(move |_| {
        d(None);
        glib::Propagation::Proceed
    });
    // The dialog owns its controller until it is destroyed (callbacks hold weak references).
    let keep = RefCell::new(Some(this.clone()));
    window.connect_destroy(move |_| {
        keep.borrow_mut().take();
    });
    window.present();
    accept.grab_focus();
    this
}

impl ExtractDialog {
    pub fn request(&self) -> ExtractRequest {
        let sub = self.subfolder.text().trim().replace('/', "_");
        ExtractRequest {
            destination: PathBuf::from(self.destination.text().as_str()),
            subfolder: (self.use_subfolder.is_active() && !sub.is_empty()).then_some(sub),
            preserve_paths: self.preserve.is_active(),
            selected_only: self.selected_only.is_active(),
            open_destination: self.open_destination.is_active(),
            close_after: self.close_after.is_active() && self.close_after.get_visible(),
        }
    }
}

pub struct PropertiesDialog {
    pub window: gtk::Window,
    pub values: Vec<(String, gtk::Label)>,
}

/// Ark's Properties dialog. Checksums are computed in the background.
pub fn properties_dialog(parent: Option<&gtk::Window>, archive: &Archive) -> Rc<PropertiesDialog> {
    let (window, body, buttons) = dialog(parent, &tr!("Properties"), 560);
    window.set_widget_name("properties-dialog");
    let listing = &archive.listing;
    let meta = std::fs::metadata(&archive.path).ok();
    let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let total = listing.total_size();
    let owner = meta.as_ref().map(|m| {
        use std::os::unix::fs::MetadataExt;
        let (u, g) = (m.uid(), m.gid());
        format!("{} / {}", name_of_user(u).unwrap_or_else(|| u.to_string()), name_of_group(g).unwrap_or_else(|| g.to_string()))
    }).unwrap_or_default();
    let modified = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| super::format_time(d.as_secs() as i64)).unwrap_or_default();
    let ratio = if total > 0 { format!("{:.1} %", file_size as f64 * 100.0 / total as f64) } else { "-".into() };
    let yes_no = |b: bool| if b { tr!("Yes") } else { tr!("No") };
    let mut rows: Vec<(String, String)> = vec![
        (tr!("Name:"), archive.name()),
        (tr!("Location:"), archive.path.parent().map(|p| p.display().to_string()).unwrap_or_default()),
        (tr!("Type:"), tr!(archive.kind.label())),
        (tr!("MIME type:"), archive.mime.clone()),
        (tr!("Owned by:"), owner),
        (tr!("Archive size:"), super::format_size(file_size)),
        (tr!("Unpacked size:"), super::format_size(total)),
        (tr!("Compression ratio:"), ratio),
        (tr!("Entries:"), tr!("{entries} ({files} files, {folders} folders)", entries = listing.entries.len() as i64,
                              files = listing.files() as i64, folders = listing.folders() as i64)),
        (tr!("Last modified:"), modified),
        (tr!("Password protected:"), yes_no(listing.encrypted || listing.header_encrypted)),
        (tr!("Encrypted file list:"), yes_no(listing.header_encrypted)),
        (tr!("Has comment:"), yes_no(listing.comment.is_some())),
        (tr!("Volumes:"), archive.volumes.len().to_string()),
        (tr!("Plugin:"), archive.reader.to_string()),
        (tr!("Read-only:"), archive.read_only_reason().unwrap_or_else(|| tr!("No"))),
    ];
    if let Some(method) = &listing.method {
        rows.insert(4, (tr!("Method:"), method.clone()));
    }
    rows.push(("MD5:".into(), tr!("Calculating…")));
    rows.push(("SHA-1:".into(), tr!("Calculating…")));
    rows.push(("SHA-256:".into(), tr!("Calculating…")));
    let grid = gtk::Grid::builder().column_spacing(12).row_spacing(6).build();
    let mut values = Vec::new();
    for (i, (k, v)) in rows.into_iter().enumerate() {
        let key = gtk::Label::builder().label(&k).xalign(1.0).yalign(0.0).build();
        key.add_css_class("dim-label");
        let value = gtk::Label::builder().label(&v).xalign(0.0).selectable(true).wrap(true).wrap_mode(pango::WrapMode::Char).hexpand(true).build();
        grid.attach(&key, 0, i as i32, 1, 1);
        grid.attach(&value, 1, i as i32, 1, 1);
        values.push((k, value));
    }
    let scroll = gtk::ScrolledWindow::builder().child(&grid).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true).max_content_height(560).build();
    body.append(&scroll);
    let close = button(&buttons, &tr!("Close"), "properties-close", Some("suggested-action"));
    let w = window.clone();
    close.connect_clicked(move |_| w.close());
    let this = Rc::new(PropertiesDialog { window: window.clone(), values });
    let path = archive.path.clone();
    let weak = Rc::downgrade(&this);
    let job = crate::job::spawn(None, crate::job::JobUi::silent(), move |ctx, _| ops::checksums(&path, ctx), move |result| {
        let Some(this) = weak.upgrade() else { return };
        let n = this.values.len();
        match result {
            Ok(sums) => {
                for (i, sum) in sums.iter().enumerate() {
                    this.values[n - 3 + i].1.set_label(sum);
                }
            }
            Err(e) => {
                for i in 0..3 {
                    this.values[n - 3 + i].1.set_label(&e.to_string());
                }
            }
        }
    });
    window.connect_close_request(move |_| {
        job.cancel();
        glib::Propagation::Proceed
    });
    // The dialog owns its controller until it is destroyed (callbacks hold weak references).
    let keep = RefCell::new(Some(this.clone()));
    window.connect_destroy(move |_| {
        keep.borrow_mut().take();
    });
    window.present();
    this
}

pub fn name_of_user(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| crate::ffi::text(pwd.pw_name)).flatten()
}

pub fn name_of_group(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| crate::ffi::text(grp.gr_name)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_follow_type() {
        assert_eq!(with_extension("photos.zip", Kind::TarGz), "photos.tar.gz");
        assert_eq!(with_extension("photos.tar.gz", Kind::SevenZip), "photos.7z");
        assert_eq!(with_extension("photos", Kind::Zip), "photos.zip");
        assert_eq!(with_extension("my.notes", Kind::Zip), "my.notes.zip");
        assert_eq!(with_extension("a.tar.Z", Kind::TarZ), "a.tar.Z");
    }
}
