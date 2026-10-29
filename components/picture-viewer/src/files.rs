//! Port of picture_files.py: file, print and preferences actions of the Pictures window.

use crate::dialogs::{ask, conflict, form, number, open_with, results};
use crate::model::{self, save_format};
use crate::ui::{pref_bool, pref_int, Action, PicturesWindow};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::Value;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use suite_core::tr;

/// Image types offered by "Set as Default Image Viewer"; the .desktop file
/// additionally lists the video types Pictures plays (VIDEO_MIME_TYPES).
pub const IMAGE_MIME_TYPES: [&str; 13] = ["image/jpeg", "image/png", "image/webp", "image/gif", "image/tiff", "image/bmp", "image/x-icon",
    "image/svg+xml", "image/svg+xml-compressed", "image/avif", "image/heif", "image/heic", "image/jxl"];
pub const VIDEO_MIME_TYPES: [&str; 8] = ["video/mp4", "video/x-m4v", "video/webm", "video/x-matroska", "video/quicktime",
    "video/x-msvideo", "video/vnd.avi", "video/ogg"];
/// `MimeType=` of org.waydock.Pictures.desktop.
pub const DESKTOP_MIME_TYPES: &str = "image/jpeg;image/png;image/webp;image/gif;image/tiff;image/bmp;image/x-icon;image/svg+xml;\
image/svg+xml-compressed;image/avif;image/heif;image/heic;image/jxl;video/mp4;video/x-m4v;video/webm;video/x-matroska;video/quicktime;\
video/x-msvideo;video/vnd.avi;video/ogg;";

/// First free "name (2).ext" style sibling of an existing destination.
pub fn unique_target(target: &Path) -> Result<PathBuf, String> {
    let name = target.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let suffix = model::suffix(target);
    let suffix = if suffix.is_empty() { String::new() } else { name[name.len() - suffix.len()..].to_string() };
    let stem = &name[..name.len() - suffix.len()];
    for number in 2..10000 {
        let candidate = target.with_file_name(format!("{stem} ({number}){suffix}"));
        if !candidate.exists() && std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(tr!("Could not find a free name for {name}.", name = name.as_str()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Write,
    Skip,
    Keep,
    Replace,
}

impl Decision {
    fn from_value(value: &str) -> Self {
        match value {
            "skip" => Self::Skip,
            "keep" => Self::Keep,
            "replace" => Self::Replace,
            _ => Self::Write,
        }
    }
}

fn file_name(path: &Path) -> String { path.file_name().unwrap_or_default().to_string_lossy().into_owned() }

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `perform_transfer` work: per-file outcome `(source, Some(target) when done, text)`.
pub fn transfer_files(decisions: &[(PathBuf, PathBuf, Decision)], moving: bool, cancelled: &[PathBuf])
    -> Vec<(PathBuf, Option<PathBuf>, String)> {
    let mut outcome = Vec::new();
    for (source, target, decision) in decisions {
        if *decision == Decision::Skip {
            outcome.push((source.clone(), None, tr!("Skipped")));
            continue;
        }
        let result = (|| -> Result<(PathBuf, String), String> {
            let mut flags = if moving { gio::FileCopyFlags::NONE } else { gio::FileCopyFlags::ALL_METADATA };
            let mut target = target.clone();
            match decision {
                Decision::Keep => target = unique_target(&target)?,
                Decision::Replace => {
                    if same_file(source, &target) {
                        return Err(String::new());
                    }
                    flags |= gio::FileCopyFlags::OVERWRITE;
                }
                _ => {}
            }
            let (src, dest) = (gio::File::for_path(source), gio::File::for_path(&target));
            let done = if moving { src.move_(&dest, flags, None::<&gio::Cancellable>, None) }
                       else { src.copy(&dest, flags, None::<&gio::Cancellable>, None) };
            done.map_err(|e| e.message().to_string())?;
            let text = match decision {
                Decision::Replace => tr!("Replaced"),
                Decision::Keep => tr!("Saved as {name}", name = file_name(&target).as_str()),
                _ if moving => tr!("Moved"),
                _ => tr!("Copied"),
            };
            Ok((target, text))
        })();
        match result {
            Ok((target, text)) => outcome.push((source.clone(), Some(target), text)),
            Err(error) if error.is_empty() => outcome.push((source.clone(), None, tr!("Skipped: source and destination are the same file"))),
            Err(error) => outcome.push((source.clone(), None, tr!("Failed: {error}", error = error.as_str()))),
        }
    }
    outcome.extend(cancelled.iter().map(|source| (source.clone(), None, tr!("Cancelled"))));
    outcome
}

impl PicturesWindow {
    pub fn choose_open(&self, folder: bool) {
        let chooser = gtk::FileDialog::builder().title(if folder { tr!("Open Folder") } else { tr!("Open Pictures") }).build();
        chooser.set_initial_folder(Some(&gio::File::for_path(&*self.folder.borrow())));
        let weak = Rc::downgrade(&self.0);
        if folder {
            chooser.select_folder(Some(&self.window), None::<&gio::Cancellable>, move |result| {
                if let (Some(window), Ok(file)) = (weak.upgrade().map(PicturesWindow), result) {
                    window.open_paths(&file.path().into_iter().collect::<Vec<_>>(), false);
                }
            });
        } else {
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            let allowed = gtk::FileFilter::new();
            allowed.set_name(Some(&tr!("Pictures and videos")));
            allowed.add_mime_type("image/*");
            allowed.add_mime_type("video/*");
            filters.append(&allowed);
            chooser.set_filters(Some(&filters));
            chooser.open_multiple(Some(&self.window), None::<&gio::Cancellable>, move |result| {
                if let (Some(window), Ok(files)) = (weak.upgrade().map(PicturesWindow), result) {
                    let paths: Vec<PathBuf> = (0..files.n_items()).filter_map(|i| files.item(i))
                        .filter_map(|o| o.downcast::<gio::File>().ok()).filter_map(|f| f.path()).collect();
                    window.open_paths(&paths, false);
                }
            });
        }
    }

    pub fn save(&self, copy: bool, after: Option<Action>) {
        if self.busy.get() {
            return;
        }
        let Some(info) = self.doc_info() else { return };
        self.stop_animation();
        if copy || save_format(&info.path).is_none() || info.frames > 1 {
            let chooser = gtk::FileDialog::builder().title(tr!("Save Picture As")).build();
            let mut suffix = if save_format(&info.path).is_some() { model::suffix(&info.path) } else { ".png".into() };
            if info.frames > 1 {
                suffix = ".png".into();
            }
            let name = file_name(&info.path);
            let stem = name.strip_suffix(&*{
                let ext = model::suffix(&info.path);
                if ext.is_empty() { String::new() } else { name[name.len() - ext.len()..].to_string() }
            }).unwrap_or(&name).to_string();
            chooser.set_initial_name(Some(&format!("{stem}{}{suffix}", if copy { "-edited" } else { "" })));
            chooser.set_initial_folder(Some(&gio::File::for_path(info.path.parent().unwrap_or(Path::new("/")))));
            let weak = Rc::downgrade(&self.0);
            chooser.save(Some(&self.window), None::<&gio::Cancellable>, move |result| {
                let (Some(window), Ok(file)) = (weak.upgrade().map(PicturesWindow), result) else { return };
                match file.path() {
                    None => window.message(&tr!("Choose a local or mounted destination.")),
                    Some(path) => window.save_to(&path, true, after.clone()),
                }
            });
        } else {
            self.save_to(&info.path, false, after);
        }
    }

    pub fn save_to(&self, path: &Path, overwrite: bool, after: Option<Action>) {
        let Some(doc) = self.doc.borrow().clone() else { return };
        self.stop_animation();
        let frame = self.doc_info().filter(|i| i.frames > 1).map(|_| self.displayed_frame.get());
        let quality = pref_int(&self.preferences.borrow(), "quality", 95);
        let hidden = pref_bool(&self.preferences.borrow(), "hidden", false);
        let path = path.to_path_buf();
        self.run(move || {
            let destination = doc.lock().unwrap().save(Some(&path), overwrite, quality, frame).map_err(|e| e.to_string())?;
            let parent = destination.parent().unwrap_or(Path::new("/")).to_path_buf();
            let entries = model::scan_folder(&parent, hidden, "name", false).unwrap_or_default();
            Ok((destination, entries))
        }, move |w, (destination, entries)| {
            w.entries.replace(entries);
            w.message(&tr!("Saved {path}", path = destination.to_string_lossy().as_ref()));
            w.update_document();
            w.folder.replace(destination.parent().unwrap_or(Path::new("/")).to_path_buf());
            w.refresh_strip();
            w.watch_folder();
            if let Some(after) = after {
                after(w);
            }
        }, None);
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        if self.stack.visible_child_name().as_deref() == Some("view") {
            let current = self.current_path.borrow().clone();
            if let Some(current) = current {
                return vec![current];
            }
        }
        (0..self.store.n_items()).filter(|i| self.selection.is_selected(*i))
            .filter_map(|i| self.store.item(i).and_then(|o| crate::ui::entry_of(&o)))
            .filter(|e| !e.folder).map(|e| e.path).collect()
    }

    pub fn file_action(&self, action: &str) {
        if self.busy.get() {
            return;
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.message(&tr!("Select a picture first."));
            return;
        }
        match action {
            "open-with" => { open_with(&self.window, &paths[0]); }
            "folder" => {
                let uri = gio::File::for_path(paths[0].parent().unwrap_or(Path::new("/"))).uri();
                let uri = uri.to_string();
                glib::spawn_future_local(async move { let _ = gio::AppInfo::launch_default_for_uri_future(&uri, None::<&gio::AppLaunchContext>).await; });
            }
            "copy-image" => {
                let (Some(doc), Some(info)) = (self.doc.borrow().clone(), self.doc_info()) else { return };
                let pixels = if info.frames > 1 {
                    match model::load_pixels(&info.path, None, self.displayed_frame.get()) {
                        Ok(loaded) => loaded.image,
                        Err(error) => return self.message(&error.to_string()),
                    }
                } else {
                    doc.lock().unwrap().image.clone()
                };
                let (w, h) = pixels.dimensions();
                let texture = gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8,
                                                      &glib::Bytes::from_owned(pixels.into_raw()), w as usize * 4);
                self.window.clipboard().set_texture(&texture);
                self.message(&tr!("Image copied to the clipboard."));
            }
            "rename" => {
                if paths.len() != 1 {
                    self.message(&tr!("Select one picture to rename."));
                    return;
                }
                let entry = gtk::Entry::builder().text(file_name(&paths[0])).build();
                let (weak, text, source) = (Rc::downgrade(&self.0), entry.clone(), paths[0].clone());
                form(&self.window, &tr!("Rename Picture"), vec![(tr!("Name"), entry.upcast())], &tr!("Rename"), move || {
                    let name = suite_core::pyjson::py_strip(&text.text()).to_string();
                    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                        return Err(tr!("Enter a file name without slashes."));
                    }
                    if let Some(window) = weak.upgrade().map(PicturesWindow) {
                        let (source, target) = (source.clone(), source.with_file_name(&name));
                        window.guard(Rc::new(move |w: &PicturesWindow| w.transfer(&[source.clone()], &target, true, true)));
                    }
                    Ok(())
                });
            }
            "copy" | "move" => {
                let moving = action == "move";
                let chooser = gtk::FileDialog::builder().title(if moving { tr!("Move Pictures To") } else { tr!("Copy Pictures To") }).build();
                let weak = Rc::downgrade(&self.0);
                chooser.select_folder(Some(&self.window), None::<&gio::Cancellable>, move |result| {
                    let (Some(window), Ok(folder)) = (weak.upgrade().map(PicturesWindow), result) else { return };
                    if let Some(target) = folder.path() {
                        let paths = paths.clone();
                        window.guard(Rc::new(move |w: &PicturesWindow| w.transfer(&paths, &target, moving, false)));
                    }
                });
            }
            "trash" => {
                let count = paths.len() as i64;
                let weak = Rc::downgrade(&self.0);
                let ask_paths = paths.clone();
                self.guard(Rc::new(move |w: &PicturesWindow| {
                    let (weak, paths) = (weak.clone(), ask_paths.clone());
                    ask(&w.window, &tr!("Move to Trash?"), &tr!("Move {count} selected picture(s) to Trash?", count = count),
                        &[(tr!("Cancel"), "cancel"), (tr!("Move to Trash"), "trash")], move |choice| {
                            if choice != "trash" {
                                return;
                            }
                            let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
                            let paths = paths.clone();
                            window.run(move || {
                                let mut done = Vec::new();
                                for path in paths {
                                    gio::File::for_path(&path).trash(None::<&gio::Cancellable>)
                                        .map_err(|e| suite_core::pyerr::python_error_text(&e))?;
                                    done.push(path);
                                }
                                Ok(done)
                            }, |w, done: Vec<PathBuf>| {
                                w.message(&tr!("Moved {count} picture(s) to Trash.", count = done.len() as i64));
                                w.last_trashed.replace(done);
                                let folder = w.folder.borrow().clone();
                                w.browse_folder(&folder, true);
                            }, None);
                        });
                }));
            }
            _ => {}
        }
    }

    /// Copy/move with per-file conflict choices and a result summary.
    pub fn transfer(&self, paths: &[PathBuf], destination: &Path, moving: bool, one: bool) {
        let plan: Vec<(PathBuf, PathBuf)> = paths.iter().map(|p| {
            (p.clone(), if one { destination.to_path_buf() } else { destination.join(p.file_name().unwrap_or_default()) })
        }).collect();
        self.run(move || Ok(plan.into_iter().map(|(s, t)| {
            let exists = t.exists() || std::fs::symlink_metadata(&t).is_ok();
            (s, t, exists)
        }).collect::<Vec<_>>()), move |w, probed| w.resolve_conflicts(probed, moving), None);
    }

    fn resolve_conflicts(&self, probed: Vec<(PathBuf, PathBuf, bool)>, moving: bool) {
        struct State {
            decisions: Vec<(PathBuf, PathBuf, Decision)>,
            pending: Vec<(PathBuf, PathBuf, bool)>,
            remembered: Option<Decision>,
        }
        let state = Rc::new(RefCell::new(State { decisions: Vec::new(), pending: probed, remembered: None }));
        fn step(window: &PicturesWindow, state: Rc<RefCell<State>>, moving: bool) {
            loop {
                let next = state.borrow().pending.first().cloned();
                let Some((source, target, exists)) = next else { break };
                let remembered = state.borrow().remembered;
                if !exists || remembered.is_some() {
                    let mut s = state.borrow_mut();
                    s.decisions.push((source, target, remembered.filter(|_| exists).unwrap_or(Decision::Write)));
                    s.pending.remove(0);
                    continue;
                }
                let remaining = state.borrow().pending.iter().filter(|item| item.2).count();
                let weak = Rc::downgrade(&window.0);
                let folder = target.parent().unwrap_or(Path::new("/")).to_string_lossy().into_owned();
                let state = state.clone();
                conflict(&window.window, &file_name(&target), &folder, remaining, move |value, every| {
                    let Some(window) = weak.upgrade().map(PicturesWindow) else { return };
                    if value == "cancel" {
                        let (decisions, cancelled): (Vec<_>, Vec<PathBuf>) = {
                            let s = state.borrow();
                            (s.decisions.clone(), s.pending.iter().map(|p| p.0.clone()).collect())
                        };
                        window.message(&if decisions.is_empty() { tr!("Cancelled. No pictures were copied or moved.") }
                                        else { tr!("Cancelled before the remaining pictures.") });
                        if !decisions.is_empty() {
                            window.perform_transfer(decisions, moving, cancelled);
                        }
                        return;
                    }
                    {
                        let mut s = state.borrow_mut();
                        let decision = Decision::from_value(value);
                        if every {
                            s.remembered = Some(decision);
                        }
                        s.decisions.push((source.clone(), target.clone(), decision));
                        s.pending.remove(0);
                    }
                    step(&window, state.clone(), moving);
                });
                return;
            }
            let decisions = state.borrow().decisions.clone();
            window.perform_transfer(decisions, moving, Vec::new());
        }
        step(self, state, moving);
    }

    fn perform_transfer(&self, decisions: Vec<(PathBuf, PathBuf, Decision)>, moving: bool, cancelled: Vec<PathBuf>) {
        let work_decisions = decisions.clone();
        self.run(move || Ok(transfer_files(&work_decisions, moving, &cancelled)), move |w, outcome| {
            let done: Vec<PathBuf> = outcome.iter().filter_map(|(_, t, _)| t.clone()).collect();
            let problems = outcome.iter().filter(|(_, t, _)| t.is_none()).count();
            if moving && !done.is_empty() {
                if let Some(info) = w.doc_info() {
                    if outcome.iter().any(|(s, t, _)| t.is_some() && *s == info.path) {
                        w.set_doc(None);
                    }
                }
            }
            if let Some(first) = done.first() {
                w.open_paths(&[first.clone()], false);
            } else if w.stack.visible_child_name().as_deref() == Some("browse") {
                let folder = w.folder.borrow().clone();
                w.browse_folder(&folder, true);
            }
            let mut summary = if moving { tr!("Moved {count} picture(s).", count = done.len() as i64) }
                              else { tr!("Copied {count} picture(s).", count = done.len() as i64) };
            if problems > 0 {
                summary += &format!(" {}", tr!("{count} not completed.", count = problems as i64));
            }
            if problems > 0 || decisions.iter().any(|(_, _, d)| *d != Decision::Write) {
                let rows: Vec<(String, String)> = outcome.iter().map(|(s, _, text)| (file_name(s), text.clone())).collect();
                let dialog = results(&w.window, &if moving { tr!("Move Results") } else { tr!("Copy Results") }, &rows);
                w.transfer_results.replace(Some(dialog));
            }
            w.message(&summary);
        }, None);
    }

    pub fn restore_trash(&self) {
        let wanted: Vec<PathBuf> = self.last_trashed.borrow().clone();
        if wanted.is_empty() {
            self.message(&tr!("No pictures trashed by this window to restore."));
            return;
        }
        self.run(move || {
            let trash = gio::File::for_uri("trash:///");
            let mut found: Vec<(PathBuf, String, String)> = Vec::new();
            let enumerator = trash.enumerate_children("standard::name,trash::orig-path,trash::deletion-date",
                                                      gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)
                .map_err(|e| suite_core::pyerr::python_error_text(&e))?;
            while let Some(entry) = enumerator.next_file(None::<&gio::Cancellable>).map_err(|e| suite_core::pyerr::python_error_text(&e))? {
                let Some(original) = entry.attribute_byte_string("trash::orig-path").map(|s| PathBuf::from(s.as_str())) else { continue };
                if !wanted.contains(&original) {
                    continue;
                }
                let stamp = entry.attribute_string("trash::deletion-date").map(|s| s.to_string()).unwrap_or_default();
                let name = entry.name().to_string_lossy().into_owned();
                match found.iter_mut().find(|(o, _, _)| *o == original) {
                    Some(old) if stamp > old.1 => *old = (original, stamp, name),
                    Some(_) => {}
                    None => found.push((original, stamp, name)),
                }
            }
            let _ = enumerator.close(None::<&gio::Cancellable>);
            let mut restored = Vec::new();
            for (original, _, name) in found {
                trash.child(&name).move_(&gio::File::for_path(&original), gio::FileCopyFlags::NONE, None::<&gio::Cancellable>, None)
                    .map_err(|e| suite_core::pyerr::python_error_text(&e))?;
                restored.push(original);
            }
            Ok(restored)
        }, |w, restored: Vec<PathBuf>| {
            w.last_trashed.borrow_mut().retain(|p| !restored.contains(p));
            let folder = w.folder.borrow().clone();
            w.browse_folder(&folder, true);
            w.message(&tr!("Restored {count} picture(s).", count = restored.len() as i64));
        }, None);
    }

    pub fn print_picture(&self) {
        if self.doc.borrow().is_none() {
            return;
        }
        let Some(surface) = self.canvas.surface() else { return };
        let operation = gtk::PrintOperation::builder().n_pages(1).build();
        operation.connect_draw_page(move |_, context, _| {
            let cr = context.cairo_context();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.paint();
            let (sw, sh) = (surface.width() as f64, surface.height() as f64);
            let scale = (context.width() / sw).min(context.height() / sh);
            cr.translate((context.width() - sw * scale) / 2.0, (context.height() - sh * scale) / 2.0);
            cr.scale(scale, scale);
            let _ = cr.set_source_surface(&surface, 0.0, 0.0);
            let _ = cr.paint();
        });
        let _ = operation.run(gtk::PrintOperationAction::PrintDialog, Some(&self.window));
    }

    pub fn show_preferences(&self) {
        let preferences = self.preferences.borrow().clone();
        let interval = number(pref_int(&preferences, "interval", 5) as f64, 1.0, 3600.0, 1.0);
        let quality = number(pref_int(&preferences, "quality", 95) as f64, 1.0, 100.0, 1.0);
        let switch = |key: &str, default: bool| gtk::Switch::builder().active(pref_bool(&preferences, key, default)).build();
        let (repeat, shuffle, hidden, keep_zoom) = (switch("loop", true), switch("shuffle", false), switch("hidden", false), switch("keep_zoom", false));
        let weak = Rc::downgrade(&self.0);
        let (i, q, r, s, h, k) = (interval.clone(), quality.clone(), repeat.clone(), shuffle.clone(), hidden.clone(), keep_zoom.clone());
        form(&self.window, &tr!("Pictures Settings"), vec![
            (tr!("Slideshow interval (seconds)"), interval.upcast()), (tr!("Loop slideshow"), repeat.upcast()),
            (tr!("Shuffle slideshow"), shuffle.upcast()), (tr!("JPEG / WebP quality"), quality.upcast()),
            (tr!("Show hidden files"), hidden.upcast()), (tr!("Keep zoom between pictures"), keep_zoom.upcast())],
            &tr!("Apply"), move || {
                let Some(window) = weak.upgrade().map(PicturesWindow) else { return Ok(()) };
                {
                    let mut preferences = window.preferences.borrow_mut();
                    for (key, value) in [("interval", Value::from(i.value_as_int())), ("quality", Value::from(q.value_as_int())),
                                         ("loop", Value::from(r.is_active())), ("shuffle", Value::from(s.is_active())),
                                         ("hidden", Value::from(h.is_active())), ("keep_zoom", Value::from(k.is_active()))] {
                        preferences.insert(key.into(), value);
                    }
                }
                window.save_preferences();
                if window.stack.visible_child_name().as_deref() == Some("browse") {
                    let folder = window.folder.borrow().clone();
                    window.browse_folder(&folder, true);
                }
                Ok(())
            });
    }

    /// `settings.json`: `json.dumps(preferences, indent=2)` via `settings.tmp` + replace.
    pub fn save_preferences(&self) {
        let text = suite_core::pyfmt::json_dumps(&Value::Object(self.preferences.borrow().clone()),
            &suite_core::pyfmt::JsonStyle { indent: Some(2), item_separator: ",", ..suite_core::pyfmt::JsonStyle::DEFAULT });
        let path = &self.config_path;
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = path.with_extension("tmp");
            std::fs::write(&temp, text)?;
            std::fs::rename(&temp, path)
        })();
        if let Err(error) = result {
            // Python lets the OSError escape into the GTK callback; report it instead.
            self.message(&suite_core::pyerr::os_error_path(&error, path));
        }
    }
}
