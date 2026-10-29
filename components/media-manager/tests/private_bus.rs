//! MPRIS client checks on a private `gio::TestDBus` with simulated players only
//! (port of lock-screen test_lock_content.MediaTests and test_media's dock
//! controller test). The user's real players are never visible here.

#[path = "support/fake_player.rs"]
mod fake_player;

use fake_player::{metadata, s, settle, FakePlayer, Options};
use glib::variant::ToVariant;
use media_manager::dock_status::{DockMedia, MediaStatus};
use media_manager::mpris::MediaSession;
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

fn client(readonly: bool) -> MediaSession { MediaSession::new(|| {}, readonly) }

fn opts(state: &str) -> Options<'static> {
    Options { state: if state == "Paused" { "Paused" } else { "Playing" }, ..Options::default() }
}

fn pause_existing_playing_only_once_with_preview_readonly() {
    let playing = FakePlayer::simple("Fixture");
    let paused = FakePlayer::new("Paused", opts("Paused"));
    let unsupported = FakePlayer::new("NoControls", Options { allowed: false, ..Options::default() });
    let preview = client(true);
    preview.start(true);
    settle(0.3);
    preview.toggle();
    assert!(playing.calls().is_empty());
    assert!(!preview.current().unwrap().controllable);
    preview.close();
    let client = client(false);
    client.start(true);
    settle(0.3);
    assert_eq!(playing.calls(), ["Pause"]);
    assert!(paused.calls().is_empty());
    assert!(unsupported.calls().is_empty());
    let later = FakePlayer::simple("Later");
    settle(0.3);
    assert!(later.calls().is_empty());
    client.start(true);
    settle(0.05);
    assert_eq!(playing.calls(), ["Pause"]);
    client.close();
    assert_eq!(playing.calls(), ["Pause"], "Unlock/close must never resume playback");
}

fn live_title_selection_controls_errors_and_player_exit() {
    let paused = FakePlayer::new("Paused", opts("Paused"));
    let active = FakePlayer::simple("Fixture");
    let client = client(false);
    client.start(false);
    settle(0.3);
    assert!(active.calls().is_empty());
    assert_eq!(client.current().unwrap().name, active.name);
    assert_eq!(client.current().unwrap().title, "A track <with> symbols");
    client.toggle();
    settle(0.05);
    assert_eq!(active.calls(), ["Pause"]);
    assert!(!client.current().unwrap().playing);
    client.toggle();
    settle(0.05);
    assert_eq!(active.calls(), ["Pause", "Play"]);
    active.change(&[("CanPause", false.to_variant())]);
    settle(0.05);
    assert!(!client.current().unwrap().controllable);
    client.toggle();
    assert_eq!(active.calls(), ["Pause", "Play"]);
    active.change(&[("CanPause", true.to_variant())]);
    active.state.borrow_mut().fail = true;
    settle(0.05);
    client.toggle();
    settle(0.05);
    assert!(client.current().unwrap().error.contains("unavailable"));
    active.change(&[("PlaybackStatus", s("Stopped"))]);
    settle(0.05);
    assert_eq!(client.current().unwrap().name, paused.name);
    drop(paused);
    drop(active);
    settle(0.1);
    assert!(client.current().is_none());
    client.close();
}

fn skip_tracks_respects_capabilities_readonly_and_player_lifetime() {
    let player = FakePlayer::simple("Fixture");
    let client = client(false);
    client.start(false);
    settle(0.3);
    assert!(client.current().unwrap().can_previous);
    assert!(client.current().unwrap().can_next);
    client.previous();
    settle(0.05);
    client.next();
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next"]);
    player.change(&[("CanGoNext", false.to_variant())]);
    settle(0.05);
    assert!(!client.current().unwrap().can_next);
    client.next();
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next"]);
    let readonly = self::client(true);
    readonly.start(false);
    settle(0.3);
    assert!(!readonly.current().unwrap().can_previous);
    readonly.previous();
    readonly.next();
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next"]);
    player.change(&[("CanControl", false.to_variant())]);
    settle(0.05);
    client.previous();
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next"]);
    drop(player);
    settle(0.1);
    client.previous();
    client.next();
    assert!(client.current().is_none());
    client.close();
    readonly.close();
}

fn player_desktop_identity_and_page_url() {
    let _player = FakePlayer::new("Firefox", Options { desktop_entry: "org.mozilla.firefox", url: "https://example.test/watch?id=1",
                                                       ..Options::default() });
    let client = client(false);
    client.start(false);
    settle(0.3);
    let media = client.current().unwrap();
    assert_eq!(media.desktop_entry, "org.mozilla.firefox");
    assert_eq!(media.identity, "Fixture player");
    assert_eq!(media.url, "https://example.test/watch?id=1");
    client.close();
}

fn album_metadata_updates_and_clears_between_tracks() {
    let player = FakePlayer::simple("Fixture");
    let client = client(false);
    client.start(false);
    settle(0.3);
    player.change(&[("Metadata", metadata(&[("xesam:title", s("Music")), ("xesam:album", s(" An album\nname ")),
                                            ("mpris:artUrl", s("file:///tmp/cover.png")), ("xesam:url", s("spotify:track:fixture"))]))]);
    settle(0.05);
    let media = client.current().unwrap();
    assert_eq!((media.album.as_str(), media.art_url.as_str(), media.kind.as_str()), ("An album name", "file:///tmp/cover.png", "audio"));
    player.change(&[("Metadata", metadata(&[("xesam:title", s("Video")), ("xesam:url", s("file:///tmp/movie.MKV"))]))]);
    settle(0.05);
    let media = client.current().unwrap();
    assert_eq!((media.album.as_str(), media.art_url.as_str(), media.kind.as_str()), ("", "", "video"));
    assert!(player.calls().is_empty(), "Observing media must not interrupt playback");
    client.close();
}

fn replacement_owner_never_receives_stale_command() {
    // Commands address the unique owner of the player that is current now.
    let first = FakePlayer::simple("Same");
    let client = client(false);
    client.start(false);
    settle(0.3);
    let old_owner = first.connection.unique_name().unwrap().to_string();
    drop(first);
    let second = FakePlayer::simple("Same");
    settle(0.3);
    assert_ne!(second.connection.unique_name().unwrap().to_string(), old_owner);
    client.toggle();
    settle(0.05);
    assert_eq!(second.calls(), ["Pause"]);
    client.close();
}

fn dock_controller_only_changes_playback_on_explicit_action() {
    let player = FakePlayer::new("spotify", Options { desktop_entry: "spotify", ..Options::default() });
    let changes: Rc<RefCell<Vec<DockMedia>>> = Rc::default();
    let sink = changes.clone();
    let feed = MediaStatus::new(move |state| sink.borrow_mut().push(state));
    feed.start();
    settle(0.4);
    let last = || changes.borrow().last().cloned().unwrap();
    assert!(last().tooltip.contains("A track <with> symbols"));
    assert_eq!(last().classes, ["playing"]);
    assert_eq!(last().text, "\u{f001}");
    assert!(player.calls().is_empty());
    feed.action("previous");
    settle(0.05);
    feed.action("next");
    settle(0.05);
    feed.action("toggle");
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next", "Pause"]);
    assert_eq!(last().classes, ["paused"]);
    player.change(&[("CanGoNext", false.to_variant())]);
    settle(0.05);
    assert!(!last().can_next);
    feed.action("next");
    settle(0.05);
    assert_eq!(player.calls(), ["Previous", "Next", "Pause"]);
    assert!(!feed.action("bogus"));
    player.change(&[("PlaybackStatus", s("Stopped"))]);
    settle(0.05);
    assert!(last().tooltip.contains("Nothing playing"));
    assert!(last().visible);
    assert_eq!(last().title, "Nothing playing");
    assert_eq!(last().text, "\u{f001}");
    feed.close();
    let count = changes.borrow().len();
    feed.refresh();
    assert_eq!(changes.borrow().len(), count);
}

fn cli_rejects_settings_and_negative_offsets() {
    let binary = env!("CARGO_BIN_EXE_media-manager");
    let output = Command::new(binary).args(["--popup-offset", "-1"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).ends_with("error: --popup-offset must be nonnegative\n"));
    let output = Command::new(binary).arg("--settings").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).ends_with("error: unrecognized arguments: --settings\n"));
    let output = Command::new(binary).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("On-demand media controls using the lock screen's MPRIS and icon backends."));
}

#[test]
fn private_bus() {
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), bus.bus_address().unwrap().as_str());
    // SAFETY: single test function; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("MEDIA_PRIVATE_BUS", "1") };
    pause_existing_playing_only_once_with_preview_readonly();
    settle(0.1);
    live_title_selection_controls_errors_and_player_exit();
    settle(0.1);
    skip_tracks_respects_capabilities_readonly_and_player_lifetime();
    settle(0.1);
    player_desktop_identity_and_page_url();
    settle(0.1);
    album_metadata_updates_and_clears_between_tracks();
    settle(0.1);
    replacement_owner_never_receives_stale_command();
    settle(0.1);
    dock_controller_only_changes_playback_on_explicit_action();
    cli_rejects_settings_and_negative_offsets();
    bus.down();
}
