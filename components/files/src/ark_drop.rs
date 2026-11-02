//! ark_drop.py: Ark drag extraction over its public D-Bus protocol; no KDE
//! library imports. Ark supplies a bus name and object path (KDE ark
//! part/archivemodel.cpp, kio dropjob.cpp) rather than extracted files.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub const SERVICE: &str = "application/x-kde-ark-dndextract-service";
pub const OBJECT: &str = "application/x-kde-ark-dndextract-path";

pub fn accepts(drop: &gdk::Drop) -> bool {
    let formats = drop.formats();
    drop.actions().contains(gdk::DragAction::COPY) && [SERVICE, OBJECT].iter().all(|mime| formats.contain_mime_type(mime))
}

/// The parts of `Gdk.Drop` ExtractionDrop uses (tests use a fake, as Python's SimpleNamespace).
pub trait DropSource {
    fn read(&self, mime: &'static str, cancel: &gio::Cancellable, done: Box<dyn FnOnce(Result<(gio::InputStream, String), glib::Error>)>);
    fn finish(&self, action: gdk::DragAction);
}

impl DropSource for gdk::Drop {
    fn read(&self, mime: &'static str, cancel: &gio::Cancellable, done: Box<dyn FnOnce(Result<(gio::InputStream, String), glib::Error>)>) {
        self.read_async(&[mime], glib::Priority::DEFAULT, Some(cancel), move |result| done(result.map(|(s, m)| (s, m.to_string()))));
    }
    fn finish(&self, action: gdk::DragAction) {
        gdk::Drop::finish(self, action)
    }
}

struct State {
    drop: Rc<dyn DropSource>,
    destination: gio::File,
    report: Rc<dyn Fn(String)>,
    cancel: gio::Cancellable,
    done: bool,
    values: HashMap<&'static str, String>,
    stream: Option<gio::InputStream>,
    timeout: Option<glib::SourceId>,
}

/// Keeps the drop alive until Ark acknowledges the extraction request.
#[derive(Clone)]
pub struct ExtractionDrop(Rc<RefCell<State>>);

impl ExtractionDrop {
    pub fn new(drop: Rc<dyn DropSource>, destination: gio::File, report: Rc<dyn Fn(String)>) -> Self {
        let this = ExtractionDrop(Rc::new(RefCell::new(State {
            drop, destination, report, cancel: gio::Cancellable::new(), done: false, values: HashMap::new(), stream: None, timeout: None,
        })));
        let weak = Rc::downgrade(&this.0);
        let source = glib::timeout_add_seconds_local(10, move || {
            if let Some(state) = weak.upgrade() {
                state.borrow_mut().timeout = None;
                ExtractionDrop(state).finish(false, "Ark did not respond to the extraction request.");
            }
            glib::ControlFlow::Break
        });
        this.0.borrow_mut().timeout = Some(source);
        this.read(SERVICE);
        this
    }

    fn read(&self, mime: &'static str) {
        let (drop, cancel) = {
            let s = self.0.borrow();
            (s.drop.clone(), s.cancel.clone())
        };
        let this = self.clone();
        drop.read(mime, &cancel, Box::new(move |result| match result {
            Ok((stream, received)) => {
                this.0.borrow_mut().stream = Some(stream.clone());
                if this.0.borrow().done {
                    stream.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
                    return;
                }
                if received.as_str() != mime {
                    this.finish(false, "Ark supplied an unexpected drag format.");
                    return;
                }
                this.chunk(mime, Vec::new());
            }
            Err(error) => this.finish(false, &tessel_core::pyerr::python_error_text(&error)),
        }));
    }

    fn chunk(&self, mime: &'static str, mut data: Vec<u8>) {
        let (stream, cancel) = {
            let s = self.0.borrow();
            (s.stream.clone(), s.cancel.clone())
        };
        let Some(stream) = stream else { return };
        let this = self.clone();
        stream.clone().read_bytes_async(1024, glib::Priority::DEFAULT, Some(&cancel), move |result| {
            let chunk = match result {
                Ok(chunk) => chunk,
                Err(error) => return this.finish(false, &tessel_core::pyerr::python_error_text(&error)),
            };
            if this.0.borrow().done {
                return;
            }
            data.extend_from_slice(&chunk);
            if data.len() > 4096 {
                return this.finish(false, "Ark supplied an invalid extraction request.");
            }
            if !chunk.is_empty() {
                return this.chunk(mime, data);
            }
            stream.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
            this.0.borrow_mut().stream = None;
            match String::from_utf8(data) {
                Ok(text) => {
                    this.0.borrow_mut().values.insert(mime, text);
                }
                Err(error) => {
                    // Python: str(UnicodeDecodeError).
                    let e = error.utf8_error();
                    let position = e.valid_up_to();
                    let byte = error.as_bytes()[position];
                    return this.finish(false, &format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: invalid start byte"));
                }
            }
            if mime == SERVICE {
                this.read(OBJECT);
            } else {
                this.extract();
            }
        });
    }

    fn extract(&self) {
        let (service, path, cancel) = {
            let s = self.0.borrow();
            (s.values[SERVICE].clone(), s.values[OBJECT].clone(), s.cancel.clone())
        };
        if !gio::dbus_is_unique_name(&service) || !glib::Variant::is_object_path(&path) || !path.starts_with("/DndExtract/") {
            return self.finish(false, "Ark supplied an invalid extraction address.");
        }
        let this = self.clone();
        gio::bus_get(gio::BusType::Session, Some(&cancel.clone()), move |result| {
            let bus = match result {
                Ok(bus) => bus,
                Err(error) => return this.finish(false, error.message()),
            };
            if this.0.borrow().done {
                return;
            }
            let destination = {
                let s = this.0.borrow();
                s.destination.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| s.destination.uri().to_string())
            };
            let done = this.clone();
            bus.call(Some(&service), &path, "org.kde.ark.DndExtract", "extractSelectedFilesTo", Some(&(destination,).to_variant()),
                     None, gio::DBusCallFlags::NO_AUTO_START, 5000, Some(&cancel), move |result| match result {
                         Ok(_) => done.finish(true, ""),
                         Err(error) => done.finish(false, error.message()),
                     });
        });
    }

    pub fn finish(&self, success: bool, error: &str) {
        let (drop, report) = {
            let mut s = self.0.borrow_mut();
            if s.done {
                return;
            }
            s.done = true;
            if let Some(source) = s.timeout.take() {
                source.remove();
            }
            s.cancel.cancel();
            if let Some(stream) = s.stream.take() {
                stream.close_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, |_| {});
            }
            (s.drop.clone(), s.report.clone())
        };
        drop.finish(if success { gdk::DragAction::COPY } else { gdk::DragAction::empty() });
        if !error.is_empty() {
            report(format!("Could not extract files from Ark: {error}"));
        }
    }
}

/// Resolve the folder at drop time, since virtualized rows are recycled.
pub fn attach_ark_drop(widget: &impl IsA<gtk::Widget>, destination: Rc<dyn Fn() -> Option<gio::File>>,
                       report: Rc<dyn Fn(String)>) -> gtk::DropTargetAsync {
    let target = gtk::DropTargetAsync::new(Some(gdk::ContentFormats::new(&[SERVICE, OBJECT])), gdk::DragAction::COPY);
    let available = {
        let destination = destination.clone();
        Rc::new(move |drop: &gdk::Drop| destination().is_some() && accepts(drop))
    };
    let a = available.clone();
    target.connect_accept(move |_, drop| a(drop));
    let a = available.clone();
    target.connect_drag_enter(move |_, drop, _, _| if a(drop) { gdk::DragAction::COPY } else { gdk::DragAction::empty() });
    let a = available.clone();
    target.connect_drag_motion(move |_, drop, _, _| if a(drop) { gdk::DragAction::COPY } else { gdk::DragAction::empty() });
    target.connect_drop(move |_, drop, _, _| {
        if !available(drop) {
            return false;
        }
        if let Some(folder) = destination() {
            ExtractionDrop::new(Rc::new(drop.clone()), folder, report.clone());
        }
        true
    });
    widget.add_controller(target.clone());
    target
}
