//! `admin_panel.py`: touch-friendly, borderless local control panel above the
//! scenic clock (WebKitGTK 6.0).
use std::cell::Cell;
use std::rc::{Rc, Weak};

use gtk::prelude::*;

use crate::keyboard::PanelKeyboard;

pub const URL: &str = "http://127.0.0.1:8765/?embedded=1";
/// The only origin allowed inside the panel (`(scheme, netloc)`).
pub const ORIGIN: (&str, &str) = ("http", "127.0.0.1:8765");
/// Local music service hosts that may open in the external browser on a tap.
pub const LOCAL_MUSIC: [&str; 3] = ["127.0.0.1:8095", "192.168.0.100:8095", "raspberrypi.local:8095"];

pub const CSS: &str = "
window.kasumi-panel { background: #10191d; color: #eef5f4; }
.kasumi-panel .panel-toolbar { background: #142025; padding: 10px 20px; border-bottom: 1px solid #40535a; }
.kasumi-panel .panel-title { font-size: 21px; font-weight: 600; }
.kasumi-panel .panel-close, .kasumi-panel .panel-retry { min-height: 48px; min-width: 110px;
  padding: 4px 18px; border-radius: 12px; font-size: 18px; color: #16392c; background: #b5e6cd; }
.kasumi-panel .panel-error { padding: 14px; font-size: 18px; }
.kasumi-panel .panel-keyboard-toggle { min-width: 48px; min-height: 48px; padding: 4px 12px; border-radius: 12px; }
";

/// `urllib.parse.urlsplit(uri)[:2]` → (scheme, netloc), or `None` where
/// Python would raise (unbalanced IPv6 brackets).
pub fn split_origin(uri: &str) -> Option<(String, String)> {
    // urlsplit strips leading C0/space and removes tab/CR/LF anywhere.
    let cleaned: String = uri.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let (scheme, rest) = match cleaned.find(':') {
        Some(index) if index > 0 && cleaned[..index].chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && cleaned[..index].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
            (cleaned[..index].to_ascii_lowercase(), &cleaned[index + 1..]),
        _ => (String::new(), cleaned.as_str()),
    };
    let netloc = match rest.strip_prefix("//") {
        Some(after) => after[..after.find(['/', '?', '#']).unwrap_or(after.len())].to_owned(),
        None => String::new(),
    };
    if netloc.contains('[') != netloc.contains(']') {
        return None;
    }
    Some((scheme, netloc))
}

/// What the panel does with a navigation / new-window request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    /// The local admin origin: WebKit's default handling.
    Allow,
    /// Ignored inside the panel.
    Ignore,
    /// Ignored inside the panel and opened in the desktop's default browser.
    External,
}

/// `AdminPanel.policy` for NAVIGATION_ACTION / NEW_WINDOW_ACTION decisions.
/// `origin` is [`ORIGIN`] in production (tests may use a fake server's port).
pub fn navigation(uri: &str, user_gesture: bool, origin: (&str, &str)) -> Navigation {
    let Some((scheme, netloc)) = split_origin(uri) else { return Navigation::Ignore };
    if scheme == origin.0 && netloc == origin.1 {
        return Navigation::Allow;
    }
    let local_music = scheme == "http" && LOCAL_MUSIC.contains(&netloc.as_str());
    // Account consent pages need a full browser for popups and OAuth
    // redirects. Only an explicit tap may open one.
    if user_gesture && (scheme == "https" || local_music) { Navigation::External } else { Navigation::Ignore }
}

pub struct Hooks {
    /// `app.window.reveal(); app.window.gear.grab_focus()`.
    pub dismissed: Box<dyn Fn()>,
    /// `app.suppress_escape_until_release`.
    pub suppress_escape: Rc<Cell<bool>>,
    /// Opens an allowed external URI (default: `gio::AppInfo::launch_default_for_uri`).
    pub launch: Box<dyn Fn(&str)>,
}

pub struct Inner {
    pub window: gtk::ApplicationWindow,
    pub close_button: gtk::Button,
    pub error: gtk::Box,
    pub web: Option<crate::webkit::WebView>,
    pub keyboard: PanelKeyboard,
    url: String,
    origin: (String, String),
    hooks: Hooks,
    /// Permission requests (media, geolocation, notifications, …) denied so far.
    denied: Option<Rc<Cell<u32>>>,
}

/// `AdminPanel(app)`.
#[derive(Clone)]
pub struct AdminPanel(pub Rc<Inner>);

fn up(weak: &Weak<Inner>) -> Option<AdminPanel> { weak.upgrade().map(AdminPanel) }

pub fn launch_default(uri: &str) {
    if gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>).is_err() {
        crate::log("WARNING", "Could not open music account sign-in browser");
    }
}

impl AdminPanel {
    /// `url` is [`URL`] in production; tests pass a fake server's address
    /// (the allowed origin is derived from it).
    pub fn new(app: &gtk::Application, clock: &gtk::Window, url: &str, load: bool, hooks: Hooks, keyboard_devices: Option<Vec<waykeys::devices::Device>>,
               watch_devices: bool) -> Self {
        let window = gtk::ApplicationWindow::builder().application(app).title("Kasumi Control").decorated(false)
            .transient_for(clock).modal(true).build();
        window.add_css_class("kasumi-panel");
        window.set_default_size(1280, 720);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&column));
        let toolbar = gtk::Box::builder().spacing(18).css_classes(["panel-toolbar"]).build();
        toolbar.append(&gtk::Label::builder().label("Kasumi control").xalign(0.0).hexpand(true).css_classes(["panel-title"]).build());
        let close_button = gtk::Button::builder().label("×  Close").css_classes(["panel-close"]).build();
        close_button.set_tooltip_text(Some("Return to the clock (Esc)"));
        toolbar.append(&close_button);
        column.append(&toolbar);
        let error = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).halign(gtk::Align::Center)
            .valign(gtk::Align::Center).vexpand(true).visible(false).css_classes(["panel-error"]).build();
        error.append(&gtk::Label::builder().label("The control panel is unavailable. Please try again.").wrap(true).build());
        let retry = gtk::Button::builder().label("Try again").css_classes(["panel-retry"]).build();
        error.append(&retry);
        column.append(&error);
        let denied = std::cell::RefCell::new(None);
        let web = match crate::webkit::WebView::new() {
            Ok(web) => {
                web.widget().set_hexpand(true);
                web.widget().set_vexpand(true);
                web.set_enable_developer_extras(false);
                denied.replace(Some(web.deny_permission_requests()));
                column.append(web.widget());
                Some(web)
            }
            Err(message) => {
                crate::log("ERROR", &format!("Could not open embedded control panel\n{message}"));
                error.set_visible(true);
                None
            }
        };
        let keyboard = PanelKeyboard::new(window.upcast_ref(), &column, keyboard_devices, None, watch_devices);
        toolbar.insert_child_after(keyboard.button(), toolbar.first_child().as_ref());
        let origin = split_origin(url).unwrap_or_default();
        let panel = Self(Rc::new(Inner { window: window.clone(), close_button: close_button.clone(), error, web: web.clone(),
                                         keyboard, url: url.to_owned(), origin, hooks,
                                         denied: denied.into_inner() }));
        let weak = Rc::downgrade(&panel.0);
        close_button.connect_clicked(move |_| if let Some(p) = up(&weak) { p.dismiss() });
        let weak = Rc::downgrade(&panel.0);
        retry.connect_clicked(move |_| if let Some(p) = up(&weak) { p.load() });
        if let Some(web) = &web {
            let weak = Rc::downgrade(&panel.0);
            web.connect_decide_policy(move |decision| match up(&weak) {
                Some(panel) => panel.policy(decision),
                None => false,
            });
            let weak = Rc::downgrade(&panel.0);
            web.connect_load_failed(move || if let Some(p) = up(&weak) { p.failed() });
            let weak = Rc::downgrade(&panel.0);
            web.connect_web_process_terminated(move || if let Some(p) = up(&weak) { p.failed() });
            web.suppress_context_menu();
        }
        if load {
            panel.load();
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&panel.0);
        keys.connect_key_pressed(move |_, key, _, _| match up(&weak) {
            Some(panel) if panel.key_press(key) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
        let weak = Rc::downgrade(&panel.0);
        window.connect_close_request(move |_| {
            if let Some(panel) = up(&weak) {
                panel.dismiss();
            }
            glib::Propagation::Stop
        });
        panel
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }
    pub fn web(&self) -> Option<&crate::webkit::WebView> { self.0.web.as_ref() }
    pub fn keyboard(&self) -> &PanelKeyboard { &self.0.keyboard }
    pub fn denied_permissions(&self) -> u32 { self.0.denied.as_ref().map_or(0, |d| d.get()) }
    pub fn close_button(&self) -> &gtk::Button { &self.0.close_button }
    pub fn error_box(&self) -> &gtk::Box { &self.0.error }
    pub fn get_visible(&self) -> bool { self.0.window.get_visible() }
    pub fn is_visible(&self) -> bool { self.0.window.get_visible() }
    pub fn is_active(&self) -> bool { self.0.window.is_active() }

    fn policy(&self, decision: &crate::webkit::Decision) -> bool {
        if decision.kind != crate::webkit::NAVIGATION_ACTION && decision.kind != crate::webkit::NEW_WINDOW_ACTION {
            return false;
        }
        let uri = decision.uri().unwrap_or_default();
        let origin = (self.0.origin.0.as_str(), self.0.origin.1.as_str());
        match navigation(&uri, decision.is_user_gesture(), origin) {
            Navigation::Allow => false,
            Navigation::Ignore => {
                decision.ignore();
                true
            }
            Navigation::External => {
                decision.ignore();
                (self.0.hooks.launch)(&uri);
                true
            }
        }
    }

    fn failed(&self) {
        if let Some(web) = &self.0.web {
            web.widget().set_visible(false);
        }
        self.0.error.set_visible(true);
    }

    pub fn load(&self) {
        if let Some(web) = &self.0.web {
            self.0.error.set_visible(false);
            web.widget().set_visible(true);
            web.load_uri(&self.0.url);
        }
    }

    pub fn reveal(&self) {
        self.0.window.fullscreen();
        self.0.window.present();
        if let Some(web) = &self.0.web {
            web.widget().grab_focus();
        }
    }

    /// Escape: suppress the clock's Escape until release, then dismiss.
    pub fn key_press(&self, key: gtk::gdk::Key) -> bool {
        if key == gtk::gdk::Key::Escape {
            self.0.hooks.suppress_escape.set(true);
            self.dismiss();
            return true;
        }
        false
    }

    pub fn dismiss(&self) {
        self.0.window.set_visible(false);
        (self.0.hooks.dismissed)();
    }

    pub fn close_resources(&self) { self.0.keyboard.close(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_policy_matches_admin_panel_py() {
        use Navigation::*;
        let o = ORIGIN;
        for (uri, gesture, expected) in [
            ("http://127.0.0.1:8765/?embedded=1", false, Allow),
            ("http://127.0.0.1:8765/settings#x", false, Allow),
            ("HTTP://127.0.0.1:8765/", false, Allow),
            ("http://127.0.0.1:8765", false, Allow),
            ("http://localhost:8765/", true, Ignore),
            ("http://127.0.0.1:8766/", true, Ignore),
            ("http://user@127.0.0.1:8765/", true, Ignore),
            ("https://127.0.0.1:8765/", false, Ignore),
            ("https://accounts.example.test/oauth", false, Ignore),
            ("https://accounts.example.test/oauth", true, External),
            ("http://127.0.0.1:8095/login", true, External),
            ("http://192.168.0.100:8095/", true, External),
            ("http://raspberrypi.local:8095/", true, External),
            ("http://raspberrypi.local:8095/", false, Ignore),
            ("http://example.test/", true, Ignore),
            ("file:///etc/passwd", true, Ignore),
            ("data:text/html,hi", true, Ignore),
            ("javascript:alert(1)", true, Ignore),
            ("about:blank", true, Ignore),
            ("spotify:track:1", true, Ignore),
            ("http://[::1:8765/", true, Ignore),
            ("", true, Ignore),
            // Origin confusion: userinfo, fragments, suffixes, ports, IPv6, dots, encodings, other schemes.
            ("http://127.0.0.1:8765@evil.test/", false, Ignore),
            ("http://127.0.0.1:8765@evil.test/", true, Ignore),
            ("http://evil.test#@127.0.0.1:8765/", true, Ignore),
            ("http://evil.test/?@127.0.0.1:8765", true, Ignore),
            ("http://127.0.0.1:8765.evil.test/", true, Ignore),
            ("http://127.0.0.1:87650/", true, Ignore),
            ("http://127.0.0.1:08765/", true, Ignore),
            ("http://127.0.0.1:8765\\@evil.test/", true, Ignore),
            ("http://127.0.0.1.:8765/", true, Ignore),
            ("http://127.0.0.1:%38765/", true, Ignore),
            ("http://[::1]:8765/", true, Ignore),
            ("http://[::ffff:127.0.0.1]:8765/", true, Ignore),
            ("http://2130706433:8765/", true, Ignore),
            ("http:127.0.0.1:8765/", true, Ignore),
            ("http:///127.0.0.1:8765/", true, Ignore),
            ("blob:http://127.0.0.1:8765/uuid", true, Ignore),
            ("view-source:http://127.0.0.1:8765/", true, Ignore),
            ("ws://127.0.0.1:8765/", true, Ignore),
            ("ftp://127.0.0.1:8765/", true, Ignore),
            ("http://127.0.0.1:8095@evil.test/", true, Ignore),
            ("http://RaspberryPi.local:8095/", true, Ignore),
            ("http://127.0.0.1:8095.evil.test/", true, Ignore),
            ("file://127.0.0.1:8765/etc/passwd", true, Ignore),
        ] {
            assert_eq!(navigation(uri, gesture, o), expected, "{uri} gesture={gesture}");
        }
        assert_eq!(split_origin(" \thttp://127.0.0.1:87\n65/x"), Some(("http".into(), "127.0.0.1:8765".into())));
        assert_eq!(split_origin("http://127.0.0.1:43123/?embedded=1"), Some(("http".into(), "127.0.0.1:43123".into())));
    }
}
