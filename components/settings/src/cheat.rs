//! `shortcuts_page.CheatSheet` from gs-system behind the shell's trait.
use std::rc::Rc;

#[cfg(feature = "pages")]
impl crate::app::CheatSheet for gs_system::shortcuts_page::CheatSheet {
    fn present(&self) { gs_system::shortcuts_page::CheatSheet::present(self) }
    fn close(&self) { gs_system::shortcuts_page::CheatSheet::close(self) }
    fn closed(&self) -> bool { gs_system::shortcuts_page::CheatSheet::closed(self) }
    fn window(&self) -> gtk::Window { gs_system::shortcuts_page::CheatSheet::window(self) }
}

#[cfg(feature = "pages")]
pub fn make(app: &gtk::Application) -> Rc<dyn crate::app::CheatSheet> {
    Rc::new(gs_system::shortcuts_page::CheatSheet::new(app))
}

/// Shell-only builds (tests with fake pages): a plain window stands in.
#[cfg(not(feature = "pages"))]
pub fn make(app: &gtk::Application) -> Rc<dyn crate::app::CheatSheet> {
    use gtk::prelude::*;
    struct Sheet(gtk::Window, Rc<std::cell::Cell<bool>>);
    impl crate::app::CheatSheet for Sheet {
        fn present(&self) { self.0.present() }
        fn close(&self) { self.0.close() }
        fn closed(&self) -> bool { self.1.get() }
        fn window(&self) -> gtk::Window { self.0.clone() }
    }
    let window = gtk::Window::builder().application(app).build();
    let closed = Rc::new(std::cell::Cell::new(false));
    let c = closed.clone();
    window.connect_close_request(move |_| { c.set(true); glib::Propagation::Proceed });
    Rc::new(Sheet(window, closed))
}
