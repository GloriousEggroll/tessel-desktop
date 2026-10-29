//! login_ui.py: Waylogin, a GTK4 Wayland greeter with the suite's lock-screen
//! visuals. Runs as the unprivileged `greetd` user; greetd owns PAM. Nothing
//! here authenticates: answers are relayed to greetd over `$GREETD_SOCK`.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Instant;

use gtk::gdk;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use suite_core::tr;
use suite_gtk::utils::{add_css, image, key_controller};
use suite_lock::config::LockConfig;
use suite_lock::login_model::{sessions, users, Appearance, Session, User};
use suite_lock::visuals::{family_css, long_date, preview_close_button, submit_icon, Avatar, LoginLayer, Wallpaper};
use waykeys::lock::{LockHost, LockKeyboard};
use zeroize::Zeroizing;

use crate::protocol::{Client, Completion};
use crate::transition::Fade;

pub const APPLICATION_ID: &str = "org.wayshell.Login";
pub const STYLE: &str = "
window.waylogin { background: #161820; color: white; }
.waylogin .caption { font-size: 10pt; text-shadow: 0 1px 3px #000; }
.waylogin entry { min-height: 34px; padding: 12px 18px; border-radius: 9999px; font-size: 13.5pt; }
.waylogin button:focus-visible, .waylogin entry:focus-within { outline: 2px solid @accent_color; outline-offset: 2px; }
.waylogin popover contents { background: #23252d; color: white; padding: 12px; }
.waylogin .confirmation { padding: 12px; border-radius: 12px; background: rgba(20,22,30,.85); }
.waylogin .session-fade { background: black; }
";
/// Accessibility menu styles; added above each window's palette provider.
pub const LARGE_TEXT: &str = "
.waylogin .clock { font-size: 120pt; }
.waylogin .date { font-size: 30pt; }
.waylogin .user-name, .waylogin dropdown.family-user-choice > button { font-size: 30pt; }
.waylogin .caption, .waylogin label { font-size: 15pt; }
.waylogin entry { font-size: 18pt; min-height: 44px; }
.waylogin dropdown button, .waylogin popover label { font-size: 15pt; }
";
pub const HIGH_CONTRAST: &str = "
.waylogin button, .waylogin entry, .waylogin dropdown button { background: black; color: white;
  border: 2px solid white; box-shadow: none; }
.waylogin button.suggested-action { background: white; color: black; }
.waylogin button:focus-visible, .waylogin entry:focus-within { outline: 3px solid #ffd800; outline-offset: 2px; }
.waylogin .clock, .waylogin .date, .waylogin .user-name, .waylogin .caption, .waylogin label { color: white;
  text-shadow: 0 0 4px black, 0 0 2px black; }
.waylogin popover contents { background: black; color: white; border: 2px solid white; }
.waylogin .confirmation { background: black; border: 2px solid white; }
";
pub const SCREEN_READER: &str = "orca";
const APPLICATION_PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_APPLICATION;

/// The greetd conversation as the UI sees it (`login_protocol.Client`, or the
/// tests' FakeClient).
pub trait Authenticator {
    fn state(&self) -> String;
    fn generation(&self) -> u64;
    fn begin(&self, username: &str, session: Option<Session>);
    fn respond(&self, text: Zeroizing<String>);
    fn cancel(&self);
    fn start_session(&self, generation: u64);
    fn close(&self);
}

impl Authenticator for Client {
    fn state(&self) -> String { Client::state(self).as_str().into() }
    fn generation(&self) -> u64 { Client::generation(self) }
    fn begin(&self, username: &str, session: Option<Session>) { Client::begin(self, username, session) }
    fn respond(&self, text: Zeroizing<String>) { Client::respond(self, text) }
    fn cancel(&self) { Client::cancel(self) }
    fn start_session(&self, generation: u64) { Client::start_session(self, generation) }
    fn close(&self) { Client::close(self) }
}

pub type Changed = Rc<dyn Fn(&str, &str)>;
pub type ClientFactory = Box<dyn Fn(&str, Changed) -> Rc<dyn Authenticator>>;

/// `Client(GREETD_SOCK, GLib.idle_add, changed)`: the worker's completions are
/// forwarded through an async channel and applied on the GLib main loop.
pub fn greetd_client(path: &str, changed: Changed) -> Rc<dyn Authenticator> {
    let (sender, receiver) = async_channel::unbounded::<Completion>();
    let client = Client::new(path, move |completion| { let _ = sender.send_blocking(completion); }, move |kind, text| changed(kind, text));
    let handler = client.clone();
    glib::spawn_future_local(async move {
        while let Ok(completion) = receiver.recv().await {
            handler.result(completion);
        }
    });
    Rc::new(client)
}

/// A running screen reader (`subprocess.Popen`).
pub trait Reader {
    fn running(&mut self) -> bool;
    fn terminate(&mut self);
}

struct ChildReader(std::process::Child);

impl Reader for ChildReader {
    fn running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
    fn terminate(&mut self) {
        // SAFETY: signal our own child.
        unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
        let _ = self.0.try_wait();
    }
}

pub type ReaderLauncher = Box<dyn Fn(&[&str]) -> Option<Box<dyn Reader>>>;

/// `subprocess.Popen([...], stdin/stdout/stderr=DEVNULL, close_fds=True, start_new_session=True)`.
fn launch_reader(argv: &[&str]) -> Option<Box<dyn Reader>> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(argv[0]);
    command.args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().ok().map(|child| Box::new(ChildReader(child)) as Box<dyn Reader>)
}

/// LoginApplication keyword seams.
#[derive(Default)]
pub struct Options {
    pub preview: bool,
    pub appearance: Option<Appearance>,
    pub sessions: Option<Vec<Session>>,
    pub users: Option<Vec<User>>,
    pub client_factory: Option<ClientFactory>,
    pub reader_launcher: Option<ReaderLauncher>,
    /// The OSK's udev touch/keyboard monitor (default on; tests turn it off).
    pub watch_devices: Option<bool>,
    /// Tests only: another GApplication id (the greeter always uses `org.wayshell.Login`).
    pub application_id: Option<String>,
}

/// One monitor's greeter window (`LoginWindow`).
pub struct LoginWindow {
    pub window: gtk::ApplicationWindow,
    pub monitor: gdk::Monitor,
    owner: Weak<AppInner>,
    provider: RefCell<Option<gtk::CssProvider>>,
    pub closed: Cell<bool>,
    point: Cell<Option<(f64, f64)>>,
    pub pending_power: RefCell<Option<String>>,
    pub action_sizes: gtk::SizeGroup,
    mapped_at: Cell<Instant>,
    pub scope: String,
    pub root: gtk::Overlay,
    pub background: Wallpaper,
    pub idle: gtk::Box,
    pub clock: gtk::Label,
    pub date: gtk::Label,
    pub form: gtk::Box,
    pub content: gtk::ScrolledWindow,
    pub login_content: gtk::Overlay,
    pub layer: LoginLayer,
    pub avatar: Avatar,
    pub user_choice: gtk::DropDown,
    pub username: gtk::Entry,
    pub session_choice: gtk::DropDown,
    pub prompt: gtk::Label,
    pub entry: gtk::Entry,
    pub eye: gtk::ToggleButton,
    pub caps: gtk::Label,
    pub status: gtk::Label,
    pub buttons: gtk::Box,
    pub enter: gtk::Button,
    pub sleep: gtk::Button,
    pub restart_button: gtk::Button,
    pub power_button: gtk::Button,
    pub accessibility: gtk::MenuButton,
    pub a11y_switches: RefCell<Vec<(String, gtk::Switch)>>,
    pub touch_keyboard: RefCell<Option<LockKeyboard>>,
    pub power_panel: gtk::Box,
    pub power_question: gtk::Label,
    pub power_confirm: gtk::Button,
    pub power_cancel: gtk::Button,
    pub fade_cover: gtk::Box,
    pub preview_close: Option<gtk::Button>,
}

fn action_button(button: &impl IsA<gtk::Widget>, icon: &str, title: &str) {
    let widget = button.as_ref();
    widget.set_halign(gtk::Align::Center);
    widget.set_valign(gtk::Align::Center);
    widget.set_size_request(44, 44);
    widget.add_css_class("action");
    widget.add_css_class("family-action");
    widget.set_tooltip_text(Some(title));
    widget.update_property(&[gtk::accessible::Property::Label(title)]);
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.set_child(Some(&image(icon, 20)));
    }
}

/// `action(icon, title, callback)`.
fn action(icon: &str, title: &str) -> gtk::Button {
    let button = gtk::Button::new();
    action_button(&button, icon, title);
    button
}

/// `session_factory(icon=True)`: session names; the closed pill also carries a desktop icon.
fn session_factory(icon: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if icon {
            row.append(&image("computer-symbolic", 16));
        }
        row.append(&gtk::Label::builder().xalign(0.0).build());
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let text = item.item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string()).unwrap_or_default();
        if let Some(label) = item.child().and_then(|c| c.last_child()).and_downcast::<gtk::Label>() {
            label.set_text(&text);
        }
    });
    factory
}

/// Copy an entry's text out of GTK exactly once into zeroizing memory: the
/// C copy from `gtk_editable_get_chars` is wiped before it is freed.
pub fn take_secret(entry: &gtk::Entry) -> Zeroizing<String> {
    use glib::translate::ToGlibPtr;
    let editable: &gtk::Editable = entry.upcast_ref();
    // SAFETY: gtk_editable_get_chars returns a newly allocated UTF-8 string we own.
    unsafe {
        let pointer = gtk::ffi::gtk_editable_get_chars(editable.to_glib_none().0, 0, -1);
        if pointer.is_null() {
            return Zeroizing::new(String::new());
        }
        let length = libc::strlen(pointer);
        let bytes = std::slice::from_raw_parts_mut(pointer as *mut u8, length);
        let mut secret = Zeroizing::new(String::with_capacity(length));
        secret.push_str(std::str::from_utf8(bytes).unwrap_or(""));
        zeroize::Zeroize::zeroize(bytes);
        glib::ffi::g_free(pointer.cast());
        secret
    }
}

/// colorsys.hsv_to_rgb
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (v, v, v);
    }
    let i = (h * 6.0).trunc();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// The per-window palette CSS of `palette_changed` (Python round = ties-to-even).
pub fn palette_css(scope: &str, color: (u8, u8, u8), transparency: i64) -> (String, (f64, f64, f64, f64)) {
    let (r, g, b) = (color.0 as f64 / 255.0, color.1 as f64 / 255.0, color.2 as f64 / 255.0);
    let (h, s, _) = suite_lock::palette::rgb_to_hsv(r, g, b);
    let (ar, ag, ab) = hsv_to_rgb(h, s.clamp(0.3, 0.65), 0.85);
    let accent = format!("rgb({}, {}, {})", (ar * 255.0).round_ties_even() as i64, (ag * 255.0).round_ties_even() as i64,
                         (ab * 255.0).round_ties_even() as i64);
    let tint = [color.0, color.1, color.2].map(|c| (c as f64 * 0.18 + 15.0).round_ties_even() as i64);
    let opacity = (100 - transparency) as f64 / 100.0;
    let opacity_text = suite_core::pyfmt::float_repr(opacity);
    let css = format!("
        .{scope} {{ --accent-color: {accent}; --accent-bg-color: {accent}; }}
        .{scope} button, .{scope} entry {{ color: white; background: rgba({}, {}, {}, {opacity_text});
          border: 1px solid rgba(255,255,255,.25); box-shadow: none; }}
        .{scope} button.suggested-action {{ background: {accent}; color: #111318; }}
        .{scope} entry:focus-within, .{scope} button:focus-visible {{ outline-color: {accent}; }}
        .{scope} selection {{ background: {accent}; color: #111318; }}
        .{scope} button:disabled {{ opacity: .45; }}
        ", tint[0], tint[1], tint[2]) + &family_css(scope);
    (css, (tint[0] as f64 / 255.0, tint[1] as f64 / 255.0, tint[2] as f64 / 255.0, opacity * 0.12))
}

/// `%H:%M` or `%I:%M %p` without the leading zero.
pub fn clock_text(now: &glib::DateTime, clock_24h: bool) -> String {
    let text = now.format(if clock_24h { "%H:%M" } else { "%I:%M %p" }).map(|s| s.to_string()).unwrap_or_default();
    if clock_24h { text } else { text.trim_start_matches('0').to_owned() }
}

impl LoginWindow {
    fn app(&self) -> Option<App> {
        self.owner.upgrade().map(App)
    }

    fn new(app: &App, monitor: &gdk::Monitor) -> Rc<LoginWindow> {
        static SCOPES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let inner = &app.0;
        let window = gtk::ApplicationWindow::builder().application(&inner.app).title("Sign In").decorated(false).build();
        window.add_css_class("waylogin");
        let scope = format!("login-{}", SCOPES.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        window.add_css_class(&scope);
        let root = gtk::Overlay::new();
        window.set_child(Some(&root));
        let background = Wallpaper::new(monitor.connector().as_deref().unwrap_or(""), || {});
        root.set_child(Some(&background));
        let idle = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16).halign(gtk::Align::Center)
            .valign(gtk::Align::Center).can_target(false).build();
        let clock = gtk::Label::builder().css_classes(["clock", "family-clock"]).build();
        let date = gtk::Label::builder().css_classes(["date", "family-date"]).build();
        idle.append(&clock);
        idle.append(&date);
        root.add_overlay(&idle);
        let form = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).halign(gtk::Align::Center)
            .valign(gtk::Align::Center).margin_start(24).margin_end(24).margin_top(24).margin_bottom(24).width_request(400).build();
        let content = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        content.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        content.set_child(Some(&form));
        let login_content = gtk::Overlay::new();
        login_content.set_child(Some(&content));
        let settled_slot: Rc<RefCell<Weak<LoginWindow>>> = Rc::default();
        let slot = settled_slot.clone();
        let layer = LoginLayer::new(&background, &login_content, move |visible| {
            if let Some(window) = slot.borrow().upgrade() {
                window.settled(visible);
            }
        });
        root.add_overlay(&layer);
        let avatar = Avatar::new();
        avatar.widget().set_margin_bottom(8);
        form.append(avatar.widget());
        // The account name is shown once: as the user selector itself.
        let mut names: Vec<String> = inner.users.iter().map(|u| u.name.clone()).collect();
        names.push(tr!("Other user…"));
        let user_choice = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
        user_choice.set_halign(gtk::Align::Center);
        user_choice.add_css_class("family-user-choice");
        user_choice.update_property(&[gtk::accessible::Property::Label(&tr!("User"))]);
        user_choice.set_tooltip_text(Some(&tr!("Choose user")));
        form.append(&user_choice);
        let username = gtk::Entry::builder().placeholder_text(tr!("Username")).visible(false).enable_undo(false).max_length(256).build();
        username.update_property(&[gtk::accessible::Property::Label(&tr!("Username"))]);
        form.append(&username);
        let session_names: Vec<String> = if inner.sessions.is_empty() {
            vec![tr!("No Wayland sessions installed")]
        } else {
            inner.sessions.iter().map(|s| s.name.clone()).collect()
        };
        let session_choice = gtk::DropDown::from_strings(&session_names.iter().map(String::as_str).collect::<Vec<_>>());
        session_choice.set_halign(gtk::Align::Center);
        session_choice.add_css_class("family-session");
        session_choice.set_factory(Some(&session_factory(true)));
        session_choice.set_list_factory(Some(&session_factory(false)));
        session_choice.set_selected(inner.sessions.iter().position(|s| s.ident == inner.appearance.default_session).unwrap_or(0) as u32);
        session_choice.update_property(&[gtk::accessible::Property::Label(&tr!("Desktop session"))]);
        session_choice.set_tooltip_text(Some(&tr!("Desktop session")));
        let prompt = gtk::Label::builder().wrap(true).max_width_chars(38).xalign(0.0).visible(false).build();
        form.append(&prompt);
        let field = gtk::Overlay::new();
        // GtkPasswordEntryBuffer: non-swappable memory, wiped when text is deleted.
        let entry = gtk::Entry::with_buffer(&gtk::PasswordEntryBuffer::new());
        entry.set_visibility(false);
        entry.set_enable_undo(false);
        entry.set_max_length(4096);
        entry.set_input_purpose(gtk::InputPurpose::Password);
        entry.update_property(&[gtk::accessible::Property::Label("Password")]);
        entry.add_css_class("family-password");
        field.set_child(Some(&entry));
        let eye = gtk::ToggleButton::new();
        action_button(&eye, "view-reveal-symbolic", "Show password");
        eye.add_css_class("eye");
        eye.add_css_class("family-eye");
        let eye_slot = gtk::Box::builder().halign(gtk::Align::End).valign(gtk::Align::Center).margin_end(6).build();
        eye_slot.append(&eye);
        field.add_overlay(&eye_slot);
        form.append(&field);
        // A small session pill under the password keeps one left edge.
        form.append(&session_choice);
        let caps = gtk::Label::builder().label("Caps Lock is on").visible(false).xalign(0.0).css_classes(["caption"]).build();
        form.append(&caps);
        let status = gtk::Label::builder().wrap(true).max_width_chars(40).css_classes(["caption"]).build();
        form.append(&status);
        let buttons = gtk::Box::builder().spacing(16).halign(gtk::Align::Center).margin_top(6).build();
        let enter = action("go-next-symbolic", "Sign in");
        enter.set_child(Some(&submit_icon()));
        enter.add_css_class("suggested-action");
        let sleep = action("weather-clear-night-symbolic", "Sleep");
        // Render the same reliable symbolic moon as the lock screen.
        let standard = gtk::IconTheme::new();
        standard.set_theme_name(Some("Adwaita"));
        if let Some(icon) = sleep.child().and_downcast::<gtk::Image>() {
            icon.set_paintable(Some(&standard.lookup_icon("weather-clear-night-symbolic", &[], 20, 1, gtk::TextDirection::Ltr,
                                                          gtk::IconLookupFlags::FORCE_SYMBOLIC)));
        }
        let restart_button = action("system-reboot-symbolic", "Restart");
        let power_button = action("system-shutdown-symbolic", "Power off");
        let accessibility = gtk::MenuButton::new();
        let action_sizes = gtk::SizeGroup::new(gtk::SizeGroupMode::Both);
        let power_panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).visible(false).margin_top(10)
            .css_classes(["confirmation"]).build();
        let power_question = gtk::Label::new(None);
        power_panel.append(&power_question);
        let power_actions = gtk::Box::builder().spacing(10).halign(gtk::Align::Center).build();
        let power_confirm = gtk::Button::builder().css_classes(["destructive-action"]).build();
        let power_cancel = gtk::Button::with_label("Cancel");
        power_actions.append(&power_cancel);
        power_actions.append(&power_confirm);
        power_panel.append(&power_actions);
        let fade_cover = gtk::Box::builder().can_target(false).css_classes(["session-fade"]).visible(false).build();
        let preview_close = inner.preview.get().then(|| {
            let weak = Rc::downgrade(inner);
            preview_close_button(move || if let Some(inner) = weak.upgrade() { inner.app.quit() })
        });
        let this = Rc::new(LoginWindow {
            window, monitor: monitor.clone(), owner: Rc::downgrade(inner), provider: RefCell::new(None), closed: Cell::new(false),
            point: Cell::new(None), pending_power: RefCell::new(None), action_sizes, mapped_at: Cell::new(Instant::now()), scope,
            root, background, idle, clock, date, form, content, login_content, layer, avatar, user_choice, username, session_choice,
            prompt, entry, eye, caps, status, buttons, enter, sleep, restart_button, power_button, accessibility,
            a11y_switches: RefCell::default(), touch_keyboard: RefCell::new(None), power_panel, power_question, power_confirm,
            power_cancel, fade_cover, preview_close,
        });
        *settled_slot.borrow_mut() = Rc::downgrade(&this);
        this.build(app);
        this
    }

    fn build(self: &Rc<Self>, app: &App) {
        let weak = Rc::downgrade(self);
        macro_rules! with {
            ($body:expr) => {{
                let weak = weak.clone();
                move |_: &_| if let Some(this) = weak.upgrade() { $body(&this) }
            }};
        }
        let mapped = weak.clone();
        self.window.connect_map(move |_| if let Some(this) = mapped.upgrade() { this.mapped_at.set(Instant::now()) });
        let owner = self.owner.clone();
        self.window.connect_close_request(move |_| {
            let preview = owner.upgrade().is_some_and(|inner| inner.preview.get());
            if preview { glib::Propagation::Proceed } else { glib::Propagation::Stop }
        });
        let changed = weak.clone();
        self.background.set_changed(move || if let Some(this) = changed.upgrade() { this.palette_changed() });
        self.user_choice.connect_selected_notify(with!(|this: &Rc<LoginWindow>| this.choose_user()));
        self.username.connect_activate(with!(|this: &Rc<LoginWindow>| this.begin()));
        self.session_choice.connect_selected_notify(with!(|this: &Rc<LoginWindow>| this.begin()));
        self.entry.connect_activate(with!(|this: &Rc<LoginWindow>| this.submit()));
        self.eye.connect_toggled(with!(|this: &Rc<LoginWindow>| this.reveal_password()));
        self.enter.connect_clicked(with!(|this: &Rc<LoginWindow>| this.submit()));
        self.sleep.connect_clicked(with!(|this: &Rc<LoginWindow>| if let Some(app) = this.app() { app.power("Suspend") }));
        self.restart_button.connect_clicked(with!(|this: &Rc<LoginWindow>| this.open_power("Reboot")));
        self.power_button.connect_clicked(with!(|this: &Rc<LoginWindow>| this.open_power("PowerOff")));
        self.power_confirm.connect_clicked(with!(|this: &Rc<LoginWindow>| this.confirm_power()));
        self.power_cancel.connect_clicked(with!(|this: &Rc<LoginWindow>| this.cancel_power()));
        self.accessibility_menu(app);
        for button in [self.enter.upcast_ref::<gtk::Widget>(), self.sleep.upcast_ref(), self.restart_button.upcast_ref(),
                       self.power_button.upcast_ref(), self.accessibility.upcast_ref()] {
            self.buttons.append(button);
        }
        for button in [self.eye.upcast_ref::<gtk::Widget>(), self.enter.upcast_ref(), self.sleep.upcast_ref(),
                       self.restart_button.upcast_ref(), self.power_button.upcast_ref(), self.accessibility.upcast_ref()] {
            self.action_sizes.add_widget(button);
        }
        let (entry_weak, submit_weak, visible_owner, focus_weak) = (weak.clone(), weak.clone(), self.owner.clone(), weak.clone());
        let keyboard = LockKeyboard::new(LockHost {
            entry: Rc::new(move || entry_weak.upgrade().map(|w| w.entry.clone()).unwrap_or_default()),
            submit: Rc::new(move || if let Some(this) = submit_weak.upgrade() { this.submit() }),
            is_visible: Rc::new(move || visible_owner.upgrade().is_some_and(|i| i.visible.get() && !i.transitioning.get())),
            focus: Rc::new(move || if let Some(this) = focus_weak.upgrade() { this.claim_input() }),
            overlay: self.login_content.clone(),
            scroll: self.content.clone().upcast(),
            monitor: Some(self.monitor.clone()),
            button_box: self.buttons.clone(),
            size_group: Some(self.action_sizes.clone()),
            button_class: "action".into(),
            scale_factor: self.window.scale_factor(),
            direction: self.window.direction(),
            helper: None,
            store: None,
            watch_devices: app.0.watch_devices,
        });
        keyboard.button().set_has_tooltip(true);
        keyboard.button().add_css_class("family-action");
        *self.touch_keyboard.borrow_mut() = Some(keyboard);
        self.form.append(&self.buttons);
        self.form.append(&self.power_panel);
        self.root.add_overlay(&self.fade_cover);
        if let Some(close) = &self.preview_close {
            self.root.add_overlay(close);
        }
        let keys = key_controller(&self.window, true, {
            let weak = weak.clone();
            move |controller, key, _, _| match weak.upgrade() {
                Some(this) => this.key(controller, key),
                None => glib::Propagation::Proceed,
            }
        });
        let released = weak.clone();
        keys.connect_key_released(move |controller, _, _, _| if let Some(this) = released.upgrade() { this.forward(controller); });
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (enter_weak, motion_weak) = (weak.clone(), weak.clone());
        motion.connect_enter(move |_, x, y| if let Some(this) = enter_weak.upgrade() { this.pointer_enter(x, y) });
        motion.connect_motion(move |_, x, y| if let Some(this) = motion_weak.upgrade() { this.motion(x, y) });
        self.root.add_controller(motion);
        let clicks = gtk::GestureClick::new();
        clicks.set_button(0);
        clicks.set_propagation_phase(gtk::PropagationPhase::Capture);
        let click_weak = weak.clone();
        clicks.connect_pressed(move |gesture, _, x, y| if let Some(this) = click_weak.upgrade() { this.click(gesture, x, y) });
        self.root.add_controller(clicks);
        let entry_click = gtk::GestureClick::new();
        entry_click.set_button(1);
        entry_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let claim_weak = weak.clone();
        entry_click.connect_pressed(move |_, _, _, _| if let Some(this) = claim_weak.upgrade() { this.claim_input() });
        self.entry.add_controller(entry_click);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let scroll_weak = weak.clone();
        scroll.connect_scroll(move |_, _, _| {
            let woke = scroll_weak.upgrade().and_then(|this| this.app().map(|app| app.activity(&this))).unwrap_or(false);
            if woke { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.root.add_controller(scroll);
        let appearance = &app.0.appearance;
        let config = LockConfig {
            wallpaper_mode: if appearance.picture.is_empty() { "color".into() } else { "picture".into() },
            picture: appearance.picture.clone(), placement: appearance.placement.clone(),
            background_color: appearance.background_color.clone(), ..LockConfig::default()
        };
        self.background.configure(&config, appearance.blur_strength);
        self.choose_user();
        self.layer.reveal(false, false);
        self.tick();
    }

    fn accessibility_menu(self: &Rc<Self>, app: &App) {
        let button = &self.accessibility;
        button.set_halign(gtk::Align::Center);
        button.set_valign(gtk::Align::Center);
        button.set_child(Some(&image("preferences-desktop-accessibility-symbolic", 20)));
        button.set_size_request(44, 44);
        button.add_css_class("action");
        button.add_css_class("family-action");
        let title = tr!("Accessibility");
        button.set_tooltip_text(Some(&title));
        button.update_property(&[gtk::accessible::Property::Label(&title)]);
        let menu = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(4).margin_bottom(4)
            .margin_start(4).margin_end(4).build();
        for (key, label) in [("screen_reader", "Screen reader"), ("large_text", "Large text"), ("high_contrast", "High contrast"),
                             ("keyboard", "On-screen keyboard")] {
            if key == "screen_reader" && !app.0.screen_reader_available {
                continue;
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.append(&gtk::Label::builder().label(tr!(label)).xalign(0.0).hexpand(true).build());
            let switch = gtk::Switch::builder().active(app.0.a11y.borrow().get(key).copied().unwrap_or(false))
                .valign(gtk::Align::Center).build();
            switch.update_property(&[gtk::accessible::Property::Label(&tr!(label))]);
            let weak = Rc::downgrade(self);
            switch.connect_active_notify(move |widget| {
                if let Some(this) = weak.upgrade() {
                    if let Some(app) = this.app() {
                        app.set_accessibility(key, widget.is_active(), Some(&this));
                    }
                }
            });
            row.append(&switch);
            menu.append(&row);
            self.a11y_switches.borrow_mut().push((key.into(), switch));
        }
        let popover = gtk::Popover::builder().child(&menu).build();
        let owner = self.owner.clone();
        popover.connect_visible_notify(move |_| if let Some(inner) = owner.upgrade() { inner.last_activity.set(Instant::now()) });
        button.set_popover(Some(&popover));
    }

    pub fn a11y_switch(&self, key: &str) -> Option<gtk::Switch> {
        self.a11y_switches.borrow().iter().find(|(k, _)| k == key).map(|(_, s)| s.clone())
    }

    pub fn keyboard(&self) -> LockKeyboard {
        self.touch_keyboard.borrow().clone().expect("touch keyboard")
    }

    fn palette_changed(&self) {
        let Some(app) = self.app() else { return };
        if self.closed.get() {
            return;
        }
        let (css, tint) = palette_css(&self.scope, self.background.palette_color(), app.0.appearance.transparency);
        let display = WidgetExt::display(&self.window);
        if let Some(provider) = self.provider.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        *self.provider.borrow_mut() = Some(add_css(&css, Some(&display), APPLICATION_PRIORITY + 10));
        self.background.set_tint(tint);
        self.background.redraw();
    }

    fn selected_user(&self) -> Option<User> {
        let app = self.app()?;
        app.0.users.get(self.user_choice.selected() as usize).cloned()
    }

    pub fn choose_user(&self) {
        let user = self.selected_user();
        self.avatar.set_user(user.as_ref().map_or("", |u| u.name.as_str()), user.as_ref().map(|u| u.username.as_str()));
        self.username.set_visible(user.is_none());
        self.entry.set_text("");
        self.eye.set_active(false);
        self.begin();
    }

    pub fn begin(&self) {
        let Some(app) = self.app() else { return };
        if !app.0.visible.get() || app.0.transitioning.get() {
            return;
        }
        app.select(self);
        let username = self.selected_username();
        let index = self.session_choice.selected() as usize;
        if username.is_empty() || index >= app.0.sessions.len() {
            app.client_cancel();
            self.entry.set_sensitive(false);
            self.enter.set_sensitive(!app.0.sessions.is_empty());
            if username.is_empty() {
                self.username.grab_focus();
            } else {
                self.status.set_text("Install a Wayland desktop session to sign in.");
            }
            return;
        }
        let session = app.0.sessions[index].clone();
        app.begin(&username, session);
    }

    pub fn selected_username(&self) -> String {
        match self.selected_user() {
            Some(user) => user.username,
            None => suite_core::pyjson::py_strip(&self.username.text()).to_owned(),
        }
    }

    fn reveal_password(&self) {
        self.entry.set_visibility(self.eye.is_active());
        self.eye.set_tooltip_text(Some(if self.eye.is_active() { "Hide password" } else { "Show password" }));
    }

    pub fn submit(&self) {
        let Some(app) = self.app() else { return };
        app.0.last_activity.set(Instant::now());
        let client = app.client();
        if app.0.preview.get() {
            self.entry.set_text("");
            self.status.set_text("Preview only — authentication and power actions are disabled.");
        } else if let Some(client) = client.filter(|c| c.state() == "prompt" && app.is_active(self)) {
            let response = take_secret(&self.entry);
            self.entry.set_text("");
            client.respond(response);
        } else {
            self.begin();
        }
    }

    pub fn claim_input(&self) {
        let Some(app) = self.app() else { return };
        if !app.is_active(self) {
            self.begin();
        }
        if self.entry.is_sensitive() {
            self.entry.grab_focus_without_selecting();
        }
    }

    pub fn open_power(&self, method: &str) {
        if method != "Reboot" && method != "PowerOff" {
            return;
        }
        *self.pending_power.borrow_mut() = Some(method.into());
        self.power_question.set_text(if method == "Reboot" { "Restart this computer?" } else { "Power off this computer?" });
        self.power_confirm.set_label(if method == "Reboot" { "Restart" } else { "Power off" });
        self.power_panel.set_visible(true);
        self.tick();
    }

    pub fn cancel_power(&self) {
        self.pending_power.borrow_mut().take();
        self.power_panel.set_visible(false);
    }

    pub fn confirm_power(&self) {
        let method = self.pending_power.borrow().clone();
        self.cancel_power();
        if let (Some(method), Some(app)) = (method, self.app()) {
            if method == "Reboot" || method == "PowerOff" {
                app.power(&method);
            }
        }
    }

    fn pointer_enter(self: &Rc<Self>, x: f64, y: f64) {
        if self.point.get().is_none() {
            self.point.set(Some((x, y)));
            // Mapping under a stationary cursor stays idle. A pointer attached
            // or moved into the surface later is genuine input.
            if self.mapped_at.get().elapsed().as_secs_f64() > 0.35 {
                if let Some(app) = self.app() {
                    app.activity(self);
                }
            }
        } else {
            self.motion(x, y);
        }
    }

    fn motion(self: &Rc<Self>, x: f64, y: f64) {
        if self.point.get() != Some((x, y)) {
            self.point.set(Some((x, y)));
            if let Some(app) = self.app() {
                app.activity(self);
            }
        }
    }

    fn click(self: &Rc<Self>, gesture: &gtk::GestureClick, x: f64, y: f64) {
        let Some(app) = self.app() else { return };
        if app.activity(self) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        } else if !app.is_active(self) {
            if let Some(bounds) = self.form.compute_bounds(&self.root) {
                let (bx, by, bw, bh) = (bounds.x() as f64, bounds.y() as f64, bounds.width() as f64, bounds.height() as f64);
                if bx <= x && x < bx + bw && by <= y && y < by + bh {
                    // Inactive monitor fields are insensitive; claim input at the
                    // root before GTK's hit testing can discard their click.
                    self.begin();
                }
            }
        }
    }

    fn key(self: &Rc<Self>, controller: &gtk::EventControllerKey, key: gdk::Key) -> glib::Propagation {
        let Some(app) = self.app() else { return glib::Propagation::Proceed };
        if app.activity(self) {
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Escape {
            app.hide_login();
            return glib::Propagation::Stop;
        }
        if self.forward(controller) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    }

    fn forward(&self, controller: &gtk::EventControllerKey) -> bool {
        let Some(app) = self.app() else { return false };
        let target = app.0.active.borrow().clone();
        let Some(target) = target else { return false };
        if app.0.forwarding.get() || !app.0.visible.get() || std::ptr::eq(Rc::as_ptr(&target), self) {
            return false;
        }
        let Some(focus) = gtk::prelude::GtkWindowExt::focus(&target.window) else { return false };
        app.0.forwarding.set(true);
        controller.forward(&focus);
        app.0.forwarding.set(false);
        true
    }

    pub fn show_login(&self, visible: bool) {
        self.idle.set_visible(false);
        self.keyboard().reveal(visible);
        if !visible {
            self.entry.set_text("");
            self.eye.set_active(false);
            self.cancel_power();
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
        self.layer.reveal(visible, true);
    }

    fn settled(&self, visible: bool) {
        let app_visible = self.app().is_some_and(|app| app.0.visible.get());
        self.idle.set_visible(!visible && !app_visible);
    }

    pub fn tick(&self) {
        let Some(app) = self.app() else { return };
        // Same breakpoints as the lock screen: smaller clock on short or narrow outputs.
        let geometry = self.monitor.geometry();
        let width = if self.window.width() != 0 { self.window.width() } else { geometry.width() };
        let height = if self.window.height() != 0 { self.window.height() } else { geometry.height() };
        if width < 900 || height < 900 {
            self.window.add_css_class("family-compact");
        } else {
            self.window.remove_css_class("family-compact");
        }
        if let Ok(now) = glib::DateTime::now_local() {
            self.clock.set_text(&clock_text(&now, app.0.appearance.clock_24h));
            self.date.set_text(&long_date(&now));
        }
        let caps = WidgetExt::display(&self.window).default_seat().and_then(|seat| seat.keyboard()).is_some_and(|k| k.is_caps_locked());
        self.caps.set_visible(caps);
        let capabilities = app.0.capabilities.borrow();
        let pending = self.pending_power.borrow().clone().unwrap_or_default();
        for (widget, method) in [(&self.sleep, "Suspend"), (&self.power_button, "PowerOff"), (&self.restart_button, "Reboot"),
                                 (&self.power_confirm, pending.as_str())] {
            widget.set_sensitive(capabilities.get(method).copied().unwrap_or(false) && !app.0.preview.get());
        }
    }

    pub fn cleanup(&self) {
        self.closed.set(true);
        self.entry.set_text("");
        if let Some(keyboard) = self.touch_keyboard.borrow().as_ref() {
            keyboard.close();
        }
        self.layer.close();
        self.background.close();
        if let Some(provider) = self.provider.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &provider);
        }
    }
}

pub struct AppInner {
    pub app: gtk::Application,
    pub preview: Cell<bool>,
    pub appearance: Appearance,
    pub sessions: Vec<Session>,
    pub users: Vec<User>,
    client_factory: ClientFactory,
    client: RefCell<Option<Rc<dyn Authenticator>>>,
    pub windows: RefCell<Vec<(gdk::Monitor, Rc<LoginWindow>)>>,
    pub active: RefCell<Option<Rc<LoginWindow>>>,
    pub visible: Cell<bool>,
    forwarding: Cell<bool>,
    pub last_activity: Cell<Instant>,
    pub capabilities: RefCell<HashMap<String, bool>>,
    pub a11y: RefCell<HashMap<&'static str, bool>>,
    pub screen_reader_available: bool,
    screen_reader: RefCell<Option<Box<dyn Reader>>>,
    reader_launcher: ReaderLauncher,
    pub a11y_styles: RefCell<HashMap<String, gtk::CssProvider>>,
    syncing_a11y: Cell<bool>,
    bus: RefCell<Option<gio::DBusConnection>>,
    timer: RefCell<Option<glib::SourceId>>,
    pub fade: RefCell<Option<Fade>>,
    pub transitioning: Cell<bool>,
    style: RefCell<Option<gtk::CssProvider>>,
    monitors: RefCell<Option<gio::ListModel>>,
    power_hook: RefCell<Option<Box<dyn Fn(&str)>>>,
    pub watch_devices: bool,
    pub startup_error: RefCell<Option<String>>,
}

/// `LoginApplication(Gtk.Application)`.
#[derive(Clone)]
pub struct App(pub Rc<AppInner>);

impl App {
    pub fn new(options: Options) -> App {
        let app = gtk::Application::new(Some(options.application_id.as_deref().unwrap_or(APPLICATION_ID)), gio::ApplicationFlags::NON_UNIQUE);
        let a11y = HashMap::from([("screen_reader", false), ("large_text", false), ("high_contrast", false), ("keyboard", false)]);
        let watch_devices = options.watch_devices.unwrap_or(true);
        let this = App(Rc::new(AppInner {
            app, preview: Cell::new(options.preview),
            appearance: options.appearance.unwrap_or_else(|| Appearance::load(None).unwrap_or_default()),
            sessions: options.sessions.unwrap_or_else(|| sessions(&suite_lock::login_model::default_session_roots())),
            users: options.users.unwrap_or_else(users),
            client_factory: options.client_factory.unwrap_or_else(|| Box::new(greetd_client)),
            client: RefCell::new(None), windows: RefCell::default(), active: RefCell::new(None), visible: Cell::new(false),
            forwarding: Cell::new(false), last_activity: Cell::new(Instant::now()), capabilities: RefCell::default(),
            a11y: RefCell::new(a11y), screen_reader_available: suite_core::shell::which(SCREEN_READER).is_some(),
            screen_reader: RefCell::new(None), reader_launcher: options.reader_launcher.unwrap_or_else(|| Box::new(launch_reader)),
            a11y_styles: RefCell::default(), syncing_a11y: Cell::new(false), bus: RefCell::new(None), timer: RefCell::new(None),
            fade: RefCell::new(None), transitioning: Cell::new(false), style: RefCell::new(None), monitors: RefCell::new(None),
            power_hook: RefCell::new(None), watch_devices, startup_error: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_activate(move |_| if let Some(inner) = weak.upgrade() { App(inner).activate() });
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_shutdown(move |_| if let Some(inner) = weak.upgrade() { App(inner).shutdown() });
        this
    }

    pub fn application(&self) -> &gtk::Application {
        &self.0.app
    }

    pub fn client(&self) -> Option<Rc<dyn Authenticator>> {
        self.0.client.borrow().clone()
    }

    pub fn windows(&self) -> Vec<Rc<LoginWindow>> {
        self.0.windows.borrow().iter().map(|(_, w)| w.clone()).collect()
    }

    pub fn active(&self) -> Option<Rc<LoginWindow>> {
        self.0.active.borrow().clone()
    }

    pub fn set_active(&self, window: Option<Rc<LoginWindow>>) {
        *self.0.active.borrow_mut() = window;
    }

    fn is_active(&self, window: &LoginWindow) -> bool {
        self.0.active.borrow().as_ref().is_some_and(|a| std::ptr::eq(Rc::as_ptr(a), window))
    }

    /// Test seam for Python's `app.power = Mock()`.
    pub fn set_power_hook(&self, hook: Option<Box<dyn Fn(&str)>>) {
        *self.0.power_hook.borrow_mut() = hook;
    }

    fn activate(&self) {
        *self.0.style.borrow_mut() = Some(add_css(&(STYLE.to_owned() + &family_css("waylogin")), None, APPLICATION_PRIORITY));
        if let Some(settings) = gtk::Settings::default() {
            #[allow(deprecated)]
            settings.set_gtk_application_prefer_dark_theme(true);
            if ["/usr/share/icons", "/usr/local/share/icons"].iter().any(|root| std::path::Path::new(root).join("Papirus-Dark").is_dir()) {
                settings.set_gtk_icon_theme_name(Some("Papirus-Dark"));
            }
        }
        let Some(display) = gdk::Display::default() else {
            *self.0.startup_error.borrow_mut() = Some("The greeter requires Wayland layer-shell and a greetd login socket".into());
            return;
        };
        let monitors = display.monitors();
        *self.0.monitors.borrow_mut() = Some(monitors.clone());
        if !self.0.preview.get() {
            let socket = std::env::var("GREETD_SOCK").unwrap_or_default();
            if !gtk4_layer_shell::is_supported() || socket.is_empty() {
                *self.0.startup_error.borrow_mut() = Some("The greeter requires Wayland layer-shell and a greetd login socket".into());
                return;
            }
            let weak = Rc::downgrade(&self.0);
            let changed: Changed = Rc::new(move |kind, text| if let Some(inner) = weak.upgrade() { App(inner).authentication_event(kind, text) });
            *self.0.client.borrow_mut() = Some((self.0.client_factory)(&socket, changed));
        }
        let weak = Rc::downgrade(&self.0);
        monitors.connect_items_changed(move |_, _, _, _| if let Some(inner) = weak.upgrade() { App(inner).update_monitors() });
        self.update_monitors();
        let weak = Rc::downgrade(&self.0);
        let timer = glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(inner) => {
                App(inner).tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *self.0.timer.borrow_mut() = Some(timer);
        if !self.0.preview.get() {
            self.load_power();
        }
    }

    pub fn update_monitors(&self) {
        let Some(model) = self.0.monitors.borrow().clone() else { return };
        let mut monitors: Vec<gdk::Monitor> = (0..model.n_items()).filter_map(|i| model.item(i).and_downcast()).collect();
        if self.0.preview.get() {
            monitors.truncate(1);
        }
        let removed: Vec<Rc<LoginWindow>> = {
            let mut windows = self.0.windows.borrow_mut();
            let (keep, gone): (Vec<_>, Vec<_>) = windows.drain(..).partition(|(m, _)| monitors.contains(m));
            *windows = keep;
            gone.into_iter().map(|(_, w)| w).collect()
        };
        for window in removed {
            if self.is_active(&window) {
                self.hide_login();
                self.set_active(None);
            }
            window.cleanup();
            window.window.destroy();
        }
        for monitor in monitors {
            if self.0.windows.borrow().iter().any(|(m, _)| *m == monitor) {
                continue;
            }
            let window = LoginWindow::new(self, &monitor);
            self.0.windows.borrow_mut().push((monitor.clone(), window.clone()));
            if self.0.preview.get() {
                window.window.set_default_size(1200, 820);
            } else {
                let w = &window.window;
                w.init_layer_shell();
                w.set_namespace(Some("waylogin"));
                w.set_monitor(Some(&monitor));
                w.set_layer(Layer::Overlay);
                w.set_keyboard_mode(KeyboardMode::Exclusive);
                w.set_exclusive_zone(-1);
                for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                    w.set_anchor(edge, true);
                }
            }
            window.window.present();
            if self.0.visible.get() {
                window.show_login(true);
            }
            if self.0.transitioning.get() {
                window.form.set_sensitive(false);
                window.fade_cover.set_visible(true);
                window.fade_cover.set_opacity(1.0);
            }
        }
    }

    /// Input on `window`: returns true when the event woke the greeter (consumed).
    pub fn activity(&self, window: &Rc<LoginWindow>) -> bool {
        if self.0.transitioning.get() {
            return true;
        }
        self.0.last_activity.set(Instant::now());
        if self.0.visible.get() {
            return false;
        }
        self.0.visible.set(true);
        for item in self.windows() {
            item.show_login(true);
        }
        window.begin();
        true
    }

    fn select(&self, window: &LoginWindow) {
        if !self.is_active(window) {
            self.client_cancel();
            for other in self.windows() {
                other.entry.set_text("");
            }
            let found = self.windows().into_iter().find(|w| std::ptr::eq(Rc::as_ptr(w), window));
            self.set_active(found);
        }
    }

    pub fn begin(&self, username: &str, session: Session) {
        if self.0.preview.get() {
            self.authentication_event("secret", "Password:");
            return;
        }
        if let Some(client) = self.client() {
            client.begin(username, Some(session));
        }
    }

    pub fn client_cancel(&self) {
        if let Some(client) = self.client() {
            client.cancel();
        }
    }

    pub fn hide_login(&self) {
        if self.0.transitioning.get() || self.client().is_some_and(|c| matches!(c.state().as_str(), "launching" | "started")) {
            return;
        }
        self.client_cancel();
        self.0.visible.set(false);
        for window in self.windows() {
            window.show_login(false);
        }
    }

    pub fn authentication_event(&self, event: &str, message: &str) {
        if event == "started" {
            self.0.app.quit(); // labwc --session exits too; greetd can now start the desktop.
            return;
        }
        if event == "error" && self.0.transitioning.get() {
            self.0.transitioning.set(false);
            if let Some(fade) = self.0.fade.borrow_mut().take() {
                fade.cancel();
            }
            for item in self.windows() {
                item.fade_cover.set_visible(false);
                item.form.set_sensitive(true);
            }
            // The monitor used for authentication may have been unplugged.
            if self.active().is_none() {
                self.set_active(self.windows().into_iter().next());
            }
        }
        let Some(window) = self.active() else { return };
        if event == "authenticated" {
            self.0.transitioning.set(true);
            let generation = self.client().map_or(0, |c| c.generation());
            for item in self.windows() {
                item.entry.set_text("");
                item.keyboard().hide();
                item.form.set_sensitive(false);
                item.fade_cover.set_visible(true);
            }
            let covers = self.windows().iter().map(|w| w.fade_cover.clone().upcast()).collect();
            let weak = Rc::downgrade(&self.0);
            let fade = Fade::new(covers, 0.0, 1.0, 250, move || {
                if let Some(client) = weak.upgrade().and_then(|inner| App(inner).client()) {
                    client.start_session(generation);
                }
            });
            *self.0.fade.borrow_mut() = Some(fade);
            return;
        }
        let prompt = event == "secret" || event == "visible";
        if event != "message" {
            let launching = self.client().is_some_and(|c| c.state() == "launching");
            for item in self.windows() {
                let is_window = Rc::ptr_eq(&item, &window);
                item.entry.set_sensitive(is_window && prompt);
                item.enter.set_sensitive(is_window && (prompt || event == "error"));
                item.user_choice.set_sensitive(!launching);
                item.session_choice.set_sensitive(item.user_choice.is_sensitive());
                item.username.set_sensitive(item.user_choice.is_sensitive());
            }
        }
        if prompt {
            window.entry.set_text("");
            window.eye.set_active(false);
            window.eye.set_visible(event == "secret");
            window.entry.set_visibility(event == "visible");
            window.entry.set_input_purpose(if event == "secret" { gtk::InputPurpose::Password } else { gtk::InputPurpose::FreeForm });
            window.prompt.set_text(message);
            let folded = suite_core::pyjson::casefold(suite_core::pyjson::py_strip(message));
            window.prompt.set_visible(folded != "password:" && folded != "password");
            window.entry.update_property(&[gtk::accessible::Property::Label(if message.is_empty() { "Password" } else { message })]);
            window.status.set_text("");
            window.entry.grab_focus();
        } else {
            window.status.set_text(message);
            if event == "error" {
                window.entry.set_text("");
                window.prompt.set_visible(false);
                // Retry is explicit: the arrow restarts PAM, with no cached secret.
                window.enter.grab_focus();
            }
        }
    }

    /// Greeter-wide accessibility options, mirrored on every monitor's menu.
    pub fn set_accessibility(&self, key: &str, enabled: bool, source: Option<&Rc<LoginWindow>>) {
        self.0.last_activity.set(Instant::now());
        if self.0.syncing_a11y.get() || self.0.a11y.borrow().get(key).copied() == Some(enabled) {
            return;
        }
        let Some(static_key) = ["screen_reader", "large_text", "high_contrast", "keyboard"].into_iter().find(|k| *k == key) else { return };
        self.0.a11y.borrow_mut().insert(static_key, enabled);
        self.0.syncing_a11y.set(true);
        for window in self.windows() {
            if let Some(switch) = window.a11y_switch(key) {
                if switch.is_active() != enabled {
                    switch.set_active(enabled);
                }
            }
        }
        self.0.syncing_a11y.set(false);
        match key {
            "screen_reader" => self.toggle_screen_reader(enabled),
            "large_text" => self.apply_style(key, LARGE_TEXT, enabled),
            "high_contrast" => self.apply_style(key, HIGH_CONTRAST, enabled),
            _ => {
                let window = source.cloned().or_else(|| self.active()).or_else(|| self.windows().into_iter().next());
                if let Some(window) = window {
                    if enabled { window.keyboard().show() } else { window.keyboard().hide() }
                }
            }
        }
    }

    fn apply_style(&self, key: &str, css: &str, enabled: bool) {
        let Some(display) = gdk::Display::default() else { return };
        if let Some(provider) = self.0.a11y_styles.borrow_mut().remove(key) {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        if enabled {
            // Above the per-window palette (APPLICATION + 10).
            self.0.a11y_styles.borrow_mut().insert(key.into(), add_css(css, Some(&display), APPLICATION_PRIORITY + 20));
        }
        if key == "large_text" {
            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_xft_dpi((96.0 * 1024.0 * if enabled { 1.25 } else { 1.0 }) as i32);
            }
        }
    }

    pub fn toggle_screen_reader(&self, enabled: bool) {
        if let Some(mut process) = self.0.screen_reader.borrow_mut().take() {
            if process.running() {
                process.terminate();
            }
        }
        if !enabled || self.0.preview.get() {
            return;
        }
        // Same private greeter bus, so Orca reaches GTK's AT-SPI tree.
        *self.0.screen_reader.borrow_mut() = (self.0.reader_launcher)(&[SCREEN_READER, "--replace"]);
    }

    pub fn tick(&self) {
        if self.0.visible.get() && self.0.last_activity.get().elapsed().as_secs_f64() >= self.0.appearance.idle_seconds as f64 {
            self.hide_login();
        }
        for window in self.windows() {
            window.tick();
        }
    }

    /// Backdate the last input (tests: `app.last_activity = time.monotonic() - 31`).
    pub fn age_activity(&self, seconds: f64) {
        let then = Instant::now().checked_sub(std::time::Duration::from_secs_f64(seconds)).unwrap_or_else(Instant::now);
        self.0.last_activity.set(then);
    }

    fn load_power(&self) {
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let Ok(bus) = gio::bus_get_future(gio::BusType::System).await else { return };
            if let Some(inner) = weak.upgrade() {
                *inner.bus.borrow_mut() = Some(bus.clone());
            }
            for method in ["Suspend", "PowerOff", "Reboot"] {
                let weak = weak.clone();
                let bus = bus.clone();
                glib::spawn_future_local(async move {
                    let reply = bus.call_future(Some("org.freedesktop.login1"), "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager", &format!("Can{method}"), None, None, gio::DBusCallFlags::NONE, 3000).await;
                    let allowed = reply.ok().and_then(|v| v.child_value(0).str().map(|s| s == "yes")).unwrap_or(false);
                    if let Some(inner) = weak.upgrade() {
                        inner.capabilities.borrow_mut().insert(method.into(), allowed);
                    }
                });
            }
        });
    }

    pub fn power(&self, method: &str) {
        if let Some(hook) = self.0.power_hook.borrow().as_ref() {
            return hook(method);
        }
        if self.client().is_some_and(|c| matches!(c.state().as_str(), "launching" | "started")) {
            return;
        }
        let bus = self.0.bus.borrow().clone();
        let allowed = self.0.capabilities.borrow().get(method).copied().unwrap_or(false);
        let Some(bus) = bus.filter(|_| !self.0.preview.get() && allowed && ["Suspend", "PowerOff", "Reboot"].contains(&method)) else {
            return;
        };
        self.hide_login();
        let weak = Rc::downgrade(&self.0);
        let method = method.to_owned();
        glib::spawn_future_local(async move {
            let result = bus.call_future(Some("org.freedesktop.login1"), "/org/freedesktop/login1", "org.freedesktop.login1.Manager",
                                         &method, Some(&(false,).to_variant()), None, gio::DBusCallFlags::NONE, 10000).await;
            if result.is_err() {
                if let Some(app) = weak.upgrade().map(App) {
                    if let Some(window) = app.windows().into_iter().next() {
                        app.activity(&window);
                        window.status.set_text("The power action was denied or is currently unavailable.");
                    }
                }
            }
        });
    }

    pub fn shutdown(&self) {
        self.toggle_screen_reader(false);
        if let Some(fade) = self.0.fade.borrow().as_ref() {
            fade.cancel();
        }
        if let Some(timer) = self.0.timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(client) = self.client() {
            client.close();
        }
        for window in self.windows() {
            window.cleanup();
        }
    }
}
