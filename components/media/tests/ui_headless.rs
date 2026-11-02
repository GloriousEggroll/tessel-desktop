//! Port of tessel-media/test_media.py AppletTests: the real popup against
//! simulated MPRIS players on the private D-Bus of a private nested headless
//! labwc (tests/run_headless.sh). Under a plain `cargo test` it skips itself.

#[path = "support/fake_player.rs"]
mod fake_player;

use fake_player::{metadata, s, settle, FakePlayer, Options};
use glib::variant::ToVariant;
use gtk::prelude::*;
use tessel_media::app::MediaApp;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn wait_for(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.02);
        if predicate() {
            return true;
        }
    }
    predicate()
}

thread_local!(static COUNTER: Cell<u32> = const { Cell::new(0) });

struct Fx(Rc<MediaApp>);

impl Fx {
    fn new() -> Self {
        let n = COUNTER.with(|c| { c.set(c.get() + 1); c.get() });
        let app = MediaApp::new(&format!("org.tessel.MediaTest{n}"), "media_manager.py");
        app.app.register(gio::Cancellable::NONE).expect("register");
        Self(app)
    }

    fn open(&self) -> Rc<tessel_media::ui::MediaPopup> {
        self.0.show("top", 40);
        settle(0.4);
        self.0.popup().expect("popup")
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some(popup) = self.0.popup.take() {
            popup.destroy();
        }
        self.0.shutdown();
        settle(0.05);
    }
}

fn shot(name: &str) {
    let path = format!("{}/{name}", std::env::var("TMPDIR").unwrap());
    let _ = std::process::Command::new("grim").arg(path).output();
}

fn open_player_raises_media_owner_and_obeys_capability() {
    let player = FakePlayer::new("browser", Options { desktop_entry: "helium", url: "https://example.test/playing", ..Options::default() });
    let fx = Fx::new();
    let popup = fx.open();
    assert!(popup.open_player.is_sensitive());
    // Root properties can change independently of playback capabilities.
    player.change_root("CanRaise", false.to_variant());
    assert!(wait_for(|| !popup.open_player.is_sensitive(), 2.0));
    assert!(!fx.0.backend.raise_player(None));
    assert!(player.calls().is_empty());
    assert_eq!(popup.open_player.tooltip_text().unwrap(), "This player does not support opening its window.");
    player.change_root("CanRaise", true.to_variant());
    assert!(wait_for(|| popup.open_player.is_sensitive(), 2.0));
    popup.open_player.emit_clicked();
    settle(0.1);
    assert_eq!(player.calls(), ["Raise"]);
    assert!(wait_for(|| fx.0.popup().is_none(), 2.0), "the popup hides first and closes after success");
}

fn popup_controls_live_metadata_and_capabilities() {
    let player = FakePlayer::new("spotify", Options { desktop_entry: "spotify", ..Options::default() });
    let fx = Fx::new();
    let popup = fx.open();
    assert!(popup.window.is_mapped());
    assert_eq!(popup.title.text(), "A track <with> symbols");
    assert!(!popup.title.uses_markup());
    assert_eq!(popup.artist.text(), "An artist");
    assert!(popup.button("next").is_sensitive());
    assert_eq!(popup.button("toggle").tooltip_text().unwrap(), "Pause");
    for (button, method) in [("previous", "Previous"), ("next", "Next"), ("toggle", "Pause")] {
        popup.button(button).emit_clicked();
        settle(0.05);
        assert_eq!(player.calls().last().unwrap(), method);
    }
    assert!(wait_for(|| popup.button("toggle").tooltip_text().as_deref() == Some("Play"), 2.0));
    player.change(&[("CanGoNext", false.to_variant())]);
    settle(0.05);
    assert!(!popup.button("next").is_sensitive());
    assert!(popup.button("previous").is_sensitive());
    shot("waymedia-popup-test.png");
    player.change(&[("PlaybackStatus", s("Stopped"))]);
    settle(0.05);
    assert_eq!(popup.title.text(), "Nothing playing");
    assert_eq!(popup.status.text(), "Start music or a video in your media app.");
    assert!(popup.buttons.iter().all(|(_, b)| !b.is_sensitive()));
}

fn moving_to_another_edge_reshows_instead_of_closing() {
    let fx = Fx::new();
    let popup = fx.open();
    assert!(popup.window.is_visible());
    // set_position() hides before the asynchronous re-show; that hide must
    // not be taken for a dismissal that destroys the popup.
    fx.0.show("bottom", 76);
    assert!(wait_for(|| popup.window.is_visible(), 3.0));
    settle(0.05);
    assert!(Rc::ptr_eq(&fx.0.popup().unwrap(), &popup));
    assert!(popup.window.is_visible());
    popup.controller().dismiss();
    settle(0.05);
    assert!(fx.0.popup().is_none(), "A real dismissal still closes the popup");
}

fn album_cover_updates_without_stale_art_and_video_uses_player_icon() {
    let folder = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap()).join("media-cover");
    std::fs::create_dir_all(&folder).unwrap();
    let cover = folder.join("cover.png");
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 512, 512).unwrap();
    pixbuf.fill(0x56856cff);
    pixbuf.savev(&cover, "png", &[]).unwrap();
    let cover_uri = format!("file://{}", cover.display());
    let player = FakePlayer::new("spotify", Options { desktop_entry: "spotify", ..Options::default() });
    let fx = Fx::new();
    let popup = fx.open();
    let meta = |url: &str, art: &str, album: &str| metadata(&[("xesam:title", s("A song with a longer title to wrap")),
        ("xesam:album", s(album)), ("xesam:url", s(url)), ("mpris:artUrl", s(art))]);
    player.change(&[("Metadata", meta("spotify:track:fixture", &cover_uri, "Fixture Album"))]);
    assert!(wait_for(|| popup.visual.visible_child_name().as_deref() == Some("album"), 3.0));
    assert!(popup.cover.gicon().and_downcast::<gio::BytesIcon>().is_some());
    assert_eq!(popup.album.text(), "Fixture Album");
    assert!(popup.album.is_visible());
    assert!(player.calls().is_empty());
    settle(0.2);
    shot("waymedia-album-popup-test.png");
    player.change(&[("Metadata", meta("spotify:track:fixture", "file:///missing-cover.png", "Fixture Album"))]);
    settle(0.1);
    assert_eq!(popup.visual.visible_child_name().as_deref(), Some("player"));
    assert!(popup.cover.gicon().is_none());
    assert_eq!(popup.album.text(), "Fixture Album");
    player.change(&[("Metadata", meta("file:///movie.mp4", &cover_uri, "Fixture Album"))]);
    settle(0.1);
    assert_eq!(popup.visual.visible_child_name().as_deref(), Some("player"));
    assert!(!popup.album.is_visible());
    player.change(&[("PlaybackStatus", s("Stopped"))]);
    settle(0.1);
    assert_eq!(popup.title.text(), "Nothing playing");
    assert!(!popup.album.is_visible());
    let _ = std::fs::remove_dir_all(folder);
}

fn seek_bar_tracks_position_and_seeks_with_track_id() {
    let player = FakePlayer::new("seekable", Options { state: "Paused", seekable: true, ..Options::default() });
    let fx = Fx::new();
    let popup = fx.open();
    assert!(wait_for(|| (popup.seek.value() - 42.0).abs() <= 0.5, 3.0));
    assert!(popup.seek_box.is_visible());
    assert!(popup.seek.is_sensitive());
    assert!((popup.seek.adjustment().upper() - 180.0).abs() < 1e-9);
    assert_eq!((popup.elapsed.text().as_str(), popup.duration.text().as_str()), ("0:42", "3:00"));
    assert!(popup.tick_source.borrow().is_some());
    popup.seek.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &90.0f64]);
    settle(0.45);
    assert_eq!(player.calls().last().unwrap(), "SetPosition /fixture/track/1 90000000");
    settle(0.2);
    assert_eq!(popup.elapsed.text(), "1:30");
    // Playing interpolates locally between occasional position reads.
    player.change(&[("PlaybackStatus", s("Playing"))]);
    settle(1.2);
    assert!(popup.seek.value() > 90.5, "{}", popup.seek.value());
    player.change(&[("CanSeek", false.to_variant())]);
    settle(0.05);
    assert!(!popup.seek.is_sensitive());
    let calls = player.calls().len();
    popup.seek.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &10.0f64]);
    settle(0.4);
    assert_eq!(player.calls().len(), calls);
    popup.controller().dismiss();
    settle(0.2);
    assert!(fx.0.popup().is_none());
    assert!(popup.tick_source.borrow().is_none());
}

fn shuffle_repeat_and_player_switcher() {
    let first = FakePlayer::new("alpha", Options { seekable: true, ..Options::default() });
    let fx = Fx::new();
    let popup = fx.open();
    assert!(!popup.player_switch.is_visible());
    assert!(popup.shuffle.is_visible() && popup.repeat.is_visible());
    assert!(!popup.shuffle.is_active());
    popup.shuffle.set_active(true);
    settle(0.05);
    assert_eq!(first.calls().last().unwrap(), "Set Shuffle true");
    assert!(popup.shuffle.is_active());
    for expected in ["Playlist", "Track", "None"] {
        popup.repeat.emit_clicked();
        settle(0.05);
        assert_eq!(first.calls().last().unwrap(), &format!("Set LoopStatus {expected}"));
    }
    assert!(!popup.repeat.is_active());
    let second = FakePlayer::new("beta", Options { state: "Paused", seekable: true, ..Options::default() });
    settle(0.4);
    assert!(popup.player_switch.is_visible());
    let model = popup.player_switch.model().and_downcast::<gtk::StringList>().unwrap();
    assert_eq!([model.string(0).unwrap(), model.string(1).unwrap()], ["alpha player", "beta player"]);
    assert_eq!(popup.title.text(), "alpha song");
    popup.player_switch.set_selected(1);
    settle(0.05);
    assert_eq!(popup.title.text(), "beta song");
    popup.button("toggle").emit_clicked();
    settle(0.05);
    assert_eq!(second.calls().last().unwrap(), "Play");
    assert!(!first.calls().iter().any(|c| c == "Pause"));
    first.change(&[("CanControl", false.to_variant())]);
    settle(0.05);
    // Plain players without play modes hide those toggles.
    let plain = FakePlayer::simple("plain");
    settle(0.4);
    let index = popup.player_names.borrow().iter().position(|n| *n == plain.name).unwrap();
    popup.player_switch.set_selected(index as u32);
    settle(0.05);
    assert!(!(popup.shuffle.is_visible() || popup.repeat.is_visible() || popup.seek_box.is_visible()));
}

fn css_parses() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(Cell::new(0));
    let e = errors.clone();
    provider.connect_parsing_error(move |_, _, _| e.set(e.get() + 1));
    provider.load_from_string(tessel_media::ui::CSS);
    assert_eq!(errors.get(), 0);
    assert_eq!(tessel_media::ui::clock(42_000_000.0), "0:42");
    assert_eq!(tessel_media::ui::clock(3_723_000_000.0), "1:02:03");
    assert_eq!(tessel_media::ui::clock(-5.0), "0:00");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var("MEDIA_PRIVATE_BUS").as_deref() != Ok("1") {
        println!("ui_headless: skipped (run components/media/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    let tests: [(&str, fn()); 7] = [
        ("css_parses", css_parses),
        ("open_player_raises_media_owner_and_obeys_capability", open_player_raises_media_owner_and_obeys_capability),
        ("popup_controls_live_metadata_and_capabilities", popup_controls_live_metadata_and_capabilities),
        ("moving_to_another_edge_reshows_instead_of_closing", moving_to_another_edge_reshows_instead_of_closing),
        ("album_cover_updates_without_stale_art_and_video_uses_player_icon", album_cover_updates_without_stale_art_and_video_uses_player_icon),
        ("seek_bar_tracks_position_and_seeks_with_track_id", seek_bar_tracks_position_and_seeks_with_track_id),
        ("shuffle_repeat_and_player_switcher", shuffle_repeat_and_player_switcher),
    ];
    let only = std::env::args().nth(1);
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        println!("test {name} ...");
        test();
        settle(0.1);
        println!("test {name} ok");
    }
    println!("ui_headless: all checks passed");
}
