//! Widget-level port of test_ui.py / test_popup.py / test_single_instance.py.
//! Runs only inside the private nested headless labwc (tests/run_headless.sh);
//! under a plain `cargo test` it skips itself. BlueZ is the in-memory
//! `FakeBluez`; transfers use a fake sender; the single-instance checks run
//! the real binary against examples/fake_bluez on the private buses.
#![allow(deprecated)]

use bluetooth_manager::backend::*;
use bluetooth_manager::dbus::BtError;
use bluetooth_manager::env::GlibEnv;
use bluetooth_manager::manager::Services;
use bluetooth_manager::testing::*;
use bluetooth_manager::transfer::{DoneFn, Progress, ProgressFn, Sender};
use bluetooth_manager::ui::{self, Options, Ui};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

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

fn drain() {
    let context = glib::MainContext::default();
    for _ in 0..1000 {
        if !context.pending() {
            return;
        }
        context.iteration(false);
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while !predicate() && Instant::now() < end {
        settle(0.02);
    }
    predicate()
}

struct FakeSender {
    source: String,
    destination: String,
    files: Vec<String>,
    on_progress: ProgressFn,
    on_done: RefCell<Option<DoneFn>>,
    started: Cell<bool>,
    cancelled: Cell<bool>,
}

impl Sender for FakeSender {
    fn start(&self) { self.started.set(true) }
    fn cancel(&self) { self.cancelled.set(true) }
}

impl FakeSender {
    fn finish(&self, error: Option<BtError>, cancelled: bool) {
        if let Some(done) = self.on_done.take() {
            done(error, cancelled);
        }
    }
}

struct Fx {
    bz: Rc<FakeBluez>,
    ui: Rc<Ui>,
    senders: Rc<RefCell<Vec<Rc<FakeSender>>>>,
    saved: Rc<RefCell<Vec<String>>>,
}

impl Fx {
    fn new(state: &str, popup: bool) -> Self {
        let bz = FakeBluez::new(state, false);
        let senders: Rc<RefCell<Vec<Rc<FakeSender>>>> = Rc::default();
        let saved: Rc<RefCell<Vec<String>>> = Rc::default();
        let (s, sv) = (senders.clone(), saved.clone());
        let factory: ui::SenderFactory = Rc::new(move |source, destination, files, on_progress, on_done| {
            let sender = Rc::new(FakeSender { source: source.into(), destination: destination.into(), files: files.to_vec(),
                                              on_progress, on_done: RefCell::new(Some(on_done)), started: Cell::new(false),
                                              cancelled: Cell::new(false) });
            s.borrow_mut().push(sender.clone());
            sender as Rc<dyn Sender>
        });
        let services = Services {
            load_receive_mode: Rc::new(|| "ask".into()),
            save_receive_mode: Rc::new(move |m| { sv.borrow_mut().push(m.into()); Ok(()) }),
            receiver: Rc::new(|_, _| panic!("no OBEX agent in tests")),
            monitor_config: false,
            ..Services::default()
        };
        let ui = Ui::new(Options { application: None, popup, popup_edge: "bottom".into(), popup_offset: 76, lang: "en".into(),
                                   bt: bz.bt.clone(), env: GlibEnv::new(), services, sender_factory: factory, handle_sigint: false });
        settle(0.1);
        Self { bz, ui, senders, saved }
    }

    fn with_object_push() -> Self {
        let f = Fx::new("multi-adapter", false);
        f.ui.adapter_combo.set_active_id(Some(ADAPTER));
        f.bz.bt.devices.borrow_mut().get_mut(KNOWN).unwrap().uuids = vec![OBJECT_PUSH_UUID.into()];
        f.refresh();
        f
    }

    fn refresh(&self) {
        self.ui.manager.refresh_ui();
        drain();
    }

    fn row(&self, path: &str) -> ui::Row {
        self.ui.rows.borrow().get(path).cloned().unwrap_or_else(|| panic!("no row {path}"))
    }

    fn complete(&self, error: Option<BtError>) -> String {
        let call = self.bz.complete(error);
        drain();
        fake_method(&call)
    }

    fn pending_method(&self) -> String {
        fake_method(&self.bz.first_pending().expect("pending"))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let dialogs = self.ui.dialogs.borrow().clone();
        for dialog in dialogs {
            dialog.window.destroy();
        }
        let add = self.ui.add.borrow().as_ref().map(|a| a.window.clone());
        if let Some(add) = add {
            add.destroy();
        }
        for sender in self.senders.borrow().iter() {
            sender.finish(None, true);
        }
        for _ in 0..20 {
            drain();
            if self.bz.first_pending().is_none() {
                break;
            }
            self.bz.complete(None);
        }
        if let Some(popup) = self.ui.popup.borrow().as_ref() {
            popup.close();
        }
        self.ui.window.destroy();
        settle(0.05);
    }
}

const CONNECTED: &str = "/org/bluez/hci0/dev_01";
const KNOWN: &str = "/org/bluez/hci0/dev_02";
const NEARBY: &str = "/org/bluez/hci0/dev_03";

fn labels_of(widget: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        out.push(label.text().to_string());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        out.extend(labels_of(&c));
        child = c.next_sibling();
    }
    out
}

fn window_layout_rows_actions_and_reuse() {
    let f = Fx::new("populated", false);
    assert!(f.ui.window.has_css_class("btmanager"));
    assert_eq!(f.ui.window.size_request(), (560, 440));
    let paths: std::collections::BTreeSet<String> = f.ui.rows.borrow().keys().cloned().collect();
    assert_eq!(paths, [CONNECTED, KNOWN].iter().map(|s| s.to_string()).collect());
    assert!(f.ui.add_btn.is_sensitive() && f.ui.power_switch.is_active());
    assert_eq!(f.ui.event_lbl.text(), "Press \"Add device\" to connect a new device.");
    assert_eq!(f.ui.list_connected.label.text(), "Connected devices");
    assert_eq!(f.ui.list_connected.badge.text(), "1");
    assert!(f.ui.list_connected.badge.has_css_class("connected-count"));
    assert_eq!(f.ui.status_lbl.text(), "Controller: My computer");
    // Unrelated refreshes keep the row and its trust switch.
    let row = f.row(KNOWN);
    f.bz.bt.devices.borrow_mut().get_mut("/org/bluez/hci0/dev_04").unwrap().alias = "A renamed phone".into();
    f.refresh();
    assert_eq!(f.row(KNOWN).row, row.row);
    assert!(row.trust_switch.as_ref().unwrap().is_active());
    // Connect / disconnect buttons drive BlueZ and regroup rows.
    assert_eq!(f.row(CONNECTED).connect_button.label().as_deref(), Some("Disconnect"));
    f.row(KNOWN).connect_button.emit_clicked();
    drain();
    assert!(!f.row(KNOWN).connect_button.is_sensitive());
    assert_eq!(f.row(KNOWN).connect_button.label().as_deref(), Some("Connecting…"));
    assert_eq!(f.complete(None), "Connect");
    assert_eq!(f.row(KNOWN).row.parent().unwrap(), f.ui.list_connected.listbox.clone().upcast::<gtk::Widget>());
    // Trust switch toggles, and reverts when BlueZ refuses.
    let trust = f.row(KNOWN).trust_switch.unwrap();
    trust.set_active(false);
    drain();
    assert_eq!(f.pending_method(), "Set:Trusted");
    assert!(!f.row(KNOWN).trust_switch.unwrap().is_sensitive());
    f.complete(Some(gerr("Trust refused")));
    assert!(f.row(KNOWN).trust_switch.unwrap().is_active(), "trust switch reverts on error");
    assert!(f.ui.event_bar.has_css_class("error"));
    assert!(f.ui.event_lbl.tooltip_text().unwrap().contains("Details: Trust refused"));
    // Forget removes the row.
    f.row(KNOWN).forget_button.unwrap().emit_clicked();
    drain();
    assert_eq!(f.complete(None), "RemoveDevice");
    assert!(!f.ui.rows.borrow().contains_key(KNOWN));
    // Battery levels.
    f.ui.manager.on_props_changed(&bluetooth_manager::dbus::Signal { sender: BLUEZ.into(), path: CONNECTED.into(),
        interface: PROPS_IFACE.into(), member: "PropertiesChanged".into(), params: glib::Variant::tuple_from_iter([
            BATTERY_IFACE.to_variant(), bluetooth_manager::dbus::vardict(&[("Percentage", 76u8.to_variant())]), Vec::<String>::new().to_variant()]) });
    drain();
    assert_eq!(f.row(CONNECTED).battery_label.unwrap().text(), "76%");
    // Receive files drop-down saves without starting an agent.
    let combo = f.ui.receive_combo.clone().unwrap();
    assert_eq!(combo.selected(), 1);
    combo.set_selected(0);
    drain();
    assert_eq!(*f.saved.borrow(), vec!["off"]);
    assert!(!f.ui.manager.has_receiver());
    // Power off shows the off page and disables actions.
    f.ui.power_switch.set_active(false);
    drain();
    assert_eq!(f.complete(None), "Set:Powered");
    assert_eq!(f.ui.content_stack.visible_child_name().as_deref(), Some("off"));
    assert!(f.ui.power_tile.has_css_class("off"));
    assert!(!f.row(CONNECTED).connect_button.is_sensitive());
    println!("ok window_layout_rows_actions_and_reuse");
}

fn add_device_window_scans_and_releases() {
    let f = Fx::new("populated", false);
    f.ui.add_btn.emit_clicked();
    drain();
    let (window, scan, spinner, status, owner) = f.ui.add.borrow().as_ref()
        .map(|a| (a.window.clone(), a.scan.clone(), a.spinner.clone(), a.status_lbl.clone(), a.adapter_lbl.clone())).unwrap();
    assert_eq!(window.transient_for().unwrap(), f.ui.window);
    assert!(!window.is_modal());
    assert_eq!(f.pending_method(), "StartDiscovery");
    assert!(!scan.button.is_sensitive());
    f.ui.add_btn.emit_clicked();
    assert_eq!(f.bz.pending().len(), 1);
    f.complete(None);
    assert!(scan.button.is_sensitive() && spinner.is_spinning() && spinner.get_visible());
    assert_eq!(scan.caption.text(), "Stop scan");
    assert_eq!(status.text(), "Scanning for nearby devices…");
    assert!(owner.text().contains("My computer"));
    let nearby = f.ui.nearby().unwrap();
    assert_eq!(f.row(NEARBY).row.parent().unwrap(), nearby.listbox.clone().upcast::<gtk::Widget>());
    assert!(f.row(NEARBY).trust_switch.is_none());
    // Connecting a nearby device pairs first.
    f.row(NEARBY).connect_button.emit_clicked();
    drain();
    assert_eq!(f.complete(None), "Pair");
    assert_eq!(f.complete(None), "Connect");
    // The scan button stops and restarts only our session.
    scan.button.emit_clicked();
    drain();
    assert_eq!(f.complete(None), "StopDiscovery");
    assert_eq!(scan.caption.text(), "Start scan");
    assert_eq!(status.text(), "Scan stopped");
    assert!(!spinner.get_visible());
    scan.button.emit_clicked();
    drain();
    f.complete(None);
    // A failed operation's message also shows in the Add device window.
    f.ui.manager.set_event("Error: x", true, None, None);
    let add_event = f.ui.add.borrow().as_ref().map(|a| a.event_lbl.clone()).unwrap();
    assert!(add_event.get_visible() && add_event.has_css_class("error"));
    // Closing releases discovery and the nearby rows.
    window.destroy();
    drain();
    assert!(f.ui.add.borrow().is_none());
    assert!(!f.ui.rows.borrow().contains_key("/org/bluez/hci0/dev_04"));
    assert_eq!(f.complete(None), "StopDiscovery");
    assert_eq!(f.bz.discovery_sessions(), 0);
    // Empty sections show placeholders.
    f.bz.bt.devices.borrow_mut().clear();
    f.refresh();
    assert!(f.ui.rows.borrow().is_empty());
    assert!(f.ui.list_connected.listbox.first_child().is_some() && f.ui.list_known.listbox.first_child().is_some());
    println!("ok add_device_window_scans_and_releases");
}

fn advanced_menu_info_and_transfers() {
    let f = Fx::with_object_push();
    let row = f.row(KNOWN);
    assert_eq!(row.advanced_button.tooltip_text().as_deref(), Some("Advanced"));
    let keys: Vec<&str> = row.advanced_items.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["info", "file", "note"]);
    assert!(row.advanced_items.iter().all(|(_, b)| b.is_sensitive()));
    row.advanced_button.set_active(true);
    settle(0.1);
    assert!(row.advanced_button.popover().unwrap().get_visible());
    row.advanced_button.set_active(false);
    // Info is read-only and queries the device's properties.
    let dev = f.bz.bt.device(KNOWN).unwrap();
    let info = f.ui.on_info_clicked(KNOWN, &dev);
    assert!(!info.view.is_editable());
    let call = f.bz.first_pending().unwrap();
    assert_eq!((call.path.as_str(), call.interface.as_str(), call.method.as_str(), call.arg_str(0).as_deref()),
               (KNOWN, PROPS_IFACE, "GetAll", Some(DEVICE_IFACE)));
    f.complete(None);
    let buffer = info.view.buffer();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string();
    assert!(text.contains("Alias: MX Master 3S") && text.contains(&dev.addr) && text.contains(OBJECT_PUSH_UUID), "{text}");
    assert!(text.contains("Paired: Yes"));
    info.refresh.emit_clicked();
    f.complete(Some(gerr("Properties unavailable")));
    assert!(info.error.text().contains("Properties unavailable") && info.error.get_visible() && info.refresh.is_sensitive());
    info.dialog.destroy();
    drain();
    assert!(!f.ui.is_tracked(&info.dialog));
    // Cancelled dialogs never send.
    let chooser = f.ui.on_send_file_clicked(KNOWN, &dev);
    chooser.response(gtk::ResponseType::Cancel);
    let note = f.ui.on_send_note_clicked(KNOWN, &dev);
    note.view.buffer().set_text("Do not send this note");
    note.dialog.response(gtk::ResponseType::Cancel);
    drain();
    assert!(f.senders.borrow().is_empty());
    // A busy device keeps the draft and shows the error.
    let note = f.ui.on_send_note_clicked(KNOWN, &dev);
    assert!(!note.send.is_sensitive());
    note.view.buffer().set_text("Keep this unsent draft");
    assert!(note.send.is_sensitive());
    f.ui.manager.mark_busy(KNOWN, "connect");
    note.dialog.response(gtk::ResponseType::Ok);
    drain();
    assert!(f.senders.borrow().is_empty() && note.error.get_visible());
    assert_eq!(note.error.text(), "Device is busy. Try again shortly.");
    note.dialog.destroy();
    f.ui.manager.unmark_busy(KNOWN, "connect");
    f.refresh();
    // Notes: private UTF-8 vNote, cleaned up on every outcome.
    for outcome in ["success", "error", "cancel"] {
        let note = f.ui.on_send_note_clicked(KNOWN, &dev);
        note.view.buffer().set_text("Café — a Bluetooth note");
        note.dialog.response(gtk::ResponseType::Ok);
        drain();
        let sender = f.senders.borrow().last().cloned().expect("a sender");
        assert!(sender.started.get());
        let path = std::path::PathBuf::from(&sender.files[0]);
        assert!(path.to_string_lossy().ends_with(".vnt"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let payload = std::fs::read_to_string(&path).unwrap();
        assert!(payload.starts_with("BEGIN:VNOTE\r\nVERSION:1.1\r\nBODY;CHARSET=UTF-8:Café — a Bluetooth note\r\n"));
        assert!(payload.ends_with("X-IRMC-LUID:000001000000\r\nEND:VNOTE\r\n"));
        let progress = f.ui.transfers.borrow().last().cloned().unwrap();
        match outcome {
            "cancel" => {
                progress.dialog.response(gtk::ResponseType::Cancel);
                assert!(sender.cancelled.get());
                assert_eq!(progress.status.text(), "Cancelling…");
                sender.finish(None, true);
            }
            "error" => sender.finish(Some(gerr("Transfer failed")), false),
            _ => sender.finish(None, false),
        }
        drain();
        assert!(!path.exists() && !path.parent().unwrap().exists());
        assert_eq!(f.ui.manager.transfers.get(), 0);
        match outcome {
            "error" => {
                assert!(progress.error.text().contains("Transfer failed"));
                assert_eq!(progress.status.text(), "File transfer failed. Make sure the receiving device accepts Bluetooth files.");
            }
            "cancel" => assert_eq!(progress.status.text(), "Transfer cancelled"),
            _ => assert_eq!(progress.progress.fraction(), 1.0),
        }
        assert_eq!(progress.button.label().as_deref(), Some("Close"));
        progress.dialog.destroy();
    }
    // A file transfer keeps the original owner after an adapter switch.
    let file = std::env::temp_dir().join(format!("btmanager-ui-{}.txt", std::process::id()));
    std::fs::write(&file, "A file for a simulated Bluetooth transfer.\n").unwrap();
    let target = f.ui.manager.send_snapshot(KNOWN, &dev);
    f.ui.adapter_combo.set_active_id(Some(SECOND_ADAPTER));
    drain();
    assert_eq!(f.ui.manager.default_adapter().as_deref(), Some(SECOND_ADAPTER));
    let progress = f.ui.start_transfer(&target, &[file.to_string_lossy().into_owned()], "Send File", None);
    let sender = f.senders.borrow().last().cloned().unwrap();
    assert_eq!((sender.source.as_str(), sender.destination.as_str()), ("18:56:80:AB:CD:EF", dev.addr.as_str()));
    (sender.on_progress)(&Progress { filename: file.to_string_lossy().into_owned(), index: 1, count: 1, transferred: 50, size: 100,
                                     completed: 0, total: 100, status: "active".into() });
    assert!(progress.progress.fraction() > 0.0 && progress.progress.fraction() < 1.0);
    assert_eq!(progress.progress.text().as_deref(), Some("50%"));
    assert!(progress.filename.text().ends_with(" (1/1)"));
    sender.finish(None, false);
    drain();
    assert_eq!(progress.progress.fraction(), 1.0);
    assert!(file.is_file());
    let _ = std::fs::remove_file(&file);
    // Unsupported services disable the transfer items.
    f.bz.bt.devices.borrow_mut().get_mut(KNOWN).unwrap().uuids = vec!["0000110b-0000-1000-8000-00805f9b34fb".into()];
    f.ui.adapter_combo.set_active_id(Some(ADAPTER));
    f.refresh();
    let row = f.row(KNOWN);
    assert!(row.item("info").unwrap().is_sensitive() && !row.item("file").unwrap().is_sensitive());
    assert_eq!(row.item("note").unwrap().tooltip_text().as_deref(), Some("This device does not support file transfers."));
    println!("ok advanced_menu_info_and_transfers");
}

fn incoming_file_dialog_fallback() {
    let f = Fx::new("populated", false);
    f.ui.manager.set_notifier(None);
    let request = bluetooth_manager::transfer::ReceiveRequest::detached("notes.txt", 0, "");
    f.ui.manager.on_receive_request(&request);
    drain();
    let dialog = f.ui.dialogs.borrow().last().cloned().unwrap();
    assert!(labels_of(dialog.window.upcast_ref()).iter().any(|t| t.contains("notes.txt")));
    dialog.window.downcast_ref::<gtk::Dialog>().unwrap().response(gtk::ResponseType::Reject);
    drain();
    assert_eq!(request.state(), "rejected");
    assert!(!dialog.alive());
    println!("ok incoming_file_dialog_fallback");
}

fn popup_width_natural_height_and_children() {
    let f = Fx::new("populated", true);
    let window = f.ui.window.clone();
    assert!(wait_until(|| window.is_mapped(), 3.0), "popup maps");
    assert!(window.has_css_class("tray-popup") && !window.is_decorated());
    assert_eq!(window.size_request(), (ui::POPUP_WIDTH, -1));
    assert!(f.ui.receive_combo.is_none());
    let child = window.child().unwrap();
    assert!(child.measure(gtk::Orientation::Horizontal, -1).1 <= ui::POPUP_WIDTH);
    let few = child.measure(gtk::Orientation::Vertical, ui::POPUP_WIDTH).1;
    for index in 0..20 {
        let (path, mut dev) = device(&format!("{:02X}", 0x40 + index), &format!("Saved device {index}"), ADAPTER);
        dev.paired = true;
        dev.bonded = true;
        f.bz.bt.devices.borrow_mut().insert(&path, dev);
    }
    f.refresh();
    let many = child.measure(gtk::Orientation::Vertical, ui::POPUP_WIDTH).1;
    assert!(many > few && many <= few + ui::POPUP_LIST_MAX_HEIGHT, "{few} {many}");
    assert_eq!(f.ui.devices_scroll.max_content_height(), ui::POPUP_LIST_MAX_HEIGHT);
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = false;
    f.refresh();
    assert!(child.measure(gtk::Orientation::Vertical, ui::POPUP_WIDTH).1 < many);
    f.bz.bt.adapters.borrow_mut().get_mut(ADAPTER).unwrap().powered = true;
    f.refresh();
    // Compact rows: icon-only forget button with a tooltip.
    let row = f.row(KNOWN);
    assert!(row.forget_button.as_ref().unwrap().has_css_class("icon-button"));
    assert_eq!(row.forget_button.as_ref().unwrap().tooltip_text().as_deref(), Some("Forget device"));
    // Opening the Add device window hides the popup; dismissing keeps it.
    settle(0.2);
    f.ui.add_btn.emit_clicked();
    drain();
    let add = f.ui.add.borrow().as_ref().map(|a| a.window.clone()).unwrap();
    assert!(add.get_visible() && !window.get_visible());
    assert!(add.transient_for().is_none());
    f.complete(None);
    let popup = f.ui.popup.borrow().clone().unwrap();
    popup.dismiss();
    assert!(add.get_visible() && f.ui.manager.scan_active());
    // activate() with a visible owned child raises the child, not the popup.
    popup.activate();
    settle(0.1);
    assert!(!window.get_visible() && add.get_visible());
    add.destroy();
    drain();
    f.complete(None);
    popup.show();
    assert!(wait_until(|| window.is_mapped(), 3.0), "popup remaps");
    popup.set_position("left", 64);
    assert_eq!((popup.edge(), popup.offset()), ("left".to_owned(), 64));
    assert!(!window.get_visible());
    println!("ok popup_width_natural_height_and_children");
}

fn css_parses() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(Cell::new(0));
    let e = errors.clone();
    provider.connect_parsing_error(move |_, _, error| { eprintln!("css: {error}"); e.set(e.get() + 1); });
    provider.load_from_string(ui::STYLE_CSS);
    assert_eq!(errors.get(), 0);
    println!("ok css_parses");
}

/// test_single_instance.py / test_popup.py remote activation with the real
/// binary: one primary; repeated launches reuse it; invalid ones don't.
fn single_instance_with_real_binary() {
    let Some(bin) = std::env::var_os("BTMANAGER_BIN") else {
        println!("skip single_instance_with_real_binary (BTMANAGER_BIN unset)");
        return;
    };
    let session = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, None::<&gtk::gio::Cancellable>).unwrap();
    let owned = |name: &str| suite_core::single_instance::name_has_owner(&session, name);
    assert!(!owned("org.btmanager.Bluetooth"));
    let mut primary = std::process::Command::new(&bin).args(["--popup", "--popup-edge", "bottom", "--popup-offset", "76"]).spawn().unwrap();
    assert!(wait_until(|| owned("org.btmanager.Bluetooth"), 10.0), "primary registered");
    let started = Instant::now();
    let second = std::process::Command::new(&bin).args(["--popup", "--popup-edge", "left", "--popup-offset", "98"]).status().unwrap();
    assert!(second.success());
    println!("  secondary launch forwarded in {} ms", started.elapsed().as_millis());
    let invalid = std::process::Command::new(&bin).args(["--popup-edge", "invalid"]).output().unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("invalid choice: 'invalid'"));
    assert!(primary.try_wait().unwrap().is_none(), "primary keeps running");
    let again = std::process::Command::new(&bin).arg("--popup").status().unwrap();
    assert!(again.success());
    unsafe { libc::kill(primary.id() as i32, libc::SIGTERM) };
    let _ = primary.wait();
    assert!(wait_until(|| !owned("org.btmanager.Bluetooth"), 5.0));
    println!("ok single_instance_with_real_binary");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("BTMANAGER_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/bluetooth-manager/tests/run_headless.sh for the private compositor)");
        return;
    }
    bluetooth_manager::i18n::set_language("en");
    gtk::init().expect("display");
    css_parses();
    window_layout_rows_actions_and_reuse();
    add_device_window_scans_and_releases();
    advanced_menu_info_and_transfers();
    incoming_file_dialog_fallback();
    popup_width_natural_height_and_children();
    single_instance_with_real_binary();
    println!("ui_headless: all checks passed");
}
