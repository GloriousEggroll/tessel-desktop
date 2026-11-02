//! `luna_clock.py`: Kasumi's GTK4 idle dashboard. This is an idle display,
//! not a session lock.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use serde_json::{json, Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::tr;
use tessel_gtk::utils::image;
use tessel_locking::config::LockConfig;
use tessel_locking::dashboard::WallpaperDashboard;
use tessel_locking::info::WeatherView;
use tessel_locking::media::{album_name, AlbumArt, LockMedia, Media, MediaIcons};
use tessel_locking::visuals::{VoiceBand, Wallpaper};
use tessel_locking::voice_state::VoiceLog;
use tessel_locking::weather::{LockWeather, WeatherReader, WeatherStore};

use crate::levels::{self, ScreenLevels};
use crate::panel::{self, AdminPanel, Hooks};
use crate::schedules::{self, ScreenTimers};

pub const APPLICATION_ID: &str = "local.luna.Clock";
pub const USAGE: &str = "Usage: luna_clock.py [--show|--hide|--status|--next-wallpaper|--quit|--volume up/down/mute/0–100|--brightness up/down/10–100]\n";

pub const STYLE: &str = "
window.tessel-lock { background: #161820; color: white; }
.tessel-lock .lock-clock { font-size: 144px; font-weight: 600; color: white; text-shadow: 0 2px 8px #000; }
.tessel-lock .lock-date { font-size: 38px; color: white; text-shadow: 0 2px 5px #000; }
.tessel-lock .lock-name { font-size: 18px; font-weight: 600; letter-spacing: 5px; color: white; text-shadow: 0 2px 5px #000; }
.tessel-lock.lock-compact .lock-clock, .tessel-lock.lock-narrow .lock-clock { font-size: 104px; }
.tessel-lock.lock-compact .lock-date, .tessel-lock.lock-narrow .lock-date { font-size: 30px; }
.tessel-lock.lock-compact .lock-forecast-day { font-size: 18px; }
.tessel-lock.lock-compact .lock-forecast-high { font-size: 22px; }
.tessel-lock.lock-compact .lock-information .lock-caption { font-size: 16px; }
.tessel-lock.lock-compact .lock-temperature { font-size: 40px; }
.tessel-lock .lock-information { font-size: 20px; color: white; }
.tessel-lock .lock-location { font-size: 24px; font-weight: 600; }
.tessel-lock .lock-forecast-day { font-size: 20px; }
.tessel-lock .lock-forecast-high { font-size: 24px; font-weight: 600; }
.tessel-lock .lock-information .lock-caption { font-size: 18px; }
.tessel-lock .lock-media-title { font-size: 26px; font-weight: 600; }
.tessel-lock .album-cover { border-radius: 12px; }
.tessel-lock .album-player { padding: 4px; border-radius: 50%; background: rgba(20,24,30,.85); }
.tessel-lock .lock-information label { color: white; text-shadow: 0 2px 6px #000; }
.tessel-lock .lock-group-heading { font-size: 18px; font-weight: 500; letter-spacing: 1px; }
.tessel-lock .lock-temperature { font-size: 48px; font-weight: 600; }
.tessel-lock .lock-condition { font-size: 22px; }
.tessel-lock .lock-media-action { border-radius: 50%; padding: 0; min-width: 58px; min-height: 58px;
    color: white; background: rgba(25,28,34,.45); border: 1px solid rgba(255,255,255,.25); }
.tessel-lock .lock-media-action:hover { background: rgba(255,255,255,.25); }
.tessel-lock .lock-media-action:disabled { opacity: .5; }
.tessel-lock .settings-gear { min-width: 64px; min-height: 64px; border-radius: 20px;
  padding: 4px; color: white; background: rgba(20,32,37,.85); border: 1px solid rgba(255,255,255,.5); }
.tessel-lock .settings-gear:hover { background: rgba(50,73,70,.95); }
";

/// `config.json` next to the program (`ROOT / 'config.json'`).
pub fn default_config_path() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("config.json"))).unwrap_or_else(|| "config.json".into())
}

pub fn load_config(path: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match serde_json::from_str(&text).map_err(|e| e.to_string())? {
        Value::Object(map) => Ok(map),
        _ => Err("config.json must be an object".into()),
    }
}

/// Constructor options (`LunaClock(config=…, application_id=…, idle=…)` plus
/// test-only injections; production uses [`Options::new`] unchanged).
pub struct Options {
    pub config: Map<String, Value>,
    pub application_id: String,
    pub idle: bool,
    /// The admin web UI ([`panel::URL`] in production; a fake server in tests).
    pub panel_url: String,
    /// `False` = tests' `patch('screen_timers.ScreenTimers.poll')`.
    pub poll_schedules: bool,
    /// `False` = tests' `patch('admin_panel.AdminPanel.load')`.
    pub load_panel: bool,
    pub weather_store: Option<Arc<WeatherStore>>,
    pub weather_client: Option<Arc<dyn WeatherReader>>,
    pub scheduler_socket: PathBuf,
    /// `None` = udev discovery + monitor (production).
    pub keyboard_devices: Option<Vec<tessel_keyboard::devices::Device>>,
    pub launch: Option<Rc<dyn Fn(&str)>>,
}

impl Options {
    pub fn new(config: Map<String, Value>) -> Self {
        Self { config, application_id: APPLICATION_ID.into(), idle: true, panel_url: panel::URL.into(), poll_schedules: true,
               load_panel: true, weather_store: None, weather_client: None, scheduler_socket: schedules::socket_path(),
               keyboard_devices: None, launch: None }
    }
}

fn get<'a>(config: &'a Map<String, Value>, key: &str) -> Option<&'a Value> { config.get(key) }

fn flag(config: &Map<String, Value>, key: &str, default: bool) -> bool {
    get(config, key).map(tessel_core::pyjson::truthy).unwrap_or(default)
}

fn required<'a>(config: &'a Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    config.get(key).ok_or_else(|| format!("KeyError: '{key}'"))
}

pub struct WindowInner {
    pub window: gtk::ApplicationWindow,
    pub size: Cell<(i32, i32)>,
    pub voice_state: Cell<&'static str>,
    pub wallpaper: Wallpaper,
    pub root: gtk::Overlay,
    pub clock: gtk::Label,
    pub date: gtk::Label,
    pub assistant: gtk::Label,
    pub weather: WeatherView,
    pub media_row: gtk::Box,
    pub media_icon: gtk::Image,
    pub media_visual: gtk::Stack,
    pub album_cover: gtk::Image,
    pub album_player_icon: gtk::Image,
    pub media_title: gtk::Label,
    pub media_subtitle: gtk::Label,
    pub media_album: gtk::Label,
    pub media_button: gtk::Button,
    pub media_previous: gtk::Button,
    pub media_next: gtk::Button,
    pub dashboard: WallpaperDashboard,
    pub countdowns: ScreenTimers,
    pub alarms: ScreenTimers,
    pub schedule_summary: gtk::Box,
    pub voice: VoiceBand,
    pub levels: ScreenLevels,
    pub gear: gtk::Button,
    app: Weak<AppInner>,
}

/// `ClockWindow(app)`.
#[derive(Clone)]
pub struct ClockWindow(pub Rc<WindowInner>);

pub struct AppInner {
    pub app: gtk::Application,
    pub config: Map<String, Value>,
    pub idle_enabled: bool,
    options: RefCell<Option<Options>>,
    pub window: RefCell<Option<ClockWindow>>,
    idle_process: RefCell<Option<Child>>,
    pub last_dismissal: RefCell<Option<String>>,
    pub panel: RefCell<Option<AdminPanel>>,
    pub suppress_escape_until_release: Rc<Cell<bool>>,
    voice: RefCell<VoiceLog>,
    pub weather: LockWeather,
    pub media: LockMedia,
    pub media_icons: MediaIcons,
    pub album_art: AlbumArt,
    timers: RefCell<Vec<glib::SourceId>>,
    provider: RefCell<Option<gtk::CssProvider>>,
    /// Test hook replacing `app.media.current()` (Python `patch.object`).
    pub media_override: RefCell<Option<Box<dyn Fn() -> Option<Media>>>>,
    /// Test hook replacing `app.media.toggle()`.
    pub toggle_override: RefCell<Option<Box<dyn Fn()>>>,
}

/// `LunaClock(config=None, application_id='local.luna.Clock', idle=True)`.
#[derive(Clone)]
pub struct LunaClock(pub Rc<AppInner>);

fn app_up(weak: &Weak<AppInner>) -> Option<LunaClock> { weak.upgrade().map(LunaClock) }

impl ClockWindow {
    fn new(app: &LunaClock) -> Self {
        let inner = &app.0;
        let config = &inner.config;
        let window = gtk::ApplicationWindow::builder().application(&inner.app).title("Kasumi Clock").decorated(false).build();
        window.add_css_class("tessel-lock");
        window.set_default_size(1280, 720);
        let wallpaper = Wallpaper::new("", || {});
        let directory = tessel_locking::expanduser(Path::new(&tessel_core::pyjson::py_str(&config["wallpaper_directory"])));
        let pictures: Vec<String> = config["wallpapers"].as_array().into_iter().flatten()
            .map(|name| directory.join(tessel_core::pyjson::py_str(name)).to_string_lossy().into_owned()).collect();
        let blur_radius = get(config, "blur_radius").and_then(|v| tessel_core::pyjson::py_int(Some(v))).unwrap_or(0);
        let lock_config = LockConfig {
            wallpaper_mode: "slideshow".into(), slideshow: pictures, interval: tessel_core::pyjson::py_int(Some(&config["wallpaper_seconds"])).unwrap_or(300),
            blur: blur_radius > 0, blur_strength: blur_radius, ..LockConfig::default()
        };
        wallpaper.configure(&lock_config, 0);
        let root = gtk::Overlay::new();
        root.set_child(Some(&wallpaper));
        window.set_child(Some(&root));
        let clock = gtk::Label::builder().css_classes(["lock-clock"]).build();
        let date = gtk::Label::builder().css_classes(["lock-date"]).build();
        let name = get(config, "assistant_name").map(tessel_core::pyjson::py_str).unwrap_or_else(|| "Kasumi".into());
        let assistant = gtk::Label::builder().label(name.to_uppercase()).css_classes(["lock-name"]).build();
        let weather = WeatherView::new();
        let media_row = gtk::Box::builder().spacing(18).halign(gtk::Align::Center).visible(false).build();
        let media_icon = image("multimedia-player", 64);
        let media_visual = gtk::Stack::builder().valign(gtk::Align::Center).build();
        media_visual.add_named(&media_icon, Some("player"));
        let album_cover = image("multimedia-player", 128);
        let cover = gtk::Overlay::builder().child(&album_cover).css_classes(["album-cover"]).build();
        cover.set_overflow(gtk::Overflow::Hidden);
        let album_player_icon = image("multimedia-player", 28);
        album_player_icon.set_halign(gtk::Align::End);
        album_player_icon.set_valign(gtk::Align::End);
        album_player_icon.add_css_class("album-player");
        cover.add_overlay(&album_player_icon);
        media_visual.add_named(&cover, Some("album"));
        media_visual.set_hhomogeneous(false);
        media_visual.set_vhomogeneous(false);
        media_row.append(&media_visual);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let ellipsized = |chars: i32| gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(chars).xalign(0.0);
        let media_title = ellipsized(40).css_classes(["lock-media-title"]).build();
        let media_subtitle = ellipsized(48).css_classes(["lock-caption"]).build();
        text.append(&media_title);
        text.append(&media_subtitle);
        let media_album = ellipsized(44).visible(false).css_classes(["lock-caption"]).build();
        text.append(&media_album);
        media_row.append(&text);
        let media_button = gtk::Button::builder().child(&image("media-playback-start-symbolic", 28)).valign(gtk::Align::Center)
            .css_classes(["lock-media-action"]).build();
        let controls = gtk::Box::builder().spacing(10).valign(gtk::Align::Center).build();
        let media_previous = gtk::Button::builder().child(&image("media-skip-backward-symbolic", 26)).tooltip_text(tr!("Previous track"))
            .css_classes(["lock-media-action"]).build();
        let media_next = gtk::Button::builder().child(&image("media-skip-forward-symbolic", 26)).tooltip_text(tr!("Next track"))
            .css_classes(["lock-media-action"]).build();
        for (button, title) in [(&media_previous, "Previous track"), (&media_button, "Play"), (&media_next, "Next track")] {
            button.update_property(&[gtk::accessible::Property::Label(&tessel_core::i18n::translate(title))]);
            controls.append(button);
        }
        media_row.append(&controls);
        let dashboard = WallpaperDashboard::new(&clock, &date, &weather, &media_row, &assistant, None);
        let options = inner.options.borrow();
        let socket = options.as_ref().map(|o| o.scheduler_socket.clone()).unwrap_or_else(schedules::socket_path);
        let countdowns = ScreenTimers::timers(socket.clone());
        dashboard.hero().insert_child_after(countdowns.widget(), Some(dashboard.time_group()));
        let alarms = ScreenTimers::alarms(socket);
        let schedule_summary = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).visible(false).build();
        let compact = countdowns.compact().expect("compact view").widget().clone();
        for widget in [compact.clone(), alarms.widget().clone()] {
            schedule_summary.append(&widget);
            let (summary, compact, alarm_widget) = (schedule_summary.downgrade(), compact.downgrade(), alarms.widget().downgrade());
            widget.connect_visible_notify(move |_| {
                if let (Some(summary), Some(compact), Some(alarm_widget)) = (summary.upgrade(), compact.upgrade(), alarm_widget.upgrade()) {
                    summary.set_visible(compact.get_visible() || alarm_widget.get_visible());
                }
            });
        }
        dashboard.hero().insert_child_after(&schedule_summary, Some(countdowns.widget()));
        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(dashboard.widget()));
        root.add_overlay(&scroll);
        let voice = VoiceBand::new();
        root.add_overlay(voice.widget());
        voice.widget().set_visible(flag(config, "show_voice", true));
        let levels_path = get(config, "levels_state").map(tessel_core::pyjson::py_str)
            .unwrap_or_else(|| "~/.config/kasumi-screen/levels.json".into());
        let levels = ScreenLevels::new(&root, Path::new(&levels_path));
        let gear = gtk::Button::builder().child(&image("emblem-system-symbolic", 34)).halign(gtk::Align::End).valign(gtk::Align::End)
            .margin_end(24).margin_bottom(24).css_classes(["settings-gear"]).build();
        gear.set_tooltip_text(Some("Open Kasumi control panel"));
        root.add_overlay(&gear);
        // Wallpaper implementations can be DrawingArea or snapshot widgets.
        // Observe the overlay allocation without depending on either API.
        let size_sensor = gtk::DrawingArea::builder().hexpand(true).vexpand(true).can_target(false).opacity(0.0).build();
        root.add_overlay(&size_sensor);
        drop(options);
        let this = Self(Rc::new(WindowInner {
            window: window.clone(), size: Cell::new((0, 0)), voice_state: Cell::new("idle"), wallpaper, root, clock, date, assistant,
            weather, media_row, media_icon, media_visual, album_cover, album_player_icon, media_title, media_subtitle, media_album,
            media_button: media_button.clone(), media_previous: media_previous.clone(), media_next: media_next.clone(), dashboard,
            countdowns, alarms, schedule_summary, voice, levels, gear: gear.clone(), app: Rc::downgrade(&app.0),
        }));
        let weak = Rc::downgrade(&app.0);
        media_button.connect_clicked(move |_| if let Some(app) = app_up(&weak) { app.toggle_media() });
        let weak = Rc::downgrade(&app.0);
        media_previous.connect_clicked(move |_| if let Some(app) = app_up(&weak) { app.0.media.previous() });
        let weak = Rc::downgrade(&app.0);
        media_next.connect_clicked(move |_| if let Some(app) = app_up(&weak) { app.0.media.next() });
        let weak = Rc::downgrade(&app.0);
        gear.connect_clicked(move |_| if let Some(app) = app_up(&weak) { app.open_panel() });
        let weak = Rc::downgrade(&this.0);
        size_sensor.connect_resize(move |_, width, height| if let Some(w) = weak.upgrade() { ClockWindow(w).resized(width, height) });
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this.0);
        key.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
            Some(w) if ClockWindow(w.clone()).key_press(key) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(&this.0);
        key.connect_key_released(move |_, key, _, _| if let Some(w) = weak.upgrade() { ClockWindow(w).key_release(key) });
        window.add_controller(key);
        window.connect_close_request(|_| glib::Propagation::Stop);
        this.tick();
        this
    }

    fn app(&self) -> Option<LunaClock> { app_up(&self.0.app) }
    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }

    fn resized(&self, width: i32, height: i32) {
        self.0.size.set((width, height));
        self.0.dashboard.configure(width, height);
        for (css, enabled) in [("lock-compact", height < 900), ("lock-narrow", width < 900)] {
            if enabled { self.0.window.add_css_class(css) } else { self.0.window.remove_css_class(css) }
        }
    }

    /// The window's key controller (CAPTURE); returns true when handled.
    pub fn key_press(&self, key: gtk::gdk::Key) -> bool {
        use gtk::gdk::Key;
        let Some(app) = self.app() else { return false };
        let action = match key {
            Key::AudioRaiseVolume => Some(("volume", "up")),
            Key::AudioLowerVolume => Some(("volume", "down")),
            Key::AudioMute => Some(("volume", "mute")),
            Key::MonBrightnessUp => Some(("brightness", "up")),
            Key::MonBrightnessDown => Some(("brightness", "down")),
            _ => None,
        };
        if let Some((kind, action)) = action {
            let _ = if kind == "volume" { self.0.levels.adjust_volume(action) } else { self.0.levels.adjust_brightness(action) };
            return true;
        }
        if key == Key::Escape {
            if app.0.suppress_escape_until_release.get() {
                return true;
            }
            let panel = app.0.panel.borrow().clone();
            if let Some(panel) = panel.filter(|p| p.get_visible()) {
                app.0.suppress_escape_until_release.set(true);
                panel.dismiss();
                return true;
            }
            self.dismiss("escape");
            return true;
        }
        false
    }

    pub fn key_release(&self, key: gtk::gdk::Key) {
        if key == gtk::gdk::Key::Escape {
            if let Some(app) = self.app() {
                app.0.suppress_escape_until_release.set(false);
            }
        }
    }

    pub fn reveal(&self) {
        let Some(app) = self.app() else { return };
        let panel = app.0.panel.borrow().clone();
        if let Some(panel) = panel.filter(|p| p.get_visible()) {
            panel.reveal();
            return;
        }
        self.0.window.fullscreen();
        self.0.window.present();
        app.0.last_dismissal.replace(None);
        self.tick();
        app.information_changed();
        crate::log("INFO", "Dashboard shown");
    }

    pub fn dismiss(&self, reason: &str) {
        self.0.window.set_visible(false);
        self.0.voice.close();
        if let Some(app) = self.app() {
            app.0.last_dismissal.replace(Some(reason.to_owned()));
            let idle = app.0.config.get("idle_seconds").map(tessel_core::pyjson::py_str).unwrap_or_default();
            crate::log("INFO", &format!("Desktop shown ({reason}); return after {idle}s idle"));
        }
    }

    pub fn tick(&self) {
        if !self.0.window.get_visible() {
            return;
        }
        let Some(app) = self.app() else { return };
        let Ok(now) = glib::DateTime::now_local() else { return };
        let h24 = flag(&app.0.config, "clock_24_hour", false);
        let mut clock = now.format(if h24 { "%H:%M" } else { "%I:%M %p" }).map(|s| s.to_string()).unwrap_or_default();
        if !h24 {
            clock = clock.trim_start_matches('0').to_owned();
        }
        self.0.clock.set_text(&clock);
        let part = |f: &str| now.format(f).map(|s| s.to_string()).unwrap_or_default();
        self.0.date.set_text(&format!("{}, {} {}, {}", part("%A"), part("%B"), now.day_of_month(), now.year()));
        self.0.wallpaper.refresh();
    }

    pub fn information_changed(&self) {
        let Some(app) = self.app() else { return };
        let config = &app.0.config;
        let show_weather = flag(config, "show_weather", true);
        self.0.weather.update(&app.0.weather, show_weather, show_weather);
        let show_media = flag(config, "show_media", true);
        let media = if show_media { app.current_media() } else { None };
        let inner = &self.0;
        inner.media_row.set_visible(media.is_some() && show_media);
        let album = album_name(media.as_ref());
        let art = app.0.album_art.resolve(media.as_ref());
        inner.media_album.set_text(&album);
        inner.media_album.set_tooltip_text((!album.is_empty()).then_some(album.as_str()));
        inner.media_album.set_visible(!album.is_empty());
        inner.media_visual.set_visible_child_name(if art.is_some() { "album" } else { "player" });
        match &art {
            Some(art) => inner.album_cover.set_from_gicon(art),
            None => inner.album_cover.clear(),
        }
        if let Some(media) = media {
            let icon = app.0.media_icons.resolve(Some(&media));
            inner.media_icon.set_from_gicon(&icon);
            inner.album_player_icon.set_from_gicon(&icon);
            inner.media_title.set_text(&media.title);
            let state = if media.playing { tr!("Playing") } else { tr!("Paused") };
            let subtitle = if media.error.is_empty() {
                [media.artist.clone(), state].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
            } else {
                media.error.clone()
            };
            inner.media_subtitle.set_text(&subtitle);
            if let Some(child) = inner.media_button.child().and_downcast::<gtk::Image>() {
                child.set_icon_name(Some(if media.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
            }
            let action = if media.playing { tr!("Pause") } else { tr!("Play") };
            inner.media_button.set_tooltip_text(Some(&action));
            inner.media_button.update_property(&[gtk::accessible::Property::Label(&action)]);
            inner.media_previous.set_sensitive(media.can_previous && !media.busy);
            inner.media_next.set_sensitive(media.can_next && !media.busy);
            inner.media_button.set_sensitive(media.controllable && !media.busy);
        }
    }

    pub fn close_resources(&self) {
        self.0.countdowns.close();
        self.0.alarms.close();
        self.0.levels.close();
        self.0.voice.close();
        self.0.wallpaper.close();
    }
}

impl LunaClock {
    pub fn new(options: Options) -> Result<Self, String> {
        for key in ["wallpaper_directory", "wallpapers", "wallpaper_seconds", "idle_seconds", "voice_log"] {
            required(&options.config, key)?;
        }
        let app = gtk::Application::builder().application_id(&options.application_id)
            .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
        let config = options.config.clone();
        let voice_log = tessel_core::pyjson::py_str(&config["voice_log"]);
        let (store, client) = (options.weather_store.clone(), options.weather_client.clone());
        let inner = Rc::new_cyclic(|weak: &Weak<AppInner>| {
            let changed = |weak: Weak<AppInner>| move || if let Some(app) = app_up(&weak) { app.information_changed() };
            AppInner {
                app: app.clone(), idle_enabled: options.idle, config, options: RefCell::new(None), window: RefCell::new(None),
                idle_process: RefCell::new(None), last_dismissal: RefCell::new(None), panel: RefCell::new(None),
                suppress_escape_until_release: Rc::new(Cell::new(false)), voice: RefCell::new(VoiceLog::new(&voice_log)),
                weather: LockWeather::new(changed(weak.clone()), store, client), media: LockMedia::new(changed(weak.clone()), false),
                media_icons: MediaIcons::default(), album_art: AlbumArt::new(changed(weak.clone()), None), timers: RefCell::new(Vec::new()),
                provider: RefCell::new(None), media_override: RefCell::new(None), toggle_override: RefCell::new(None),
            }
        });
        inner.options.replace(Some(options));
        let clock = Self(inner);
        let weak = Rc::downgrade(&clock.0);
        app.connect_startup(move |_| if let Some(app) = app_up(&weak) { app.startup() });
        let weak = Rc::downgrade(&clock.0);
        app.connect_command_line(move |_, command_line| match app_up(&weak) {
            Some(app) => app.command_line(command_line),
            None => glib::ExitCode::FAILURE,
        });
        let weak = Rc::downgrade(&clock.0);
        app.connect_shutdown(move |_| if let Some(app) = app_up(&weak) { app.shutdown() });
        Ok(clock)
    }

    pub fn application(&self) -> &gtk::Application { &self.0.app }
    pub fn window(&self) -> Option<ClockWindow> { self.0.window.borrow().clone() }
    pub fn panel(&self) -> Option<AdminPanel> { self.0.panel.borrow().clone() }
    pub fn idle_running(&self) -> bool {
        self.0.idle_process.borrow_mut().as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn current_media(&self) -> Option<Media> {
        if let Some(current) = self.0.media_override.borrow().as_ref() {
            return current();
        }
        self.0.media.current()
    }

    fn toggle_media(&self) {
        if let Some(toggle) = self.0.toggle_override.borrow().as_ref() {
            return toggle();
        }
        self.0.media.toggle();
    }

    fn startup(&self) {
        let inner = &self.0;
        // Hold: the dashboard keeps running while hidden.
        std::mem::forget(inner.app.hold());
        let css = [STYLE, levels::CSS, schedules::TIMER_CSS, schedules::ALARM_CSS, panel::CSS].concat();
        inner.provider.replace(Some(tessel_gtk::utils::add_css(&css, None, tessel_gtk::utils::PRIORITY_APPLICATION)));
        let window = ClockWindow::new(self);
        inner.window.replace(Some(window.clone()));
        let poll_schedules = inner.options.borrow().as_ref().is_none_or(|o| o.poll_schedules);
        window.0.levels.refresh();
        if poll_schedules {
            window.0.countdowns.poll();
            window.0.alarms.poll();
        }
        if inner.idle_enabled {
            self.start_idle_monitor();
        }
        inner.media.start(false);
        self.poll_weather();
        let mut timers = Vec::new();
        let every = |ms: u64, f: Box<dyn Fn()>| glib::timeout_add_local(Duration::from_millis(ms), move || { f(); glib::ControlFlow::Continue });
        let weak = Rc::downgrade(&self.0);
        let w = move || app_up(&weak);
        let w1 = w.clone();
        timers.push(every(150, Box::new(move || if let Some(app) = w1() { app.poll_voice() })));
        let w1 = w.clone();
        timers.push(every(1000, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.tick() })));
        let w1 = w.clone();
        timers.push(every(10_000, Box::new(move || if let Some(app) = w1() { app.check_idle_monitor() })));
        let w1 = w.clone();
        timers.push(every(5000, Box::new(move || if let Some(app) = w1() { app.poll_weather() })));
        let w1 = w.clone();
        timers.push(every(1000, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.0.levels.refresh() })));
        if poll_schedules {
            let w1 = w.clone();
            timers.push(every(1000, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.0.countdowns.poll() })));
            let w1 = w.clone();
            timers.push(every(1000, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.0.alarms.poll() })));
        }
        let w1 = w.clone();
        timers.push(every(250, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.0.countdowns.tick() })));
        let w1 = w.clone();
        timers.push(every(1000, Box::new(move || if let Some(win) = w1().and_then(|a| a.window()) { win.0.alarms.tick() })));
        for signal in [libc::SIGTERM, libc::SIGINT] {
            let w1 = w.clone();
            timers.push(tessel_core::glib_compat::unix_signal_add_local(signal, move || {
                if let Some(app) = w1() {
                    app.0.app.quit();
                }
                glib::ControlFlow::Continue
            }));
        }
        inner.timers.replace(timers);
    }

    fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let args: Vec<String> = command_line.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let Some(window) = self.window() else { return glib::ExitCode::FAILURE };
        match args.as_slice() {
            [] | ["--show"] => window.reveal(),
            ["--hide"] => window.dismiss("command"),
            ["--panel"] => self.open_panel(),
            ["--close-panel"] => {
                if let Some(panel) = self.panel() {
                    panel.dismiss();
                }
            }
            ["--next-wallpaper"] => {
                window.0.wallpaper.set_next_rotation(0.0);
                window.0.wallpaper.refresh();
            }
            [kind @ ("--volume" | "--brightness"), value] => {
                let result = if *kind == "--volume" { window.0.levels.adjust_volume(value) } else { window.0.levels.adjust_brightness(value) };
                if let Err(error) = result {
                    command_line.printerr_literal(&format!("{error}\n"));
                    return glib::ExitCode::from(2);
                }
            }
            ["--status"] => command_line.print_literal(&format!("{}\n", json_dumps(&self.status(), &JsonStyle::DEFAULT))),
            ["--quit"] => self.0.app.quit(),
            _ => {
                command_line.printerr_literal(USAGE);
                return glib::ExitCode::from(2);
            }
        }
        glib::ExitCode::SUCCESS
    }

    /// The `--status` object (exact keys and order).
    pub fn status(&self) -> Value {
        let window = self.window().expect("window");
        let w = &window.0;
        let panel = self.panel();
        let (width, height) = w.size.get();
        let mut map = Map::new();
        let mut put = |key: &str, value: Value| { map.insert(key.into(), value); };
        put("visible", w.window.get_visible().into());
        put("voice", w.voice_state.get().into());
        put("wallpaper", w.wallpaper.current_path().map(|p| Value::from(p.to_string_lossy().into_owned())).unwrap_or(Value::Null));
        put("size", json!([width, height]));
        put("idle_seconds", self.0.config.get("idle_seconds").cloned().unwrap_or(Value::Null));
        put("last_dismissal", self.0.last_dismissal.borrow().clone().map(Value::from).unwrap_or(Value::Null));
        put("idle_monitor_running", self.idle_running().into());
        put("weather_visible", w.weather.get_visible().into());
        put("location", self.0.weather.config().map(|c| Value::from(c.name)).unwrap_or(Value::Null));
        put("media_visible", w.media_row.get_visible().into());
        put("gtk_version", 4.into());
        put("media_controls", json!(["previous", "play-pause", "next"]));
        put("media_album_visible", (w.media_row.get_visible() && w.media_album.get_visible()).into());
        put("media_art_visible", (w.media_row.get_visible() && w.media_visual.visible_child_name().as_deref() == Some("album")).into());
        put("levels", w.levels.status());
        put("timer_countdowns", Value::Array(w.countdowns.status().into_iter().map(Value::Object).collect()));
        put("compact_timers", Value::Array(w.countdowns.compact().map(|c| c.status()).unwrap_or_default().into_iter().map(Value::Object).collect()));
        put("today_alarms", Value::Array(w.alarms.status().into_iter().map(Value::Object).collect()));
        put("panel_visible", panel.as_ref().is_some_and(|p| p.get_visible()).into());
        put("panel_focused", panel.as_ref().is_some_and(|p| p.is_active()).into());
        put("keyboard", panel.as_ref().map(|p| p.keyboard().status()).unwrap_or_else(|| json!({"visible": false})));
        put("clock_focused", w.window.is_active().into());
        Value::Object(map)
    }

    pub fn open_panel(&self) {
        let Some(window) = self.window() else { return };
        if self.panel().is_none() {
            let options = self.0.options.borrow();
            let url = options.as_ref().map(|o| o.panel_url.clone()).unwrap_or_else(|| panel::URL.into());
            let devices = options.as_ref().and_then(|o| o.keyboard_devices.clone());
            let watch = devices.is_none();
            let launch = options.as_ref().and_then(|o| o.launch.clone());
            let load = options.as_ref().is_none_or(|o| o.load_panel);
            drop(options);
            let weak = Rc::downgrade(&self.0);
            let hooks = Hooks {
                dismissed: Box::new(move || if let Some(win) = app_up(&weak).and_then(|a| a.window()) {
                    win.reveal();
                    win.0.gear.grab_focus();
                }),
                suppress_escape: self.0.suppress_escape_until_release.clone(),
                launch: Box::new(move |uri| match &launch { Some(f) => f(uri), None => panel::launch_default(uri) }),
            };
            let created = AdminPanel::new(&self.0.app, window.window().upcast_ref(), &url, load, hooks, devices, watch);
            self.0.panel.replace(Some(created));
        }
        if !window.window().get_visible() {
            window.reveal();
        }
        if let Some(panel) = self.panel() {
            panel.reveal();
        }
    }

    fn start_idle_monitor(&self) {
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "luna-clock".into());
        let command = shell_join(&[&exe, "--show"]);
        let idle = self.0.config.get("idle_seconds").map(tessel_core::pyjson::py_str).unwrap_or_default();
        match Command::new("swayidle").args(["-w", "-C", "/dev/null", "timeout", &idle, &command]).spawn() {
            Ok(child) => {
                self.0.idle_process.replace(Some(child));
            }
            Err(error) => crate::log("ERROR", &format!("Could not start swayidle: {error}")),
        }
    }

    fn check_idle_monitor(&self) {
        let exited = self.0.idle_process.borrow_mut().as_mut().is_some_and(|child| !matches!(child.try_wait(), Ok(None)));
        if self.0.idle_enabled && exited {
            crate::log("WARNING", "Restarting idle monitor");
            self.start_idle_monitor();
        }
    }

    pub fn information_changed(&self) {
        if let Some(window) = self.window() {
            window.information_changed();
        }
    }

    pub fn poll_weather(&self) { self.0.weather.poll(true); }

    fn poll_voice(&self) {
        let state = self.0.voice.borrow_mut().poll();
        if let Some(window) = self.window() {
            window.0.voice_state.set(state);
            if window.window().get_visible() {
                window.0.voice.update(state);
            }
        }
    }

    fn shutdown(&self) {
        let inner = &self.0;
        if let Some(panel) = inner.panel.borrow_mut().take() {
            panel.close_resources();
            panel.window().destroy();
        }
        for id in inner.timers.borrow_mut().drain(..) {
            id.remove();
        }
        inner.weather.close();
        inner.media.close();
        inner.media_icons.close();
        inner.album_art.close();
        if let Some(window) = self.window() {
            window.close_resources();
        }
        if let Some(mut child) = inner.idle_process.borrow_mut().take() {
            if matches!(child.try_wait(), Ok(None)) {
                // SAFETY: plain kill(2) on our own child.
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
        }
        if let Some(provider) = inner.provider.borrow_mut().take() {
            if let Some(display) = gtk::gdk::Display::default() {
                tessel_gtk::utils::remove_css(&display, &provider);
            }
        }
    }

    /// Test teardown (`app.do_shutdown(); app.window.destroy()`).
    pub fn teardown(&self) {
        self.shutdown();
        if let Some(window) = self.0.window.borrow_mut().take() {
            window.window().destroy();
        }
    }
}

/// `shlex.join(args)`.
pub fn shell_join(args: &[&str]) -> String {
    args.iter().map(|arg| {
        if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
            (*arg).to_owned()
        } else {
            format!("'{}'", arg.replace('\'', "'\"'\"'"))
        }
    }).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_join_matches_shlex() {
        assert_eq!(shell_join(&["/usr/bin/python3", "/home/k/makima/screensaver/luna_clock.py", "--show"]),
                   "/usr/bin/python3 /home/k/makima/screensaver/luna_clock.py --show");
        assert_eq!(shell_join(&["/a b/luna-clock", "it's"]), "'/a b/luna-clock' 'it'\"'\"'s'");
        assert_eq!(shell_join(&[""]), "''");
    }

    #[test]
    fn missing_required_config_keys_are_fatal() {
        let config: Map<String, Value> = serde_json::from_str(r#"{"wallpaper_directory": "/x", "wallpapers": []}"#).unwrap();
        assert_eq!(LunaClock::new(Options::new(config)).err().unwrap(), "KeyError: 'wallpaper_seconds'");
    }
}
