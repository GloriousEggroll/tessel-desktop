//! tessel-lock/lock_settings.py: tessel-lock appearance and the existing
//! power service's idle lock policy.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gs_base::Host;
use gtk::prelude::*;
use serde_json::{Map, Value};
use tessel_core::tr;

use super::config::{ConfigStore, IMAGE_EXTENSIONS, PLACEMENTS, WALLPAPER_MODES};
use super::power::{LockPower, PowerFactory};

pub const LOCK_CSS: &str = ".lock-settings .card { padding: 16px; border-radius: 12px; border: 1px solid alpha(currentColor,.12); } .lock-settings .title { font-size: 18pt; font-weight: 700; } .lock-settings headerbar .title { font-size: 1em; } .lock-settings .section-title { font-weight: 600; }";

pub type Detector = Rc<dyn Fn() -> bool>;
pub type Preview = Rc<dyn Fn() -> Result<(), String>>;
pub type WeatherLocation = Rc<dyn Fn() -> Option<String>>;

/// `systemd-run --user --collect --quiet --property=Type=exec tessel-lock --preview`.
pub fn preview_command() -> Vec<String> {
    let program = tessel_core::paths::program("tessel-lock");
    ["systemd-run", "--user", "--collect", "--quiet", "--property=Type=exec"].iter()
        .map(|s| s.to_string()).chain([program.to_string_lossy().into_owned(), "--preview".into()]).collect()
}

fn start_preview_process() -> Result<(), String> {
    std::process::Command::new("systemd-run")
        .args(&preview_command()[1..])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn().map(|_| ()).map_err(|e| tessel_core::pyerr::os_error_path(&e, Path::new("systemd-run")))
}

use std::os::unix::process::CommandExt;

/// Python keyword arguments of `LockSettings(host, store=None, power_factory=…, assistant_detector=…)`
/// plus seams for the preview process and the weather location.
#[derive(Default, Clone)]
pub struct LockOptions {
    pub store_path: Option<std::path::PathBuf>,
    pub power_factory: Option<PowerFactory>,
    pub assistant_detector: Option<Detector>,
    pub preview: Option<Preview>,
    pub weather_location: Option<WeatherLocation>,
}

pub struct LockSettings {
    pub host: Host,
    pub root: gtk::Box,
    pub store: ConfigStore,
    style: gtk::CssProvider,
    assistant_detector: Detector,
    preview_process: Preview,
    weather_location: WeatherLocation,
    pub closed: Cell<bool>,
    pub busy: Cell<bool>,
    picker: RefCell<Option<tessel_shell::wallpaper_chooser::media_chooser::MediaChooser>>,
    pub lock_baseline: Cell<Option<i64>>,
    pub mode: gtk::DropDown,
    pub picture: RefCell<String>,
    pub slides: RefCell<Vec<String>>,
    pub pictures: gtk::Box,
    pub choose: gtk::Button,
    pub placement: gtk::DropDown,
    pub interval: gtk::SpinButton,
    pub color: gtk::Entry,
    pub follow: gtk::Switch,
    pub custom: gtk::Box,
    pub transparent: gtk::Switch,
    pub transparency: gtk::Scale,
    pub blur: gtk::Switch,
    pub strength: gtk::Scale,
    pub assistant: gtk::Box,
    pub show_name: gtk::Switch,
    pub name: gtk::Entry,
    pub show_voice: gtk::Switch,
    pub weather_card: gtk::Box,
    pub weather_location_label: gtk::Label,
    pub weather_options: gtk::Box,
    pub show_weather: gtk::Switch,
    pub show_forecast: gtk::Switch,
    pub pause_media: gtk::Switch,
    pub show_media: gtk::Switch,
    pub status_card: gtk::Box,
    pub show_battery: gtk::Switch,
    pub show_notifications: gtk::Switch,
    pub notification_names: gtk::Switch,
    pub delay: gtk::SpinButton,
    pub power_status: gtk::Label,
    pub status: gtk::Label,
    pub preview: gtk::Button,
    pub save: gtk::Button,
    pub search_targets: RefCell<HashMap<String, gtk::Widget>>,
    pub power: RefCell<Option<Rc<dyn LockPower>>>,
}

/// `label(text, style)`: translated, wrapping, left-aligned.
fn label(text: &str, style: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::builder().label(tr!(text)).xalign(0.0).wrap(true).build();
    if let Some(style) = style {
        widget.add_css_class(style);
    }
    widget
}

fn card(root: &gtk::Box, title: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("card");
    card.append(&label(title, Some("section-title")));
    root.append(&card);
    card
}

fn row(text: &str, control: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>)) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let caption = label(text, None);
    caption.set_hexpand(true);
    row.append(&caption);
    control.set_valign(gtk::Align::Center);
    control.update_property(&[gtk::accessible::Property::Label(&tr!(text))]);
    row.append(control);
    row
}

fn scale(value: i64) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_value(value as f64);
    scale.set_digits(0);
    scale.set_size_request(180, -1);
    scale
}

fn dropdown(items: &[&str], selected: usize) -> gtk::DropDown {
    let translated: Vec<String> = items.iter().map(|v| tr!(*v)).collect();
    let refs: Vec<&str> = translated.iter().map(String::as_str).collect();
    let widget = gtk::DropDown::from_strings(&refs);
    widget.set_selected(selected as u32);
    widget
}

fn switch(active: bool) -> gtk::Switch {
    gtk::Switch::builder().active(active).build()
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Python `round()` of a scale value.
fn round(value: f64) -> i64 { crate::util::py_round(value) }

fn default_weather_location() -> Option<String> {
    super::weather::configured_weather(None).map(|config| config.name)
}

impl LockSettings {
    pub fn new(host: &Host, options: LockOptions) -> Result<Rc<Self>, String> {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(24).margin_bottom(24)
            .margin_start(24).margin_end(24).build();
        let store = ConfigStore::new(options.store_path.clone());
        root.add_css_class("lock-settings");
        let style = tessel_gtk::utils::add_css(LOCK_CSS, None, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let config = match store.load() {
            Ok(config) => config,
            Err(error) => {
                gtk::style_context_remove_provider_for_display(&WidgetExt::display(&root), &style);
                return Err(error);
            }
        };
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        heading.append(&tessel_gtk::utils::image("changes-prevent-symbolic", 30));
        heading.append(&label("Lock Screen", Some("title")));
        root.append(&heading);

        let wallpaper = card(&root, "Wallpaper");
        let mode = dropdown(&["Desktop wallpaper", "Picture", "Slideshow", "Solid color"],
                            WALLPAPER_MODES.iter().position(|m| *m == config.wallpaper_mode).unwrap_or(0));
        wallpaper.append(&row("Background", &mode));
        let pictures = gtk::Box::new(gtk::Orientation::Vertical, 8);
        wallpaper.append(&pictures);
        let choose = gtk::Button::builder().label(tr!("Choose picture")).halign(gtk::Align::End).build();
        wallpaper.append(&choose);
        let placement = dropdown(&["Fill", "Fit", "Stretch", "Center"],
                                 PLACEMENTS.iter().position(|p| *p == config.placement).unwrap_or(0));
        wallpaper.append(&row("Placement", &placement));
        let interval = gtk::SpinButton::with_range(30.0, 86400.0, 30.0);
        interval.set_value(config.interval as f64);
        wallpaper.append(&row("Change picture every (seconds)", &interval));
        let color = gtk::Entry::builder().text(&config.background_color).max_width_chars(12).build();
        wallpaper.append(&row("Background color", &color));

        let appearance = card(&root, "Appearance");
        let follow = switch(config.follow_appearance);
        appearance.append(&row("Follow application appearance", &follow));
        let custom = gtk::Box::new(gtk::Orientation::Vertical, 12);
        appearance.append(&custom);
        let transparent = switch(config.transparency_enabled);
        let transparency = scale(config.transparency);
        let blur = switch(config.blur);
        let strength = scale(config.blur_strength);
        custom.append(&row("Transparency", &transparent));
        custom.append(&row("Transparency (%)", &transparency));
        custom.append(&row("Blur", &blur));
        custom.append(&row("Blur strength (%)", &strength));
        custom.set_sensitive(!config.follow_appearance);
        appearance.append(&label("Locking starts on the clear wallpaper with the clock, weather and media. Move the mouse or press a key to reveal the login. The login uses these transparency and blur settings and slides away again after 30 seconds without input.", Some("subtitle")));

        let assistant = card(&root, "Home Assistant");
        let show_name = switch(config.show_assistant_name);
        let name = gtk::Entry::builder().text(&config.assistant_name).max_length(80).build();
        let show_voice = switch(config.show_voice);
        assistant.append(&row("Show assistant name", &show_name));
        assistant.append(&row("Assistant name", &name));
        assistant.append(&row("Show voice activity", &show_voice));
        assistant.append(&label("Shows activity from the installed voice assistant. The lock screen does not open the microphone.", Some("subtitle")));
        let assistant_detector = options.assistant_detector.clone().unwrap_or_else(|| Rc::new(super::config::home_assistant_installed));
        assistant.set_visible(assistant_detector());

        let weather_card = card(&root, "Weather");
        let weather_location_label = label("", Some("subtitle"));
        weather_card.append(&weather_location_label);
        let weather_options = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let show_weather = switch(config.show_weather);
        let show_forecast = switch(config.show_forecast);
        weather_options.append(&row("Show current weather", &show_weather));
        weather_options.append(&row("Show 7-day forecast", &show_forecast));
        weather_card.append(&weather_options);

        let media = card(&root, "Media");
        let pause_media = switch(config.pause_media);
        let show_media = switch(config.show_media);
        media.append(&row("Pause media when locking", &pause_media));
        media.append(&label("When disabled, playback continues. When enabled, currently playing media pauses once the screen is locked.", Some("subtitle")));
        media.append(&row("Show media title and playback controls", &show_media));
        media.append(&label("Show the media title, application icon and play/pause button on the wallpaper screen while something is playing. Unlocking does not change playback.", Some("subtitle")));

        let status_card = card(&root, "Battery and notifications");
        let show_battery = switch(config.show_battery);
        let show_notifications = switch(config.show_notifications);
        let notification_names = switch(config.notification_app_names);
        status_card.append(&row("Show battery level", &show_battery));
        status_card.append(&row("Show new notification count", &show_notifications));
        status_card.append(&row("Show application names", &notification_names));
        status_card.append(&label("The lock screen shows how many notifications arrived, such as \"3 new notifications\". Their text is never shown. Application names are optional.", Some("subtitle")));
        notification_names.set_sensitive(config.show_notifications);

        let policy = card(&root, "Automatic locking");
        let delay = gtk::SpinButton::with_range(0.0, 86400.0, 30.0);
        delay.set_sensitive(false);
        policy.append(&row("Lock after (seconds; 0 means never)", &delay));
        let power_status = label("", Some("subtitle"));
        policy.append(&power_status);
        policy.append(&label("Keep Awake and active idle inhibitors block automatic locking. Manual locking and locking before sleep remain available.", Some("subtitle")));
        let status = label("", Some("subtitle"));
        root.append(&status);
        let actions = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
        let preview = gtk::Button::with_label(&tr!("Preview"));
        actions.append(&preview);
        let save = gtk::Button::with_label(&tr!("Apply"));
        save.add_css_class("suggested-action");
        actions.append(&save);
        root.append(&actions);

        let mut targets: HashMap<String, gtk::Widget> = HashMap::new();
        for (key, widget) in [("wallpaper", mode.clone().upcast::<gtk::Widget>()), ("blur", blur.clone().upcast()),
                              ("transparency", transparency.clone().upcast()), ("automatic", delay.clone().upcast()),
                              ("weather", weather_card.clone().upcast()), ("forecast", weather_card.clone().upcast()),
                              ("pause_media", pause_media.clone().upcast()), ("media", show_media.clone().upcast()),
                              ("battery", show_battery.clone().upcast()), ("notifications", show_notifications.clone().upcast()),
                              ("notification_names", notification_names.clone().upcast())] {
            targets.insert(key.into(), widget);
        }
        let this = Rc::new(Self {
            host: host.clone(), root, store, style, assistant_detector,
            preview_process: options.preview.clone().unwrap_or_else(|| Rc::new(start_preview_process)),
            weather_location: options.weather_location.clone().unwrap_or_else(|| Rc::new(default_weather_location)),
            closed: Cell::new(false), busy: Cell::new(false), picker: RefCell::new(None), lock_baseline: Cell::new(None),
            mode: mode.clone(), picture: RefCell::new(config.picture.clone()), slides: RefCell::new(config.slideshow.clone()),
            pictures, choose: choose.clone(), placement, interval, color, follow: follow.clone(), custom: custom.clone(),
            transparent, transparency, blur, strength, assistant, show_name, name, show_voice, weather_card,
            weather_location_label, weather_options, show_weather, show_forecast, pause_media, show_media, status_card,
            show_battery, show_notifications: show_notifications.clone(), notification_names: notification_names.clone(),
            delay, power_status, status, preview: preview.clone(), save: save.clone(), search_targets: RefCell::new(targets),
            power: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        choose.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.browse();
            }
        });
        let weak = Rc::downgrade(&this);
        mode.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.sync_pictures();
            }
        });
        this.sync_pictures();
        follow.connect_active_notify(move |follow| custom.set_sensitive(!follow.is_active()));
        this.update_weather_location();
        show_notifications.connect_active_notify(move |show| notification_names.set_sensitive(show.is_active()));
        let weak = Rc::downgrade(&this);
        preview.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.apply(true);
            }
        });
        let weak = Rc::downgrade(&this);
        save.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.apply(false);
            }
        });
        let weak = Rc::downgrade(&this);
        let changed: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.power_changed();
            }
        });
        let factory = options.power_factory.clone().unwrap_or_else(super::power::default_factory);
        let power = factory(changed);
        this.power.replace(Some(power.clone()));
        power.start();
        Ok(this)
    }

    fn power(&self) -> Rc<dyn LockPower> {
        self.power.borrow().clone().expect("power client")
    }

    pub fn sync_pictures(self: &Rc<Self>) {
        crate::util::clear_box(&self.pictures);
        let mode = self.mode.selected();
        self.choose.set_visible(mode == 1 || mode == 2);
        self.choose.set_label(&if mode == 2 { tr!("Add picture") } else { tr!("Choose picture") });
        self.placement.set_sensitive(mode != 3);
        self.interval.set_sensitive(mode == 2);
        if mode == 1 {
            let picture = self.picture.borrow().clone();
            let text = if picture.is_empty() { "No picture selected".to_owned() } else { file_name(&picture) };
            self.pictures.append(&label(&text, Some("subtitle")));
        } else if mode == 2 {
            for path in self.slides.borrow().clone() {
                let button = gtk::Button::builder().icon_name("list-remove-symbolic").tooltip_text(tr!("Remove from slideshow")).build();
                let weak = Rc::downgrade(self);
                let value = path.clone();
                button.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.remove_picture(&value);
                    }
                });
                let line = row(&file_name(&path), &button);
                line.set_tooltip_text(Some(&path));
                self.pictures.append(&line);
            }
        }
    }

    pub fn remove_picture(self: &Rc<Self>, path: &str) {
        {
            let mut slides = self.slides.borrow_mut();
            if let Some(index) = slides.iter().position(|p| p == path) {
                slides.remove(index);
            }
        }
        self.sync_pictures();
    }

    /// What the media chooser's callback does with a chosen file path.
    pub fn picture_chosen(self: &Rc<Self>, path: &str, slideshow: bool) {
        if self.closed.get() || path.is_empty() {
            return;
        }
        if slideshow {
            if !self.slides.borrow().iter().any(|p| p == path) {
                self.slides.borrow_mut().push(path.to_owned());
            }
        } else {
            self.picture.replace(path.to_owned());
        }
        self.sync_pictures();
    }

    pub fn browse(self: &Rc<Self>) {
        if let Some(picker) = self.picker.borrow().as_ref() {
            picker.present();
            return;
        }
        use tessel_shell::wallpaper_chooser::{media, media_chooser};
        let slideshow = self.mode.selected() == 2;
        let weak = Rc::downgrade(self);
        let mut extensions: Vec<&'static str> = IMAGE_EXTENSIONS.iter().map(|e| e.trim_start_matches('.')).collect();
        extensions.sort();
        let config = media_chooser::Config {
            title: tr!("Choose Lock Screen Picture"),
            initial_folder: media::default_wallpaper_folder(),
            filters: vec![("Pictures".to_owned(), extensions)],
            validate: Some(Arc::new(media::validate_media)),
            multiple: false,
            accept_label: None,
        };
        let picker = media_chooser::MediaChooser::new(&self.host.window(), move |files| {
            let Some(this) = weak.upgrade() else { return };
            this.picker.take();
            let Some(file) = files.and_then(|f| f.into_iter().next()) else { return };
            if let Some(path) = file.path() {
                this.picture_chosen(&path.to_string_lossy(), slideshow);
            }
        }, config);
        self.picker.replace(Some(picker.clone()));
        picker.present();
    }

    pub fn power_changed(&self) {
        if self.closed.get() {
            return;
        }
        let power = self.power();
        self.delay.set_sensitive(power.ready() && !self.busy.get());
        if power.ready() {
            let current = power.state().get("settings").and_then(|s| s.get("lock_after")).and_then(Value::as_i64).unwrap_or(0);
            if self.lock_baseline.get().is_none() || Some(self.delay.value_as_int() as i64) == self.lock_baseline.get() {
                self.delay.set_value(current as f64);
                self.lock_baseline.set(Some(current));
            }
            self.power_status.set_text(&tr!("Shared with Power settings."));
        } else {
            let error = power.error();
            self.power_status.set_text(&if error.is_empty() { tr!("Power management is unavailable in this session.") } else { error });
        }
    }

    pub fn apply(self: &Rc<Self>, preview: bool) {
        if self.busy.get() || self.closed.get() {
            return;
        }
        let power = self.power();
        let delay = self.delay.value_as_int() as i64;
        let changed = self.lock_baseline.get().is_some_and(|baseline| delay != baseline);
        let result = (|| -> Result<Option<Map<String, Value>>, String> {
            let mode = WALLPAPER_MODES[(self.mode.selected() as usize).min(3)];
            if mode == "picture" && !Path::new(&*self.picture.borrow()).is_file() {
                return Err(tr!("Choose a picture first."));
            }
            if mode == "slideshow" && !self.slides.borrow().iter().any(|p| Path::new(p).is_file()) {
                return Err(tr!("Add at least one slideshow picture."));
            }
            let mut settings = None;
            if changed {
                if !power.ready() {
                    return Err(tr!("Power management is unavailable."));
                }
                let mut values = power.state().get("settings").and_then(Value::as_object).cloned().unwrap_or_default();
                if values.get("lock_after").and_then(Value::as_i64) != self.lock_baseline.get() {
                    return Err(tr!("The lock delay changed in Power settings. Reopen this page before applying."));
                }
                values.insert("lock_after".into(), delay.into());
                settings = Some(values);
            }
            let mut changes = Map::new();
            changes.insert("wallpaper_mode".into(), mode.into());
            changes.insert("picture".into(), self.picture.borrow().clone().into());
            changes.insert("slideshow".into(), self.slides.borrow().clone().into());
            changes.insert("interval".into(), (self.interval.value_as_int() as i64).into());
            changes.insert("placement".into(), PLACEMENTS[(self.placement.selected() as usize).min(3)].into());
            changes.insert("background_color".into(), self.color.text().to_string().into());
            changes.insert("follow_appearance".into(), self.follow.is_active().into());
            changes.insert("transparency_enabled".into(), self.transparent.is_active().into());
            changes.insert("transparency".into(), round(self.transparency.value()).into());
            changes.insert("blur".into(), self.blur.is_active().into());
            changes.insert("blur_strength".into(), round(self.strength.value()).into());
            changes.insert("show_assistant_name".into(), self.show_name.is_active().into());
            changes.insert("assistant_name".into(), self.name.text().to_string().into());
            changes.insert("show_voice".into(), self.show_voice.is_active().into());
            changes.insert("show_weather".into(), self.show_weather.is_active().into());
            changes.insert("show_forecast".into(), self.show_forecast.is_active().into());
            changes.insert("pause_media".into(), self.pause_media.is_active().into());
            changes.insert("show_media".into(), self.show_media.is_active().into());
            changes.insert("show_battery".into(), self.show_battery.is_active().into());
            changes.insert("show_notifications".into(), self.show_notifications.is_active().into());
            changes.insert("notification_app_names".into(), self.notification_names.is_active().into());
            self.store.update(&changes)?;
            Ok(settings)
        })();
        let settings = match result {
            Ok(settings) => settings,
            Err(error) => {
                self.status.set_text(&error);
                return;
            }
        };
        let weak = Rc::downgrade(self);
        let done = move |ok: bool, error: String| {
            let Some(this) = weak.upgrade() else { return };
            this.busy.set(false);
            this.save.set_sensitive(true);
            this.preview.set_sensitive(true);
            if this.closed.get() {
                return;
            }
            if ok {
                if changed {
                    this.lock_baseline.set(Some(delay));
                }
                this.status.set_text(&tr!("Lock screen settings saved."));
                if preview {
                    this.start_preview();
                }
            } else {
                this.status.set_text(&(tr!("Appearance saved. The automatic lock delay was not changed: ") + &error));
            }
            this.power_changed();
        };
        match settings {
            Some(settings) => {
                self.busy.set(true);
                self.save.set_sensitive(false);
                self.preview.set_sensitive(false);
                power.set_settings(&settings, Box::new(done));
            }
            None => done(true, String::new()),
        }
    }

    pub fn start_preview(&self) {
        let preview = self.preview_process.clone();
        if let Err(error) = preview() {
            self.status.set_text(&error);
        }
    }

    pub fn activate(&self) {
        self.assistant.set_visible((self.assistant_detector)());
        self.update_weather_location();
        self.power_changed();
    }

    pub fn update_weather_location(&self) {
        let location = (self.weather_location)();
        self.weather_options.set_visible(location.is_some());
        self.weather_location_label.set_text(&match location {
            Some(name) => tr!("Uses your Weather settings: ") + &name,
            None => tr!("Choose a location in Weather settings to enable lock screen weather."),
        });
    }

    pub fn can_close(&self) -> bool {
        !self.busy.get()
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Some(power) = self.power.borrow().as_ref() {
            power.close();
        }
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.root), &self.style);
        if let Some(picker) = self.picker.take() {
            picker.window().close();
        }
    }
}
