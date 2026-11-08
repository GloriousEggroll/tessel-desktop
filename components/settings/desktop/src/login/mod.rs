//! Login Screen (login_page.py): appearance of the system Wayland login
//! screen, saved through the privileged helper
//! `pkexec /usr/local/lib/tessel/tessel-login-helper`
//! (JSON request on stdin). The helper argv is injectable; tests only ever
//! run it against a fake `pkexec`.
/// Moved to the shared `tessel-locking` crate (wave 5).
pub use tessel_locking::login_model as model;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gs_base::style::page_header;
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use serde_json::{Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::tr;

use model::{Appearance, Session, PLACEMENTS};

pub const HELPER_COMMAND: [&str; 2] = ["pkexec", tessel_core::system_program!("tessel-login-helper")];

/// Python keyword seams: appearance file, session roots, helper argv.
#[derive(Default, Clone)]
pub struct LoginOptions {
    pub config_path: Option<PathBuf>,
    pub session_roots: Option<Vec<PathBuf>>,
    pub helper_command: Option<Vec<String>>,
}

pub struct LoginSettings {
    pub host: Host,
    pub root: gtk::Box,
    pub busy: Cell<bool>,
    chooser: RefCell<Option<tessel_shell::wallpaper_chooser::WallpaperChooser>>,
    pub search_targets: RefCell<HashMap<String, gtk::Widget>>,
    pub original: RefCell<Map<String, Value>>,
    pub picture: gtk::Entry,
    pub placement: gtk::DropDown,
    pub color: gtk::Entry,
    pub scales: Vec<(String, gtk::Scale)>,
    pub idle: gtk::SpinButton,
    pub clock: gtk::Switch,
    pub sessions: Vec<Session>,
    pub session: gtk::DropDown,
    pub status: gtk::Label,
    pub apply: gtk::Button,
    helper: Vec<String>,
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

impl LoginSettings {
    pub fn new(host: &Host, options: LoginOptions) -> Result<Rc<Self>, String> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        tessel_gtk::utils::set_margins(&root, 24);
        let original = Appearance::load(options.config_path.as_deref())?.to_map();
        root.append(&page_header(&tr!("Login Screen"),
            Some(&tr!("System-wide appearance shown before signing in. Changes apply the next time the login screen starts.")),
            Some("system-users-symbolic")).root);
        let mut targets: HashMap<String, gtk::Widget> = HashMap::new();
        let row = |title: &str, key: &str, control: &gtk::Widget, targets: &mut HashMap<String, gtk::Widget>| {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 20);
            line.append(&gtk::Label::builder().label(tr!(title)).xalign(0.0).hexpand(true).build());
            line.append(control);
            root.append(&line);
            targets.insert(key.into(), control.clone());
        };
        let text = |key: &str| original.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        let number = |key: &str| original.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let picture = gtk::Entry::builder().text(text("picture")).hexpand(true).build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&picture);
        let browse = gtk::Button::with_label(&tr!("Browse"));
        line.append(&browse);
        row("Wallpaper", "picture", line.upcast_ref(), &mut targets);
        root.append(&gtk::Label::builder()
            .label(tr!("Choose an image readable by all users, such as one in /usr/share/wallpapers. Leave empty for a solid background."))
            .xalign(0.0).wrap(true).build());
        let names: Vec<String> = PLACEMENTS.iter().map(|p| tr!(title_case(p))).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let placement = gtk::DropDown::from_strings(&refs);
        placement.set_selected(PLACEMENTS.iter().position(|p| *p == text("placement")).unwrap_or(0) as u32);
        row("Wallpaper placement", "placement", placement.upcast_ref(), &mut targets);
        let color = gtk::Entry::builder().text(text("background_color")).max_length(7).build();
        row("Background color", "background_color", color.upcast_ref(), &mut targets);
        let mut scales = Vec::new();
        for (key, title) in [("blur_strength", "Blur strength"), ("transparency", "Transparency")] {
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
            scale.set_draw_value(true);
            scale.set_value(number(key));
            scale.set_hexpand(true);
            row(title, key, scale.upcast_ref(), &mut targets);
            scales.push((key.to_owned(), scale));
        }
        let idle = gtk::SpinButton::with_range(10.0, 3600.0, 5.0);
        idle.set_value(number("idle_seconds"));
        row("Show idle clock after (seconds)", "idle_seconds", idle.upcast_ref(), &mut targets);
        let clock = gtk::Switch::builder().active(original.get("clock_24h").and_then(Value::as_bool).unwrap_or(false))
            .valign(gtk::Align::Center).build();
        row("24-hour clock", "clock_24h", clock.upcast_ref(), &mut targets);
        let sessions = model::sessions(&options.session_roots.clone().unwrap_or_else(model::default_session_roots));
        let session_names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        let session = gtk::DropDown::from_strings(&session_names);
        let default_session = text("default_session");
        session.set_selected(sessions.iter().position(|s| s.ident == default_session).unwrap_or(0) as u32);
        row("Default session", "default_session", session.upcast_ref(), &mut targets);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
        root.append(&status);
        let apply = gtk::Button::builder().label(tr!("Apply")).halign(gtk::Align::End).sensitive(!sessions.is_empty()).build();
        apply.add_css_class("suggested-action");
        root.append(&apply);
        let this = Rc::new(Self {
            host: host.clone(), root, busy: Cell::new(false), chooser: RefCell::new(None),
            search_targets: RefCell::new(targets), original: RefCell::new(original), picture, placement, color, scales,
            idle, clock, sessions, session, status, apply: apply.clone(),
            helper: options.helper_command.unwrap_or_else(|| HELPER_COMMAND.iter().map(|s| s.to_string()).collect()),
        });
        let weak = Rc::downgrade(&this);
        browse.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.browse();
            }
        });
        let weak = Rc::downgrade(&this);
        apply.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.save();
            }
        });
        Ok(this)
    }

    pub fn browse(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let chooser = tessel_shell::wallpaper_chooser::WallpaperChooser::new(Some(&self.host.window()), move |path| {
            if let Some(this) = weak.upgrade() {
                this.chooser.take();
                if let Some(path) = path {
                    this.picture.set_text(&path.to_string_lossy());
                }
            }
        });
        self.chooser.replace(Some(chooser.clone()));
        chooser.present();
    }

    /// The `{"expected", "appearance"}` request for the current controls.
    pub fn request_value(&self) -> Result<Map<String, Value>, String> {
        let mut value = self.original.borrow().clone();
        value.insert("picture".into(), tessel_core::pyjson::py_strip(&self.picture.text()).to_owned().into());
        value.insert("placement".into(), PLACEMENTS[(self.placement.selected() as usize).min(3)].into());
        value.insert("background_color".into(), tessel_core::pyjson::py_strip(&self.color.text()).to_owned().into());
        value.insert("clock_24h".into(), self.clock.is_active().into());
        value.insert("idle_seconds".into(), (self.idle.value_as_int() as i64).into());
        let session = self.sessions.get(self.session.selected() as usize).ok_or("list index out of range")?;
        value.insert("default_session".into(), session.ident.clone().into());
        for (key, scale) in &self.scales {
            value.insert(key.clone(), crate::util::py_round(scale.value()).into());
        }
        Appearance::from_dict(&Value::Object(value.clone()))?;
        Ok(value)
    }

    pub fn save(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let value = match self.request_value() {
            Ok(value) => value,
            Err(error) => {
                self.status.set_text(&error);
                return;
            }
        };
        let args: Vec<&std::ffi::OsStr> = self.helper.iter().map(std::ffi::OsStr::new).collect();
        let process = match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDIN_PIPE
            | gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE) {
            Ok(process) => process,
            Err(error) => {
                self.status.set_text(&tessel_core::pyerr::python_error_text(&error));
                return;
            }
        };
        self.busy.set(true);
        self.root.set_sensitive(false);
        let mut request = Map::new();
        request.insert("expected".into(), Value::Object(self.original.borrow().clone()));
        request.insert("appearance".into(), Value::Object(value.clone()));
        let payload = json_dumps(&Value::Object(request), &JsonStyle::DEFAULT);
        let weak = Rc::downgrade(self);
        let child = process.clone();
        process.communicate_utf8_async(Some(payload), gio::Cancellable::NONE, move |result| {
            let Some(this) = weak.upgrade() else { return };
            this.busy.set(false);
            this.root.set_sensitive(true);
            match result {
                Ok((_out, error)) => {
                    if !child.is_successful() {
                        let error = error.map(|e| e.to_string()).unwrap_or_default();
                        let error = tessel_core::pyjson::py_strip(&error).to_owned();
                        this.status.set_text(&if error.is_empty() { tr!("Changes were not applied.") } else { error });
                        return;
                    }
                    this.original.replace(value);
                    this.status.set_text(&tr!("Login screen settings saved."));
                }
                Err(error) => this.status.set_text(&tessel_core::pyerr::python_error_text(&error)),
            }
        });
    }

    pub fn close(&self) {
        if let Some(chooser) = self.chooser.take() {
            chooser.close();
        }
    }
}

/// `login_page.build_page("login", host)` with injection.
pub fn build_login_page(host: &Host, options: LoginOptions) -> Result<SettingsPage, String> {
    let controller = LoginSettings::new(host, options)?;
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&controller.root));
    let page = SettingsPage::new(scroll);
    let (c1, c2) = (controller.clone(), controller.clone());
    page.set_close(move || c1.close());
    page.set_can_close(move || !c2.busy.get());
    for (key, widget) in controller.search_targets.borrow().iter() {
        page.add_target(key, widget);
    }
    page.set_controller(controller);
    Ok(page)
}
