//! Minimal poppler-glib binding loaded with `dlopen("libpoppler-glib.so.8")`
//! (Python: `gi.require_version('Poppler', '0.18')`). No build-time link, so a
//! system without poppler-glib still runs Files; PDF previews then report
//! "Preview unavailable" exactly like the Python `ImportError` path.

use glib::translate::ToGlibPtr;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

type GError = glib::ffi::GError;

#[allow(clippy::type_complexity)]
struct Api {
    new_from_gfile: unsafe extern "C" fn(*mut gio::ffi::GFile, *const c_char, *mut gio::ffi::GCancellable, *mut *mut GError) -> *mut c_void,
    new_from_file: unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut GError) -> *mut c_void,
    n_pages: unsafe extern "C" fn(*mut c_void) -> c_int,
    get_page: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    title: unsafe extern "C" fn(*mut c_void) -> *mut c_char,
    author: unsafe extern "C" fn(*mut c_void) -> *mut c_char,
    page_size: unsafe extern "C" fn(*mut c_void, *mut c_double, *mut c_double),
    render: unsafe extern "C" fn(*mut c_void, *mut cairo_sys::cairo_t),
    render_for_printing: unsafe extern "C" fn(*mut c_void, *mut cairo_sys::cairo_t),
}

use gtk::cairo::ffi as cairo_sys;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let name = CString::new("libpoppler-glib.so.8").unwrap();
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            return None;
        }
        let symbol = |name: &str| -> Option<*mut c_void> {
            let name = CString::new(name).unwrap();
            let pointer = libc::dlsym(handle, name.as_ptr());
            (!pointer.is_null()).then_some(pointer)
        };
        Some(Api {
            new_from_gfile: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_new_from_gfile")?),
            new_from_file: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_new_from_file")?),
            n_pages: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_get_n_pages")?),
            get_page: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_get_page")?),
            title: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_get_title")?),
            author: std::mem::transmute::<*mut c_void, _>(symbol("poppler_document_get_author")?),
            page_size: std::mem::transmute::<*mut c_void, _>(symbol("poppler_page_get_size")?),
            render: std::mem::transmute::<*mut c_void, _>(symbol("poppler_page_render")?),
            render_for_printing: std::mem::transmute::<*mut c_void, _>(symbol("poppler_page_render_for_printing")?),
        })
    }).as_ref()
}

/// Whether poppler-glib could be loaded.
pub fn available() -> bool {
    api().is_some()
}

/// Why a document could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// poppler-glib is not installed (Python `ImportError`).
    Missing,
    Glib(glib::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Missing => f.write_str("Poppler is not installed"),
            OpenError::Glib(error) => f.write_str(error.message()),
        }
    }
}

/// An owned `PopplerDocument` (one thread at a time).
pub struct Document(*mut c_void);
// SAFETY: a PopplerDocument is used by a single thread at a time (moved into
// the worker that renders it, or kept on the main thread for printing).
unsafe impl Send for Document {}

/// An owned `PopplerPage`.
pub struct Page(*mut c_void);
unsafe impl Send for Page {}

fn take_error(error: *mut GError) -> OpenError {
    if error.is_null() {
        OpenError::Glib(glib::Error::new(gio::IOErrorEnum::Failed, "The document could not be opened"))
    } else {
        OpenError::Glib(unsafe { glib::translate::from_glib_full(error) })
    }
}

fn owned_text(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    unsafe { glib::ffi::g_free(pointer.cast()) };
    Some(text)
}

impl Document {
    /// `Poppler.Document.new_from_gfile(file, None, cancellable)`.
    pub fn from_gfile(file: &gio::File, cancellable: Option<&gio::Cancellable>) -> Result<Self, OpenError> {
        let api = api().ok_or(OpenError::Missing)?;
        let mut error = std::ptr::null_mut();
        let document = unsafe {
            (api.new_from_gfile)(file.to_glib_none().0, std::ptr::null(),
                                 cancellable.map(|c| c.to_glib_none().0).unwrap_or(std::ptr::null_mut()), &mut error)
        };
        if document.is_null() { Err(take_error(error)) } else { Ok(Self(document)) }
    }

    /// `Poppler.Document.new_from_file(uri, None)`.
    pub fn from_uri(uri: &str) -> Result<Self, OpenError> {
        let api = api().ok_or(OpenError::Missing)?;
        let uri = CString::new(uri).map_err(|_| OpenError::Glib(glib::Error::new(gio::IOErrorEnum::InvalidArgument, "Invalid URI")))?;
        let mut error = std::ptr::null_mut();
        let document = unsafe { (api.new_from_file)(uri.as_ptr(), std::ptr::null(), &mut error) };
        if document.is_null() { Err(take_error(error)) } else { Ok(Self(document)) }
    }

    pub fn n_pages(&self) -> i32 {
        unsafe { (api().unwrap().n_pages)(self.0) }
    }

    pub fn page(&self, index: i32) -> Option<Page> {
        let page = unsafe { (api().unwrap().get_page)(self.0, index) };
        (!page.is_null()).then_some(Page(page))
    }

    pub fn title(&self) -> Option<String> {
        owned_text(unsafe { (api().unwrap().title)(self.0) })
    }

    pub fn author(&self) -> Option<String> {
        owned_text(unsafe { (api().unwrap().author)(self.0) })
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        unsafe { glib::gobject_ffi::g_object_unref(self.0.cast()) };
    }
}

impl Page {
    /// Size in PostScript points.
    pub fn size(&self) -> (f64, f64) {
        let (mut width, mut height) = (0.0, 0.0);
        unsafe { (api().unwrap().page_size)(self.0, &mut width, &mut height) };
        (width, height)
    }

    pub fn render(&self, cr: &gtk::cairo::Context) {
        unsafe { (api().unwrap().render)(self.0, cr.to_raw_none()) };
    }

    pub fn render_for_printing(&self, cr: &gtk::cairo::Context) {
        unsafe { (api().unwrap().render_for_printing)(self.0, cr.to_raw_none()) };
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe { glib::gobject_ffi::g_object_unref(self.0.cast()) };
    }
}
