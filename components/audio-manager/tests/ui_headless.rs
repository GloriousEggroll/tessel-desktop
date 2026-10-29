//! UI tests (test_audio.py, test_popup_mixer.py, test_loopback_ui.py,
//! test_volume_app.py) with the in-memory fake backend. Runs only inside the
//! private headless labwc of tests/run_headless.sh; under plain `cargo test`
//! it skips. Every case runs in its own process (fresh GtkApplication).

#![allow(deprecated)]

use audio_manager::api::AudioApi;
use audio_manager::app::{AudioApplication, BackendFactory, APPLICATION_ID};
use audio_manager::fake::{self, Call, FakeAudioBackend, FakeMode};
use audio_manager::model::Node;
use audio_manager::ui::popup::VolumePopup;
use audio_manager::ui::settings::AudioSettingsWindow;
use gtk::prelude::*;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use suite_gtk::gtk;

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool, description: &str, timeout: f64) {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return;
        }
        assert!(Instant::now() < end, "Timed out waiting for {description}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Harness {
    app: AudioApplication,
    fake: Rc<FakeAudioBackend>,
}

impl Harness {
    fn new(mode: FakeMode) -> Self {
        suite_core::i18n::initialize();
        let fake = FakeAudioBackend::new(mode);
        let shared = fake.clone();
        let factory: BackendFactory = Box::new(move |changed, error| {
            shared.set_callbacks(changed, error);
            shared.clone() as Rc<dyn AudioApi>
        });
        let app = AudioApplication::new(APPLICATION_ID, Some(factory));
        app.0.app.register(None::<&gio::Cancellable>).expect("register on the private bus");
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_enable_animations(false);
        }
        Self { app, fake }
    }

    fn settings(&self) -> Rc<AudioSettingsWindow> {
        self.app.show_settings();
        let settings = self.app.0.settings.borrow().clone().unwrap();
        let window = settings.window.clone();
        wait_for(|| window.is_mapped(), "settings window", 5.0);
        settle(0.05);
        settings
    }

    fn popup(&self) -> Rc<VolumePopup> {
        self.app.show_popup("bottom", 76);
        let popup = self.app.0.popup.borrow().clone().unwrap();
        let window = popup.window.clone();
        wait_for(|| window.is_mapped(), "volume popup", 5.0);
        settle(0.05);
        popup
    }

    fn history(&self) -> Vec<Call> {
        self.fake.history.borrow().clone()
    }

    fn assert_operation(&self, expected: Call) {
        let fake = self.fake.clone();
        let close = |a: &Call, b: &Call| match (a, b) {
            (Call::Volume(i, v), Call::Volume(j, w)) => i == j && (v - w).abs() < 1e-4,
            (Call::Channel(i, c, v), Call::Channel(j, d, w)) => i == j && c == d && (v - w).abs() < 1e-4,
            _ => a == b,
        };
        wait_for(|| fake.history.borrow().last().is_some_and(|last| close(last, &expected)), &format!("{expected:?}"), 3.0);
    }

    fn node(&self, id: i64) -> Node {
        self.fake.state.borrow().nodes[&id].clone()
    }
}

fn assert_accessible_label(widget: &impl IsA<gtk::Accessible>) {
    assert!(gtk::test_accessible_has_property(widget, gtk::AccessibleProperty::Label), "accessible label");
}

fn model_rows(combo: &gtk::ComboBox) -> Vec<(String, String, bool)> {
    let model = combo.model().and_downcast::<gtk::ListStore>().unwrap();
    let mut rows = Vec::new();
    if let Some(mut iter) = model.iter_first() {
        loop {
            rows.push((model.get::<String>(&iter, 0), model.get::<String>(&iter, 1), model.get::<bool>(&iter, 2)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    rows
}

fn focus(window: &gtk::ApplicationWindow) -> Option<gtk::Widget> {
    gtk::prelude::GtkWindowExt::focus(window)
}

// ----- test_audio.py ----------------------------------------------------------

fn five_settings_pages_and_external_state() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    for name in ["playback", "recording", "outputs", "inputs", "configuration", "loopbacks"] {
        assert!(window.stack.child_by_name(name).is_some(), "{name}");
    }
    assert_eq!(window.device_rows.borrow().len(), 2);
    assert!(window.window.has_css_class("audiomanager"));
    assert_eq!(window.window.title().unwrap(), "Audio Settings");
    assert!(window.navigation.has_css_class("navigation"));
    let playback = window.node_rows.borrow()[&fake::BROWSER].clone();
    assert!(playback.volume_scale.width() as f64 > 0.75 * playback.root.width() as f64 - 60.0,
            "Trailing percentage must leave the slider most of the row width");
    assert!(window.status_label.height() < 40);
    window.stack.set_visible_child_name("outputs");
    settle(0.1);
    let row = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    row.volume_scale.grab_focus();
    let focused = focus(&window.window);
    assert!(focused.is_some());
    let model = window.node_rows.borrow()[&fake::BROWSER].target_combo.as_ref().unwrap().model.clone();
    let deleted = Rc::new(std::cell::Cell::new(0));
    let counter = deleted.clone();
    model.connect_row_deleted(move |_, _| counter.set(counter.get() + 1));
    h.fake.update(fake::SPEAKERS, |n| { n.volume = 0.34; n.channel_volumes = vec![0.34, 0.34]; n.mute = true; });
    wait_for(|| (row.volume_scale.value() - 34.0).abs() < 0.01, "external volume update", 3.0);
    assert!(Rc::ptr_eq(&window.node_rows.borrow()[&fake::SPEAKERS], &row));
    assert_eq!(focus(&window.window), focused);
    assert!(row.mute_button.is_active());
    assert_eq!(row.percent_label.text(), "34%");
    assert!(h.history().is_empty(), "Refresh must not send audio commands");
    assert_eq!(deleted.get(), 0, "Unchanged routing menus must not be rebuilt");
    h.fake.remove(fake::HEADPHONES);
    wait_for(|| !window.node_rows.borrow().contains_key(&fake::HEADPHONES), "hotplug removal", 3.0);
    assert!(Rc::ptr_eq(&window.node_rows.borrow()[&fake::SPEAKERS], &row));
    assert_eq!(focus(&window.window), focused);
    assert_eq!(window.stack.visible_child_name().unwrap(), "outputs");
    assert_eq!(window.status_label.text(), "Changes take effect immediately.");
}

fn volume_mute_and_default_output_and_input() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let output = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    output.volume_scale.set_value(74.0);
    h.assert_operation(Call::Volume(fake::SPEAKERS, 0.74));
    output.mute_button.emit_clicked();
    h.assert_operation(Call::Mute(fake::SPEAKERS, true));
    let headphones = window.node_rows.borrow()[&fake::HEADPHONES].default_button.clone().unwrap();
    headphones.emit_clicked();
    h.assert_operation(Call::Default(fake::HEADPHONES));
    assert_eq!(h.fake.state().default_id("Audio/Sink"), Some(fake::HEADPHONES));
    let usb = window.node_rows.borrow()[&fake::USB_MICROPHONE].default_button.clone().unwrap();
    usb.emit_clicked();
    h.assert_operation(Call::Default(fake::USB_MICROPHONE));
    assert_eq!(h.fake.state().default_id("Audio/Source"), Some(fake::USB_MICROPHONE));
}

fn stream_routing_profiles_ports_and_individual_channels() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let rows = window.node_rows.borrow().clone();
    assert!(rows[&fake::BROWSER].target_combo.as_ref().unwrap().combo.set_active_id(Some(&fake::HEADPHONES.to_string())));
    h.assert_operation(Call::Target(fake::BROWSER, fake::HEADPHONES));
    assert!(rows[&fake::RECORDER].target_combo.as_ref().unwrap().combo.set_active_id(Some(&fake::USB_MICROPHONE.to_string())));
    h.assert_operation(Call::Target(fake::RECORDER, fake::USB_MICROPHONE));
    let output = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    assert!(output.route_combo.as_ref().unwrap().combo.set_active_id(Some("1")));
    h.assert_operation(Call::Route(fake::SPEAKERS, 1));
    let device = window.device_rows.borrow()[&fake::BUILTIN].clone();
    assert!(device.profile_combo.combo.set_active_id(Some("2")));
    h.assert_operation(Call::Profile(fake::BUILTIN, 2));
    output.channels_expander.set_expanded(true);
    output.channel_lock_button.set_active(false);
    let right = h.node(fake::SPEAKERS).channel_volumes[1];
    let scale = output.channel_scales.borrow()[&0].clone();
    scale.set_value(28.0);
    h.assert_operation(Call::Channel(fake::SPEAKERS, 0, 0.28));
    assert!((h.node(fake::SPEAKERS).channel_volumes[1] - right).abs() < 1e-9);
}

fn channel_lock_switches_between_linked_and_independent_edits() {
    let h = Harness::new(FakeMode::Populated);
    h.fake.update(fake::SPEAKERS, |n| { n.channels = vec!["MONO".into()]; n.channel_volumes = vec![0.62]; });
    let window = h.settings();
    let row = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    assert!(!row.channels_controls.get_visible());
    h.fake.update(fake::SPEAKERS, |n| { n.channels = vec!["FL".into(), "FR".into()]; n.channel_volumes = vec![0.62, 0.62]; });
    assert!(row.channels_controls.get_visible());
    assert!(row.channel_lock_button.get_visible() && row.channel_lock_icon.get_visible() && row.channel_lock_button.is_active());
    assert_eq!(row.channel_lock_icon.icon_name().unwrap(), "changes-prevent-symbolic");
    assert_eq!(row.channel_lock_button.tooltip_text().unwrap(), "Unlock channels to adjust them separately");
    assert_accessible_label(&row.channel_lock_button);
    row.channel_lock_button.set_active(false);
    assert_eq!(row.channel_lock_icon.icon_name().unwrap(), "changes-allow-symbolic");
    assert!(h.history().is_empty(), "Locking is a UI preference, not an audio command");
    let first = row.channel_scales.borrow()[&0].clone();
    first.set_value(25.0);
    h.assert_operation(Call::Channel(fake::SPEAKERS, 0, 0.25));
    assert_eq!(h.node(fake::SPEAKERS).channel_volumes, [0.25, 0.62]);
    h.fake.history.borrow_mut().clear();
    row.channel_lock_button.set_active(true);
    settle(0.1);
    assert!(h.history().is_empty(), "Enabling the lock must not change existing balance");
    let second = row.channel_scales.borrow()[&1].clone();
    second.set_value(47.0);
    let values: Vec<f64> = row.channel_scales.borrow().values().map(|s| s.value()).collect();
    assert_eq!(values, [47.0, 47.0]);
    let fake = h.fake.clone();
    wait_for(|| fake.history.borrow().len() == 2, "linked channel volume commands", 3.0);
    let mut history = h.history();
    history.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(history, [Call::Channel(fake::SPEAKERS, 0, 0.47), Call::Channel(fake::SPEAKERS, 1, 0.47)]);
    assert_eq!(h.node(fake::SPEAKERS).channel_volumes, [0.47, 0.47]);
    h.fake.update(fake::SPEAKERS, |n| { n.volume = 0.3; n.channel_volumes = vec![0.2, 0.3]; });
    assert!(row.channel_lock_button.is_active(), "Refresh must preserve the lock preference");
    let values: Vec<f64> = row.channel_scales.borrow().values().map(|s| s.value()).collect();
    assert_eq!(values, [20.0, 30.0]);
    assert_eq!(h.history().len(), 2, "Refresh must not force channels to match");
    h.fake.update(fake::SPEAKERS, |n| n.volume_available = false);
    assert!(!row.channel_lock_button.is_sensitive());
}

fn mute_buttons_use_accessible_speaker_icons() {
    let h = Harness::new(FakeMode::Populated);
    let row = h.settings().node_rows.borrow()[&fake::SPEAKERS].clone();
    let popup = h.popup();
    assert!(row.mute_button.child().and_downcast::<gtk::Image>().is_some());
    assert!(popup.mute_button.child().and_downcast::<gtk::Image>().is_some());
    assert_accessible_label(&row.mute_button);
    assert_accessible_label(&popup.mute_button);
    row.mute_button.emit_clicked();
    h.assert_operation(Call::Mute(fake::SPEAKERS, true));
    for (icon, button) in [(&row.mute_icon, &row.mute_button), (&popup.mute_icon, &popup.mute_button)] {
        assert_eq!(icon.icon_name().unwrap(), "audio-volume-muted-symbolic");
        assert!(button.is_active());
        assert!(button.tooltip_text().unwrap().contains("Unmute"));
    }
    popup.mute_button.emit_clicked();
    h.assert_operation(Call::Mute(fake::SPEAKERS, false));
    h.fake.update(fake::SPEAKERS, |n| { n.volume = 0.2; n.channel_volumes = vec![0.2, 0.2]; });
    for (icon, button) in [(&row.mute_icon, &row.mute_button), (&popup.mute_icon, &popup.mute_button)] {
        assert!(!button.is_active());
        assert_eq!(icon.icon_name().unwrap(), "audio-volume-low-symbolic");
    }
    assert_eq!(popup.mute_button.tooltip_text().unwrap(), "Mute output");
    assert_eq!(row.mute_button.tooltip_text().unwrap(), "Mute");
}

fn virtual_input_listing_routing_and_default_indication() {
    let h = Harness::new(FakeMode::Populated);
    h.fake.update(fake::USB_MICROPHONE, |n| { n.media_class = "Audio/Source/Virtual".into(); n.description = "Noise-cancelled Microphone".into(); });
    let window = h.settings();
    let virtual_row = window.node_rows.borrow()[&fake::USB_MICROPHONE].clone();
    let inputs = window.pages.borrow()["inputs"].0.clone();
    assert_eq!(virtual_row.root.parent().unwrap(), inputs.upcast::<gtk::Widget>());
    assert_eq!(virtual_row.name_label.text(), "Noise-cancelled Microphone");
    let button = virtual_row.default_button.clone().unwrap();
    assert_eq!(button.label().unwrap(), "Set as default");
    assert!(button.is_sensitive());
    let recording = window.node_rows.borrow()[&fake::RECORDER].clone();
    let combo = recording.target_combo.clone().unwrap();
    assert!(combo.combo.set_active_id(Some(&fake::USB_MICROPHONE.to_string())));
    h.assert_operation(Call::Target(fake::RECORDER, fake::USB_MICROPHONE));
    assert_eq!(combo.combo.active_id().unwrap(), fake::USB_MICROPHONE.to_string());
    let browser = window.node_rows.borrow()[&fake::BROWSER].clone();
    let playback = model_rows(&browser.target_combo.as_ref().unwrap().combo);
    assert!(!playback.iter().any(|r| r.0 == fake::USB_MICROPHONE.to_string()));
    button.emit_clicked();
    h.assert_operation(Call::Default(fake::USB_MICROPHONE));
    assert_eq!(h.fake.state().default_id("Audio/Source"), Some(fake::USB_MICROPHONE));
    assert!(h.fake.state().default_id("Audio/Source/Virtual").is_none());
    assert_eq!(button.label().unwrap(), "Default");
    assert!(!button.is_sensitive() && button.has_css_class("primary"));
    let microphone = window.node_rows.borrow()[&fake::MICROPHONE].default_button.clone().unwrap();
    assert_eq!(microphone.label().unwrap(), "Set as default");
    assert!(microphone.is_sensitive());
    let choices = model_rows(&combo.combo);
    assert_eq!(choices.iter().find(|r| r.0 == fake::SPEAKERS.to_string()).unwrap().1, "Monitor of Built-in Speakers");
    assert!(combo.combo.set_active_id(Some(&fake::SPEAKERS.to_string())));
    h.assert_operation(Call::Target(fake::RECORDER, fake::SPEAKERS));
}

fn pending_settings_edits_cancel_when_node_or_channel_layout_changes() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let row = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    row.volume_scale.set_value(93.0);
    let scale = row.channel_scales.borrow()[&0].clone();
    scale.set_value(87.0);
    {
        let mut state = h.fake.state.borrow_mut();
        let node = state.nodes.get_mut(&fake::SPEAKERS).unwrap();
        node.serial += 500;
        node.volume = 0.25;
        node.channel_volumes = vec![0.25, 0.25];
    }
    h.fake.changed();
    wait_for(|| !Rc::ptr_eq(&window.node_rows.borrow()[&fake::SPEAKERS], &row), "replacement node row", 3.0);
    settle(0.2);
    assert!(h.history().is_empty(), "A removed node's pending edits must cancel");
    let row = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    let scale = row.channel_scales.borrow()[&0].clone();
    scale.set_value(81.0);
    h.fake.update(fake::SPEAKERS, |n| { n.channels = vec!["FR".into(), "FL".into()]; n.channel_volumes = vec![0.4, 0.3]; });
    settle(0.2);
    assert!(h.history().is_empty(), "An old channel index must not edit a new layout");
}

fn disconnected_backend_preserves_display_but_disables_controls() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let popup = h.popup();
    {
        let mut state = h.fake.state.borrow_mut();
        state.ready = false;
        state.error = "PipeWire disconnected".into();
    }
    h.fake.report_error("PipeWire disconnected");
    h.fake.changed();
    wait_for(|| !popup.volume_scale.is_sensitive(), "audio disconnection", 3.0);
    let row = window.node_rows.borrow()[&fake::SPEAKERS].clone();
    assert!(!row.volume_scale.is_sensitive() || !row.root.is_sensitive());
    assert!(!row.root.is_sensitive());
    assert!(!popup.mute_button.is_sensitive());
    assert!(popup.settings_button.is_sensitive());
    assert!(window.status_label.text().contains("disconnected"));
    assert!(window.status_label.has_css_class("error"));
    assert!(popup.status_label.text().contains("disconnected") && popup.status_label.get_visible());
    assert!(popup.status_label.has_css_class("error"));
    {
        let mut state = h.fake.state.borrow_mut();
        state.ready = true;
        state.error.clear();
    }
    h.fake.changed();
    wait_for(|| popup.volume_scale.is_sensitive(), "audio reconnection", 3.0);
    assert!(row.volume_scale.is_sensitive() && row.root.is_sensitive());
    assert_eq!(popup.status_label.text(), "");
    assert!(!popup.status_label.get_visible());
    assert!(!window.status_label.has_css_class("error"));
    assert!(h.history().is_empty());
    h.fake.update(fake::SPEAKERS, |n| n.volume_available = false);
    wait_for(|| !popup.volume_scale.is_sensitive(), "unsupported software mixer state", 3.0);
    assert!(!row.volume_scale.is_sensitive() && !row.mute_button.is_sensitive());
    assert_eq!(popup.status_label.text(), "This output does not expose software volume controls.");
}

// ----- popup (test_audio.py / test_popup_mixer.py) --------------------------------

fn popup_controls_follow_default_and_open_settings() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    assert!(!popup.window.is_decorated());
    assert!(popup.window.width() <= 460);
    assert!(popup.window.has_css_class("tray-popup") && popup.window.has_css_class("audiomanager"));
    assert_eq!(popup.window.title().unwrap(), "Volume");
    assert!(gtk4_layer_shell::LayerShell::is_layer_window(&popup.window));
    assert_eq!(gtk4_layer_shell::LayerShell::namespace(&popup.window).unwrap(), "desktop-applet");
    popup.volume_scale.set_value(48.0);
    h.assert_operation(Call::Volume(fake::SPEAKERS, 0.48));
    assert_eq!(popup.volume_label.text(), "48%");
    popup.mute_button.emit_clicked();
    h.assert_operation(Call::Mute(fake::SPEAKERS, true));
    h.fake.state.borrow_mut().defaults.insert("Audio/Sink".into(), fake::HEADPHONES);
    h.fake.changed();
    wait_for(|| (popup.volume_scale.value() - 45.0).abs() < 0.01, "popup default output switch", 3.0);
    assert_eq!(popup.output_combo.combo.active_id().unwrap(), fake::HEADPHONES.to_string());
    popup.volume_scale.set_value(37.0);
    h.assert_operation(Call::Volume(fake::HEADPHONES, 0.37));
    popup.settings_button.emit_clicked();
    wait_for(|| h.app.0.settings.borrow().as_ref().is_some_and(|s| s.window.is_mapped()), "Audio Settings button", 5.0);
    assert!(!popup.window.get_visible());
    let settings = h.app.0.settings.borrow().clone().unwrap();
    h.app.show_popup("bottom", 76);
    wait_for(|| popup.window.is_mapped(), "popup while settings is open", 5.0);
    assert!(Rc::ptr_eq(h.app.0.settings.borrow().as_ref().unwrap(), &settings));
    assert!(Rc::ptr_eq(h.app.0.popup.borrow().as_ref().unwrap(), &popup));
    popup.settings_button.emit_clicked();
    settle(0.05);
    assert!(Rc::ptr_eq(h.app.0.settings.borrow().as_ref().unwrap(), &settings));
    // A different placement recreates the popup.
    h.app.show_popup("top", 10);
    settle(0.1);
    assert!(!Rc::ptr_eq(h.app.0.popup.borrow().as_ref().unwrap(), &popup));
    assert!(popup.is_destroyed());
}

fn missing_output_disables_popup_and_hotplug_recovers() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    let speakers = h.node(fake::SPEAKERS);
    h.fake.remove(fake::SPEAKERS);
    h.fake.remove(fake::HEADPHONES);
    wait_for(|| !popup.volume_scale.is_sensitive(), "missing output state", 3.0);
    assert!(!popup.mute_button.is_sensitive() && popup.settings_button.is_sensitive());
    assert_eq!(popup.output_name.text(), "No output device");
    assert!(!popup.output_combo.combo.get_visible() && popup.output_name.get_visible());
    assert_eq!(popup.volume_label.text(), "—");
    assert_eq!(popup.status_label.text(), "Connect an audio device or choose an output in Audio Settings.");
    {
        let mut state = h.fake.state.borrow_mut();
        state.nodes.insert(speakers.id, speakers.clone());
        state.defaults.insert("Audio/Sink".into(), speakers.id);
    }
    h.fake.changed();
    wait_for(|| popup.volume_scale.is_sensitive(), "hotplug output recovery", 3.0);
    assert!(popup.mute_button.is_sensitive());
}

fn popup_discards_drag_if_default_changes_before_dispatch() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    h.fake.history.borrow_mut().clear();
    popup.volume_scale.set_value(96.0);
    h.fake.state.borrow_mut().defaults.insert("Audio/Sink".into(), fake::HEADPHONES);
    h.fake.changed();
    settle(0.25);
    assert!(h.history().is_empty(), "A stale drag must not change either output");
    assert!((popup.volume_scale.value() - 45.0).abs() < 0.01);
}

fn pending_popup_drag_does_not_target_reused_pipewire_id() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    h.fake.history.borrow_mut().clear();
    popup.volume_scale.set_value(96.0);
    {
        let mut state = h.fake.state.borrow_mut();
        let node = state.nodes.get_mut(&fake::SPEAKERS).unwrap();
        node.serial += 500;
        node.description = "Replacement speakers".into();
        node.volume = 0.25;
        node.channel_volumes = vec![0.25, 0.25];
    }
    h.fake.changed();
    settle(0.25);
    assert!(h.history().is_empty(), "An old drag must not affect a new node");
    assert!((popup.volume_scale.value() - 25.0).abs() < 0.01);
}

fn input_level_meters_run_only_while_visible() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    let fake = h.fake.clone();
    wait_for(|| fake.active_meters() == [fake::MICROPHONE], "popup default input meter", 3.0);
    let rows = popup.node_rows.borrow().clone();
    let microphone = rows[&fake::MICROPHONE].clone();
    assert!(microphone.level_bar.as_ref().unwrap().get_visible());
    assert!(!rows[&fake::USB_MICROPHONE].level_bar.as_ref().unwrap().get_visible());
    assert!(rows[&fake::BROWSER].level_bar.is_none());
    h.fake.emit_level(Some(0.8));
    assert!((microphone.level_value() - 0.8).abs() < 1e-9);
    h.fake.emit_level(Some(0.0));
    assert!(microphone.level_value() < 0.8);
    h.fake.state.borrow_mut().defaults.insert("Audio/Source".into(), fake::USB_MICROPHONE);
    h.fake.changed();
    wait_for(|| fake.active_meters() == [fake::USB_MICROPHONE], "meter follows default", 3.0);
    assert_eq!(microphone.level_value(), 0.0);
    popup.dismiss();
    wait_for(|| fake.active_meters().is_empty(), "meter stops with hidden popup", 3.0);
    let window = h.settings();
    settle(0.1);
    assert!(h.fake.active_meters().is_empty());
    window.stack.set_visible_child_name("inputs");
    wait_for(|| fake.active_meters() == [fake::MICROPHONE, fake::USB_MICROPHONE], "input page meters", 3.0);
    window.stack.set_visible_child_name("outputs");
    wait_for(|| fake.active_meters().is_empty(), "meters stop when the page is hidden", 3.0);
    assert!(!h.history().iter().any(|c| matches!(c, Call::Default(_))));
}

fn popup_device_names_switch_default_output_and_input() {
    let h = Harness::new(FakeMode::Populated);
    let popup = h.popup();
    assert_eq!(popup.output_combo.labels(), ["Built-in Speakers", "USB Headphones"]);
    assert!(!popup.output_name.get_visible());
    assert!(popup.output_combo.combo.set_active_id(Some(&fake::HEADPHONES.to_string())));
    h.assert_operation(Call::Default(fake::HEADPHONES));
    assert!(popup.input_combo.combo.set_active_id(Some(&fake::USB_MICROPHONE.to_string())));
    h.assert_operation(Call::Default(fake::USB_MICROPHONE));
    let count = h.history().len();
    h.fake.state.borrow_mut().defaults.insert("Audio/Sink".into(), fake::SPEAKERS);
    h.fake.changed();
    let speakers = fake::SPEAKERS.to_string();
    wait_for(|| popup.output_combo.combo.active_id().as_deref() == Some(speakers.as_str()), "external default", 3.0);
    settle(0.1);
    assert_eq!(h.history().len(), count, "external defaults are not written back");
    let sinks: Vec<i64> = h.fake.state().nodes.values().filter(|n| n.media_class == "Audio/Sink").map(|n| n.id).collect();
    for id in sinks {
        h.fake.remove(id);
    }
    wait_for(|| !popup.output_combo.combo.get_visible(), "no outputs", 3.0);
    assert!(popup.output_name.get_visible());
}

// ----- test_loopback_ui.py ------------------------------------------------------

fn loopback_creation_and_unavailable_selection() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    window.stack.set_visible_child_name("loopbacks");
    assert!(window.add_loopback_button.is_sensitive());
    window.add_loopback_button.emit_clicked();
    let dialog = window.loopback_dialog.borrow().clone().unwrap();
    wait_for(|| dialog.dialog.is_mapped(), "loopback dialog", 3.0);
    let ok = dialog.dialog.widget_for_response(gtk::ResponseType::Ok).unwrap();
    assert!(!ok.is_sensitive(), "a name is required");
    assert!(ok.has_css_class("primary"));
    let target = h.node(fake::SPEAKERS);
    assert_eq!(dialog.target_combo.combo.active_id().unwrap(), target.serial.to_string(), "default output preselected");
    dialog.name_entry.set_text("Headset Loopback");
    assert!(ok.is_sensitive());
    h.fake.remove(target.id);
    assert!(!ok.is_sensitive());
    let rows = model_rows(&dialog.target_combo.combo);
    let selected = rows.iter().find(|r| r.0 == target.serial.to_string()).unwrap();
    assert!(selected.1.contains("unavailable") && !selected.2);
    // Reusing an object id must not silently select different hardware.
    let mut replacement = target.clone();
    replacement.serial += 100;
    replacement.name = "replacement".into();
    h.fake.state.borrow_mut().nodes.insert(target.id, replacement);
    h.fake.changed();
    assert!(!ok.is_sensitive());
    assert!(h.history().is_empty());
    h.fake.state.borrow_mut().nodes.insert(target.id, target.clone());
    h.fake.changed();
    assert!(ok.is_sensitive());
    dialog.dialog.response(gtk::ResponseType::Ok);
    settle(0.05);
    assert!(window.loopback_dialog.borrow().is_none());
    h.assert_operation(Call::AddLoopback("Headset Loopback".into(), target.id, target.serial));
    assert_eq!(window.loopback_rows.borrow().len(), 1);
}

fn loopback_survives_missing_target_and_can_be_removed_offline() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let target = h.node(fake::SPEAKERS);
    h.fake.add_loopback("Speaker Loopback", target.id, target.serial).unwrap();
    let entry = h.fake.state().loopbacks.values().next().unwrap().clone();
    let row = window.loopback_rows.borrow()[&entry.id].clone();
    assert!(row.state_label.text().contains("Starting"));
    assert_eq!(row.target_label.text(), "Output: Built-in Speakers");
    let mut managed = target.clone();
    managed.id = 90;
    managed.serial = 1090;
    managed.name = "managed.loopback".into();
    managed.loopback_id = Some(entry.id.clone());
    h.fake.state.borrow_mut().nodes.insert(90, managed);
    h.fake.changed();
    assert!(row.state_label.text().contains("Active"));
    h.fake.remove(target.id);
    assert_eq!(row.state_label.text(), "Disabled · Output device unavailable");
    assert!(h.fake.state().loopbacks.contains_key(&entry.id));
    assert!(row.remove_button.is_sensitive());
    h.fake.state.borrow_mut().nodes.insert(target.id, target.clone());
    h.fake.changed();
    assert!(row.state_label.text().contains("Active"));
    {
        let mut state = h.fake.state.borrow_mut();
        state.ready = false;
        state.nodes.clear();
    }
    h.fake.changed();
    assert_eq!(row.state_label.text(), "Disabled · Audio service unavailable");
    row.remove_button.emit_clicked();
    h.assert_operation(Call::RemoveLoopback(entry.id.clone()));
    assert!(window.loopback_rows.borrow().is_empty());
}

fn managed_loopbacks_cannot_target_each_other_and_internal_streams_are_hidden() {
    let h = Harness::new(FakeMode::Populated);
    let window = h.settings();
    let mut managed = h.node(fake::SPEAKERS);
    managed.id = 90;
    managed.serial = 1090;
    managed.loopback_id = Some("saved".into());
    managed.name = "managed.loopback".into();
    let mut internal = h.node(fake::MUSIC);
    internal.id = 91;
    internal.serial = 1091;
    internal.loopback_id = Some("saved".into());
    internal.loopback_internal = true;
    {
        let mut state = h.fake.state.borrow_mut();
        state.nodes.insert(90, managed);
        state.nodes.insert(91, internal);
    }
    h.fake.changed();
    assert!(window.node_rows.borrow().contains_key(&90));
    assert!(!window.node_rows.borrow().contains_key(&91));
    window.add_loopback_button.emit_clicked();
    let dialog = window.loopback_dialog.borrow().clone().unwrap();
    assert!(!dialog.target_combo.combo.set_active_id(Some("1090")));
}

// ----- test_volume_app.py: real CLI forwarding into this primary ----------------

fn remote_many(options: &[&str]) -> f64 {
    let started = Instant::now();
    let binary = env!("CARGO_BIN_EXE_audio-manager");
    let mut children: Vec<_> = options.iter().map(|option| {
        Command::new(binary).arg(option).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
    }).collect();
    wait_for(|| children.iter_mut().all(|c| c.try_wait().unwrap().is_some()), "remote audio key launchers to return", 3.0);
    let elapsed = started.elapsed().as_secs_f64();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{:?} {}", output.status, String::from_utf8_lossy(&output.stderr));
    }
    assert!(elapsed < 1.5, "Remote commands blocked for {elapsed:.2}s");
    elapsed
}

fn find_label(widget: &gtk::Widget, text: &str) -> bool {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        if label.has_css_class("osd-value") && label.text() == text {
            return true;
        }
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if find_label(&c, text) {
            return true;
        }
        child = c.next_sibling();
    }
    false
}

fn osd_at(app: &AudioApplication, text: &str) -> bool {
    let Some(osd) = app.0.osd.borrow().clone() else { return false };
    let Some(window) = osd.window() else { return false };
    window.is_mapped() && find_label(window.upcast_ref(), text)
}

fn command(app: &AudioApplication, option: &str, text: &str) {
    remote_many(&[option]);
    let keys = app.0.volume_keys.borrow().clone().unwrap();
    wait_for(|| osd_at(app, text) && !keys.pending(), &format!("{text} confirmed indicator"), 5.0);
}

fn key_only(a: &AudioApplication, f: &FakeAudioBackend) {
    let keys = a.0.volume_keys.borrow().clone().unwrap();
    assert!(a.key_held(), "The first keyboard launch must hold its startup request");
    assert!(a.0.popup.borrow().is_none() && a.0.settings.borrow().is_none());
    assert_eq!(a.0.backends_created.get(), 1);
    remote_many(&["--volume-up", "--volume-up", "--volume-up"]);
    wait_for(|| osd_at(a, "70%") && !keys.pending(), "initial key plus forwarded repeats", 5.0);
    assert!((f.state().nodes[&20].volume - 0.7).abs() < 1e-5);
    let window = a.0.osd.borrow().as_ref().unwrap().window().unwrap();
    assert!(gtk4_layer_shell::LayerShell::is_layer_window(&window));
    assert_eq!(gtk4_layer_shell::LayerShell::keyboard_mode(&window), gtk4_layer_shell::KeyboardMode::None,
               "Volume feedback must not steal keyboard focus");
    assert_eq!(gtk4_layer_shell::LayerShell::namespace(&window).unwrap(), "desktop-adjustment-osd");
    assert!(window.application().is_none(), "A hidden indicator must not keep the application alive");
    assert!(a.0.app.windows().is_empty(), "Keyboard-only launch must not register a settings or popup window");
    command(a, "--toggle-mute", "Muted");
    assert!(f.state().nodes[&20].mute);
    command(a, "--volume-up", "Muted");
    assert!((f.state().nodes[&20].volume - 0.75).abs() < 1e-5 && f.state().nodes[&20].mute);
    command(a, "--toggle-mute", "75%");
    assert!(!f.state().nodes[&20].mute);
    command(a, "--volume-down", "70%");
    assert_eq!(a.0.osd.borrow().as_ref().unwrap().window().unwrap(), window, "Repeated keys should update one indicator");
    assert!(a.0.popup.borrow().is_none() && a.0.settings.borrow().is_none());
    assert_eq!(a.0.backends_created.get(), 1);
}

fn popup_resident(a: &AudioApplication, f: &FakeAudioBackend) {
    wait_for(|| a.0.popup.borrow().as_ref().is_some_and(|p| p.window.is_mapped()) && f.state().ready, "resident volume popup", 5.0);
    let popup = a.0.popup.borrow().clone().unwrap();
    assert!(a.held() && !a.key_held());
    popup.dismiss();
    command(a, "--volume-down", "45%");
    assert!(Rc::ptr_eq(a.0.popup.borrow().as_ref().unwrap(), &popup) && !popup.window.get_visible());
    assert!(a.0.settings.borrow().is_none());
    wait_for(|| a.0.osd.borrow().as_ref().is_some_and(|o| o.window().is_none()), "automatic indicator dismissal", 4.0);
    assert!(!a.key_held() && a.held(), "Only the transient key hold should be released");
    assert!(!f.closed.get());
    remote_many(&["--popup"]);
    wait_for(|| popup.window.is_mapped(), "resident popup reopening through real CLI forwarding", 5.0);
    assert!(Rc::ptr_eq(a.0.popup.borrow().as_ref().unwrap(), &popup) && a.0.backends_created.get() == 1);
    assert!(a.0.settings.borrow().is_none());
    // Validation errors are reported by the launcher itself, not forwarded.
    let output = Command::new(env!("CARGO_BIN_EXE_audio-manager")).args(["--popup", "--volume-up"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).ends_with("error: argument --volume-up: not allowed with argument --popup\n"));
    popup.dismiss();
    a.0.app.quit();
}

fn run_app_case(case: &'static str) {
    suite_core::i18n::initialize();
    let fake = FakeAudioBackend::new(FakeMode::Keys);
    let shared = fake.clone();
    let factory: BackendFactory = Box::new(move |changed, error| {
        shared.set_callbacks(changed, error);
        shared.clone() as Rc<dyn AudioApi>
    });
    let app = AudioApplication::new(APPLICATION_ID, Some(factory));
    let failures: Rc<std::cell::RefCell<Vec<String>>> = Rc::default();
    let final_time = Rc::new(std::cell::Cell::new(None::<Instant>));
    let (a, f, sink, last) = (app.clone(), fake.clone(), failures.clone(), final_time.clone());
    glib::idle_add_local_once(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if case == "key-only" {
                key_only(&a, &f);
                // Return without quit(): the 1500 ms OSD expiry must end the app.
                last.set(Some(Instant::now()));
            } else {
                popup_resident(&a, &f);
            }
        }));
        if let Err(error) = result {
            let message = error.downcast_ref::<String>().cloned().or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            sink.borrow_mut().push(message);
            a.0.app.quit();
        }
    });
    let (watchdog_app, watchdog_failures) = (app.clone(), failures.clone());
    let fired = Rc::new(std::cell::Cell::new(false));
    let flag = fired.clone();
    let watchdog = glib::timeout_add_local_once(Duration::from_secs(12), move || {
        flag.set(true);
        watchdog_failures.borrow_mut().push("Audio application did not exit after the expected OSD lifecycle".into());
        watchdog_app.0.app.quit();
    });
    let argv = vec!["audio_manager.py".to_owned(), if case == "key-only" { "--volume-up" } else { "--popup" }.to_owned()];
    let code = app.run(&argv);
    if !fired.get() {
        watchdog.remove();
    }
    if let Some(failure) = failures.borrow().first() {
        panic!("{failure}");
    }
    assert_eq!(code, 0);
    assert!(fake.closed.get(), "backend closed at shutdown");
    assert!(!app.key_held() && !app.held());
    assert!(app.0.osd.borrow().as_ref().unwrap().window().is_none());
    if case == "key-only" {
        let elapsed = final_time.get().unwrap().elapsed().as_secs_f64();
        assert!((1.0..3.0).contains(&elapsed), "Key-only process exit did not follow indicator expiry: {elapsed:.2}s");
    }
}

fn key_only_launch_forwarding_and_osd_lifetime() {
    run_app_case("key-only");
}

fn popup_resident_keeps_backend() {
    run_app_case("popup-resident");
}

fn remote_launch_reuses_backend_and_routes_between_windows() {
    let h = Harness::new(FakeMode::Populated);
    let settings = h.settings();
    let binary = env!("CARGO_BIN_EXE_audio-manager");
    let mut child = Command::new(binary).args(["--popup", "--popup-edge", "top", "--popup-offset", "12"]).spawn().unwrap();
    wait_for(|| child.try_wait().unwrap().is_some(), "secondary launcher exit", 10.0);
    assert!(child.wait().unwrap().success());
    wait_for(|| h.app.0.popup.borrow().as_ref().is_some_and(|p| p.window.is_mapped()), "remote volume popup", 5.0);
    let popup = h.app.0.popup.borrow().clone().unwrap();
    assert!(Rc::ptr_eq(h.app.0.settings.borrow().as_ref().unwrap(), &settings));
    let mut child = Command::new(binary).arg("--settings").spawn().unwrap();
    wait_for(|| child.try_wait().unwrap().is_some(), "secondary launcher exit", 10.0);
    assert!(child.wait().unwrap().success());
    wait_for(|| settings.window.is_mapped() && !popup.window.get_visible(), "remote settings activation", 5.0);
    assert_eq!(h.app.0.backends_created.get(), 1);
    assert!(Rc::ptr_eq(h.app.0.settings.borrow().as_ref().unwrap(), &settings));
    assert!(Rc::ptr_eq(h.app.0.popup.borrow().as_ref().unwrap(), &popup));
    // The primary owns the D-Bus name install_adjustment_osd.py looks up.
    let connection = h.app.0.app.dbus_connection().unwrap();
    assert!(suite_core::single_instance::name_has_owner(&connection, APPLICATION_ID));
}

fn unavailable_backend_shows_error_in_both_views() {
    let h = Harness::new(FakeMode::Unavailable);
    let window = h.settings();
    let popup = h.popup();
    settle(0.1);
    assert_eq!(window.status_label.text(), "PipeWire is unavailable");
    assert!(popup.status_label.text().contains("unavailable"));
    assert!(!window.add_loopback_button.is_sensitive());
    assert_eq!(popup.output_name.text(), "Connecting to audio service…");
}

type Case = (&'static str, fn());

const CASES: &[Case] = &[
    ("five_settings_pages_and_external_state", five_settings_pages_and_external_state),
    ("volume_mute_and_default_output_and_input", volume_mute_and_default_output_and_input),
    ("stream_routing_profiles_ports_and_individual_channels", stream_routing_profiles_ports_and_individual_channels),
    ("channel_lock_switches_between_linked_and_independent_edits", channel_lock_switches_between_linked_and_independent_edits),
    ("mute_buttons_use_accessible_speaker_icons", mute_buttons_use_accessible_speaker_icons),
    ("virtual_input_listing_routing_and_default_indication", virtual_input_listing_routing_and_default_indication),
    ("pending_settings_edits_cancel_when_node_or_channel_layout_changes", pending_settings_edits_cancel_when_node_or_channel_layout_changes),
    ("disconnected_backend_preserves_display_but_disables_controls", disconnected_backend_preserves_display_but_disables_controls),
    ("popup_controls_follow_default_and_open_settings", popup_controls_follow_default_and_open_settings),
    ("missing_output_disables_popup_and_hotplug_recovers", missing_output_disables_popup_and_hotplug_recovers),
    ("popup_discards_drag_if_default_changes_before_dispatch", popup_discards_drag_if_default_changes_before_dispatch),
    ("pending_popup_drag_does_not_target_reused_pipewire_id", pending_popup_drag_does_not_target_reused_pipewire_id),
    ("input_level_meters_run_only_while_visible", input_level_meters_run_only_while_visible),
    ("popup_device_names_switch_default_output_and_input", popup_device_names_switch_default_output_and_input),
    ("loopback_creation_and_unavailable_selection", loopback_creation_and_unavailable_selection),
    ("loopback_survives_missing_target_and_can_be_removed_offline", loopback_survives_missing_target_and_can_be_removed_offline),
    ("managed_loopbacks_cannot_target_each_other_and_internal_streams_are_hidden", managed_loopbacks_cannot_target_each_other_and_internal_streams_are_hidden),
    ("key_only_launch_forwarding_and_osd_lifetime", key_only_launch_forwarding_and_osd_lifetime),
    ("popup_resident_keeps_backend", popup_resident_keeps_backend),
    ("remote_launch_reuses_backend_and_routes_between_windows", remote_launch_reuses_backend_and_routes_between_windows),
    ("unavailable_backend_shows_error_in_both_views", unavailable_backend_shows_error_in_both_views),
];

fn main() {
    if std::env::var("AUDIO_MANAGER_UI").as_deref() != Ok("1") {
        println!("skipped: run components/audio-manager/tests/run_headless.sh (private headless labwc)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(runtime != format!("/run/user/{}", unsafe { libc::getuid() }) && std::env::var_os("WAYLAND_DISPLAY").is_some(),
            "refusing to run outside the private compositor");
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|a| a == "--case") {
        let name = &args[index + 1];
        let (_, case) = CASES.iter().find(|(n, _)| n == name).expect("known case");
        case();
        return;
    }
    let filter = args.get(1).filter(|a| !a.starts_with('-')).cloned();
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, _) in CASES {
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        let started = Instant::now();
        let output = Command::new(&args[0]).args(["--case", name]).output().unwrap();
        let ok = output.status.success();
        println!("test {name} ... {} ({:.1}s)", if ok { "ok" } else { "FAILED" }, started.elapsed().as_secs_f64());
        if !ok {
            println!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            failed.push(*name);
        }
    }
    println!("\n{} passed; {} failed", ran - failed.len(), failed.len());
    if !failed.is_empty() {
        std::process::exit(1);
    }
}
