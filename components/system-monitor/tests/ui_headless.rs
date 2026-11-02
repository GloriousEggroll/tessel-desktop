//! Port of test_monitor_ui.py. Runs only inside the private nested headless
//! labwc (tests/run_headless.sh); under plain `cargo test` it skips itself.
//! Process controls target a recording fake backend only; the launcher test
//! starts the real binary, which only samples (read-only) and never acts.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tessel_system_monitor::backend::ProcessActionError;
use tessel_system_monitor::ui::{MonitorWindow, ProcessBackend, Prompt, PromptHook, PAGES};

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

fn sample() -> Value {
    let gib = 1024f64.powi(3);
    let processes: Vec<Value> = [(5100, 10.0, "Browser", 15.0), (5101, 11.0, "Text Editor", 2.0)].iter().map(|(pid, created, name, cpu)| json!({
        "pid": pid, "create_time": created, "name": name, "user": "tester", "status": "running", "cpu_percent": cpu,
        "rss": gib, "read_bytes_per_sec": 1024, "write_bytes_per_sec": 2048})).collect();
    json!({
        "cpu": {"percent": 24, "per_core": [20, 28], "logical_cores": 2, "physical_cores": 2, "frequency": {"current": 3400}},
        "memory": {"total": 32.0 * gib, "used": 12.0 * gib, "available": 20.0 * gib, "percent": 37.5},
        "swap": {"total": 8.0 * gib, "used": 0},
        "system": {"hostname": "Desktop", "os": "Linux", "kernel": "6.18", "uptime": 100000},
        "processes": processes,
        "disks": {"read_bytes_per_sec": 1048576, "write_bytes_per_sec": 1048576,
                  "devices": [{"name": "nvme0n1", "read_bytes_per_sec": 1048576, "write_bytes_per_sec": 1048576}],
                  "partitions": [{"device": "/dev/nvme0n1p2", "mountpoint": "/", "fstype": "btrfs",
                                  "total": 1024f64.powi(4), "used": 512.0 * gib, "free": 512.0 * gib, "percent": 50}]},
        "network": {"recv_bytes_per_sec": 2097152, "sent_bytes_per_sec": 1024,
                    "interfaces": [{"name": "enp1s0", "up": true, "speed_mbps": 1000, "recv_bytes_per_sec": 2097152, "sent_bytes_per_sec": 1024}]},
        "gpus": [{"name": "Radeon Graphics", "utilization_percent": 27, "memory_used": gib, "memory_total": 8.0 * gib, "temp_c": 55}],
        "warnings": []})
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Details(i64, Option<f64>),
    End(i64, Option<f64>, bool),
    Suspend(i64),
    Resume(i64),
    Priority(i64, i64),
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Call>>);

impl Recorder {
    fn calls(&self) -> Vec<Call> { self.0.lock().unwrap().clone() }
}

impl ProcessBackend for Recorder {
    fn end_process(&self, pid: i64, created: Option<f64>, force: bool) -> Result<bool, ProcessActionError> {
        self.0.lock().unwrap().push(Call::End(pid, created, force));
        Ok(true)
    }
    fn suspend_process(&self, pid: i64, _: Option<f64>) -> Result<bool, ProcessActionError> {
        self.0.lock().unwrap().push(Call::Suspend(pid));
        Ok(true)
    }
    fn resume_process(&self, pid: i64, _: Option<f64>) -> Result<bool, ProcessActionError> {
        self.0.lock().unwrap().push(Call::Resume(pid));
        Ok(true)
    }
    fn set_process_priority(&self, pid: i64, _: Option<f64>, nice: i64) -> Result<bool, ProcessActionError> {
        self.0.lock().unwrap().push(Call::Priority(pid, nice));
        Ok(true)
    }
    fn process_details(&self, pid: i64, created: Option<f64>) -> Result<Map<String, Value>, ProcessActionError> {
        self.0.lock().unwrap().push(Call::Details(pid, created));
        Ok(json!({"name": "Browser", "pid": pid, "command_line": ["browser", "--safe"]}).as_object().unwrap().clone())
    }
}

struct Case {
    app: gtk::Application,
    win: MonitorWindow,
    recorder: Arc<Recorder>,
}

thread_local! { static SEQUENCE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }

impl Case {
    fn new() -> Self {
        let sequence = SEQUENCE.with(|s| { s.set(s.get() + 1); s.get() });
        let app = gtk::Application::new(Some(&format!("org.tessel.MonitorTest{sequence}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(gio::Cancellable::NONE).unwrap();
        let win = MonitorWindow::new(&app, None, false);
        let recorder = Arc::new(Recorder::default());
        win.backend.replace(recorder.clone());
        win.window.present();
        win.render_snapshot(sample());
        settle(0.1);
        Case { app, win, recorder }
    }

    fn other_windows(&self) -> Vec<gtk::Window> {
        let me: gtk::Window = self.win.window.clone().upcast();
        self.app.windows().into_iter().filter(|w| *w != me).collect()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        for window in self.app.windows() {
            window.destroy();
        }
        settle(0.1);
        self.app.quit();
    }
}

fn name_at(win: &MonitorWindow, index: u32) -> String {
    win.sorted.item(index).and_downcast::<tessel_system_monitor::ui::ProcessRow>().unwrap().get("name").as_str().unwrap().to_owned()
}

fn descendants(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(widget.clone());
    let mut child = widget.first_child();
    while let Some(current) = child {
        descendants(&current, out);
        child = current.next_sibling();
    }
}

fn accept_prompt(prompts: Rc<RefCell<Vec<Prompt>>>, before: impl Fn() + 'static) -> PromptHook {
    Rc::new(move |prompt: &Prompt, answer: Box<dyn FnOnce(Option<i32>)>| {
        prompts.borrow_mut().push(prompt.clone());
        before();
        answer(Some(1));
    })
}

fn rows_sort_search_and_preserve_identity() {
    let case = Case::new();
    let win = &case.win;
    assert_eq!(name_at(win, 0), "Browser");
    win.select_positions(&[1]);
    let original = win.selected_rows()[0].clone();
    let mut data = sample();
    data["processes"][1]["cpu_percent"] = json!(30);
    win.render_snapshot(data.clone());
    assert_eq!(win.selected_rows(), vec![original]);
    assert_eq!(name_at(win, 0), "Text Editor");
    win.search.set_text("browser");
    win.refilter();
    assert_eq!(win.sorted.n_items(), 1);
    assert_eq!(win.sorted.item(0).and_downcast::<tessel_system_monitor::ui::ProcessRow>().unwrap().pid(), 5100);
    win.search.set_text("");
    win.refilter();
    win.select_positions(&[0]);
    data["processes"][1]["create_time"] = json!(20);
    win.render_snapshot(data);
    assert!(win.selected_rows().is_empty(), "PID reuse must clear the selection");
    println!("ok rows_sort_search_and_preserve_identity");
}

fn all_pages_render_missing_metrics_and_local_graphs() {
    let case = Case::new();
    let win = &case.win;
    for (key, _, _) in PAGES {
        win.show_page(key);
        settle(0.04);
        assert_eq!(win.stack.visible_child_name().as_deref(), Some(*key));
    }
    let mut data = sample();
    data["gpus"] = json!([]);
    data["cpu"]["frequency"] = Value::Null;
    win.render_snapshot(data);
    win.show_page("gpu");
    let first = win.info_boxes["gpu"].first_child().and_downcast::<gtk::Label>().unwrap();
    assert!(first.text().contains("not available"));
    assert_eq!(win.graphs["cpu"].values.borrow().len(), 2);
    assert_eq!(win.metrics["cpu"].label(), "24.0%");
    println!("ok all_pages_render_missing_metrics_and_local_graphs");
}

fn storage_hides_virtual_devices_and_sorts_naturally() {
    let case = Case::new();
    let win = &case.win;
    let mut data = sample();
    data["disks"]["devices"].as_array_mut().unwrap().push(json!({"name": "loop0", "read_bytes_per_sec": 0, "write_bytes_per_sec": 0}));
    win.render_snapshot(data);
    win.show_page("storage");
    settle(0.04);
    let mut all = Vec::new();
    descendants(win.info_boxes["storage"].upcast_ref(), &mut all);
    let rows = all.iter().filter(|w| w.has_css_class("monitor-storage-row")).count();
    assert_eq!(rows, 2); // nvme0n1 and "/"; loop0 is hidden
    assert!(win.navigation.has_css_class("suite-sidebar"));
    println!("ok storage_hides_virtual_devices_and_sorts_naturally");
}

fn paused_sample_does_not_replace_displayed_values() {
    let case = Case::new();
    let win = &case.win;
    win.pause.set_active(true);
    let previous = win.snapshot.borrow().clone().unwrap();
    let mut data = sample();
    data["cpu"]["percent"] = json!(90);
    win.sampled(Some(data), None);
    assert!(Rc::ptr_eq(&previous, win.snapshot.borrow().as_ref().unwrap()));
    assert!(win.status.label().contains("Paused"));
    assert!(!win.timer_active());
    println!("ok paused_sample_does_not_replace_displayed_values");
}

fn unavailable_metrics_sort_and_display_without_inventing_values() {
    let case = Case::new();
    let win = &case.win;
    let mut data = sample();
    data["processes"][0]["user"] = Value::Null;
    data["processes"][0]["status"] = Value::Null;
    data["network"]["interfaces"][0]["up"] = Value::Null;
    data["network"]["interfaces"][0]["speed_mbps"] = Value::Null;
    data["network"]["recv_bytes_per_sec"] = Value::Null;
    data["network"]["sent_bytes_per_sec"] = Value::Null;
    data["disks"]["read_bytes_per_sec"] = Value::Null;
    data["disks"]["write_bytes_per_sec"] = Value::Null;
    win.render_snapshot(data);
    win.table.sort_by_column(Some(&win.columns["user"]), gtk::SortType::Ascending);
    win.show_page("network");
    settle(0.1);
    assert_eq!(win.sorted.n_items(), 2);
    assert_eq!(win.metrics["network"].label(), "—");
    assert_eq!(win.metrics["disk"].label(), "—");
    println!("ok unavailable_metrics_sort_and_display_without_inventing_values");
}

fn monitor_windows(client: &mut tessel_wayland::client::Client) -> Vec<u64> {
    let _ = client.roundtrip();
    client.model.windows().filter(|w| w.app_id == "org.tessel.SystemMonitor").map(|w| w.id).collect()
}

fn launcher_reuses_existing_window_and_exits_cleanly() {
    let Some(binary) = std::env::var_os("SYSTEM_MONITOR_BIN") else {
        println!("skip launcher_reuses_existing_window_and_exits_cleanly (SYSTEM_MONITOR_BIN unset)");
        return;
    };
    let mut native = tessel_wayland::client::Client::connect(tessel_wayland::client::Protocols::TOPLEVELS).expect("toplevel client");
    let _ = native.roundtrip();
    let mut child = std::process::Command::new(&binary).args(["--page", "performance"]).spawn().unwrap();
    let pid = child.id();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let end = Instant::now() + Duration::from_secs(5);
        let mut windows = Vec::new();
        while Instant::now() < end && child.try_wait().unwrap().is_none() {
            settle(0.05);
            windows = monitor_windows(&mut native);
            if !windows.is_empty() {
                break;
            }
        }
        assert_eq!(windows.len(), 1);
        let original = windows[0];
        let status = std::process::Command::new(&binary).args(["--page", "processes"]).status().unwrap();
        assert!(status.success());
        settle(0.2);
        assert_eq!(monitor_windows(&mut native), vec![original]);
        let status = std::process::Command::new(&binary).arg("--quit").status().unwrap();
        assert!(status.success());
        let end = Instant::now() + Duration::from_secs(5);
        let mut code = None;
        while Instant::now() < end {
            if let Some(status) = child.try_wait().unwrap() {
                code = status.code();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(code, Some(0));
    }));
    if child.try_wait().ok().flatten().is_none() {
        // Only the child this test started (recorded PID).
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        let _ = child.wait();
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    println!("ok launcher_reuses_existing_window_and_exits_cleanly");
}

fn details_and_process_controls_capture_exact_selection() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[0]);
    win.synchronous.set(true);
    win.show_details();
    assert_eq!(case.recorder.calls(), vec![Call::Details(5100, Some(10.0))]);
    let details = case.other_windows();
    assert_eq!(details.len(), 1);
    details[0].destroy();
    assert!(win.end_button.is_sensitive());
    // Changing selection while the prompt is open must not change the target.
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let target = win.clone();
    win.prompt.replace(Some(accept_prompt(prompts.clone(), move || target.select_positions(&[1]))));
    win.confirm_action("end");
    assert_eq!(case.recorder.calls().last(), Some(&Call::End(5100, Some(10.0), false)));
    println!("ok details_and_process_controls_capture_exact_selection");
}

fn late_confirmation_after_close_never_submits_process_action() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[0]);
    let pending: Rc<RefCell<Vec<Box<dyn FnOnce(Option<i32>)>>>> = Rc::default();
    let store = pending.clone();
    win.prompt.replace(Some(Rc::new(move |_: &Prompt, answer: Box<dyn FnOnce(Option<i32>)>| store.borrow_mut().push(answer))));
    win.confirm_action("end");
    win.close();
    settle(0.1);
    let answer = pending.borrow_mut().pop().unwrap();
    answer(Some(1));
    assert!(!win.run_worker(|| Ok(()), |_, _: Option<()>, _| {}));
    assert!(!win.perform(|| Ok(())));
    assert!(case.recorder.calls().is_empty());
    println!("ok late_confirmation_after_close_never_submits_process_action");
}

fn mapped_modal_dialogs_close_without_process_actions() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[0]);
    win.synchronous.set(true);
    for which in 0..2 {
        if which == 0 { win.show_details(); } else { win.priority_dialog(5100, Some(10.0), "Browser"); }
        settle(0.15);
        let dialog = case.other_windows().into_iter().next().expect("dialog");
        assert!(dialog.is_mapped());
        assert!(dialog.must_destroy_with_parent());
        dialog.close();
        settle(0.1);
        assert_eq!(case.app.windows().len(), 1);
    }
    assert!(!case.recorder.calls().iter().any(|c| matches!(c, Call::Priority(..))));
    println!("ok mapped_modal_dialogs_close_without_process_actions");
}

fn parent_close_destroys_mapped_priority_dialog() {
    let case = Case::new();
    let win = &case.win;
    let dialog = win.priority_dialog(5100, Some(10.0), "Browser").unwrap();
    settle(0.15);
    assert!(dialog.is_mapped());
    win.close();
    settle(0.1);
    assert!(!dialog.is_mapped());
    assert!(!dialog.is_realized());
    assert!(case.app.windows().is_empty());
    assert!(case.recorder.calls().is_empty());
    println!("ok parent_close_destroys_mapped_priority_dialog");
}

fn cleanup_stops_sampler_and_capture_preview() {
    let case = Case::new();
    let win = &case.win;
    win.show_page("performance");
    settle(0.2);
    if let Some(path) = std::env::var_os("MONITOR_TEST_SCREENSHOT") {
        for index in 0..60 {
            for (key, graph) in &win.graphs {
                let value = if matches!(*key, "cpu" | "memory") { 30.0 + 12.0 * (index as f64 / 4.0).sin() }
                            else { 20000.0 + 7000.0 * (index as f64 / 3.0).sin() };
                graph.push(Some(value));
            }
        }
        settle(0.15);
        let _ = std::process::Command::new("grim").arg(path).status();
    }
    win.schedule();
    assert!(win.timer_active());
    win.cleanup();
    assert!(!win.timer_active());
    assert!(win.closed.get());
    assert!(!win.request_sample());
    println!("ok cleanup_stops_sampler_and_capture_preview");
}

fn multi_selection_single_confirmation_lists_every_process() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[0, 1]);
    assert_eq!(win.selected_rows().len(), 2);
    assert!(!win.details_button.is_sensitive());
    assert!(!win.window.lookup_action("priority").unwrap().is_enabled());
    assert!(win.window.lookup_action("kill").unwrap().is_enabled());
    win.synchronous.set(true);
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let target = win.clone();
    win.prompt.replace(Some(accept_prompt(prompts.clone(), move || target.select_positions(&[0]))));
    assert!(win.key(gdk::Key::k, gdk::ModifierType::CONTROL_MASK));
    let prompts = prompts.borrow();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].message.contains('2'));
    assert!(prompts[0].detail.contains("Browser (5100)"));
    assert!(prompts[0].detail.contains("Text Editor (5101)"));
    let mut calls = case.recorder.calls();
    calls.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(calls, vec![Call::End(5100, Some(10.0), true), Call::End(5101, Some(11.0), true)]);
    println!("ok multi_selection_single_confirmation_lists_every_process");
}

fn delete_and_ctrl_e_end_with_confirmation_but_not_while_typing() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[0]);
    let asked: Rc<RefCell<Vec<Prompt>>> = Rc::default();
    let store = asked.clone();
    win.prompt.replace(Some(Rc::new(move |prompt: &Prompt, _answer| store.borrow_mut().push(prompt.clone()))));
    assert!(win.key(gdk::Key::Delete, gdk::ModifierType::empty()));
    assert!(win.key(gdk::Key::e, gdk::ModifierType::CONTROL_MASK));
    win.search.grab_focus();
    settle(0.1);
    assert!(!win.key(gdk::Key::Delete, gdk::ModifierType::empty()));
    assert!(case.recorder.calls().is_empty());
    assert_eq!(asked.borrow().len(), 2);
    assert!(asked.borrow()[0].message.contains("Browser"));
    println!("ok delete_and_ctrl_e_end_with_confirmation_but_not_while_typing");
}

fn context_menu_header_menu_and_accessible_graphs() {
    let case = Case::new();
    let win = &case.win;
    win.select_positions(&[1]);
    assert!(win.popup_context_menu(Some((10.0, 10.0))));
    settle(0.15);
    assert!(win.context_menu.borrow().as_ref().unwrap().is_mapped());
    let labels: Vec<String> = (0..win.process_model.n_items()).map(|i| win.process_model
        .item_attribute_value(i, "label", None).unwrap().get::<String>().unwrap()).collect();
    assert_eq!(labels, ["End Task", "Force Quit", "Suspend", "Resume", "Set Priority…", "Open File Location", "Properties"]);
    let menu = win.context_menu.borrow().clone().unwrap();
    menu.popdown();
    settle(0.1);
    let button = win.menu_button.borrow().clone().unwrap();
    assert_eq!(button.ancestor(gtk::HeaderBar::static_type()).as_ref(), Some(win.header.upcast_ref::<gtk::Widget>()));
    assert!(tessel_gtk::app_menu::has_menu_icon(&button));
    assert!(case.app.lookup_action("quit").is_some());
    win.show_page("performance");
    settle(0.15);
    let graph = win.graphs["cpu"].clone();
    assert!(graph.area.is_mapped());
    win.render_snapshot(sample());
    assert!(graph.described.borrow().as_ref().unwrap().contains("24.0%"));
    let hidden = win.graphs["network"].described.borrow().clone();
    win.show_page("processes");
    settle(0.1);
    let mut data = sample();
    data["network"]["recv_bytes_per_sec"] = json!(9 * 1024 * 1024);
    win.render_snapshot(data);
    assert_eq!(*win.graphs["network"].described.borrow(), hidden, "hidden graphs are not re-described");
    println!("ok context_menu_header_menu_and_accessible_graphs");
}

fn window_contract() {
    let case = Case::new();
    let win = &case.win;
    assert!(win.window.has_css_class("tessel-system-monitor"));
    assert_eq!(win.window.title().as_deref(), Some("System Monitor"));
    // Before mapping (the 1280x720 headless output clamps a mapped window).
    let fresh = MonitorWindow::new(&case.app, None, false);
    assert_eq!(fresh.window.default_size(), (1120, 760));
    fresh.window.destroy();
    assert_eq!(win.window.size_request(), (820, 540));
    assert_eq!(win.interval.selected(), 1);
    assert!(win.all_users.is_active());
    assert!(win.end_button.has_css_class("destructive-action"));
    assert!(win.status.label().starts_with("2 processes · Updated "));
    println!("ok window_contract");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("WAYDOCK_TEST_ISOLATED").as_deref() != Some(std::ffi::OsStr::new("1")) {
        println!("ui_headless: skipped (run components/system-monitor/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    rows_sort_search_and_preserve_identity();
    all_pages_render_missing_metrics_and_local_graphs();
    storage_hides_virtual_devices_and_sorts_naturally();
    paused_sample_does_not_replace_displayed_values();
    unavailable_metrics_sort_and_display_without_inventing_values();
    launcher_reuses_existing_window_and_exits_cleanly();
    details_and_process_controls_capture_exact_selection();
    late_confirmation_after_close_never_submits_process_action();
    mapped_modal_dialogs_close_without_process_actions();
    parent_close_destroys_mapped_priority_dialog();
    cleanup_stops_sampler_and_capture_preview();
    multi_selection_single_confirmation_lists_every_process();
    delete_and_ctrl_e_end_with_confirmation_but_not_while_typing();
    context_menu_header_menu_and_accessible_graphs();
    window_contract();
    println!("ui_headless: all checks passed");
}
