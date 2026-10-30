//! Ark's drag-out protocol. A drag of archive entries offers
//!   application/x-kde-ark-dndextract-service = our unique D-Bus name,
//!   application/x-kde-ark-dndextract-path    = /DndExtract/<n>,
//! and an Ark-aware target (Files, Dolphin) calls
//! `org.kde.ark.DndExtract.extractSelectedFilesTo(s destination)` on that
//! object; we answer at once and extract the dragged entries there with the
//! normal progress and conflict handling (Ark: part/archivemodel.cpp,
//! part/part.cpp `extractSelectedFilesTo`). For other targets the drag also
//! offers text/uri-list, produced lazily: only when a target asks for it are
//! the entries extracted to a private staging folder below $XDG_RUNTIME_DIR.

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

pub const SERVICE: &str = "application/x-kde-ark-dndextract-service";
pub const OBJECT: &str = "application/x-kde-ark-dndextract-path";
pub const URI_LIST: &str = "text/uri-list";
pub const INTERFACE: &str = "org.kde.ark.DndExtract";
const XML: &str = "<node><interface name='org.kde.ark.DndExtract'><method name='extractSelectedFilesTo'>\
<arg name='path' type='s' direction='in'/></method></interface></node>";

thread_local! {
    static COUNTER: Cell<u32> = const { Cell::new(0) };
}

/// The destination string Files sends: a path, or a URI for non-local folders.
pub fn destination_path(text: &str) -> Option<PathBuf> {
    if text.contains("://") {
        gio::File::for_uri(text).path()
    } else if text.starts_with('/') {
        Some(PathBuf::from(text))
    } else {
        None
    }
}

/// One exported `/DndExtract/<n>` object.
pub struct Exported {
    pub service: String,
    pub path: String,
    connection: gio::DBusConnection,
    id: RefCell<Option<gio::RegistrationId>>,
}

impl Exported {
    pub fn unregister(&self) {
        if let Some(id) = self.id.borrow_mut().take() {
            let _ = self.connection.unregister_object(id);
        }
    }
}

impl Drop for Exported {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Export a new extraction object; `extract(destination)` runs on the GTK thread
/// after the call has been answered.
pub fn export(connection: &gio::DBusConnection, extract: Rc<dyn Fn(Result<PathBuf, String>)>) -> Result<Rc<Exported>, String> {
    let service = connection.unique_name().ok_or("no unique D-Bus name")?.to_string();
    let n = COUNTER.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let path = format!("/DndExtract/{n}");
    let info = gio::DBusNodeInfo::for_xml(XML).map_err(|e| e.to_string())?;
    let interface = info.lookup_interface(INTERFACE).ok_or("no interface")?;
    let id = connection.register_object(&path, &interface)
        .method_call(move |_, _, _, _, method, parameters, invocation| {
            if method != "extractSelectedFilesTo" {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", "Unknown method");
                return;
            }
            let destination = parameters.child_value(0).str().unwrap_or_default().to_string();
            // Answer first: the target waits for the reply (Files: 5 s) before finishing the drop.
            invocation.return_value(None);
            let extract = extract.clone();
            glib::idle_add_local_once(move || {
                extract(destination_path(&destination).ok_or_else(|| destination.clone()))
            });
        }).build().map_err(|e| e.to_string())?;
    Ok(Rc::new(Exported { service, path, connection: connection.clone(), id: RefCell::new(Some(id)) }))
}

/// Produces the uri-list fallback: extract the drag to a staging folder, then report the top-level paths.
pub type Stager = Rc<dyn Fn(Box<dyn FnOnce(Result<Vec<PathBuf>, String>)>)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArkContent {
        pub service: RefCell<String>,
        pub path: RefCell<String>,
        pub stager: RefCell<Option<Stager>>,
        pub staged: RefCell<Option<Vec<PathBuf>>>,
        pub requests: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArkContent {
        const NAME: &'static str = "WayarchiveArkContent";
        type Type = super::ArkContent;
        type ParentType = gdk::ContentProvider;
    }

    impl ObjectImpl for ArkContent {}

    impl ContentProviderImpl for ArkContent {
        fn formats(&self) -> gdk::ContentFormats {
            let mut mimes = vec![SERVICE, OBJECT];
            if self.stager.borrow().is_some() {
                mimes.push(URI_LIST);
            }
            gdk::ContentFormats::new(&mimes)
        }

        fn write_mime_type_future(&self, mime_type: &str, stream: &gio::OutputStream, priority: glib::Priority)
                                  -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
            let stream = stream.clone();
            let obj = self.obj().clone();
            let mime = mime_type.to_string();
            Box::pin(async move {
                let imp = obj.imp();
                let bytes: Vec<u8> = match mime.as_str() {
                    SERVICE => imp.service.borrow().clone().into_bytes(),
                    OBJECT => imp.path.borrow().clone().into_bytes(),
                    URI_LIST => {
                        imp.requests.set(imp.requests.get() + 1);
                        let paths = match imp.staged.borrow().clone() {
                            Some(paths) => Ok(paths),
                            None => {
                                let stager = imp.stager.borrow().clone();
                                let Some(stager) = stager else {
                                    return Err(glib::Error::new(gio::IOErrorEnum::NotSupported, "no file list"));
                                };
                                let (sender, receiver) = futures_oneshot();
                                stager(Box::new(move |result| sender(result)));
                                receiver.await
                            }
                        };
                        match paths {
                            Ok(paths) => {
                                *imp.staged.borrow_mut() = Some(paths.clone());
                                uri_list(&paths).into_bytes()
                            }
                            Err(error) => return Err(glib::Error::new(gio::IOErrorEnum::Failed, &error)),
                        }
                    }
                    _ => return Err(glib::Error::new(gio::IOErrorEnum::NotSupported, "unsupported format")),
                };
                stream.write_all_future(bytes, priority).await.map_err(|(_, e)| e)?;
                stream.close_future(priority).await
            })
        }
    }
}

glib::wrapper! {
    pub struct ArkContent(ObjectSubclass<imp::ArkContent>) @extends gdk::ContentProvider;
}

impl ArkContent {
    pub fn new(service: &str, path: &str, stager: Option<Stager>) -> Self {
        let object: Self = glib::Object::new();
        *object.imp().service.borrow_mut() = service.to_string();
        *object.imp().path.borrow_mut() = path.to_string();
        *object.imp().stager.borrow_mut() = stager;
        object
    }

    /// How many times a target asked for the uri-list fallback.
    pub fn uri_list_requests(&self) -> u32 {
        self.imp().requests.get()
    }

    pub fn service(&self) -> String {
        self.imp().service.borrow().clone()
    }

    pub fn object_path(&self) -> String {
        self.imp().path.borrow().clone()
    }
}

pub fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| format!("{}\r\n", gio::File::for_path(p).uri())).collect()
}

/// A one-shot callback → future bridge (GTK thread only).
fn futures_oneshot<T: 'static>() -> (impl FnOnce(T), impl Future<Output = T>) {
    let (sender, receiver) = async_channel::bounded::<T>(1);
    (move |value: T| {
        let _ = sender.try_send(value);
    }, async move { receiver.recv().await.expect("staging result") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(destination_path("/home/u/Downloads"), Some(PathBuf::from("/home/u/Downloads")));
        assert_eq!(destination_path("file:///home/u/My%20Files"), Some(PathBuf::from("/home/u/My Files")));
        assert_eq!(destination_path("relative"), None);
    }

    #[test]
    fn uri_lists() {
        assert_eq!(uri_list(&[PathBuf::from("/a b/c")]), "file:///a%20b/c\r\n");
    }
}
