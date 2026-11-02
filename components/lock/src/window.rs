//! lock_screen.py LockWindow: one secure surface per monitor.
//!
//! Hints are labels in an in-surface `Gtk.Fixed` (native tooltips would be
//! xdg popups, which cannot be lock surfaces). Keys are forwarded between
//! monitors with `EventControllerKey::forward`, never by copying text.
//! `cleanup()` (unrealize) never unlocks.
use crate::app::App;
use crate::status::{notification_summary, StatusRow};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_gtk::utils::image;
use tessel_locking::config::appearance;
use tessel_locking::dashboard::WallpaperDashboard;
use tessel_locking::info::WeatherView;
use tessel_locking::visuals::{family_css, long_date, preview_close_button, submit_icon, Avatar, LoginLayer, VoiceBand, Wallpaper};
use tessel_keyboard::lock::{LockHost, LockKeyboard};
use zeroize::Zeroizing;

/// The entry's text copied once, straight from GTK's buffer, into a zeroizing
/// String. `EditableExt::text()` would first make an un-zeroed GString copy.
pub fn secret_text(entry: &gtk::Entry) -> Zeroizing<String> {
    use glib::translate::ToGlibPtr;
    let editable: &gtk::Editable = entry.upcast_ref();
    // SAFETY: gtk_editable_get_text returns the buffer's own NUL-terminated
    // UTF-8 (transfer none), valid until the next edit; it is copied at once.
    let pointer = unsafe { gtk::ffi::gtk_editable_get_text(editable.to_glib_none().0) };
    let mut text = Zeroizing::new(String::new());
    if !pointer.is_null() {
        // SAFETY: non-null C string owned by the entry buffer (see above).
        let bytes = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_bytes();
        // Invalid UTF-8 cannot come from GTK; fail closed with an empty secret.
        if let Ok(value) = std::str::from_utf8(bytes) {
            text.reserve_exact(value.len());
            text.push_str(value);
        }
    }
    text
}

/// Never put the secret on the clipboard. GTK already refuses copy/cut while
/// the text is hidden, but not after the eye toggle revealed it; clipboard
/// managers (wlr-data-control) would then record it. Keybindings and the
/// context menu both emit these action signals on the GtkText delegate.
pub fn block_clipboard(entry: &gtk::Entry) {
    if let Some(text) = entry.delegate() {
        for signal in ["copy-clipboard", "cut-clipboard"] {
            text.connect_local(signal, false, move |values| {
                if let Some(object) = values.first().and_then(|v| v.get::<glib::Object>().ok()) {
                    object.stop_signal_emission_by_name(signal);
                }
                None
            });
        }
    }
}

/// Compact outputs also shrink the dashboard; family_css scales the clock.
const COMPACT_STYLE: &str = "
.{scope}.lock-compact .lock-forecast-day { font-size: 13.5pt; }
.{scope}.lock-compact .lock-forecast-high { font-size: 16.5pt; }
.{scope}.lock-compact .lock-information .lock-caption { font-size: 12pt; }
.{scope}.lock-compact .lock-temperature { font-size: 30pt; }
";

static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn accessible_label(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// `action_button(icon, title, toggle=...)`.
fn action_button<B: IsA<gtk::Button> + IsA<gtk::Widget> + IsA<gtk::Accessible>>(button: B, icon: &str, title: &str, visible: bool) -> B {
    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
    button.set_visible(visible);
    button.set_child(Some(&image(icon, 20)));
    button.add_css_class("lock-action");
    button.add_css_class("family-action");
    button.set_size_request(44, 44);
    let title = tr!(title);
    button.set_tooltip_text(Some(&title));
    // Native GTK tooltips create an xdg popup, which cannot be used as a lock
    // surface. LockWindow draws this text in its own secure surface instead.
    button.set_has_tooltip(false);
    accessible_label(&button, &title);
    button
}

/// `account_display_name`: the GECOS full name, else the login name.
pub fn account_display_name(gecos: Option<&str>, login: &str) -> String {
    let full = gecos.unwrap_or("").split(',').next().unwrap_or("").trim().to_string();
    if full.is_empty() { login.to_string() } else { full }
}

fn current_account() -> (String, Option<String>) {
    // SAFETY: getpwuid on our uid; strings copied immediately.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return (String::new(), None);
        }
        let text = |p: *const libc::c_char| (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned());
        (text((*entry).pw_name).unwrap_or_default(), text((*entry).pw_gecos))
    }
}

pub struct LockWindow {
    pub window: gtk::Window,
    pub monitor: gdk::Monitor,
    owner: Weak<App>,
    me: Weak<LockWindow>,
    pub closed: Cell<bool>,
    provider: RefCell<Option<gtk::CssProvider>>,
    pub signature: RefCell<String>,
    hint_timer: RefCell<Option<glib::SourceId>>,
    hint_button: RefCell<Option<gtk::Widget>>,
    pointer_position: Cell<Option<(f64, f64)>>,
    pub action_sizes: gtk::SizeGroup,
    pub scope: String,
    pub root: gtk::Overlay,
    pub background: Wallpaper,
    pub wallpaper_content: gtk::Overlay,
    pub login_content: gtk::Overlay,
    pub login_layer: LoginLayer,
    pub layout: gtk::Box,
    pub clock: gtk::Label,
    pub date: gtk::Label,
    pub weather: WeatherView,
    pub panel: gtk::Box,
    pub avatar: Avatar,
    pub user: gtk::Label,
    pub prompt: gtk::Label,
    pub password_field: gtk::Overlay,
    pub entry: gtk::Entry,
    pub show_password: gtk::ToggleButton,
    pub unlock_button: gtk::Button,
    pub caps: gtk::Label,
    pub fingerprint_hint: gtk::Label,
    pub status: gtk::Label,
    pub actions: gtk::Box,
    pub sleep: gtk::Button,
    pub switch_user: gtk::Button,
    pub media_row: gtk::Box,
    pub media_icon: gtk::Image,
    pub media_title: gtk::Label,
    pub media_subtitle: gtk::Label,
    pub media_button: gtk::Button,
    pub assistant: gtk::Label,
    pub status_row: StatusRow,
    pub information_layout: WallpaperDashboard,
    pub voice: VoiceBand,
    pub touch_keyboard: RefCell<Option<LockKeyboard>>,
    pub preview_close: Option<gtk::Button>,
    pub hint_area: gtk::Fixed,
    pub hint: gtk::Label,
    pub scroll: gtk::ScrolledWindow,
}

impl LockWindow {
    pub fn new(app: &Rc<App>, monitor: &gdk::Monitor) -> Rc<Self> {
        let window = gtk::Window::builder().application(&app.application).title(tr!("Screen Locked")).decorated(false).build();
        let scope = format!("tessel-dock-companion-{}", SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        window.add_css_class(&scope);
        window.add_css_class("tessel-lock");
        let root = gtk::Overlay::new();
        window.set_child(Some(&root));
        let weak_app = Rc::downgrade(app);
        let this = Rc::new_cyclic(|me: &Weak<LockWindow>| {
            let refresh = { let me = me.clone(); move || if let Some(w) = me.upgrade() { w.refresh() } };
            let background = Wallpaper::new(&monitor.connector().map(|c| c.to_string()).unwrap_or_default(), refresh);
            root.set_child(Some(&background));
            let wallpaper_content = gtk::Overlay::builder().hexpand(true).vexpand(true).visible(false).build();
            root.add_overlay(&wallpaper_content);
            let information_scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
            information_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            wallpaper_content.set_child(Some(&information_scroll));
            let login_content = gtk::Overlay::builder().hexpand(true).vexpand(true).build();
            let settled = { let me = me.clone(); move |visible| if let Some(w) = me.upgrade() { w.login_settled(visible) } };
            let login_layer = LoginLayer::new(&background, &login_content, settled);
            root.add_overlay(&login_layer);
            let layout = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).halign(gtk::Align::Center)
                .valign(gtk::Align::Center).margin_top(22).margin_bottom(22).margin_start(20).margin_end(20).build();
            let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_child(Some(&layout));
            login_content.set_child(Some(&scroll));
            let clock = gtk::Label::builder().css_classes(["lock-clock", "family-clock"]).build();
            let date = gtk::Label::builder().css_classes(["lock-date", "family-date"]).build();
            let weather = WeatherView::new();
            let panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).width_request(420).margin_top(20)
                .halign(gtk::Align::Center).build();
            panel.add_css_class("lock-panel");
            let (login, gecos) = current_account();
            let display_name = account_display_name(gecos.as_deref(), &login);
            let avatar = Avatar::new();
            avatar.set_user(&display_name, Some(&login));
            avatar.widget().set_margin_bottom(8);
            panel.append(avatar.widget());
            let user = gtk::Label::builder().label(&display_name).wrap(true).max_width_chars(24).justify(gtk::Justification::Center).build();
            user.add_css_class("lock-user");
            user.add_css_class("family-user");
            user.set_margin_bottom(22);
            panel.append(&user);
            // Only additional PAM instructions (such as a second factor) need a
            // visible prompt. The ordinary password field keeps an accessible name.
            let prompt = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).visible(false).build();
            panel.append(&prompt);
            let password_field = gtk::Overlay::builder().hexpand(true).build();
            let entry = gtk::Entry::builder().visibility(false).hexpand(true).max_length(1024)
                .input_purpose(gtk::InputPurpose::Password).enable_undo(false).build();
            entry.add_css_class("lock-password");
            entry.add_css_class("family-password");
            accessible_label(&entry, &tr!("Password"));
            password_field.set_child(Some(&entry));
            let show_password = action_button(gtk::ToggleButton::new(), "view-reveal-symbolic", "Show password", true);
            show_password.add_css_class("lock-password-eye");
            show_password.add_css_class("family-eye");
            let eye_slot = gtk::Box::builder().halign(gtk::Align::End).valign(gtk::Align::Center).margin_end(6).build();
            eye_slot.append(&show_password);
            password_field.add_overlay(&eye_slot);
            let unlock_button = action_button(gtk::Button::new(), "go-next-symbolic", "Unlock", true);
            unlock_button.set_child(Some(&submit_icon()));
            unlock_button.add_css_class("suggested-action");
            panel.append(&password_field);
            let caps = gtk::Label::builder().label(tr!("Caps Lock is on")).xalign(0.0).visible(false).build();
            panel.append(&caps);
            let fingerprint_hint = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).visible(false).build();
            fingerprint_hint.add_css_class("lock-caption");
            panel.append(&fingerprint_hint);
            let status = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).build();
            status.add_css_class("lock-caption");
            panel.append(&status);
            let actions = gtk::Box::builder().spacing(16).halign(gtk::Align::Center).margin_top(6).build();
            actions.append(&unlock_button);
            let sleep = action_button(gtk::Button::new(), "weather-clear-night-symbolic", "Sleep", false);
            // Some Papirus versions render the transformed moon SVG as blank.
            let standard = gtk::IconTheme::new();
            standard.set_theme_name(Some("Adwaita"));
            if let Some(image) = sleep.child().and_downcast::<gtk::Image>() {
                image.set_paintable(Some(&standard.lookup_icon("weather-clear-night-symbolic", &[], 20, window.scale_factor(),
                                                               window.direction(), gtk::IconLookupFlags::FORCE_SYMBOLIC)));
            }
            actions.append(&sleep);
            let switch_user = action_button(gtk::Button::new(), "system-users-symbolic", "Switch User", false);
            actions.append(&switch_user);
            panel.append(&actions);
            layout.append(&panel);
            let media_row = gtk::Box::builder().spacing(18).halign(gtk::Align::Center).visible(false).build();
            media_row.add_css_class("lock-information");
            let media_icon = gtk::Image::builder().pixel_size(64).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
            media_row.append(&media_icon);
            let media_text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
            let media_title = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(40).xalign(0.0).build();
            media_title.add_css_class("lock-media-title");
            let media_subtitle = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(48).xalign(0.0).build();
            media_subtitle.add_css_class("lock-caption");
            media_text.append(&media_title);
            media_text.append(&media_subtitle);
            media_row.append(&media_text);
            let media_button = action_button(gtk::Button::new(), "media-playback-start-symbolic", "Play", true);
            media_button.add_css_class("lock-media-action");
            media_button.set_size_request(58, 58);
            if let Some(image) = media_button.child().and_downcast::<gtk::Image>() {
                image.set_pixel_size(28);
            }
            media_row.append(&media_button);
            let assistant = gtk::Label::builder().halign(gtk::Align::Center).build();
            assistant.add_css_class("lock-name");
            assistant.set_can_target(false);
            let status_row = StatusRow::new();
            let information_layout = WallpaperDashboard::new(&clock, &date, &weather, &media_row, &assistant,
                                                             Some(status_row.widget.upcast_ref()));
            information_scroll.set_child(Some(information_layout.widget()));
            let voice = VoiceBand::new();
            wallpaper_content.add_overlay(voice.widget());
            let preview_close = app.preview.then(|| {
                let app = weak_app.clone();
                let button = preview_close_button(move || if let Some(app) = app.upgrade() { app.quit() });
                root.add_overlay(&button);
                button
            });
            let hint_area = gtk::Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
            let hint = gtk::Label::builder().visible(false).build();
            hint.add_css_class("lock-hint");
            hint_area.put(&hint, 0.0, 0.0);
            LockWindow {
                window: window.clone(), monitor: monitor.clone(), owner: weak_app.clone(), me: me.clone(), closed: false.into(),
                provider: RefCell::new(None), signature: RefCell::new(String::new()), hint_timer: RefCell::new(None),
                hint_button: RefCell::new(None), pointer_position: Cell::new(None),
                action_sizes: gtk::SizeGroup::new(gtk::SizeGroupMode::Both), scope: scope.clone(), root: root.clone(), background,
                wallpaper_content, login_content, login_layer, layout, clock, date, weather, panel, avatar, user, prompt,
                password_field, entry, show_password, unlock_button, caps, fingerprint_hint, status, actions, sleep, switch_user,
                media_row, media_icon, media_title, media_subtitle, media_button, assistant, status_row, information_layout, voice,
                touch_keyboard: RefCell::new(None), preview_close, hint_area, hint, scroll,
            }
        });
        this.connect(app);
        this
    }

    fn owner(&self) -> Rc<App> { self.owner.upgrade().expect("lock application outlives its windows") }
    fn me(&self) -> Rc<LockWindow> { self.me.upgrade().expect("window alive") }

    fn connect(self: &Rc<Self>, app: &Rc<App>) {
        self.window.connect_close_request(|_| glib::Propagation::Stop);
        block_clipboard(&self.entry);
        let weak = Rc::downgrade(self);
        self.window.connect_unrealize(move |_| if let Some(w) = weak.upgrade() { w.cleanup() });
        let weak = Rc::downgrade(self);
        let keys = tessel_gtk::utils::key_controller(&self.window, true, move |controller, key, _, _| match weak.upgrade() {
            Some(w) if w.key(Some(controller), key) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        // Each forwarded key carries its modifier state. Forwarding the
        // modifiers signal too can deliver that same key event twice.
        let weak = Rc::downgrade(self);
        keys.connect_key_released(move |controller, _, _, _| if let Some(w) = weak.upgrade() { w.forward_input(Some(controller)); });
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| if let Some(w) = weak.upgrade() { w.submit() });
        let entry_click = gtk::GestureClick::builder().button(1).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        entry_click.connect_pressed(move |_, _, _, _| if let Some(w) = weak.upgrade() { w.focus_password() });
        self.entry.add_controller(entry_click);
        let weak = Rc::downgrade(self);
        self.show_password.connect_toggled(move |button| if let Some(w) = weak.upgrade() { w.password_visibility(button) });
        let weak = Rc::downgrade(self);
        self.unlock_button.connect_clicked(move |_| if let Some(w) = weak.upgrade() { w.submit() });
        let weak = Rc::downgrade(app);
        self.sleep.connect_clicked(move |_| if let Some(app) = weak.upgrade() { app.sleep() });
        let weak = Rc::downgrade(app);
        self.switch_user.connect_clicked(move |_| if let Some(app) = weak.upgrade() { app.switch_user() });
        let weak = Rc::downgrade(self);
        self.media_button.connect_clicked(move |_| if let Some(w) = weak.upgrade() { w.toggle_media() });
        // Touch keyboard (osk_lock.LockKeyboard): edits this entry directly.
        let (w1, w2, w3, w4) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(app), Rc::downgrade(self));
        let entry = self.entry.clone();
        let keyboard = LockKeyboard::new(LockHost {
            entry: Rc::new(move || entry.clone()),
            submit: Rc::new(move || if let Some(w) = w1.upgrade() { w.submit() }),
            is_visible: Rc::new(move || w3.upgrade().is_some_and(|app| app.login_visible.get())),
            focus: Rc::new(move || if let Some(w) = w2.upgrade() {
                let app = w.owner();
                app.activity(&w);
                *app.input_window.borrow_mut() = Some(Rc::downgrade(&w));
                w.entry.grab_focus_without_selecting();
            }),
            overlay: self.login_content.clone(), scroll: self.scroll.clone().upcast(), monitor: Some(self.monitor.clone()),
            button_box: self.actions.clone(), size_group: Some(self.action_sizes.clone()), button_class: "lock-action".into(),
            scale_factor: self.window.scale_factor(), direction: self.window.direction(), helper: None, store: None,
            watch_devices: true,
        });
        keyboard.button().add_css_class("family-action");
        let _ = w4;
        *self.touch_keyboard.borrow_mut() = Some(keyboard);
        if app.preview {
            self.status.set_text(&tr!("Preview — your session is not locked."));
            self.entry.set_sensitive(false);
            self.show_password.set_sensitive(false);
            self.unlock_button.set_sensitive(false);
        }
        self.root.add_overlay(&self.hint_area);
        let keyboard_button = self.touch_keyboard.borrow().as_ref().map(|k| k.button().clone().upcast::<gtk::Widget>());
        let buttons: Vec<gtk::Widget> = [Some(self.show_password.clone().upcast()), Some(self.unlock_button.clone().upcast()),
            Some(self.sleep.clone().upcast()), Some(self.switch_user.clone().upcast()), Some(self.media_button.clone().upcast()),
            keyboard_button].into_iter().flatten().collect();
        let media: gtk::Widget = self.media_button.clone().upcast();
        for button in buttons {
            // Match both dimensions to the largest action, including the
            // password toggle, regardless of theme or icon requisitions.
            if button != media {
                self.action_sizes.add_widget(&button);
            }
            self.bind_hint(&button);
        }
        // Typed controllers on the root (stays targetable when the login slides away).
        let pointer = gtk::EventControllerMotion::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        pointer.connect_motion(move |_, x, y| if let Some(w) = weak.upgrade() { w.pointer_activity(x, y) });
        let weak = Rc::downgrade(self);
        pointer.connect_enter(move |_, x, y| if let Some(w) = weak.upgrade() { w.pointer_enter(x, y) });
        self.root.add_controller(pointer);
        let click = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| if let Some(w) = weak.upgrade() { w.pointer_pressed(gesture, x, y) });
        self.root.add_controller(click);
        let scroll_events = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll_events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        scroll_events.connect_scroll(move |_, _, _| match weak.upgrade() {
            Some(w) if w.pointer_scroll() => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        self.root.add_controller(scroll_events);
        app.palette.add(self);
        self.tick();
        self.login_layer.reveal(app.login_visible.get(), false);
    }

    pub fn keyboard(&self) -> Option<LockKeyboard> { self.touch_keyboard.borrow().clone() }

    fn bind_hint(self: &Rc<Self>, button: &gtk::Widget) {
        let hover = gtk::EventControllerMotion::new();
        let (weak, b) = (Rc::downgrade(self), button.clone());
        hover.connect_enter(move |_, _, _| if let Some(w) = weak.upgrade() { w.request_hint(&b) });
        let (weak, b) = (Rc::downgrade(self), button.clone());
        hover.connect_leave(move |_| if let Some(w) = weak.upgrade() { w.hide_hint(Some(&b)) });
        button.add_controller(hover);
        let focus = gtk::EventControllerFocus::new();
        let (weak, b) = (Rc::downgrade(self), button.clone());
        focus.connect_enter(move |_| if let Some(w) = weak.upgrade() { w.request_hint(&b) });
        let (weak, b) = (Rc::downgrade(self), button.clone());
        focus.connect_leave(move |_| if let Some(w) = weak.upgrade() { w.hide_hint(Some(&b)) });
        button.add_controller(focus);
    }

    pub fn request_hint(self: &Rc<Self>, button: &gtk::Widget) {
        self.hide_hint(None);
        if self.closed.get() || !self.hint_available(button) {
            return;
        }
        *self.hint_button.borrow_mut() = Some(button.clone());
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            if let Some(w) = weak.upgrade() {
                w.hint_timer.borrow_mut().take();
                w.show_hint();
            }
        });
        *self.hint_timer.borrow_mut() = Some(id);
    }

    fn show_hint(&self) {
        let button = self.hint_button.borrow().clone();
        let Some(button) = button.filter(|b| !self.closed.get() && self.hint_available(b)) else { return };
        let Some(bounds) = button.compute_bounds(&self.hint_area) else { return };
        self.hint.set_text(&button.tooltip_text().map(|t| t.to_string()).unwrap_or_default());
        self.hint.set_visible(true);
        let (_, width, _, _) = self.hint.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = self.hint.measure(gtk::Orientation::Vertical, width);
        let x = (bounds.x() as f64 + (bounds.width() as f64 - width as f64) / 2.0)
            .min((self.window.width() - width - 8) as f64).max(8.0);
        let mut y = (bounds.y() + bounds.height() + 8.0) as f64;
        if y + height as f64 > (self.window.height() - 8) as f64 {
            y = (bounds.y() - height as f32 - 8.0) as f64;
        }
        self.hint_area.move_(&self.hint, x, y);
    }

    fn hint_available(&self, button: &gtk::Widget) -> bool {
        let app = self.owner();
        if button == self.media_button.upcast_ref::<gtk::Widget>() {
            return !app.login_visible.get() && self.wallpaper_content.get_visible() && self.media_row.get_visible();
        }
        app.login_visible.get()
    }

    pub fn hide_hint(&self, button: Option<&gtk::Widget>) {
        if let Some(button) = button {
            if self.hint_button.borrow().as_ref() != Some(button) {
                return;
            }
        }
        if let Some(source) = self.hint_timer.borrow_mut().take() {
            source.remove();
        }
        self.hint_button.borrow_mut().take();
        self.hint.set_visible(false);
    }

    fn wallpaper_control_at(&self, x: f64, y: f64) -> bool {
        let app = self.owner();
        let mut buttons: Vec<gtk::Widget> = self.preview_close.iter().map(|b| b.clone().upcast()).collect();
        if !app.login_visible.get() && self.wallpaper_content.get_visible() && self.media_row.get_visible() {
            buttons.push(self.media_button.clone().upcast());
        }
        buttons.iter().filter(|b| b.is_mapped()).any(|button| {
            button.compute_bounds(&self.root).is_some_and(|b| {
                let (bx, by, bw, bh) = (b.x() as f64, b.y() as f64, b.width() as f64, b.height() as f64);
                bx <= x && x < bx + bw && by <= y && y < by + bh
            })
        })
    }

    fn pointer_pressed(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        if self.wallpaper_control_at(x, y) {
            return;
        }
        let app = self.owner();
        let hidden = !app.login_visible.get();
        app.activity(&self.me());
        if hidden {
            // A mouse/touch wake cannot also activate an action sliding in.
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    }

    fn pointer_scroll(&self) -> bool {
        let app = self.owner();
        let hidden = !app.login_visible.get();
        app.activity(&self.me());
        hidden
    }

    pub fn pointer_activity(&self, x: f64, y: f64) {
        if self.pointer_position.get() == Some((x, y)) {
            return;
        }
        // GTK may synthesize crossing events when the sliding layer changes
        // its hit target. A stationary pointer must not wake it immediately.
        self.pointer_position.set(Some((x, y)));
        if self.wallpaper_control_at(x, y) {
            return;
        }
        self.owner().activity(&self.me());
    }

    pub fn reveal_login(&self, visible: bool) {
        self.hide_hint(None);
        self.wallpaper_content.set_visible(false);
        if !visible {
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
        self.login_layer.reveal(visible, true);
        if let Some(keyboard) = self.keyboard() {
            keyboard.reveal(visible);
        }
    }

    fn pointer_enter(&self, x: f64, y: f64) {
        // Mapping the initial lock surface generates an enter even when the
        // mouse is stationary. Record it without opening the login dropdown.
        if self.pointer_position.get().is_none() {
            self.pointer_position.set(Some((x, y)));
            return;
        }
        self.pointer_activity(x, y);
    }

    fn focus_password(&self) {
        let app = self.owner();
        if app.login_visible.get() && self.entry.is_sensitive() {
            let me = self.me();
            app.activity(&me);
            *app.input_window.borrow_mut() = Some(Rc::downgrade(&me));
            self.entry.grab_focus();
        }
    }

    fn login_settled(&self, visible: bool) {
        if !self.closed.get() {
            self.wallpaper_content.set_visible(!visible && !self.owner().login_visible.get());
        }
    }

    pub fn refresh(&self) {
        if self.closed.get() {
            return;
        }
        let app = self.owner();
        let config = app.config.borrow().clone();
        let palette = app.palette.palette();
        let map = palette.as_ref().map(crate::palette::palette_map);
        let (transparency, blur) = appearance(&config, map.as_ref());
        self.background.configure(&config, blur);
        let is_dark = palette.as_ref().map_or(true, |p| p.is_dark);
        let (r, g, b) = self.background.palette_color();
        let entry = tessel_gtk::palette::color_palette(is_dark, Some([r, g, b]));
        let transparency = transparency.clamp(0, 100);
        let monitor = tessel_gtk::theme::MonitorPalette { name: String::new(), geometry: [0; 4], background: entry.background,
                                                         foreground: entry.foreground, accent: entry.accent };
        let options = tessel_gtk::theme::ThemeOptions { transparency: transparency as u8, frosted: false, ..Default::default() };
        let mut data = tessel_gtk::theme::theme_css(&self.scope, &monitor, is_dark, &options).unwrap_or_default();
        let accent = format!("rgba({},{},{},{:.3})", entry.accent[0], entry.accent[1], entry.accent[2], (100 - transparency) as f64 / 100.0);
        let scope = &self.scope;
        data += &format!("window.{scope} {{ background: #161820; border-radius: 0; }}\
.{scope} .lock-panel {{ background: transparent; border: none; box-shadow: none; }}\
.{scope} .lock-panel > label, .{scope} .lock-panel > image {{ color: white; text-shadow: 0 1px 5px rgba(0,0,0,.65); }}");
        data += &family_css(scope);
        data += &format!(".{scope} button.lock-action.suggested-action:hover {{ background-color: shade({accent},1.10); }}\
.{scope} button.lock-media-action {{ min-width: 56px; min-height: 56px; }}\
.{scope} .lock-hint {{ background: rgba(18,20,26,.96); color: white; padding: 7px 11px; border: 1px solid rgba(255,255,255,.22); border-radius: 8px; font-size: 10pt; box-shadow: 0 2px 8px rgba(0,0,0,.25); }}\
.{scope} .lock-clock, .{scope} .lock-date, .{scope} .lock-name {{ color: white; }}\
.{scope} .lock-information label, .{scope} .lock-information image {{ color: white; text-shadow: 0 2px 6px rgba(0,0,0,.9); }}");
        data += &COMPACT_STYLE.replace("{scope}", scope);
        if data != *self.signature.borrow() {
            let display = WidgetExt::display(&self.window);
            if let Some(old) = self.provider.borrow_mut().take() {
                tessel_gtk::utils::remove_css(&display, &old);
            }
            *self.provider.borrow_mut() = Some(tessel_gtk::utils::add_css(&data, Some(&display),
                                                                         gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 30));
            *self.signature.borrow_mut() = data;
        }
        let bg = entry.background;
        self.background.set_tint((bg[0] as f64 / 255.0, bg[1] as f64 / 255.0, bg[2] as f64 / 255.0,
                                  (100 - transparency) as f64 / 100.0 * 0.12));
        self.background.redraw();
        self.assistant.set_text(&config.assistant_name.to_uppercase());
        self.assistant.set_visible(app.has_assistant.get() && config.show_assistant_name);
        self.voice.widget().set_visible(app.has_assistant.get() && config.show_voice);
        self.update_information();
    }

    pub fn update_information(&self) {
        if self.closed.get() {
            return;
        }
        let app = self.owner();
        let config = app.config.borrow().clone();
        self.weather.update(&app.weather, config.show_weather, config.show_forecast);
        let hint = app.fingerprint_hint.borrow().clone();
        let fingerprint = if hint.is_empty() { String::new() } else { tr!(hint) };
        let battery = app.status.battery();
        let notifications = app.status.notifications();
        let summary = if config.show_notifications { notification_summary(Some(&notifications), config.notification_app_names) }
                      else { None };
        self.status_row.update(if config.show_battery { battery.as_ref() } else { None }, summary.as_deref(), &fingerprint);
        self.fingerprint_hint.set_text(&fingerprint);
        self.fingerprint_hint.set_visible(!fingerprint.is_empty());
        let media = if config.show_media { app.media.current() } else { None };
        self.media_icon.set_from_gicon(&app.media_icons.resolve(media.as_ref()));
        self.media_row.set_visible(media.is_some());
        if let Some(media) = media {
            self.media_title.set_text(&tr!(media.title.clone()));
            let state = tr!(if media.playing { "Playing" } else { "Paused" });
            let subtitle = if !media.error.is_empty() { tr!(media.error.clone()) } else {
                [media.artist.clone(), state].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
            };
            self.media_subtitle.set_text(&subtitle);
            let title = tr!(if media.playing { "Pause" } else { "Play" });
            self.media_button.set_tooltip_text(Some(&title));
            self.media_button.set_has_tooltip(false);
            accessible_label(&self.media_button, &title);
            if let Some(image) = self.media_button.child().and_downcast::<gtk::Image>() {
                image.set_icon_name(Some(if media.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
            }
            self.media_button.set_sensitive(media.controllable && !media.busy && !app.preview);
            if self.hint_button.borrow().as_ref() == Some(self.media_button.upcast_ref()) {
                self.hint.set_text(&title);
            }
        }
    }

    pub fn toggle_media(&self) {
        let app = self.owner();
        if app.locked.get() && !app.preview && !app.login_visible.get() && self.wallpaper_content.get_visible()
            && app.config.borrow().show_media {
            app.media.toggle();
        }
    }

    pub fn tick(&self) {
        let app = self.owner();
        let geometry = self.monitor.geometry();
        let width = if self.window.width() > 0 { self.window.width() } else { geometry.width() };
        let height = if self.window.height() > 0 { self.window.height() } else { geometry.height() };
        self.information_layout.configure(width, height);
        let toggle = |class: &str, on: bool| if on { self.window.add_css_class(class) } else { self.window.remove_css_class(class) };
        toggle("lock-narrow", width < 900);
        let compact = height < 900;
        // The shared clock scales down on the same outputs as the greeter's.
        toggle("family-compact", compact || width < 900);
        if compact != self.window.has_css_class("lock-compact") {
            toggle("lock-compact", compact);
            self.layout.set_spacing(if compact { 10 } else { 18 });
            self.panel.set_spacing(if compact { 8 } else { 12 });
            self.panel.set_margin_top(if compact { 8 } else { 20 });
        }
        if let Ok(now) = glib::DateTime::now_local() {
            self.clock.set_text(&app.clock_store.borrow_mut().load().format_time(&now));
            self.date.set_text(&long_date(&now));
        }
        let keyboard = WidgetExt::display(&self.window).default_seat().and_then(|seat| seat.keyboard());
        self.caps.set_visible(keyboard.is_some_and(|k| k.is_caps_locked()));
        self.background.refresh();
        self.sleep.set_visible(app.can_sleep.get());
        self.switch_user.set_visible(app.greeter.borrow().is_some());
    }

    pub fn submit(&self) {
        let app = self.owner();
        if app.preview || !self.entry.is_sensitive() {
            return;
        }
        let text = secret_text(&self.entry);
        self.entry.set_text("");
        self.show_password.set_active(false);
        app.authenticate(&self.me(), text);
    }

    fn password_visibility(&self, button: &gtk::ToggleButton) {
        let visible = button.is_active();
        self.entry.set_visibility(visible);
        if let Some(image) = button.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if visible { "view-conceal-symbolic" } else { "view-reveal-symbolic" }));
        }
        let title = if visible { tr!("Hide password") } else { tr!("Show password") };
        button.set_tooltip_text(Some(&title));
        button.set_has_tooltip(false);
        accessible_label(button, &title);
        if self.hint_button.borrow().as_ref() == Some(button.upcast_ref()) && self.hint.get_visible() {
            self.hint.set_text(&title);
        }
    }

    pub fn set_prompt(&self, message: &str) {
        let folded = message.trim().trim_end_matches(':').to_lowercase();
        let standard = folded.is_empty() || folded == "password" || folded == tr!("Password").to_lowercase();
        self.prompt.set_text(if standard { "" } else { message });
        self.prompt.set_visible(!standard);
        accessible_label(&self.entry, &if standard { tr!("Password") } else { message.to_string() });
    }

    pub fn forward_input(&self, controller: Option<&gtk::EventControllerKey>) -> bool {
        let app = self.owner();
        let target = app.input_window();
        let Some(target) = target else { return false };
        if app.forwarding_input.get() || !app.login_visible.get() || std::ptr::eq(Rc::as_ptr(&target), self)
            || target.closed.get() || !target.entry.is_sensitive() {
            return false;
        }
        let Some(controller) = controller else { return false };
        // Some compositors retain keyboard focus on the first lock surface
        // after a click on another monitor. Forward through GTK so the chosen
        // field keeps normal editing, compose/input-method and shortcuts.
        let focus = gtk::prelude::GtkWindowExt::focus(&target.window).or_else(|| target.entry.delegate().map(|d| d.upcast()));
        app.forwarding_input.set(true);
        if let Some(focus) = focus {
            controller.forward(&focus);
        }
        app.forwarding_input.set(false);
        true
    }

    /// The window's capture-phase key handler; true = handled.
    pub fn key(&self, controller: Option<&gtk::EventControllerKey>, key: gdk::Key) -> bool {
        let app = self.owner();
        if key == gdk::Key::Print {
            // Never open a permission dialog or reveal the login layer here.
            app.capture_visible_lock_screen();
            return true;
        }
        let hidden = !app.login_visible.get();
        app.activity(&self.me());
        if hidden {
            return true;
        }
        if key == gdk::Key::Escape {
            if app.preview {
                app.quit();
            } else {
                app.cancel_authentication();
                self.entry.set_text("");
                self.show_password.set_active(false);
            }
            return true;
        }
        self.forward_input(controller)
    }

    /// Unrealize / shutdown cleanup. Never unlocks.
    pub fn cleanup(&self) {
        if self.closed.get() {
            return;
        }
        let Some(app) = self.owner.upgrade() else { return };
        if app.auth_window().is_some_and(|w| std::ptr::eq(Rc::as_ptr(&w), self)) {
            app.cancel_authentication();
        }
        self.closed.set(true);
        if app.input_window().is_some_and(|w| std::ptr::eq(Rc::as_ptr(&w), self)) {
            app.input_window.borrow_mut().take();
        }
        self.hide_hint(None);
        self.entry.set_text("");
        if let Some(keyboard) = self.keyboard() {
            keyboard.close();
        }
        self.login_layer.close();
        self.background.close();
        self.voice.close();
        app.palette.remove(self);
        if let Some(provider) = self.provider.borrow_mut().take() {
            tessel_gtk::utils::remove_css(&WidgetExt::display(&self.window), &provider);
        }
        app.windows.borrow_mut().retain(|w| !std::ptr::eq(Rc::as_ptr(w), self));
    }
}
