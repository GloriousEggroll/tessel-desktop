//! The part of companion_theme.PaletteClient lock_screen.py uses: the
//! validated dock palette (`palette.palette`) and a 2 s poll that calls
//! `window.refresh()` on every registered window (Python `_poll`). The
//! directory monitor's 60 ms fast path is folded into the poll.
use crate::window::LockWindow;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_gtk::gtk::glib;
use tessel_gtk::theme::{palette_path, validate_palette, PaletteFile, MAX_PALETTE_BYTES};

#[derive(Default)]
struct Inner {
    path: PathBuf,
    palette: Option<PaletteFile>,
    payload: Option<Vec<u8>>,
    windows: Vec<Weak<LockWindow>>,
    timer: Option<glib::SourceId>,
}

#[derive(Clone)]
pub struct PaletteState(Rc<RefCell<Inner>>);

/// The validated palette as the Python dict keys `appearance()` reads.
pub fn palette_map(palette: &PaletteFile) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("is_dark".into(), palette.is_dark.into());
    map.insert("apps_transparency_enabled".into(), palette.apps_transparency_enabled.into());
    map.insert("apps_transparency".into(), palette.apps_transparency.into());
    map.insert("apps_frosted".into(), palette.apps_frosted.into());
    map.insert("apps_frost_strength".into(), palette.apps_frost_strength.into());
    map
}

impl PaletteState {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self(Rc::new(RefCell::new(Inner { path: path.unwrap_or_else(palette_path), ..Default::default() })))
    }

    pub fn palette(&self) -> Option<PaletteFile> { self.0.borrow().palette.clone() }

    pub fn reload(&self) {
        use std::io::Read;
        let path = self.0.borrow().path.clone();
        let mut data = Vec::new();
        let read = std::fs::File::open(&path).and_then(|f| f.take(MAX_PALETTE_BYTES as u64 + 1).read_to_end(&mut data));
        let mut inner = self.0.borrow_mut();
        if read.is_err() || data.len() > MAX_PALETTE_BYTES {
            inner.payload = None;
            inner.palette = None;
            return;
        }
        if inner.payload.as_deref() == Some(&data[..]) {
            return;
        }
        inner.palette = serde_json::from_slice::<Value>(&data).ok().and_then(|v| validate_palette(&v).ok());
        inner.payload = Some(data);
    }

    pub fn add(&self, window: &Rc<LockWindow>) {
        let first = {
            let mut inner = self.0.borrow_mut();
            inner.windows.retain(|w| w.strong_count() > 0);
            let first = inner.windows.is_empty();
            inner.windows.push(Rc::downgrade(window));
            first
        };
        if first {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_seconds_local(2, move || {
                let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                PaletteState(inner).poll()
            });
            if let Some(old) = self.0.borrow_mut().timer.replace(id) {
                old.remove();
            }
        }
        self.reload();
        window.refresh();
    }

    pub fn remove(&self, window: &LockWindow) {
        let empty = {
            let mut inner = self.0.borrow_mut();
            inner.windows.retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), window));
            inner.windows.is_empty()
        };
        if empty {
            self.close();
        }
    }

    fn poll(&self) -> glib::ControlFlow {
        let windows: Vec<Rc<LockWindow>> = self.0.borrow().windows.iter().filter_map(Weak::upgrade).collect();
        if windows.is_empty() {
            self.0.borrow_mut().timer = None;
            return glib::ControlFlow::Break;
        }
        self.reload();
        for window in windows {
            window.refresh();
        }
        glib::ControlFlow::Continue
    }

    pub fn close(&self) {
        let timer = self.0.borrow_mut().timer.take();
        if let Some(timer) = timer {
            timer.remove();
        }
    }
}
