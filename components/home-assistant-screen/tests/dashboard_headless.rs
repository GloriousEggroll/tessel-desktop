//! test_dashboard.py → Rust, plus end-to-end CLI / fake-endpoint checks.
//! Runs ONLY inside the private headless labwc of tests/run_headless.sh
//! (private bus, fake HOME, fake pactl/wpctl/systemctl/swayidle on PATH, fake
//! scheduler socket, fake HTTP server on a random 127.0.0.1 port). Never a
//! real Home Assistant / admin endpoint or token.
#[path = "../../media/tests/support/fake_player.rs"]
mod fake_player;

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glib::variant::ToVariant;
use gtk::prelude::*;
use kasumi_screen::clock::{LunaClock, Options};
use kasumi_screen::webkit;
use serde_json::{json, Map, Value};
use tessel_locking::media::Media;
use tessel_locking::weather::{WeatherReader, WeatherStore};
use tessel_weather::backend::{week_dates, CurrentWeather, ForecastDay, WeatherConfig, WeatherSnapshot};
use tessel_weather::dates::Date;

fn spin(mut predicate: impl FnMut() -> bool, seconds: f64) -> bool {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    predicate()
}

fn wait(seconds: f64) { spin(|| false, seconds); }

fn wall() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() }

struct FakeWeather(Mutex<Vec<WeatherConfig>>);

impl WeatherReader for FakeWeather {
    fn read(&self, config: &WeatherConfig) -> Option<WeatherSnapshot> {
        self.0.lock().unwrap().push(config.clone());
        let days = week_dates(Date::new(2026, 9, 24).unwrap(), "upcoming").unwrap().into_iter()
            .map(|day| ForecastDay { date: day, temp_min: Some(55.0), temp_max: Some(75.0), code: Some(0), precip_probability: None, past: false })
            .collect();
        Some(WeatherSnapshot { config: config.clone(), current: Some(CurrentWeather { time: "12:00".into(), temp: 72.0, code: 0, is_day: true,
            apparent: None, humidity: None, wind: None }), days, fetched_at: Some(wall()), stale: false, error: None })
    }
}

struct Fixture {
    app: LunaClock,
    store: Arc<WeatherStore>,
    weather: Arc<FakeWeather>,
    tmp: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.teardown();
        wait(0.1);
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("KASUMI_TEST_TMP").unwrap()).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str, id: &str, levels: bool, change: impl FnOnce(&mut Options)) -> Fixture {
    let tmp = tmpdir(name);
    let t = tmp.to_string_lossy().into_owned();
    let mut config: Map<String, Value> = serde_json::from_value(json!({"wallpaper_directory": t, "wallpapers": [], "wallpaper_seconds": 300,
        "idle_seconds": 60, "voice_log": format!("{t}/voice.log")})).unwrap();
    if levels {
        config.insert("levels_state".into(), json!(format!("{t}/levels.json")));
    }
    let store = Arc::new(WeatherStore::new(Some(tmp.join("weather.json"))));
    let weather = Arc::new(FakeWeather(Mutex::new(Vec::new())));
    let mut options = Options::new(config);
    options.application_id = id.into();
    options.idle = false;
    options.weather_store = Some(store.clone());
    options.weather_client = Some(weather.clone());
    options.scheduler_socket = tmp.join("no-scheduler.sock");
    options.keyboard_devices = Some(Vec::new());
    change(&mut options);
    let app = LunaClock::new(options).unwrap();
    app.application().register(gio::Cancellable::NONE).unwrap();
    Fixture { app, store, weather, tmp }
}

macro_rules! check {
    ($cond:expr) => { if !$cond { return Err(format!("{}:{}: {}", file!(), line!(), stringify!($cond))) } };
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!("{}:{}: {} — {}", file!(), line!(), stringify!($cond), format!($($msg)+))) } };
}

type Outcome = Result<(), String>;

fn png(path: &Path, size: i32, color: u32) {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, size, size).unwrap();
    pixbuf.fill(color);
    pixbuf.savev(path, "png", &[]).unwrap();
}

#[allow(deprecated)]
fn allocation(widget: &impl IsA<gtk::Widget>) -> gtk::Allocation { widget.allocation() }

// test_music_album_controls_video_fallback_and_player_exit
fn music_album_controls_video_fallback_and_player_exit() -> Outcome {
    let f = fixture("album", "org.example.KasumiAlbumTest", true, |o| o.poll_schedules = false);
    let cover = f.tmp.join("album.png");
    png(&cover, 512, 0x527eaaff);
    let cover_uri = glib::filename_to_uri(&cover, None).unwrap().to_string();
    let player = fake_player::FakePlayer::new("AlbumTest", fake_player::Options { desktop_entry: "spotify", ..Default::default() });
    let metadata = |overrides: &[(&str, &str)]| {
        let mut items: Vec<(String, glib::Variant)> = vec![
            ("xesam:title".into(), "A song with a long title to test the dashboard layout".to_variant()),
            ("xesam:album".into(), "The Album Name".to_variant()), ("mpris:artUrl".into(), cover_uri.to_variant()),
            ("xesam:url".into(), "spotify:track:fixture".to_variant()), ("xesam:artist".into(), vec!["Fixture artist".to_owned()].to_variant())];
        for (key, value) in overrides {
            items.retain(|(k, _)| k != key);
            items.push(((*key).into(), value.to_variant()));
        }
        let refs: Vec<(&str, glib::Variant)> = items.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        fake_player::metadata(&refs)
    };
    let window = f.app.window().unwrap();
    window.reveal();
    player.change(&[("Metadata", metadata(&[]))]);
    let w = window.0.clone();
    check!(spin(|| w.media_visual.visible_child_name().as_deref() == Some("album"), 4.0));
    check!(w.media_row.get_visible() && w.media_album.get_visible());
    check!(w.media_album.text() == "The Album Name");
    check!(w.album_cover.gicon().is_some() && w.album_player_icon.gicon().is_some());
    check!(player.calls().is_empty());
    for (button, method) in [(&w.media_previous, "Previous"), (&w.media_button, "Pause"), (&w.media_button, "Play"), (&w.media_next, "Next")] {
        button.emit_clicked();
        check!(spin(|| player.calls().last().map(String::as_str) == Some(method), 2.0), "{method} {:?}", player.calls());
        check!(spin(|| f.app.0.media.current().is_some_and(|m| !m.busy), 2.0));
    }
    wait(0.3);
    let bounds = w.media_row.compute_bounds(&window.0.window).ok_or("bounds")?;
    check!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= window.0.window.width() as f32);
    player.change(&[("CanGoNext", false.to_variant())]);
    check!(spin(|| !w.media_next.is_sensitive(), 2.0));
    player.change(&[("Metadata", metadata(&[("xesam:url", "file:///movie.mp4")]))]);
    check!(spin(|| !w.media_album.get_visible(), 2.0));
    check!(w.media_visual.visible_child_name().as_deref() == Some("player"));
    check!(w.album_cover.gicon().is_none());
    player.change(&[("Metadata", metadata(&[("mpris:artUrl", "file:///missing-cover.png")]))]);
    check!(spin(|| w.media_album.get_visible(), 2.0));
    check!(w.media_visual.visible_child_name().as_deref() == Some("player"));
    player.change(&[("PlaybackStatus", "Stopped".to_variant())]);
    check!(spin(|| !w.media_row.get_visible(), 2.0));
    check!(!w.media_album.get_visible());
    player.close();
    Ok(())
}

fn javascript(web: &webkit::WebView, code: &str) -> Result<String, String> {
    let result: Rc<RefCell<Option<Result<String, String>>>> = Rc::default();
    let sink = result.clone();
    web.evaluate_javascript(code, move |value| {
        sink.replace(Some(value));
    });
    if !spin(|| result.borrow().is_some(), 3.0) {
        return Err("javascript timed out".into());
    }
    result.take().unwrap()
}

fn wait_loaded(web: &webkit::WebView) -> Rc<RefCell<Vec<i32>>> {
    let loaded: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = loaded.clone();
    web.connect_load_changed(move |event| sink.borrow_mut().push(event));
    loaded
}

// test_embedded_keyboard_types_in_web_panel_and_hides_without_losing_field
fn embedded_keyboard_types_in_web_panel_and_hides_without_losing_field() -> Outcome {
    let f = fixture("keyboard", "org.example.KasumiKeyboardTest", false, |o| { o.poll_schedules = false; o.load_panel = false; });
    let window = f.app.window().unwrap();
    window.reveal();
    wait(0.3);
    f.app.open_panel();
    let panel = f.app.panel().ok_or("panel")?;
    let keys = panel.keyboard().clone();
    let web = panel.web().ok_or("web")?.clone();
    let loaded = wait_loaded(&web);
    web.load_html("<html><body><input id=\"field\"><input id=\"second\"></body></html>", Some("http://127.0.0.1:8765/"));
    check!(spin(|| loaded.borrow().contains(&webkit::LOAD_FINISHED), 8.0));
    keys.button().emit_clicked();
    check!(spin(|| keys.ready(), 3.0), "{}", keys.error());
    javascript(&web, "document.getElementById(\"field\").focus(); \"ok\"")?;
    wait(0.3);
    keys.widget().character_button(30).ok_or("key 30")?.emit_clicked();
    keys.widget().modifier(1);
    keys.widget().press(48);
    keys.widget().character("€");
    wait(0.3);
    let value = javascript(&web, "document.getElementById(\"field\").value")?;
    check!(value == "aB€", "{value:?}");
    check!(panel.is_active());
    keys.widget().hide_button().emit_clicked();
    wait(0.2);
    check!(!keys.widget().root().get_visible());
    keys.devices_changed(&[tessel_keyboard::devices::Device::new("Touch", "touch", "touchscreen")]);
    wait(0.5);
    javascript(&web, "document.getElementById(\"second\").focus(); \"ok\"")?;
    wait(0.3);
    check!(keys.widget().root().get_visible(), "Text-field focus should open the keyboard on touch-only hardware");
    keys.widget().hide_button().emit_clicked();
    wait(0.2);
    check!(!keys.widget().root().get_visible(), "Hide should stick for the current text field");
    panel.dismiss();
    wait(0.2);
    check!(keys.bridge().is_none());
    check!(window.0.window.get_visible());
    Ok(())
}

// test_panel_focus_escape_and_click_lifecycle
fn panel_focus_escape_and_click_lifecycle() -> Outcome {
    let f = fixture("panel", "org.example.KasumiPanelTest", false, |o| { o.poll_schedules = false; o.load_panel = false; });
    // Keep a virtual keyboard attached throughout focus assertions.
    let mut keyboard = Command::new("wtype").args(["-k", "Shift_L", "-s", "20000"]).spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let window = f.app.window().unwrap();
        window.reveal();
        wait(0.3);
        let controllers = window.0.root.observe_controllers();
        check!(!(0..controllers.n_items()).any(|i| controllers.item(i).is_some_and(|c| c.is::<gtk::GestureClick>())));
        window.0.gear.emit_clicked();
        let panel = f.app.panel().ok_or("panel")?;
        panel.web().ok_or("web")?.load_html("<html><body><h1>Control panel fixture</h1><button>Test control</button></body></html>",
                                            Some("http://127.0.0.1:8765/"));
        wait(0.3);
        let pw = panel.window().clone();
        check!(spin(|| pw.is_active() && pw.is_fullscreen(), 3.0), "active={} fullscreen={} clock_active={}", pw.is_active(),
               pw.is_fullscreen(), window.0.window.is_active());
        check!(window.0.window.get_visible());
        check!(!pw.is_decorated());
        check!(pw.is_modal());
        check!(pw.transient_for().as_ref() == Some(window.0.window.upcast_ref()));
        window.reveal();
        wait(0.1);
        check!(pw.is_active(), "Idle timeout must not steal focus from panel");
        panel.key_press(gtk::gdk::Key::Escape);
        check!(spin(|| window.0.window.is_active(), 3.0));
        check!(!pw.get_visible() && window.0.window.get_visible());
        window.key_press(gtk::gdk::Key::Escape);
        check!(window.0.window.get_visible(), "Holding Escape must not also dismiss the clock");
        window.key_release(gtk::gdk::Key::Escape);
        window.0.gear.emit_clicked();
        wait(0.2);
        panel.close_button().emit_clicked();
        wait(0.2);
        check!(window.0.window.is_active() && !pw.get_visible());
        window.key_press(gtk::gdk::Key::Escape);
        check!(!window.0.window.get_visible());
        window.reveal();
        wait(0.2);
        check!(window.0.window.get_visible());
        Ok(())
    })();
    let _ = keyboard.kill();
    let _ = keyboard.wait();
    result
}

fn ids(rows: &[Map<String, Value>]) -> Vec<Value> { rows.iter().map(|r| r["id"].clone()).collect() }
fn texts(rows: &[Map<String, Value>], key: &str) -> Vec<Value> { rows.iter().map(|r| r[key].clone()).collect() }

// 2026-10-01 10:00 America/Denver.
const DENVER_TEN: f64 = 1_790_870_400.0;

// test_today_alarms_show_am_pm_beside_each_other_and_with_timers
fn today_alarms_show_am_pm_beside_each_other_and_with_timers() -> Outcome {
    let f = fixture("alarms", "org.example.KasumiAlarmTest", false, |o| o.poll_schedules = false);
    let window = f.app.window().unwrap();
    window.reveal();
    let panel = window.0.alarms.clone();
    let now = DENVER_TEN;
    let h = 3600.0;
    let zone = "America/Denver";
    panel.set_rows(vec![json!({"id": 1, "label": "Work", "zone": zone, "due": now - 3.0 * h}),
                        json!({"id": 2, "label": "Evening", "zone": zone, "due": now + 9.0 * h}),
                        json!({"id": 3, "label": "Tomorrow", "zone": zone, "due": now + 21.0 * h})], 0.0);
    panel.set_clock(move || now);
    window.0.countdowns.set_clock(move || now);
    panel.tick();
    wait(0.3);
    check!(texts(&panel.status(), "alarm_time") == [json!("7:00 AM"), json!("7:00 PM")]);
    check!(texts(&panel.status(), "text") == [json!("0:00"), json!("9:00:00")]);
    let cards = panel.cards().cards();
    let (first, second) = (allocation(&cards[0].1 .0.parent().unwrap()), allocation(&cards[1].1 .0.parent().unwrap()));
    check!(first.y() == second.y() && second.x() >= first.x() + first.width());
    let timer = window.0.countdowns.clone();
    timer.set_rows(vec![json!({"id": 4, "position": 1, "label": "Pasta", "due": now + 20.0})], 0.0);
    timer.tick();
    wait(0.3);
    check!(timer.widget().get_visible() && panel.widget().get_visible());
    check!(timer.status()[0]["text"] == json!("0:20"));
    panel.set_clock(move || now + 14.0 * h);
    panel.tick();
    check!(ids(&panel.status()) == [json!(3)]);
    panel.set_rows(Vec::new(), 0.0);
    panel.tick();
    check!(!panel.widget().get_visible());
    Ok(())
}

// test_timer_countdowns_are_side_by_side_and_follow_lifecycle
fn timer_countdowns_are_side_by_side_and_follow_lifecycle() -> Outcome {
    let f = fixture("timers", "org.example.KasumiTimerTest", false, |o| o.poll_schedules = false);
    let window = f.app.window().unwrap();
    window.reveal();
    let panel = window.0.countdowns.clone();
    let now = wall();
    panel.set_rows(vec![json!({"id": 1, "position": 1, "label": "Pasta", "due": now + 31.0}),
                        json!({"id": 2, "position": 2, "label": "Timer", "due": now + 33.0})], 0.0);
    let clock = Rc::new(Cell::new(now));
    let c = clock.clone();
    panel.set_clock(move || c.get());
    panel.tick();
    let compact = panel.compact().unwrap().clone();
    check!(!panel.widget().get_visible() && compact.widget().get_visible());
    check!(texts(&compact.status(), "text") == [json!("0:31"), json!("0:33")]);
    clock.set(now + 1.0);
    panel.tick();
    check!(ids(&panel.status()) == [json!(1)] && ids(&compact.status()) == [json!(2)]);
    clock.set(now + 3.0);
    panel.tick();
    wait(0.3);
    check!(!compact.widget().get_visible());
    check!(texts(&panel.status(), "text") == [json!("0:28"), json!("0:30")]);
    let cards = panel.cards().cards();
    let (first, second) = (allocation(&cards[0].1 .0.parent().unwrap()), allocation(&cards[1].1 .0.parent().unwrap()));
    check!(first.y() == second.y());
    check!(second.x() >= first.x() + first.width());
    check!(panel.widget().width() - (second.x() + second.width()) <= 12, "{} {:?}", panel.widget().width(), second);
    let rows = panel.rows()[1..].to_vec();
    panel.set_rows(rows, 0.0);
    clock.set(now + 34.0);
    panel.tick();
    check!(panel.cards().cards().len() == 1);
    check!(panel.status()[0]["text"] == json!("0:00"));
    check!(panel.cards().card(&json!(2)).unwrap().0.has_css_class("timer-ringing"));
    panel.set_rows(Vec::new(), 0.0);
    panel.tick();
    check!(!panel.widget().get_visible());
    Ok(())
}

// test_compact_schedules_share_space_with_urgent_timers_and_expire_cleanly
fn compact_schedules_share_space_with_urgent_timers_and_expire_cleanly() -> Outcome {
    let f = fixture("compact", "org.example.KasumiCompactTest", true, |o| o.poll_schedules = false);
    let window = f.app.window().unwrap();
    window.reveal();
    let (timer, alarms) = (window.0.countdowns.clone(), window.0.alarms.clone());
    let now = DENVER_TEN;
    let zone = "America/Denver";
    timer.set_rows(vec![json!({"id": 1, "position": 1, "label": "Pasta", "due": now + 22.0}),
                        json!({"id": 2, "position": 2, "label": "Bread", "due": now + 3723.0}),
                        json!({"id": 3, "position": 3, "label": "Tea", "due": now + 90.0})], 0.0);
    alarms.set_rows(vec![json!({"id": 4, "label": "Lunch", "zone": zone, "due": now + 7200.0}),
                         json!({"id": 5, "label": "Evening", "zone": zone, "due": now + 32400.0})], 0.0);
    timer.set_clock(move || now);
    alarms.set_clock(move || now);
    timer.tick();
    alarms.tick();
    wait(0.5);
    check!(window.0.schedule_summary.get_visible());
    let compact = timer.compact().unwrap().clone();
    check!(ids(&timer.status()) == [json!(1)]);
    check!(ids(&compact.status()) == [json!(2), json!(3)]);
    check!(texts(&compact.status(), "text") == [json!("1:02:03"), json!("1:30")]);
    for cards in [timer.cards(), &compact, alarms.cards()] {
        for (_, (card, ..)) in cards.cards() {
            let bounds = card.compute_bounds(&window.0.window).ok_or("bounds")?;
            check!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= window.0.window.width() as f32);
        }
    }
    let large = timer.cards().card(&json!(1)).unwrap().2.height();
    let small = compact.card(&json!(2)).unwrap().2.height();
    check!(small < large, "{small} {large}");
    timer.set_rows(Vec::new(), 0.0);
    alarms.set_rows(Vec::new(), 0.0);
    timer.tick();
    alarms.tick();
    check!(!timer.widget().get_visible());
    check!(!window.0.schedule_summary.get_visible());
    // A disconnected scheduler must not leave frozen small cards.
    timer.set_rows(vec![json!({"id": 6, "position": 1, "label": "Stale", "due": now + 300.0})], 4.0);
    timer.tick();
    check!(!compact.widget().get_visible());
    Ok(())
}

// test_dashboard_location_lifecycle_and_media (+ voice log state)
fn dashboard_location_lifecycle_and_media() -> Outcome {
    let tmp = tmpdir("dashboard-pre");
    let stamp = glib::DateTime::now_local().unwrap().format("%Y-%m-%d %H:%M:%S").unwrap();
    let log = tmp.join("voice.log");
    std::fs::write(&log, format!("{stamp} INFO tools.voice_mode: Voice recording started\n")).unwrap();
    let log_path = log.to_string_lossy().into_owned();
    let f = fixture("dashboard", "org.example.KasumiTest", false, move |o| { o.config.insert("voice_log".into(), json!(log_path)); });
    let window = f.app.window().unwrap();
    window.reveal();
    wait(0.2);
    let w = window.0.clone();
    check!(w.window.is_mapped());
    check!(!f.app.idle_running());
    check!(!w.weather.get_visible() && !w.dashboard.weather_group().get_visible() && !w.weather.forecast_group().get_visible());
    check!(f.weather.0.lock().unwrap().is_empty());
    check!(w.assistant.text() == "KASUMI");
    check!(spin(|| w.voice_state.get() == "listening", 1.0), "voice {}", w.voice_state.get());
    check!(spin(|| w.voice.alpha() > 0.0, 1.0));
    f.store.save(&WeatherConfig { name: "Test City".into(), ..WeatherConfig::default() }).map_err(|e| e.0)?;
    f.app.poll_weather();
    check!(spin(|| f.app.0.weather.snapshot().is_some(), 2.0));
    check!(w.dashboard.weather_group().get_visible());
    let location = w.weather.location_group().last_child().and_downcast::<gtk::Label>().ok_or("location")?;
    check!(location.text() == "Test City");
    let day = w.weather.days().and_then(|d| d.first_child()).and_downcast::<gtk::FlowBoxChild>().and_then(|c| c.child())
        .and_then(|b| b.first_child()).and_downcast::<gtk::Label>().ok_or("day")?;
    check!(day.text() == "Sun 27", "{}", day.text());
    let fake = Media { name: "org.mpris.MediaPlayer2.fixture".into(), title: "Test track".into(), artist: "Artist".into(), playing: true,
                       controllable: true, desktop_entry: "spotify".into(), identity: "Spotify".into(), ..Media::default() };
    f.app.0.media_override.replace(Some(Box::new(move || Some(fake.clone()))));
    let toggled = Rc::new(Cell::new(0));
    let t = toggled.clone();
    f.app.0.toggle_override.replace(Some(Box::new(move || t.set(t.get() + 1))));
    f.app.information_changed();
    check!(w.media_row.get_visible());
    check!(w.media_title.text() == "Test track");
    w.media_button.emit_clicked();
    check!(toggled.get() == 1);
    check!(w.window.get_visible());
    f.app.0.media_override.replace(None);
    f.app.0.toggle_override.replace(None);
    std::fs::remove_file(&f.store.path).unwrap();
    f.app.poll_weather();
    check!(!w.dashboard.weather_group().get_visible() && !w.weather.forecast_group().get_visible());
    window.dismiss("test");
    check!(!w.window.get_visible());
    window.reveal();
    check!(w.window.get_visible());
    let status = f.app.status();
    let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    check!(keys == ["visible", "voice", "wallpaper", "size", "idle_seconds", "last_dismissal", "idle_monitor_running", "weather_visible",
                    "location", "media_visible", "gtk_version", "media_controls", "media_album_visible", "media_art_visible", "levels",
                    "timer_countdowns", "compact_timers", "today_alarms", "panel_visible", "panel_focused", "keyboard", "clock_focused"], "{keys:?}");
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// A tiny HTTP/1.1 server on a random 127.0.0.1 port (never a real endpoint).
struct FakeServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

fn fake_server(body: &'static str) -> FakeServer {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
            }
            log.lock().unwrap().push(line.trim().to_owned());
            let mut stream = stream;
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nSet-Cookie: kasumi_fixture=1; Max-Age=3600; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                           body.len());
        }
    });
    FakeServer { port, requests }
}

// Navigation policy against fake servers, failure page.
fn admin_panel_origin_policy_with_fake_server() -> Outcome {
    let allowed = fake_server("<html><head><title>Fake admin</title></head><body><a id=\"x\" href=\"/next\">next</a></body></html>");
    let other = fake_server("<html><body>other origin</body></html>");
    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = launched.clone();
    let url = format!("http://127.0.0.1:{}/?embedded=1", allowed.port);
    let f = fixture("server", "org.example.KasumiServerTest", false, move |o| {
        o.poll_schedules = false;
        o.panel_url = url;
        o.launch = Some(Rc::new(move |uri: &str| sink.borrow_mut().push(uri.to_owned())));
    });
    f.app.window().unwrap().reveal();
    f.app.open_panel();
    let panel = f.app.panel().ok_or("panel")?;
    let web = panel.web().ok_or("web")?.clone();
    check!(spin(|| web.title().as_deref() == Some("Fake admin"), 8.0), "title {:?}", web.title());
    check!(allowed.requests.lock().unwrap().iter().any(|r| r == "GET /?embedded=1 HTTP/1.1"), "{:?}", allowed.requests.lock().unwrap());
    // Same origin navigation is allowed.
    javascript(&web, "document.getElementById('x').click(); 'ok'")?;
    check!(spin(|| allowed.requests.lock().unwrap().iter().any(|r| r.starts_with("GET /next")), 4.0));
    // Another origin (other port), a new window and an https page are refused without a tap.
    javascript(&web, &format!("location.href = 'http://127.0.0.1:{}/elsewhere'; 'ok'", other.port))?;
    javascript(&web, &format!("window.open('http://127.0.0.1:{}/popup'); 'ok'", other.port))?;
    javascript(&web, "window.open('https://accounts.example.test/oauth'); 'ok'")?;
    wait(1.0);
    check!(other.requests.lock().unwrap().is_empty(), "{:?}", other.requests.lock().unwrap());
    check!(launched.borrow().is_empty(), "{:?}", launched.borrow());
    check!(web.uri().is_some_and(|u| u.starts_with(&format!("http://127.0.0.1:{}/", allowed.port))));
    check!(!panel.error_box().get_visible());
    // Permission requests from the panel page are denied by the panel's own handler.
    javascript(&web, "window.__perm = 'pending'; Notification.requestPermission().then(p => window.__perm = p, e => window.__perm = 'error'); 'ok'")?;
    check!(spin(|| javascript(&web, "window.__perm").ok().as_deref() == Some("denied"), 4.0),
           "notification permission {:?}", javascript(&web, "window.__perm"));
    check!(panel.denied_permissions() >= 1, "denied {}", panel.denied_permissions());
    // WebKit's website-data directories are user-only.
    for base in [glib::user_data_dir(), glib::user_cache_dir()] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(base.join("webkitgtk")).map(|m| m.permissions().mode() & 0o777);
        check!(matches!(mode, Ok(0o700)), "{} {mode:?}", base.display());
    }
    drop(f);
    // An unreachable admin service shows the retry page.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let url = format!("http://127.0.0.1:{port}/?embedded=1");
    let f = fixture("down", "org.example.KasumiDownTest", false, move |o| { o.poll_schedules = false; o.panel_url = url; });
    f.app.window().unwrap().reveal();
    f.app.open_panel();
    let panel = f.app.panel().ok_or("panel")?;
    let error = panel.error_box().clone();
    check!(spin(|| error.get_visible(), 8.0));
    check!(!panel.web().unwrap().widget().get_visible());
    Ok(())
}

/// The scheduler socket end to end: the real worker reads a fake socket.
fn scheduler_socket_feeds_cards_and_errors_go_stale() -> Outcome {
    let tmp = tmpdir("sched");
    let socket = tmp.join("voice.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let due = wall() + 100.0;
    let failing = Arc::new(Mutex::new(false));
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let (fail, log) = (failing.clone(), requests.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            log.lock().unwrap().push(line.clone());
            let reply = if *fail.lock().unwrap() { "{\"error\": \"down\"}".to_owned() }
                else if line.contains("countdowns") { format!("{{\"timers\": [{{\"id\": \"t1\", \"position\": 1, \"label\": \"Tea\", \"due\": {due}}}]}}") }
                else { "{\"alarms\": []}".to_owned() };
            let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
        }
    });
    let sock = socket.clone();
    let f = fixture("sched-app", "org.example.KasumiSchedTest", false, move |o| o.scheduler_socket = sock);
    let window = f.app.window().unwrap();
    window.reveal();
    let compact = window.0.countdowns.compact().unwrap().clone();
    check!(spin(|| compact.widget().get_visible(), 4.0));
    check!(compact.status()[0]["title"] == json!("Tea · Timer 1"));
    check!(f.app.status()["compact_timers"][0]["id"] == json!("t1"));
    *failing.lock().unwrap() = true;
    check!(spin(|| !compact.widget().get_visible(), 6.0), "stale cards must clear");
    let seen = requests.lock().unwrap().clone();
    check!(seen.iter().all(|r| r == "{\"operation\": \"countdowns\"}\n" || r == "{\"operation\": \"alarms_today\"}\n"), "{seen:?}");
    drop(f);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

fn calls_log() -> String { std::fs::read_to_string(std::env::var("KASUMI_CALLS").unwrap()).unwrap_or_default() }

fn run_app(args: &[&str]) -> (i32, String, String) {
    let dir = PathBuf::from(std::env::var("KASUMI_APP_DIR").unwrap());
    let output = Command::new(dir.join("luna-clock")).args(args).output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn name_owned(name: &str) -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return false };
    bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", Some(&(name,).to_variant()),
                  Some(glib::VariantTy::new("(b)").unwrap()), gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE)
        .ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|v| v.0)
}

fn spin_plain(mut predicate: impl FnMut() -> bool, seconds: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

/// The real `luna-clock` binary as primary + remote command lines, and `voice-controls`.
fn cli_primary_status_levels_and_voice_controls() -> Outcome {
    let dir = PathBuf::from(std::env::var("KASUMI_APP_DIR").unwrap());
    let mut primary = Command::new(dir.join("luna-clock")).stderr(std::fs::File::create(dir.join("primary.log")).unwrap()).spawn()
        .map_err(|e| e.to_string())?;
    let result = (|| {
        // Probe only once the primary owns its name (else the probe itself would become the primary).
        check!(spin_plain(|| name_owned("local.luna.Clock"), 10.0), "primary never owned local.luna.Clock");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = Value::Null;
        while Instant::now() < deadline {
            let (code, out, _) = run_app(&["--status"]);
            if code == 0 {
                if let Ok(value) = serde_json::from_str::<Value>(&out) {
                    if value["visible"] == json!(true) {
                        status = value;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        check!(status["gtk_version"] == json!(4), "{status}");
        check!(status["idle_monitor_running"] == json!(true), "{status}");
        check!(calls_log().contains("swayidle -w -C /dev/null timeout 60 "), "{}", calls_log());
        check!(calls_log().contains("luna-clock --show"));
        let (code, out, _) = run_app(&["--status"]);
        check!(code == 0 && out.ends_with("}\n") && out.starts_with("{\"visible\": true, \"voice\": \"idle\", \"wallpaper\": null, \"size\": ["), "{out}");
        let (code, _, err) = run_app(&["--bogus"]);
        check!(code == 2 && err.contains("Usage: luna_clock.py [--show|--hide|--status|--next-wallpaper|--quit|--volume up/down/mute/0–100|--brightness up/down/10–100]"), "{code} {err}");
        let (code, _, err) = run_app(&["--brightness", "5"]);
        check!(code == 2 && err.contains("Choose a level from 10 to 100 percent"), "{code} {err}");
        let (code, _, err) = run_app(&["--volume", "loud"]);
        check!(code == 2 && err.contains("Use up, down, or a percentage"), "{code} {err}");
        let (code, ..) = run_app(&["--brightness", "40"]);
        check!(code == 0);
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        check!(std::fs::read_to_string(home.join(".config/kasumi-screen/levels.json")).ok().as_deref() == Some("{\"brightness\": 40}\n"));
        let (_, out, _) = run_app(&["--status"]);
        let value: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        check!(value["levels"]["screen_brightness"] == json!(40) && value["levels"]["indicator"]["kind"] == json!("brightness"), "{out}");
        check!(value["levels"]["indicator"]["direction"] == json!("↓"));
        let (code, ..) = run_app(&["--volume", "30"]);
        check!(code == 0);
        check!(spin_plain(|| calls_log().contains("pactl set-sink-volume fake_sink 30%"), 4.0), "{}", calls_log());
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut value = Value::Null;
        while Instant::now() < deadline {
            value = serde_json::from_str(&run_app(&["--status"]).1).unwrap_or(Value::Null);
            if value["levels"]["volume"]["value"] == json!(30) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        check!(value["levels"]["volume"] == json!({"sink": "fake_sink", "value": 30, "muted": false}), "{value}");
        let voice = |args: &[&str]| String::from_utf8_lossy(&Command::new(dir.join("voice-controls")).args(args).output().unwrap().stdout).into_owned();
        let out = voice(&["volume", "up"]);
        check!(out == "{\"success\": true, \"response\": \"Volume is 40 percent.\", \"sink\": \"fake_sink\", \"value\": 40, \"muted\": false}\n", "{out}");
        let out = voice(&["brightness", "up"]);
        check!(out == "{\"success\": true, \"value\": 50, \"response\": \"Screen brightness is 50 percent.\"}\n", "{out}");
        check!(calls_log().contains("systemctl --user is-active --quiet luna-clock"));
        check!(voice(&["weather", "current"]) == "{\"success\": false, \"response\": \"I could not check the weather just now.\"}\n");
        let (code, ..) = run_app(&["--hide"]);
        check!(code == 0);
        let value: Value = serde_json::from_str(&run_app(&["--status"]).1).unwrap_or(Value::Null);
        check!(value["visible"] == json!(false) && value["last_dismissal"] == json!("command"), "{value}");
        check!(run_app(&["--next-wallpaper"]).0 == 0 && run_app(&["--close-panel"]).0 == 0);
        let (code, ..) = run_app(&["--quit"]);
        check!(code == 0);
        check!(spin_plain(|| primary.try_wait().ok().flatten().is_some(), 5.0), "--quit must exit the primary");
        check!(spin_plain(|| calls_log().contains("swayidle exited"), 3.0), "swayidle child must be terminated: {}", calls_log());
        Ok(())
    })();
    if primary.try_wait().ok().flatten().is_none() {
        let _ = primary.kill();
        let _ = primary.wait();
    }
    result
}

fn suite_lock_widgets() -> Outcome {
    // LoginLayer on a real frame clock + opaque underlay / stationary backdrop pixels.
    use tessel_locking::config::LockConfig;
    use tessel_locking::visuals::{LoginLayer, Wallpaper};
    let window = gtk::Window::builder().default_width(400).default_height(300).build();
    let overlay = gtk::Overlay::new();
    let wallpaper = Wallpaper::new("", || {});
    overlay.set_child(Some(&wallpaper));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let settled: Rc<RefCell<Vec<bool>>> = Rc::default();
    let sink = settled.clone();
    let layer = LoginLayer::new(&wallpaper, &content, move |v| sink.borrow_mut().push(v));
    overlay.add_overlay(&layer);
    window.set_child(Some(&overlay));
    window.present();
    wallpaper.configure(&LockConfig { wallpaper_mode: "color".into(), background_color: "#91b5d7".into(), ..LockConfig::default() }, 0);
    check!(spin(|| window.is_mapped() && wallpaper.width() > 0, 3.0));
    wait(0.1);
    check!(wallpaper.palette_color() == (0x91, 0xb5, 0xd7), "{:?}", wallpaper.palette_color());
    layer.reveal(false, true);
    check!(spin(|| !settled.borrow().is_empty(), 2.0));
    check!(*settled.borrow() == [false] && layer.progress() == 0.0 && !layer.can_target() && !content.is_sensitive());
    // Fake-clock reversal (test_frame_clock_and_reversing_animation).
    layer.reveal(true, true);
    layer.cancel_animation();
    let mut now = 10_000_000;
    layer.frame_at(now);
    check!(layer.progress() == 0.0);
    now += 175_000;
    layer.frame_at(now);
    check!((layer.progress() - 0.875).abs() < 1e-9);
    let previous = layer.progress();
    layer.reveal(false, true);
    layer.cancel_animation();
    layer.frame_at(now);
    check!(layer.progress() == previous, "Reversing should not jump");
    now += 175_000;
    layer.frame_at(now);
    check!(layer.progress() > 0.0 && layer.progress() < previous);
    now += 175_000;
    check!(layer.frame_at(now) == glib::ControlFlow::Break && layer.progress() == 0.0);
    check!(!layer.can_target() && !content.is_sensitive());
    // Render: progress 0 shows the opaque colour; progress .5 clips the blurred backdrop at half height.
    let pixel = |widget: &gtk::Widget, x: i32, y: i32| -> Option<[u8; 4]> {
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, widget.width() as f64, widget.height() as f64);
        let node = snapshot.to_node()?;
        let renderer = window.native()?.renderer()?;
        let texture = renderer.render_texture(&node, None);
        let mut data = vec![0u8; (texture.width() * texture.height() * 4) as usize];
        texture.download(&mut data, texture.width() as usize * 4);
        let i = ((y * texture.width() + x) * 4) as usize;
        Some([data[i + 2], data[i + 1], data[i], data[i + 3]])
    };
    wait(0.2);
    let clear = pixel(overlay.upcast_ref(), 15, 15).ok_or("render")?;
    check!(clear == [0x91, 0xb5, 0xd7, 255], "{clear:?}");
    layer.set_progress(1.0);
    wait(0.2);
    let full_top = pixel(overlay.upcast_ref(), 15, 20).ok_or("render")?;
    let full_low = pixel(overlay.upcast_ref(), 15, 225).ok_or("render")?;
    layer.set_progress(0.5);
    wait(0.2);
    let half_top = pixel(overlay.upcast_ref(), 15, 20).ok_or("render")?;
    let half_low = pixel(overlay.upcast_ref(), 15, 225).ok_or("render")?;
    check!(half_top == full_top, "Backdrop must not slide with the form {half_top:?} {full_top:?}");
    check!(half_low == clear && half_low != full_low, "Below the dropdown stays opaque wallpaper {half_low:?} {full_low:?} {clear:?}");
    layer.close();
    window.destroy();
    Ok(())
}

fn main() {
    if std::env::var("KASUMI_HEADLESS").as_deref() != Ok("1") {
        println!("dashboard_headless: skipped (UI checks run only via tests/run_headless.sh in a private headless labwc)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "refusing to run against the live session");
    assert!(std::env::var("WAYLAND_DISPLAY").is_ok(), "private WAYLAND_DISPLAY required");
    // SAFETY: single-threaded start-up before GTK.
    unsafe { std::env::set_var("MEDIA_PRIVATE_BUS", "1") };
    let _ = tessel_core::i18n::initialize();
    gtk::init().expect("gtk");
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let tests: [(&str, fn() -> Outcome); 11] = [
        ("suite_lock_widgets", suite_lock_widgets),
        ("music_album_controls_video_fallback_and_player_exit", music_album_controls_video_fallback_and_player_exit),
        ("embedded_keyboard_types_in_web_panel_and_hides_without_losing_field", embedded_keyboard_types_in_web_panel_and_hides_without_losing_field),
        ("panel_focus_escape_and_click_lifecycle", panel_focus_escape_and_click_lifecycle),
        ("today_alarms_show_am_pm_beside_each_other_and_with_timers", today_alarms_show_am_pm_beside_each_other_and_with_timers),
        ("timer_countdowns_are_side_by_side_and_follow_lifecycle", timer_countdowns_are_side_by_side_and_follow_lifecycle),
        ("compact_schedules_share_space_with_urgent_timers_and_expire_cleanly", compact_schedules_share_space_with_urgent_timers_and_expire_cleanly),
        ("dashboard_location_lifecycle_and_media", dashboard_location_lifecycle_and_media),
        ("admin_panel_origin_policy_with_fake_server", admin_panel_origin_policy_with_fake_server),
        ("scheduler_socket_feeds_cards_and_errors_go_stale", scheduler_socket_feeds_cards_and_errors_go_stale),
        ("cli_primary_status_levels_and_voice_controls", cli_primary_status_levels_and_voice_controls),
    ];
    let mut failed = 0;
    let mut ran = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        let started = Instant::now();
        match std::panic::catch_unwind(test) {
            Ok(Ok(())) => println!("ok   {name} ({:.1}s)", started.elapsed().as_secs_f64()),
            Ok(Err(message)) => {
                failed += 1;
                println!("FAIL {name}: {message}");
            }
            Err(_) => {
                failed += 1;
                println!("FAIL {name}: panicked");
            }
        }
        let _ = std::io::stdout().flush();
    }
    println!("{} passed, {failed} failed", ran - failed);
    let _ = std::io::stdout().flush();
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
