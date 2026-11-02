//! `choose_image_for_monitor`'s chooser: the shared
//! `crate::wallpaper_chooser::WallpaperChooser` (large previews, nothing is
//! applied on selection). An open chooser is re-presented instead of
//! opening a second one (Python `self._wallpaper_chooser`).

use crate::wallpaper_chooser::WallpaperChooser;
use std::cell::RefCell;

pub type Chosen = Box<dyn FnOnce(Option<String>)>;

thread_local! {
    static CURRENT: RefCell<Option<WallpaperChooser>> = const { RefCell::new(None) };
}

/// Present the open chooser; returns false when none is open.
pub fn present_existing() -> bool {
    CURRENT.with(|current| current.borrow().as_ref().map(|c| c.present()).is_some())
}

pub fn choose_wallpaper(parent: Option<&gtk::Window>, chosen: Chosen) {
    if present_existing() {
        return;
    }
    let chooser = WallpaperChooser::new(parent, move |path| {
        CURRENT.with(|current| current.replace(None));
        chosen(path.map(|p| p.to_string_lossy().into_owned()));
    });
    CURRENT.with(|current| current.replace(Some(chooser.clone())));
    chooser.present();
}
