#![allow(deprecated, clippy::type_complexity)]
//! Port of the Python Files GUI suite (test_files_ui.py, test_files_context.py,
//! test_files_browsing.py, test_files_navigation.py, test_files_monitor.py,
//! test_files_sorting.py, test_files_memory.py, test_files_thumbnails.py,
//! test_files_information.py, test_location_completion.py, test_files_launch.py,
//! test_file_dnd.py, test_git_refresh.py). Runs only inside the private nested
//! headless labwc started by tests/run_headless.sh (own XDG_RUNTIME_DIR and
//! Wayland socket, private D-Bus without service activation, GIO_USE_VFS=local,
//! private HOME/XDG dirs); `wtype`/`wlrctl` drive that compositor's virtual
//! keyboard/pointer only. Every file operation happens below $TMPDIR (= $R/tmp).
//! Under plain `cargo test` it skips itself.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};
use wayfiles::bridge::{FilePreferences, GTKBookmarks};
use wayfiles::tile::{self, FileTile};
use wayfiles::{BrowserPane, FilesApplication, FilesWindow};

type Outcome = Result<(), String>;
type Check = fn(&mut Fixture) -> Outcome;

macro_rules! ensure {
    ($cond:expr) => {
        if !$cond {
            return Err(format!("{}:{}: assertion failed: {}", file!(), line!(), stringify!($cond)));
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!("{}:{}: {}", file!(), line!(), format!($($arg)+)));
        }
    };
}

macro_rules! ensure_eq {
    ($left:expr, $right:expr) => {{
        match (&$left, &$right) { (l, r) => if l != r {
            return Err(format!("{}:{}: {} != {}\n  left: {:?}\n right: {:?}", file!(), line!(), stringify!($left), stringify!($right), l, r));
        } }
    }};
}

// ------------------------------------------------------------------- helpers

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_for(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    predicate()
}

macro_rules! wait {
    ($pred:expr) => {
        if !wait_for(5.0, $pred) {
            return Err(format!("{}:{}: timed out waiting for {}", file!(), line!(), stringify!($pred)));
        }
    };
    ($timeout:expr, $pred:expr) => {
        if !wait_for($timeout, $pred) {
            return Err(format!("{}:{}: timed out waiting for {}", file!(), line!(), stringify!($pred)));
        }
    };
}

fn run(program: &str, args: &[&str]) -> Outcome {
    let status = Command::new(program).args(args).status().map_err(|e| format!("{program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {args:?} failed: {status}"))
    }
}

fn uri(path: &Path) -> String {
    gio::File::for_path(path).uri().to_string()
}

fn file(path: &Path) -> gio::File {
    gio::File::for_path(path)
}

fn names_of(model: &impl IsA<gio::ListModel>) -> Vec<String> {
    (0..model.n_items()).filter_map(|i| model.item(i).as_ref().and_then(wayfiles::pane::item_of)).map(|i| i.name()).collect()
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

struct Fixture {
    app: FilesApplication,
    window: FilesWindow,
    root: PathBuf,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn pane(&self) -> BrowserPane {
        self.window.active_pane().expect("active pane")
    }

    fn names(&self) -> Vec<String> {
        names_of(&self.pane().sorted)
    }

    fn pane_names(pane: &BrowserPane) -> Vec<String> {
        names_of(&pane.sorted)
    }

    fn selected_names(&self) -> Vec<String> {
        self.pane().selected().iter().map(|i| i.name()).collect()
    }

    fn select(&self, name: &str) -> Outcome {
        let pane = self.pane();
        let selection = pane.selection();
        selection.unselect_all();
        for index in 0..pane.sorted.n_items() {
            if names_of(&pane.sorted)[index as usize] == name {
                selection.select_item(index, true);
                return Ok(());
            }
        }
        Err(format!("Could not select {name}; visible items: {:?}", self.names()))
    }

    fn loaded(&self) -> bool {
        !self.pane().loading()
    }

    fn job_idle(&self) -> bool {
        !self.window.job_running() && !self.pane().loading()
    }

    fn tile(&self, name: &str) -> Option<FileTile> {
        let pane = self.pane();
        let visible = pane.views.visible_child()?;
        tile::tile_for_uri(&visible, &uri(&self.source.join(name)))
    }

    fn popover_visible(&self) -> bool {
        self.window.popover.borrow().as_ref().is_some_and(|p| p.is_visible())
    }

    fn popover(&self) -> gtk::Popover {
        self.window.popover.borrow().clone().expect("context menu")
    }

    fn maximize(&self) -> Outcome {
        self.window.window.maximize();
        wait!(|| self.window.window.is_maximized());
        settle(0.1);
        Ok(())
    }

    /// A real compositor pointer click in the middle (or lower-right blank) of `widget`.
    fn pointer_click(&self, widget: &impl IsA<gtk::Widget>, button: &str, blank: bool) -> Outcome {
        self.maximize()?;
        let bounds = widget.compute_bounds(&self.window.window).ok_or("widget has no bounds")?;
        ensure!(bounds.width() > 0. && bounds.height() > 0.);
        let display = gdk::Display::default().unwrap();
        let surface = self.window.window.surface().ok_or("no surface")?;
        let geometry = display.monitor_at_surface(&surface).ok_or("no monitor")?.geometry();
        let local_x = if blank { bounds.width() - 30. } else { bounds.width() / 2. };
        let local_y = if blank { bounds.height() - 30. } else { bounds.height() / 2. };
        let x = geometry.x() as f32 + bounds.x() + local_x;
        let y = geometry.y() as f32 + bounds.y() + local_y;
        run("wlrctl", &["pointer", "move", "-100000", "-100000"])?;
        settle(0.05);
        run("wlrctl", &["pointer", "move", &(x as i32).to_string(), &(y as i32).to_string()])?;
        settle(0.05);
        run("wlrctl", &["pointer", "click", button])?;
        settle(0.05);
        Ok(())
    }

    fn right_click(&self, widget: &impl IsA<gtk::Widget>, blank: bool) -> Outcome {
        if let Some(popover) = self.window.popover.borrow().clone() {
            popover.popdown();
        }
        settle(0.05);
        let bounds = widget.compute_bounds(&self.window.window).ok_or("no bounds")?;
        ensure!(bounds.width() > 40. || !blank, "widget too narrow");
        self.pointer_click(widget, "right", blank)?;
        wait!(|| self.popover_visible());
        Ok(())
    }

    fn key(&self, name: &str) -> Outcome {
        run("wtype", &["-k", name])?;
        settle(0.05);
        Ok(())
    }

    /// `menu_actions()`: action → label of the open context menu.
    fn menu_actions(&self) -> Vec<(String, String)> {
        let popover = self.popover();
        let Some(menu) = popover.downcast_ref::<gtk::PopoverMenu>().and_then(|m| m.menu_model()) else { return Vec::new() };
        let mut actions = Vec::new();
        fn collect(model: &gio::MenuModel, actions: &mut Vec<(String, String)>) {
            for index in 0..model.n_items() {
                let action = model.item_attribute_value(index, "action", None).and_then(|v| v.str().map(str::to_string));
                let label = model.item_attribute_value(index, "label", None).and_then(|v| v.str().map(str::to_string));
                if let Some(action) = action {
                    actions.push((action, label.unwrap_or_default()));
                }
                for link in ["section", "submenu"] {
                    if let Some(child) = model.item_link(index, link) {
                        collect(&child, actions);
                    }
                }
            }
        }
        collect(&menu, &mut actions);
        actions
    }

    fn menu_action(&self, action: &str) -> Option<String> {
        self.menu_actions().into_iter().find(|(a, _)| a == action).map(|(_, l)| l)
    }

    fn activate(&self, detailed: &str) -> bool {
        self.popover().activate_action(detailed, None).is_ok()
    }

    /// Terminal launches recorded by the `launch_terminal` hook.
    fn terminal_hook(&self) -> Rc<RefCell<Vec<String>>> {
        let calls: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = calls.clone();
        *self.app.hooks.launch_terminal.borrow_mut() = Some(Rc::new(move |directory: &gio::File| {
            sink.borrow_mut().push(directory.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| directory.uri().to_string()));
            Ok(())
        }));
        calls
    }

    fn fixture_window(&mut self) -> Outcome {
        self.window = FilesWindow::new(&self.app);
        self.window.new_tab(Some(&file(&self.source)), None);
        self.window.window.present();
        wait!(|| self.loaded());
        Ok(())
    }

    fn browser_windows(&self) -> Vec<FilesWindow> {
        self.app.files_windows().into_iter().filter(|w| w.alive.get()).collect()
    }
}

fn private_root(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("files-ui-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn setup(app: &FilesApplication, name: &str) -> Fixture {
    let root = private_root(name);
    let (source, target) = (root.join("source"), root.join("target"));
    std::fs::create_dir_all(source.join("Documents")).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    write(&source.join("Documents/nested.txt"), "deep needle\n");
    write(&source.join("Alpha.txt"), "alpha\n");
    write(&source.join("Beta.txt"), "beta\n");
    write(&source.join(".secret"), "hidden\n");
    app.set_preferences(FilePreferences::new(Some(root.join("prefs.json"))));
    app.set_bookmarks(GTKBookmarks::new(Some(root.join("bookmarks"))));
    app.set_operations(files_ops::operations::Operations::new());
    app.clipboard_files.borrow_mut().clear();
    *app.clipboard_provider.borrow_mut() = None;
    let window = FilesWindow::new(app);
    window.new_tab(Some(&file(&source)), None);
    window.window.present();
    let fixture = Fixture { app: app.clone(), window, root, source, target };
    wait_for(5.0, || fixture.loaded());
    settle(0.1);
    fixture
}

fn teardown(fixture: &Fixture) {
    fixture.app.operations().cancel_all();
    wait_for(5.0, || !fixture.window.job_running());
    let hooks = &fixture.app.hooks;
    *hooks.launch_terminal.borrow_mut() = None;
    *hooks.open_files.borrow_mut() = None;
    *hooks.confirm.borrow_mut() = None;
    *hooks.ask_keep_running.borrow_mut() = None;
    *hooks.action.borrow_mut() = None;
    *hooks.run_operation.borrow_mut() = None;
    *hooks.folder_index.borrow_mut() = None;
    *hooks.classify_delay.borrow_mut() = None;
    for window in fixture.app.app.windows() {
        window.close();
    }
    settle(0.05);
    for window in fixture.app.app.windows() {
        window.destroy();
    }
    settle(0.05);
    let _ = std::fs::remove_dir_all(&fixture.root);
}

// ------------------------------------------------------------- test_files_ui

fn test_folder_views_hidden_files_selection_and_information(f: &mut Fixture) -> Outcome {
    ensure_eq!(f.names(), vec!["Documents", "Alpha.txt", "Beta.txt"]);
    ensure!(unsafe { f.window.window.data::<suite_gtk::theme::WindowTheme>("wayfiles-theme") }.is_some());
    for mode in ["compact", "details", "icons"] {
        f.window.action(mode);
        wait!(|| f.loaded());
        ensure_eq!(f.pane().views.visible_child_name().unwrap().as_str(), mode);
        ensure_eq!(f.names().len(), 3);
    }
    f.window.action("select-all");
    ensure_eq!(f.pane().selected().len(), 3);
    f.window.action("invert-selection");
    ensure!(f.pane().selected().is_empty());
    f.window.action("hidden");
    wait!(|| f.loaded());
    ensure!(f.names().contains(&".secret".to_string()));
    f.select("Alpha.txt")?;
    ensure!(f.window.information.widget().is_visible());
    f.window.action("information");
    ensure!(!f.window.information.widget().is_visible());
    f.window.action("information");
    ensure!(f.window.information.widget().is_visible());
    ensure_eq!(f.window.information.title.text().as_str(), "Alpha.txt");
    ensure!(f.window.information.text.text().contains("Alpha.txt"));
    Ok(())
}

fn test_tabs_split_history_and_bookmark(f: &mut Fixture) -> Outcome {
    f.window.action("new-tab");
    ensure_eq!(f.window.pages().len(), 2);
    wait!(|| f.loaded());
    f.window.action("split");
    wait!(|| f.loaded());
    let page = f.window.current_page().unwrap();
    ensure!(Some(f.pane()) == page.right());
    f.pane().navigate(&file(&f.source.join("Documents")), true, None);
    wait!(|| f.loaded());
    ensure_eq!(f.names(), vec!["nested.txt"]);
    f.window.action("back");
    wait!(|| f.loaded());
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    f.window.action("forward");
    wait!(|| f.loaded());
    ensure_eq!(f.names(), vec!["nested.txt"]);
    f.window.action("bookmark");
    let list = f.app.bookmarks().borrow().list();
    ensure_eq!(list.len(), 1);
    ensure_eq!((list[0].uri.clone(), list[0].label.clone()), (uri(&f.source.join("Documents")), "Documents".to_string()));
    f.window.action("close-tab");
    ensure_eq!(f.window.pages().len(), 1);
    f.window.action("reopen-tab");
    ensure_eq!(f.window.pages().len(), 2);
    wait!(|| f.loaded());
    let right = f.window.current_page().unwrap().right().ok_or("split not reopened")?;
    ensure_eq!(right.current().path().unwrap(), f.source.join("Documents"));
    Ok(())
}

fn test_tab_close_button_closes_its_tab(f: &mut Fixture) -> Outcome {
    f.window.action("new-tab");
    wait!(|| f.loaded());
    let page = f.window.current_page().unwrap();
    let label = f.window.notebook.tab_label(&page.paned).unwrap();
    let close = label.last_child().and_downcast::<gtk::Button>().unwrap();
    ensure!(close.has_css_class("files-tab-close"));
    close.emit_clicked();
    ensure_eq!(f.window.pages().len(), 1);
    ensure!(!f.window.pages().contains(&page));
    Ok(())
}

fn test_filter_recursive_name_search_and_contents_search(f: &mut Fixture) -> Outcome {
    f.window.action("filter");
    f.pane().filter_entry.set_text("alpha");
    wait!(|| f.names() == vec!["Alpha.txt"]);
    f.pane().show_filter(false);
    wait!(|| f.names().len() == 3);
    f.window.action("search");
    let pane = f.pane();
    pane.search_entry.set_text("nested");
    pane.search();
    wait!(|| f.loaded());
    ensure_eq!(f.names(), vec!["nested.txt"]);
    pane.search_entry.set_text("needle");
    pane.contents_check.set_active(true);
    pane.search();
    wait!(|| f.loaded());
    ensure_eq!(f.names(), vec!["nested.txt"]);
    pane.stop_search();
    wait!(|| f.loaded());
    ensure_eq!(f.names(), vec!["Documents", "Alpha.txt", "Beta.txt"]);
    Ok(())
}

fn test_clipboard_copy_cut_paste_and_undo(f: &mut Fixture) -> Outcome {
    f.select("Alpha.txt")?;
    f.window.action("copy");
    ensure!(!f.app.clipboard_cut.get());
    f.pane().navigate(&file(&f.target), true, None);
    wait!(|| f.loaded());
    f.window.action("paste");
    wait!(|| f.job_idle() && f.target.join("Alpha.txt").exists());
    wait!(|| f.job_idle() && f.names().contains(&"Alpha.txt".to_string()));
    ensure_eq!(std::fs::read_to_string(f.target.join("Alpha.txt")).unwrap(), "alpha\n");
    ensure!(!f.window.jobs_button.widget().is_visible());
    f.window.action("undo");
    wait!(|| f.job_idle() && !f.target.join("Alpha.txt").exists());
    f.pane().navigate(&file(&f.source), true, None);
    wait!(|| f.loaded());
    f.select("Beta.txt")?;
    f.window.action("cut");
    ensure!(f.app.clipboard_cut.get());
    f.pane().navigate(&file(&f.target), true, None);
    wait!(|| f.loaded());
    f.window.action("paste");
    wait!(|| f.job_idle() && !f.source.join("Beta.txt").exists());
    ensure_eq!(std::fs::read_to_string(f.target.join("Beta.txt")).unwrap(), "beta\n");
    f.window.action("undo");
    wait!(|| f.job_idle() && f.source.join("Beta.txt").exists());
    ensure_eq!(std::fs::read_to_string(f.source.join("Beta.txt")).unwrap(), "beta\n");
    ensure!(!f.target.join("Beta.txt").exists());
    Ok(())
}

fn blocking_task(f: &Fixture, title: &str) -> Result<(Rc<files_widgets::jobs::JobEntry>, std::sync::Arc<std::sync::atomic::AtomicBool>), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let (r, s) = (release.clone(), started.clone());
    let entry = f.window.run_task(title, move |cancel| {
        s.store(true, Ordering::SeqCst);
        while !r.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    });
    wait!(|| started.load(Ordering::SeqCst));
    Ok((entry, release))
}

fn test_jobs_run_concurrently_and_share_one_progress_popover(f: &mut Fixture) -> Outcome {
    use std::sync::atomic::Ordering;
    let (entry, release) = blocking_task(f, "Blocking task")?;
    ensure!(f.window.jobs_button.widget().is_visible());
    f.select("Alpha.txt")?;
    f.window.action("copy");
    f.pane().navigate(&file(&f.target), true, None);
    wait!(|| f.loaded());
    f.window.action("paste");
    wait!(|| f.target.join("Alpha.txt").exists());
    wait!(|| f.app.jobs.entries().len() == 1);
    let second = FilesWindow::new(&f.app);
    second.new_tab(Some(&file(&f.source)), None);
    second.window.present();
    ensure!(second.jobs_button.widget().is_visible());
    ensure_eq!(second.jobs_button.idents(), vec![entry.ident]);
    let (_, _, _, cancel) = second.jobs_button.row(entry.ident).unwrap();
    cancel.emit_clicked();
    wait!(|| f.app.jobs.entries().is_empty());
    ensure!(!f.window.jobs_button.widget().is_visible());
    ensure!(!second.jobs_button.widget().is_visible());
    release.store(true, Ordering::SeqCst);
    Ok(())
}

fn test_closing_window_keeps_jobs_running_and_notifies_when_done(f: &mut Fixture) -> Outcome {
    use std::sync::atomic::Ordering;
    let (entry, release) = blocking_task(f, "Long copy")?;
    let sent: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = sent.clone();
    f.app.jobs.set_notifier(Some(Rc::new(move |id: &str, _: &gio::Notification| sink.borrow_mut().push(id.to_string()))));
    *f.app.hooks.ask_keep_running.borrow_mut() = Some(Rc::new(|callback| callback(Some(files_widgets::jobs::KeepAnswer::Keep))));
    f.window.window.close();
    wait!(|| !f.window.alive.get());
    ensure!(f.app.jobs.entries().iter().any(|e| Rc::ptr_eq(e, &entry)));
    release.store(true, Ordering::SeqCst);
    wait!(|| f.app.jobs.entries().is_empty());
    f.app.jobs.set_notifier(None);
    ensure_eq!(*sent.borrow(), vec![format!("files-job-{}", entry.ident)]);
    Ok(())
}

fn test_closing_window_can_cancel_its_jobs(f: &mut Fixture) -> Outcome {
    use std::sync::atomic::Ordering;
    let (entry, release) = blocking_task(f, "Blocking task")?;
    *f.app.hooks.ask_keep_running.borrow_mut() = Some(Rc::new(|callback| callback(Some(files_widgets::jobs::KeepAnswer::Cancel))));
    f.window.window.close();
    wait!(|| f.app.jobs.entries().is_empty());
    ensure!(entry.cancelled.get());
    release.store(true, Ordering::SeqCst);
    Ok(())
}

fn test_ctrl_q_asks_once_about_running_jobs_then_closes(f: &mut Fixture) -> Outcome {
    use std::sync::atomic::Ordering;
    let menu = f.window.menu_button.borrow().clone().unwrap();
    ensure!(suite_gtk::app_menu::has_menu_icon(&menu));
    ensure!(menu.is_primary());
    ensure_eq!(f.app.app.accels_for_action("app.quit").iter().map(|s| s.to_string()).collect::<Vec<_>>(), vec!["<Control>q"]);
    let (entry, release) = blocking_task(f, "Quit copy")?;
    let asked: Rc<RefCell<Vec<Box<dyn FnOnce(Option<files_widgets::jobs::KeepAnswer>)>>>> = Rc::default();
    let sink = asked.clone();
    *f.app.hooks.ask_keep_running.borrow_mut() = Some(Rc::new(move |callback| sink.borrow_mut().push(callback)));
    f.app.app.activate_action("quit", None);
    ensure_eq!(asked.borrow().len(), 1);
    ensure!(f.window.alive.get(), "the prompt runs before anything closes");
    let callback = asked.borrow_mut().pop().unwrap();
    callback(Some(files_widgets::jobs::KeepAnswer::Cancel));
    wait!(|| !f.window.alive.get());
    ensure!(asked.borrow().is_empty());
    ensure!(entry.cancelled.get());
    release.store(true, Ordering::SeqCst);
    Ok(())
}

fn find_window(f: &Fixture, title: &str) -> Option<gtk::Window> {
    f.app.app.windows().into_iter().find(|w| w.title().as_deref() == Some(title))
}

fn test_paste_folder_into_its_own_folder_asks_for_a_name(f: &mut Fixture) -> Outcome {
    f.select("Documents")?;
    f.window.action("copy");
    f.window.action("paste");
    wait!(|| find_window(f, "Paste Copy").is_some());
    let dialog = find_window(f, "Paste Copy").unwrap();
    let entry = tile::descendants(dialog.upcast_ref()).into_iter().find_map(|w| w.downcast::<gtk::Entry>().ok()).unwrap();
    ensure_eq!(entry.text().as_str(), "Documents (copy)");
    if let Some(shot) = std::env::var_os("WAYFILES_UI_SHOT").filter(|s| !s.is_empty()) {
        settle(0.3);
        let _ = std::process::Command::new("grim").arg(shot).status();
    }
    entry.set_text("Documents backup");
    entry.emit_activate();
    wait!(|| f.job_idle() && f.source.join("Documents backup/nested.txt").exists());
    ensure_eq!(std::fs::read_to_string(f.source.join("Documents/nested.txt")).unwrap(), "deep needle\n");
    // Dismissing the prompt pastes nothing.
    f.window.action("paste");
    wait!(|| find_window(f, "Paste Copy").is_some());
    find_window(f, "Paste Copy").unwrap().close();
    wait!(|| f.job_idle());
    settle(0.05);
    ensure!(!f.source.join("Documents (copy)").exists());
    Ok(())
}

fn test_new_folder_dialog_accept_and_cancel(f: &mut Fixture) -> Outcome {
    f.window.action("new-folder");
    wait!(|| find_window(f, "New Folder").is_some());
    let dialog = find_window(f, "New Folder").unwrap();
    let entry = tile::descendants(dialog.upcast_ref()).into_iter().find_map(|w| w.downcast::<gtk::Entry>().ok()).unwrap();
    entry.set_text("Created from dialog");
    entry.emit_activate();
    wait!(|| f.job_idle() && f.source.join("Created from dialog").is_dir());
    f.window.action("new-file");
    wait!(|| find_window(f, "New Text File").is_some());
    find_window(f, "New Text File").unwrap().close();
    settle(0.05);
    ensure!(!f.source.join("Untitled.txt").exists());
    Ok(())
}

fn test_statusbar_and_single_click_preferences_apply_to_existing_views(f: &mut Fixture) -> Outcome {
    let prefs = f.app.preferences();
    wayfiles::bridge::pupdate(&prefs, wayfiles::bridge::map(&[("single_click", Value::Bool(true)), ("show_statusbar", Value::Bool(false))]))?;
    f.window.refresh_all();
    wait!(|| f.loaded());
    let pane = f.pane();
    ensure!(!pane.footer.is_visible());
    for view in [pane.icons.clone().upcast::<gtk::Widget>(), pane.compact.clone().upcast(), pane.details.clone().upcast()] {
        ensure!(BrowserPane::single_click(&view));
    }
    Ok(())
}

fn test_details_tree_expansion_selection_and_copy_nested_item(f: &mut Fixture) -> Outcome {
    f.window.action("details");
    wait!(|| f.loaded());
    let pane = f.pane();
    ensure!(pane.selection() == pane.tree_selection);
    let row = pane.tree.item(0).and_downcast::<gtk::TreeListRow>().unwrap();
    ensure_eq!(wayfiles::pane::item_of(row.upcast_ref()).unwrap().name(), "Documents");
    row.set_expanded(true);
    wait!(|| pane.tree.n_items() == 4);
    let nested = wayfiles::pane::item_of(&pane.tree.item(1).unwrap()).unwrap();
    ensure_eq!(nested.name(), "nested.txt");
    pane.selection().select_item(1, true);
    ensure_eq!(pane.selected()[0].file().path().unwrap(), f.source.join("Documents/nested.txt"));
    f.window.action("copy");
    pane.navigate(&file(&f.target), true, None);
    wait!(|| f.loaded());
    f.window.action("paste");
    wait!(|| f.job_idle() && f.target.join("nested.txt").exists());
    ensure_eq!(std::fs::read_to_string(f.target.join("nested.txt")).unwrap(), "deep needle\n");
    Ok(())
}

fn test_entry_shortcuts_do_not_copy_or_delete_selected_files(f: &mut Fixture) -> Outcome {
    f.select("Alpha.txt")?;
    f.window.action("location");
    let pane = f.pane();
    pane.location.set_text("editable text");
    pane.location.select_region(0, -1);
    pane.location.grab_focus();
    settle(0.1);
    run("wtype", &["-M", "ctrl", "-k", "c", "-m", "ctrl"])?;
    settle(0.1);
    ensure!(f.app.clipboard_files.borrow().is_empty(), "Ctrl+C in the location entry must copy text, not files");
    run("wtype", &["-k", "Delete"])?;
    settle(0.1);
    ensure_eq!(pane.location.text().as_str(), "");
    ensure!(f.source.join("Alpha.txt").exists(), "Delete in a text field must not trash files");
    Ok(())
}

fn test_file_context_copy_after_location_entry_focus(f: &mut Fixture) -> Outcome {
    f.maximize()?;
    f.window.action("location");
    let pane = f.pane();
    pane.location.set_text("text that must not be copied");
    pane.location.select_region(0, -1);
    pane.location.grab_focus();
    settle(0.1);
    ensure!(gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w.is::<gtk::Editable>() || w.is::<gtk::Text>()));
    let tile = f.tile("Alpha.txt").ok_or("no Alpha tile")?;
    f.pointer_click(&tile, "right", false)?;
    wait!(|| f.popover_visible());
    ensure!(!gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w.is::<gtk::Text>()));
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    ensure!(f.activate("win.copy"));
    ensure_eq!(f.app.clipboard_files.borrow().iter().map(|f| f.path().unwrap()).collect::<Vec<_>>(), vec![f.source.join("Alpha.txt")]);
    f.popover().popdown();
    Ok(())
}

fn sidebar_button(f: &Fixture, location: &str) -> Option<gtk::Button> {
    tile::descendants(f.window.sidebar.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| tile::location_uri(b).as_deref() == Some(location))
}

fn test_bookmarks_remove_only_from_context_menu(f: &mut Fixture) -> Outcome {
    let chosen = f.source.join("Documents");
    f.app.bookmarks().borrow().add(&uri(&chosen), "Saved Documents").map_err(|e| e.to_string())?;
    f.app.bookmarks().borrow().add(&uri(&f.target), "Keep this bookmark").map_err(|e| e.to_string())?;
    f.window.refresh_sidebar();
    let button = tile::descendants(f.window.sidebar.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some("Saved Documents")).unwrap();
    ensure!(button.next_sibling().is_none(), "Bookmarks must have no inline remove button");
    let before = f.pane().current().uri();
    let controllers = button.observe_controllers();
    let gesture = (0..controllers.n_items()).filter_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
        .find(|g| g.button() == 3).unwrap();
    gesture.emit_by_name::<()>("pressed", &[&1i32, &20f64, &10f64]);
    wait!(|| f.popover_visible());
    ensure_eq!(f.pane().current().uri(), before);
    f.popover().popdown();
    ensure_eq!(f.app.bookmarks().borrow().list().len(), 2);
    f.window.bookmark_context_menu(button.upcast_ref(), &uri(&chosen), 20., 10.);
    ensure!(f.activate("bookmark.remove"));
    settle(0.05);
    let list = f.app.bookmarks().borrow().list();
    ensure_eq!(list.iter().map(|b| (b.uri.clone(), b.label.clone())).collect::<Vec<_>>(), vec![(uri(&f.target), "Keep this bookmark".to_string())]);
    ensure!(chosen.join("nested.txt").is_file());
    Ok(())
}

// --------------------------------------------------------- test_files_context

fn test_directory_menu_opens_terminal_and_new_tab_for_clicked_folder(f: &mut Fixture) -> Outcome {
    let original = f.window.current_page().unwrap();
    f.select("Alpha.txt")?;
    f.right_click(&f.tile("Documents").ok_or("no tile")?, false)?;
    ensure_eq!(f.selected_names(), vec!["Documents"]);
    ensure_eq!(f.menu_action("win.open-new-tab").as_deref(), Some("Open in New Tab"));
    ensure_eq!(f.menu_action("win.open-terminal").as_deref(), Some("Open in Terminal"));
    ensure!(f.window.lookup_action("open-new-tab").is_enabled());
    ensure!(f.window.lookup_action("open-terminal").is_enabled());
    let calls = f.terminal_hook();
    ensure!(f.activate("win.open-terminal"));
    ensure_eq!(*calls.borrow(), vec![f.source.join("Documents").to_string_lossy().into_owned()]);
    ensure!(f.activate("win.open-new-tab"));
    wait!(|| f.window.pages().len() == 2 && f.loaded());
    ensure_eq!(f.pane().current().path().unwrap(), f.source.join("Documents"));
    ensure_eq!(original.left.current().path().unwrap(), f.source);
    Ok(())
}

fn test_regular_file_menu_excludes_directory_only_actions(f: &mut Fixture) -> Outcome {
    f.select("Documents")?;
    f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    ensure!(f.menu_action("win.open-new-tab").is_none());
    ensure!(f.menu_action("win.open-terminal").is_none());
    ensure!(!f.window.lookup_action("open-new-tab").is_enabled());
    ensure!(!f.window.lookup_action("open-terminal").is_enabled());
    Ok(())
}

fn assert_menu_unscrolled(f: &Fixture) -> Outcome {
    settle(0.3);
    let scroller = tile::descendants(f.popover().upcast_ref()).into_iter().find_map(|w| w.downcast::<gtk::ScrolledWindow>().ok())
        .ok_or("menu has no scroller")?;
    let adjustment = scroller.vadjustment();
    ensure!(adjustment.upper() <= adjustment.page_size() + 1., "menu scrolls: upper {} page {}", adjustment.upper(), adjustment.page_size());
    // The fix: parented to the window child, opening to the right.
    ensure!(f.popover().parent() == f.window.window.child());
    ensure_eq!(f.popover().position(), gtk::PositionType::Right);
    Ok(())
}

fn test_context_menus_show_every_item_without_scrolling(f: &mut Fixture) -> Outcome {
    f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
    assert_menu_unscrolled(f)?;
    f.right_click(&f.pane().view_widget(None), true)?;
    assert_menu_unscrolled(f)?;
    Ok(())
}

fn test_dismissed_row_menu_is_released_before_navigation_and_mount_refresh(f: &mut Fixture) -> Outcome {
    for mode in ["details", "icons", "compact"] {
        f.pane().navigate(&file(&f.source), true, None);
        f.window.action(mode);
        wait!(|| f.loaded());
        f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
        let menu = f.popover();
        menu.popdown();
        settle(0.2);
        f.pane().navigate(&file(&f.target), true, None);
        wait!(|| f.loaded());
        if let Some(monitor) = f.window.mount_monitor.borrow().clone() {
            monitor.emit_by_name::<()>("mounts-changed", &[]);
        }
        ensure!(f.window.popover.borrow().is_none(), "{mode}: menu retained");
        ensure!(menu.parent().is_none(), "{mode}: menu still parented");
    }
    Ok(())
}

fn test_visible_row_menu_is_released_when_its_anchor_disappears(f: &mut Fixture) -> Outcome {
    f.window.action("details");
    wait!(|| f.loaded());
    f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
    let menu = f.popover();
    f.pane().navigate(&file(&f.target), true, None);
    wait!(|| f.loaded());
    ensure!(f.window.popover.borrow().is_none());
    ensure!(menu.parent().is_none());
    Ok(())
}

fn test_closed_tab_menu_does_not_leave_a_dangling_parent(f: &mut Fixture) -> Outcome {
    let page = f.window.new_tab(Some(&file(&f.source)), None);
    f.window.action("details");
    wait!(|| f.loaded());
    f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
    f.popover().popdown();
    settle(0.2);
    f.window.close_tab(Some(&page));
    drop(page);
    settle(0.1);
    if let Some(monitor) = f.window.mount_monitor.borrow().clone() {
        monitor.emit_by_name::<()>("mounts-changed", &[]);
    }
    ensure!(f.window.popover.borrow().is_none());
    Ok(())
}

fn test_location_menu_is_released_after_escape_and_sidebar_refresh(f: &mut Fixture) -> Outcome {
    let home = sidebar_button(f, &uri(&suite_core::paths::home())).ok_or("no Home place")?;
    f.right_click(&home, false)?;
    let menu = f.popover();
    f.key("Escape")?;
    wait!(|| f.window.popover.borrow().is_none());
    ensure!(menu.parent().is_none());
    if let Some(monitor) = f.window.mount_monitor.borrow().clone() {
        monitor.emit_by_name::<()>("mounts-changed", &[]);
    }
    Ok(())
}

fn test_menu_keyboard_activation_still_opens_a_folder_in_a_new_tab(f: &mut Fixture) -> Outcome {
    f.right_click(&f.tile("Documents").ok_or("no tile")?, false)?;
    let label = tile::descendants(f.popover().upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Label>().ok())
        .find(|l| l.text() == "Open in New Tab").ok_or("no item")?;
    let mut button: Option<gtk::Widget> = Some(label.upcast());
    while let Some(widget) = button.clone() {
        if widget.css_name() == "modelbutton" {
            break;
        }
        button = widget.parent();
    }
    let button = button.ok_or("no modelbutton")?;
    button.grab_focus();
    f.key("Return")?;
    wait!(|| f.window.pages().len() == 2 && f.loaded());
    ensure_eq!(f.pane().current().path().unwrap(), f.source.join("Documents"));
    wait!(|| f.window.popover.borrow().is_none());
    Ok(())
}

fn test_breadcrumb_context_targets_ancestor_instead_of_selection(f: &mut Fixture) -> Outcome {
    f.pane().navigate(&file(&f.source.join("Documents")), true, None);
    wait!(|| f.loaded());
    f.select("nested.txt")?;
    let button = tile::descendants(f.pane().breadcrumbs.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| tile::location_uri(b).as_deref() == Some(uri(&f.source).as_str())).ok_or("no crumb")?;
    f.right_click(&button, false)?;
    ensure_eq!(f.menu_action("location.window").as_deref(), Some("Open in New Window"));
    let calls = f.terminal_hook();
    f.activate("location.terminal");
    ensure_eq!(*calls.borrow(), vec![f.source.to_string_lossy().into_owned()]);
    f.right_click(&button, false)?;
    f.activate("location.tab");
    wait!(|| f.window.pages().len() == 2 && f.loaded());
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    Ok(())
}

fn test_places_new_window_preserves_current_tab_and_bookmarks_keep_actions(f: &mut Fixture) -> Outcome {
    f.select("Alpha.txt")?;
    let original = f.window.current_page().unwrap();
    let home = sidebar_button(f, &uri(&suite_core::paths::home())).ok_or("no Home")?;
    f.right_click(&home, false)?;
    ensure!(f.menu_action("location.tab").is_some());
    ensure!(f.menu_action("bookmark.remove").is_none());
    f.activate("location.window");
    wait!(|| f.browser_windows().len() == 2);
    let other = f.browser_windows().into_iter().find(|w| w != &f.window).unwrap();
    wait!(|| !other.active_pane().unwrap().loading());
    ensure_eq!(other.active_pane().unwrap().current().path().unwrap(), suite_core::paths::home());
    ensure!(f.window.current_page().unwrap() == original);
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    other.window.close();
    f.window.window.present();
    settle(0.05);
    f.app.bookmarks().borrow().add(&uri(&f.target), "Target").map_err(|e| e.to_string())?;
    f.window.refresh_sidebar();
    let bookmark = sidebar_button(f, &uri(&f.target)).ok_or("no bookmark")?;
    f.right_click(&bookmark, false)?;
    ensure!(f.menu_action("bookmark.remove").is_some());
    let calls = f.terminal_hook();
    f.activate("location.terminal");
    ensure_eq!(*calls.borrow(), vec![f.target.to_string_lossy().into_owned()]);
    Ok(())
}

fn test_virtual_place_still_opens_tabs_but_cannot_be_a_terminal_directory(f: &mut Fixture) -> Outcome {
    let trash = sidebar_button(f, "trash:///").ok_or("no Trash place")?;
    f.right_click(&trash, false)?;
    let calls = f.terminal_hook();
    f.activate("location.terminal");
    ensure!(calls.borrow().is_empty());
    ensure!(f.menu_action("location.tab").is_some());
    Ok(())
}

fn test_trash_menu_requires_confirmation_before_emptying(f: &mut Fixture) -> Outcome {
    let trash = sidebar_button(f, "trash:///").ok_or("no Trash place")?;
    f.right_click(&trash, false)?;
    ensure_eq!(f.menu_action("location.empty-trash").as_deref(), Some("Empty Trash…"));
    let asked: Rc<RefCell<Vec<(String, Box<dyn FnOnce(bool)>)>>> = Rc::default();
    let sink = asked.clone();
    *f.app.hooks.confirm.borrow_mut() = Some(Rc::new(move |_, message, callback| sink.borrow_mut().push((message.to_string(), callback))));
    let operations = Rc::new(Cell::new(0));
    let o = operations.clone();
    *f.app.hooks.run_operation.borrow_mut() = Some(Rc::new(move |_, _, _| {
        o.set(o.get() + 1);
        None
    }));
    f.activate("location.empty-trash");
    ensure_eq!(asked.borrow().len(), 1);
    let (message, callback) = asked.borrow_mut().pop().unwrap();
    ensure!(message.contains("cannot be undone"));
    callback(false);
    settle(0.05);
    ensure_eq!(operations.get(), 0);
    Ok(())
}

fn test_blank_menu_clears_previous_selection_and_uses_current_directory_in_all_views(f: &mut Fixture) -> Outcome {
    for mode in ["icons", "compact", "details"] {
        if let Some(p) = f.window.popover.borrow().clone() {
            p.popdown();
        }
        settle(0.05);
        f.window.action(mode);
        wait!(|| f.loaded());
        f.select("Documents")?;
        let view = f.pane().views.visible_child().and_downcast::<gtk::ScrolledWindow>().and_then(|s| s.child()).ok_or("no view")?;
        f.right_click(&view, true)?;
        ensure!(f.pane().selected().is_empty(), "{mode}: selection kept");
        ensure_eq!(f.menu_action("win.open-terminal").as_deref(), Some("Open in Terminal"));
        ensure!(f.menu_action("win.open-new-tab").is_none());
        let calls = f.terminal_hook();
        ensure!(f.activate("win.open-terminal"));
        ensure_eq!(*calls.borrow(), vec![f.source.to_string_lossy().into_owned()]);
    }
    Ok(())
}

fn test_details_non_name_cell_targets_its_row_instead_of_blank_directory(f: &mut Fixture) -> Outcome {
    f.window.action("details");
    wait!(|| f.loaded());
    f.select("Alpha.txt")?;
    let mut cell: Option<gtk::Widget> = f.tile("Documents").map(|t| t.upcast());
    while let Some(widget) = cell.clone() {
        if widget.css_name() == "cell" {
            break;
        }
        cell = widget.parent();
    }
    let size_cell = cell.and_then(|c| c.next_sibling()).ok_or("no size cell")?;
    f.right_click(&size_cell, false)?;
    ensure_eq!(f.selected_names(), vec!["Documents"]);
    ensure_eq!(f.menu_action("win.open-terminal").as_deref(), Some("Open in Terminal"));
    let calls = f.terminal_hook();
    ensure!(f.activate("win.open-terminal"));
    ensure_eq!(*calls.borrow(), vec![f.source.join("Documents").to_string_lossy().into_owned()]);
    Ok(())
}

// -------------------------------------------------------- test_files_browsing

fn test_space_opens_quick_preview_that_steps_and_closes(f: &mut Fixture) -> Outcome {
    f.select("Alpha.txt")?;
    let pane = f.pane();
    let view = pane.view_widget(None);
    ensure!(pane.view_key_pressed(&view, gdk::Key::space, 65, gdk::ModifierType::empty()));
    let look = f.window.quicklook.borrow().clone().ok_or("no quick look")?;
    wait!(|| look.preview.widget().visible_child_name().as_deref() == Some("text"));
    ensure_eq!(look.heading.text().as_str(), "Alpha.txt");
    look.key_pressed(gdk::Key::Right, gdk::ModifierType::empty());
    ensure_eq!(f.selected_names(), vec!["Beta.txt"]);
    ensure_eq!(look.heading.text().as_str(), "Beta.txt");
    look.key_pressed(gdk::Key::space, gdk::ModifierType::empty());
    wait!(|| f.window.quicklook.borrow().is_none());
    // Space while typing a name belongs to type-ahead, never the preview.
    pane.set_type_prefix("my");
    ensure!(!pane.view_key_pressed(&view, gdk::Key::space, 65, gdk::ModifierType::empty()));
    ensure!(f.window.quicklook.borrow().is_none());
    Ok(())
}

fn test_arrow_key_without_selection_selects_first_item(f: &mut Fixture) -> Outcome {
    for mode in ["details", "icons", "compact", "columns"] {
        for key in [gdk::Key::Down, gdk::Key::Up, gdk::Key::Left, gdk::Key::Right] {
            f.window.action(mode);
            wait!(|| f.pane().views.visible_child_name().as_deref() == Some(mode) && f.loaded());
            let pane = f.pane();
            pane.selection().unselect_all();
            let view = pane.view_widget(None);
            ensure!(pane.view_key_pressed(&view, key, 0, gdk::ModifierType::empty()), "{mode} {key:?} not handled");
            ensure_eq!(pane.current().path().unwrap(), f.source);
            let first = names_of(&pane.selection())[0].clone();
            ensure_eq!(f.selected_names(), vec![first]);
        }
    }
    Ok(())
}

fn delete_selected(f: &Fixture) {
    *f.app.hooks.confirm.borrow_mut() = Some(Rc::new(|_, _, callback| callback(true)));
    f.window.action("delete");
    *f.app.hooks.confirm.borrow_mut() = None;
}

fn test_deleting_selects_the_item_before_the_deleted_items(f: &mut Fixture) -> Outcome {
    for name in ["Delta.txt", "Gamma.txt"] {
        write(&f.source.join(name), name);
    }
    f.pane().refresh();
    wait!(|| f.names().contains(&"Gamma.txt".to_string()) && f.loaded());
    f.select("Beta.txt")?;
    let delta = f.names().iter().position(|n| n == "Delta.txt").unwrap();
    f.pane().selection().select_item(delta as u32, false);
    delete_selected(f);
    wait!(|| !f.source.join("Delta.txt").exists() && f.selected_names() == vec!["Alpha.txt"]);
    settle(0.6);
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    // Removing the first item selects whatever is now first.
    f.select("Documents")?;
    delete_selected(f);
    wait!(|| !f.source.join("Documents").exists() && f.selected_names() == vec!["Alpha.txt"]);
    Ok(())
}

fn test_trashing_selects_the_item_before_the_trashed_item(f: &mut Fixture) -> Outcome {
    // Trash runs only on the private XDG_DATA_HOME (same filesystem as TMPDIR).
    use std::os::unix::fs::MetadataExt;
    let data = suite_core::paths::data_home();
    std::fs::create_dir_all(&data).unwrap();
    ensure!(data.starts_with(std::env::var("XDG_DATA_HOME").unwrap_or_default()) && data.to_string_lossy().contains("/cbt/"),
            "refusing to trash outside the private data home: {}", data.display());
    if std::fs::metadata(&data).unwrap().dev() != std::fs::metadata(&f.source).unwrap().dev() {
        println!("  (skipped: fixture and private data home on different filesystems)");
        return Ok(());
    }
    f.select("Beta.txt")?;
    f.window.action("trash");
    wait!(|| !f.source.join("Beta.txt").exists() && f.selected_names() == vec!["Alpha.txt"]);
    ensure!(data.join("Trash/files/Beta.txt").exists(), "not in the private trash");
    Ok(())
}

fn test_fuzzy_ranking_and_go_anywhere(f: &mut Fixture) -> Outcome {
    use files_model::goto;
    ensure!(goto::fuzzy_score("xyz", "Alpha.txt").is_none());
    ensure!(goto::fuzzy_score("alp", "Alpha.txt") > goto::fuzzy_score("apt", "Alpha.txt"));
    let nested = f.source.join("Documents/deep/deeper");
    std::fs::create_dir_all(&nested).unwrap();
    write(&nested.join("needle-notes.md"), "x");
    let mut index = goto::FolderIndex::new(&[f.source.clone()], false);
    ensure!(index.join(Duration::from_secs(5)));
    let paths: Vec<String> = index.entries().into_iter().map(|e| e.0).collect();
    ensure!(paths.contains(&nested.join("needle-notes.md").to_string_lossy().into_owned()));
    let slot = Rc::new(RefCell::new(Some(index)));
    let s = slot.clone();
    *f.app.hooks.folder_index.borrow_mut() = Some(Rc::new(move || s.borrow_mut().take().unwrap()));
    f.window.action("go-anywhere");
    let palette = f.window.goto.borrow().clone().ok_or("no palette")?;
    palette.entry.set_text("needle");
    wait!(|| palette.list.row_at_index(0).and_then(|r| palette.row_target(&r))
        .is_some_and(|(l, _)| l.basename().is_some_and(|b| b.to_string_lossy() == "needle-notes.md")));
    palette.entry.emit_activate();
    wait!(|| f.pane().current().path().as_deref() == Some(nested.as_path()) && f.loaded());
    wait!(|| f.selected_names() == vec!["needle-notes.md"]);
    ensure!(palette.closed.get());
    ensure!(palette.index.borrow().as_ref().is_none_or(|i| i.is_empty()));
    Ok(())
}

fn test_miller_columns_show_parents_current_and_selected_folder(f: &mut Fixture) -> Outcome {
    std::fs::create_dir_all(f.source.join("Documents/Inner")).unwrap();
    f.window.action("columns");
    wait!(|| f.pane().views.visible_child_name().as_deref() == Some("columns") && f.loaded());
    let pane = f.pane();
    let columns = pane.columns();
    ensure!(pane.view_widget(None) == *columns.current.upcast_ref::<gtk::Widget>());
    ensure!(columns.current.model().is_some_and(|m| m == *pane.flat_selection.upcast_ref::<gtk::SelectionModel>()));
    let parent = columns.parents().last().cloned().ok_or("no parent columns")?;
    ensure_eq!(parent.folder.path().unwrap(), f.source.parent().unwrap());
    wait!(|| parent.loaded.get());
    ensure_eq!(parent.selection.selected_item().and_downcast::<files_model::FileItem>().unwrap().name(), f.source.file_name().unwrap().to_string_lossy());
    f.select("Documents")?;
    wait!(|| columns.trail().is_some_and(|t| t.loaded.get()));
    ensure_eq!(columns.trail().unwrap().names(), vec!["Inner", "nested.txt"]);
    columns.trail().unwrap().selection.set_selected(1);
    wait!(|| pane.current().path().unwrap() == f.source.join("Documents") && f.loaded());
    wait!(|| f.selected_names() == vec!["nested.txt"]);
    ensure_eq!(columns.parents().last().unwrap().selection.selected_item().and_downcast::<files_model::FileItem>().unwrap().name(), "Documents");
    let current = columns.current.clone().upcast::<gtk::Widget>();
    ensure!(pane.view_key_pressed(&current, gdk::Key::Left, 113, gdk::ModifierType::empty()));
    wait!(|| pane.current().path().unwrap() == f.source && f.loaded());
    wait!(|| f.selected_names() == vec!["Documents"]);
    ensure!(pane.view_key_pressed(&current, gdk::Key::Right, 114, gdk::ModifierType::empty()));
    wait!(|| pane.current().path().unwrap() == f.source.join("Documents") && f.loaded());
    wait!(|| pane.selected().len() == 1);
    f.window.action("icons");
    wait!(|| pane.views.visible_child_name().as_deref() == Some("icons"));
    ensure!(columns.parents().is_empty() && columns.trail().is_none());
    Ok(())
}

fn test_folder_peek_lists_contents_without_navigating(f: &mut Fixture) -> Outcome {
    for index in 0..3 {
        write(&f.source.join(format!("Documents/file{index}.txt")), "x");
    }
    write(&f.source.join("Documents/.hidden"), "x");
    let pane = f.pane();
    let view = pane.view_widget(None);
    let folder = (0..pane.sorted.n_items()).filter_map(|i| pane.sorted.item(i).and_downcast::<files_model::FileItem>())
        .find(|i| i.name() == "Documents").unwrap();
    pane.peek_open(&view, &folder);
    wait!(|| pane.peek_popover().is_some());
    let labels: Vec<String> = tile::descendants(pane.peek_popover().unwrap().upcast_ref()).into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok()).map(|l| l.label().to_string()).collect();
    ensure_eq!(labels, vec!["Documents", "file0.txt", "file1.txt", "file2.txt", "nested.txt"]);
    ensure_eq!(pane.current().path().unwrap(), f.source);
    pane.peek_cancel();
    ensure!(pane.peek_popover().is_none());
    wayfiles::bridge::pset(&f.app.preferences(), "folder_peek", Value::Bool(false))?;
    pane.peek_schedule(&view, &folder);
    ensure!(!pane.peek_source_pending());
    Ok(())
}

fn test_vim_keys_and_shortcuts_by_key_position(f: &mut Fixture) -> Outcome {
    use wayfiles::keys::latin_keyval;
    let pane = f.pane();
    let view = pane.view_widget(None);
    let none = gdk::ModifierType::empty();
    let cyrillic_o = gdk::Key::from_name("Cyrillic_o").unwrap();
    ensure_eq!(latin_keyval(None, cyrillic_o, 44), gdk::Key::j);
    ensure_eq!(latin_keyval(None, gdk::Key::C, 54), gdk::Key::c);
    f.select("Alpha.txt")?;
    ensure!(!pane.view_key_pressed(&view, gdk::Key::j, 44, none));
    wayfiles::bridge::pset(&f.app.preferences(), "vim_keys", Value::Bool(true))?;
    ensure!(pane.view_key_pressed(&view, cyrillic_o, 44, none));
    ensure_eq!(f.selected_names(), vec!["Beta.txt"]);
    ensure!(pane.view_key_pressed(&view, gdk::Key::k, 45, none));
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    ensure!(pane.view_key_pressed(&view, gdk::Key::h, 43, none));
    wait!(|| pane.current().path().unwrap() == f.source.parent().unwrap() && f.loaded());
    ensure!(pane.view_key_pressed(&view, gdk::Key::l, 46, none));
    wait!(|| pane.current().path().unwrap() == f.source && f.loaded());
    let actions: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = actions.clone();
    *f.app.hooks.action.borrow_mut() = Some(Rc::new(move |name| {
        sink.borrow_mut().push(name.to_string());
        true
    }));
    let cyrillic_es = gdk::Key::from_name("Cyrillic_es").unwrap();
    ensure!(f.window.layout_shortcut(cyrillic_es, 54, gdk::ModifierType::CONTROL_MASK));
    ensure!(!f.window.layout_shortcut(cyrillic_es, 54, none));
    ensure!(!f.window.layout_shortcut(gdk::Key::c, 54, gdk::ModifierType::CONTROL_MASK));
    *f.app.hooks.action.borrow_mut() = None;
    ensure_eq!(*actions.borrow(), vec!["copy"]);
    Ok(())
}

fn test_widened_name_column_shows_long_names(f: &mut Fixture) -> Outcome {
    let name = "a-rather-long-file-name-that-needs-more-than-forty-characters.txt";
    write(&f.source.join(name), "x");
    wait!(|| f.names().contains(&name.to_string()));
    f.window.action("details");
    wait!(|| f.loaded());
    f.window.window.set_default_size(1600, 700);
    f.pane().sort_column("name").unwrap().set_fixed_width(1000);
    let label = || tile::descendants(f.pane().details.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Label>().ok())
        .find(|l| l.label() == name && l.is_mapped());
    wait!(|| label().is_some_and(|l| l.width() > 600));
    settle(0.05);
    ensure!(!label().unwrap().layout().is_ellipsized());
    Ok(())
}

fn test_text_and_binary_previews(f: &mut Fixture) -> Outcome {
    write(&f.source.join("script.py"), "def hello():\n    return 42\n");
    std::fs::write(f.source.join("blob.txt"), b"text\x00binary").unwrap();
    wait!(|| f.names().contains(&"script.py".to_string()) && f.names().contains(&"blob.txt".to_string()));
    let panel = f.window.information.clone();
    if !panel.widget().is_visible() {
        f.window.information_toggle.emit_clicked();
    }
    f.select("script.py")?;
    wait!(|| panel.preview.widget().visible_child_name().as_deref() == Some("text"));
    let buffer = panel.preview.buffer.clone();
    ensure_eq!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).as_str(), "def hello():\n    return 42\n");
    ensure!(panel.rows().iter().any(|(k, v)| k == "Lines" && v == "2"));
    ensure!(panel.print_button.is_visible());
    f.select("blob.txt")?;
    wait!(|| panel.hint.text() == "This file contains binary data.");
    ensure!(!panel.print_button.is_visible());
    f.select("Documents")?;
    ensure_eq!(panel.preview.widget().visible_child_name().as_deref(), Some("placeholder"));
    Ok(())
}

// ---------------------------------------------------------------------- main

fn checks() -> Vec<(&'static str, Check)> {
    let mut list: Vec<(&'static str, Check)> = vec![
        ("test_folder_views_hidden_files_selection_and_information", test_folder_views_hidden_files_selection_and_information),
        ("test_tabs_split_history_and_bookmark", test_tabs_split_history_and_bookmark),
        ("test_tab_close_button_closes_its_tab", test_tab_close_button_closes_its_tab),
        ("test_filter_recursive_name_search_and_contents_search", test_filter_recursive_name_search_and_contents_search),
        ("test_clipboard_copy_cut_paste_and_undo", test_clipboard_copy_cut_paste_and_undo),
        ("test_jobs_run_concurrently_and_share_one_progress_popover", test_jobs_run_concurrently_and_share_one_progress_popover),
        ("test_closing_window_keeps_jobs_running_and_notifies_when_done", test_closing_window_keeps_jobs_running_and_notifies_when_done),
        ("test_closing_window_can_cancel_its_jobs", test_closing_window_can_cancel_its_jobs),
        ("test_ctrl_q_asks_once_about_running_jobs_then_closes", test_ctrl_q_asks_once_about_running_jobs_then_closes),
        ("test_paste_folder_into_its_own_folder_asks_for_a_name", test_paste_folder_into_its_own_folder_asks_for_a_name),
        ("test_new_folder_dialog_accept_and_cancel", test_new_folder_dialog_accept_and_cancel),
        ("test_statusbar_and_single_click_preferences_apply_to_existing_views", test_statusbar_and_single_click_preferences_apply_to_existing_views),
        ("test_details_tree_expansion_selection_and_copy_nested_item", test_details_tree_expansion_selection_and_copy_nested_item),
        ("test_entry_shortcuts_do_not_copy_or_delete_selected_files", test_entry_shortcuts_do_not_copy_or_delete_selected_files),
        ("test_file_context_copy_after_location_entry_focus", test_file_context_copy_after_location_entry_focus),
        ("test_bookmarks_remove_only_from_context_menu", test_bookmarks_remove_only_from_context_menu),
        ("test_directory_menu_opens_terminal_and_new_tab_for_clicked_folder", test_directory_menu_opens_terminal_and_new_tab_for_clicked_folder),
        ("test_regular_file_menu_excludes_directory_only_actions", test_regular_file_menu_excludes_directory_only_actions),
        ("test_context_menus_show_every_item_without_scrolling", test_context_menus_show_every_item_without_scrolling),
        ("test_dismissed_row_menu_is_released_before_navigation_and_mount_refresh", test_dismissed_row_menu_is_released_before_navigation_and_mount_refresh),
        ("test_visible_row_menu_is_released_when_its_anchor_disappears", test_visible_row_menu_is_released_when_its_anchor_disappears),
        ("test_closed_tab_menu_does_not_leave_a_dangling_parent", test_closed_tab_menu_does_not_leave_a_dangling_parent),
        ("test_location_menu_is_released_after_escape_and_sidebar_refresh", test_location_menu_is_released_after_escape_and_sidebar_refresh),
        ("test_menu_keyboard_activation_still_opens_a_folder_in_a_new_tab", test_menu_keyboard_activation_still_opens_a_folder_in_a_new_tab),
        ("test_breadcrumb_context_targets_ancestor_instead_of_selection", test_breadcrumb_context_targets_ancestor_instead_of_selection),
        ("test_places_new_window_preserves_current_tab_and_bookmarks_keep_actions", test_places_new_window_preserves_current_tab_and_bookmarks_keep_actions),
        ("test_virtual_place_still_opens_tabs_but_cannot_be_a_terminal_directory", test_virtual_place_still_opens_tabs_but_cannot_be_a_terminal_directory),
        ("test_trash_menu_requires_confirmation_before_emptying", test_trash_menu_requires_confirmation_before_emptying),
        ("test_blank_menu_clears_previous_selection_and_uses_current_directory_in_all_views", test_blank_menu_clears_previous_selection_and_uses_current_directory_in_all_views),
        ("test_details_non_name_cell_targets_its_row_instead_of_blank_directory", test_details_non_name_cell_targets_its_row_instead_of_blank_directory),
        ("test_space_opens_quick_preview_that_steps_and_closes", test_space_opens_quick_preview_that_steps_and_closes),
        ("test_arrow_key_without_selection_selects_first_item", test_arrow_key_without_selection_selects_first_item),
        ("test_deleting_selects_the_item_before_the_deleted_items", test_deleting_selects_the_item_before_the_deleted_items),
        ("test_trashing_selects_the_item_before_the_trashed_item", test_trashing_selects_the_item_before_the_trashed_item),
        ("test_fuzzy_ranking_and_go_anywhere", test_fuzzy_ranking_and_go_anywhere),
        ("test_miller_columns_show_parents_current_and_selected_folder", test_miller_columns_show_parents_current_and_selected_folder),
        ("test_folder_peek_lists_contents_without_navigating", test_folder_peek_lists_contents_without_navigating),
        ("test_vim_keys_and_shortcuts_by_key_position", test_vim_keys_and_shortcuts_by_key_position),
        ("test_widened_name_column_shows_long_names", test_widened_name_column_shows_long_names),
        ("test_text_and_binary_previews", test_text_and_binary_previews),
    ];
    list.extend(more_checks());
    list
}

fn main() {
    if std::env::var("WAYFILES_UI_ISOLATED_TEST").as_deref() != Ok("1") {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private compositor)");
        return;
    }
    // Belt and braces: never touch the live session.
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if runtime == "/run/user/1000" || display.is_empty() || !std::env::temp_dir().to_string_lossy().contains("/cbt/") {
        eprintln!("ui_headless: refusing to run outside the private compositor");
        std::process::exit(1);
    }
    let filters: Vec<String> = std::env::args().skip(1).collect();
    suite_core::i18n::initialize();
    gtk::init().expect("gtk");
    // A persistent virtual keyboard so the seat has one (Python: wtype -s 60000).
    let mut keyboard = Command::new("wtype").args(["-s", "600000"]).spawn().ok();
    let app = FilesApplication::new();
    app.app.register(None::<&gio::Cancellable>).expect("register org.waydock.Files on the private bus");
    assert!(!app.app.is_remote(), "the private test application became remote");
    let _hold = app.app.hold();
    let (mut failures, mut ran) = (0, 0);
    for (name, check) in checks() {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        let started = Instant::now();
        let mut fixture = setup(&app, &format!("{ran}"));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&mut fixture)));
        let outcome = match result {
            Ok(Ok(())) => "ok".to_string(),
            Ok(Err(error)) => {
                failures += 1;
                format!("FAILED: {error}")
            }
            Err(panic) => {
                failures += 1;
                format!("PANICKED: {:?}", panic.downcast_ref::<String>().cloned().or(panic.downcast_ref::<&str>().map(|s| s.to_string())))
            }
        };
        println!("test {name} ... {outcome} ({:.1}s)", started.elapsed().as_secs_f64());
        teardown(&fixture);
    }
    if let Some(child) = keyboard.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    println!("ui_headless: {} passed, {failures} failed", ran - failures);
    std::process::exit(if failures == 0 { 0 } else { 1 });
}

fn more_checks() -> Vec<(&'static str, Check)> {
    vec![
        ("test_context_menu_on_a_short_output_shows_every_item", test_context_menu_on_a_short_output_shows_every_item),
        ("test_git_status_labels_appear_and_refresh_is_throttled", test_git_status_labels_appear_and_refresh_is_throttled),
        ("test_tabs_only_appear_for_multiple_tabs_below_the_address_bar", test_tabs_only_appear_for_multiple_tabs_below_the_address_bar),
        ("test_address_aligns_with_file_area_at_two_sizes_and_when_split", test_address_aligns_with_file_area_at_two_sizes_and_when_split),
        ("test_new_tabs_open_next_to_the_active_tab", test_new_tabs_open_next_to_the_active_tab),
        ("test_tab_changes_show_the_selected_tabs_address", test_tab_changes_show_the_selected_tabs_address),
        ("test_split_pane_changes_keep_the_address_with_the_active_pane", test_split_pane_changes_keep_the_address_with_the_active_pane),
        ("test_blank_address_space_opens_edit_mode_and_enter_navigates", test_blank_address_space_opens_edit_mode_and_enter_navigates),
        ("test_escape_discards_an_edited_address_without_changing_selection", test_escape_discards_an_edited_address_without_changing_selection),
        ("test_always_editable_address_does_not_intercept_search_or_filter_escape", test_always_editable_address_does_not_intercept_search_or_filter_escape),
        ("test_home_breadcrumbs_are_short_and_click_without_entering_edit_mode", test_home_breadcrumbs_are_short_and_click_without_entering_edit_mode),
        ("test_busy_visible_log_updates_without_rebuilding_folder", test_busy_visible_log_updates_without_rebuilding_folder),
        ("test_create_rename_delete_updates_without_losing_other_selection", test_create_rename_delete_updates_without_losing_other_selection),
        ("test_hidden_writes_do_not_churn_visible_models", test_hidden_writes_do_not_churn_visible_models),
        ("test_unrelated_log_preserves_details_expansion_and_child_selection", test_unrelated_log_preserves_details_expansion_and_child_selection),
        ("test_navigation_drops_queued_changes_from_previous_folder", test_navigation_drops_queued_changes_from_previous_folder),
        ("test_desktop_entry_icon_appears_in_all_views_and_information", test_desktop_entry_icon_appears_in_all_views_and_information),
        ("test_header_sorts_root_and_expanded_children_with_same_policy", test_header_sorts_root_and_expanded_children_with_same_policy),
        ("test_folder_header_preferences_and_menu_stay_in_sync_after_navigation", test_folder_header_preferences_and_menu_stay_in_sync_after_navigation),
        ("test_only_displayed_layout_binds_rows", test_only_displayed_layout_binds_rows),
        ("test_hidden_tab_keeps_tree_selection_scroll_and_live_updates", test_hidden_tab_keeps_tree_selection_scroll_and_live_updates),
        ("test_navigation_reuses_factories_and_closed_tabs_release_objects", test_navigation_reuses_factories_and_closed_tabs_release_objects),
        ("test_reveal_in_hidden_tab_scrolls_on_return", test_reveal_in_hidden_tab_scrolls_on_return),
        ("test_split_panes_both_render_and_closed_split_is_released", test_split_panes_both_render_and_closed_split_is_released),
        ("test_jpg_jpeg_png_render_in_all_folder_views_and_information", test_jpg_jpeg_png_render_in_all_folder_views_and_information),
        ("test_large_visible_batch_does_not_drop_previews", test_large_visible_batch_does_not_drop_previews),
        ("test_broken_cached_thumbnail_falls_back_to_jpeg_original", test_broken_cached_thumbnail_falls_back_to_jpeg_original),
        ("test_invalid_cache_is_ignored_and_camera_orientation_is_respected", test_invalid_cache_is_ignored_and_camera_orientation_is_respected),
        ("test_scrolled_away_requests_cancel_without_decoding_backlog", test_scrolled_away_requests_cancel_without_decoding_backlog),
        ("test_corrupt_image_keeps_icon_and_does_not_cache_failed_decode", test_corrupt_image_keeps_icon_and_does_not_cache_failed_decode),
        ("test_split_opens_one_tab_with_two_distinct_folders", test_split_opens_one_tab_with_two_distinct_folders),
        ("test_reveal_scrolls_to_capture_in_all_presentations", test_reveal_scrolls_to_capture_in_all_presentations),
        ("test_reveal_groups_same_parent_without_losing_selected_items", test_reveal_groups_same_parent_without_losing_selected_items),
        ("test_split_reveal_keeps_independent_selections", test_split_reveal_keeps_independent_selections),
        ("test_async_classification_preserves_argument_order", test_async_classification_preserves_argument_order),
        ("test_real_secondary_invocations_reuse_primary_and_preserve_arguments", test_real_secondary_invocations_reuse_primary_and_preserve_arguments),
        ("test_dependency_check_does_not_open_a_window", test_dependency_check_does_not_open_a_window),
        ("test_home_shortcut_opens_home_instead_of_reusing_current_folder", test_home_shortcut_opens_home_instead_of_reusing_current_folder),
        ("test_external_sources_are_copy_only_and_choice_rules", test_external_sources_are_copy_only_and_choice_rules),
        ("test_external_drop_menu_runs_the_chosen_operation", test_external_drop_menu_runs_the_chosen_operation),
        ("test_item_press_suspends_rubberband_so_quick_drags_start", test_item_press_suspends_rubberband_so_quick_drags_start),
        ("test_ark_drop_negotiation_and_extraction_request", test_ark_drop_negotiation_and_extraction_request),
        ("test_hidden_prefix_offers_only_real_directories_including_symlinks", test_hidden_prefix_offers_only_real_directories_including_symlinks),
        ("test_tab_completes_a_path_and_arrow_enter_opens_a_child", test_tab_completes_a_path_and_arrow_enter_opens_a_child),
        ("test_mouse_selects_folder_without_losing_the_edited_address", test_mouse_selects_folder_without_losing_the_edited_address),
        ("test_fast_tab_waits_for_completion_instead_of_leaving_the_address", test_fast_tab_waits_for_completion_instead_of_leaving_the_address),
        ("test_escape_then_escape_dismisses_suggestions_then_the_editor", test_escape_then_escape_dismisses_suggestions_then_the_editor),
        ("test_relative_paths_and_uri_escaping_navigate_to_the_suggested_folder", test_relative_paths_and_uri_escaping_navigate_to_the_suggested_folder),
        ("test_stale_lookup_invalid_path_and_tab_switch_never_restore_old_suggestions", test_stale_lookup_invalid_path_and_tab_switch_never_restore_old_suggestions),
        ("test_both_panes_and_preview_width_restore_after_reopening", test_both_panes_and_preview_width_restore_after_reopening),
        ("test_selection_title_follows_folder_and_multi_selection", test_selection_title_follows_folder_and_multi_selection),
    ]
}

/// Regression for the scrolled-menu bug: on an output too short for the menu
/// below *or* above the pointer, GTK used to shrink it into a scrolled list.
fn test_context_menu_on_a_short_output_shows_every_item(f: &mut Fixture) -> Outcome {
    let display = gdk::Display::default().unwrap();
    let monitor = display.monitors().item(0).and_downcast::<gdk::Monitor>().ok_or("no monitor")?;
    let connector = monitor.connector().ok_or("no connector")?.to_string();
    let original = monitor.geometry();
    run("wlr-randr", &["--output", &connector, "--custom-mode", "1280x560"])?;
    let result = (|| -> Outcome {
        wait!(|| monitor.geometry().height() == 560);
        f.window.window.unmaximize();
        settle(0.2);
        f.right_click(&f.tile("Alpha.txt").ok_or("no tile")?, false)?;
        let (_, natural, _, _) = f.popover().measure(gtk::Orientation::Vertical, -1);
        ensure!(natural > 280, "menu too short to exercise the bug: {natural}");
        assert_menu_unscrolled(f)
    })();
    let _ = run("wlr-randr", &["--output", &connector, "--custom-mode", &format!("{}x{}", original.width(), original.height())]);
    wait_for(3.0, || monitor.geometry().height() == original.height());
    result
}

// ------------------------------------------------------ test_files_navigation

fn test_tabs_only_appear_for_multiple_tabs_below_the_address_bar(f: &mut Fixture) -> Outcome {
    ensure_eq!(f.window.notebook.n_pages(), 1);
    ensure!(!f.window.notebook.shows_tabs());
    ensure!(!f.window.notebook.shows_border());
    let first = f.window.current_page().unwrap();
    f.window.new_tab(Some(&file(&f.target)), None);
    wait!(|| f.loaded());
    settle(0.1);
    ensure!(f.window.notebook.shows_tabs());
    let address = f.pane().location_box.compute_bounds(&f.window.window).ok_or("no address bounds")?;
    let tab = f.window.notebook.tab_label(&f.window.current_page().unwrap().paned).unwrap();
    let tab_bounds = tab.compute_bounds(&f.window.window).ok_or("no tab bounds")?;
    ensure!(tab_bounds.height() > 0.);
    ensure!(address.y() + address.height() <= tab_bounds.y() + 1.);
    f.window.action("close-tab");
    wait!(|| f.window.current_page().unwrap() == first);
    ensure!(!f.window.notebook.shows_tabs());
    ensure!(f.pane().location_box.parent() == Some(f.window.location_host.clone().upcast()));
    Ok(())
}

fn test_address_aligns_with_file_area_at_two_sizes_and_when_split(f: &mut Fixture) -> Outcome {
    ensure!(f.window.sidebar_scroll.is_visible());
    let mut widths = Vec::new();
    for width in [950, 1200] {
        f.window.window.set_default_size(width, 650);
        wait!(|| (f.window.window.width() - width).abs() <= 2);
        settle(0.1);
        widths.push(f.window.window.width());
        let address = f.pane().location_box.compute_bounds(&f.window.window).unwrap();
        let content = f.window.notebook.compute_bounds(&f.window.window).unwrap();
        ensure!((address.x() - content.x()).abs() <= 1., "{width}: address {} content {}", address.x(), content.x());
    }
    ensure!(widths[1] > widths[0]);
    f.window.action("split");
    wait!(|| f.loaded());
    settle(0.1);
    let address = f.pane().location_box.compute_bounds(&f.window.window).unwrap();
    let content = f.window.notebook.compute_bounds(&f.window.window).unwrap();
    ensure!((address.x() - content.x()).abs() <= 1.);
    Ok(())
}

fn tab_uris(window: &FilesWindow) -> Vec<String> {
    window.pages().iter().map(|p| p.left.current().uri().to_string()).collect()
}

fn test_new_tabs_open_next_to_the_active_tab(f: &mut Fixture) -> Outcome {
    let first = f.window.current_page().unwrap();
    let middle = f.window.new_tab(Some(&file(&f.target)), None);
    let last = f.window.new_tab(Some(&file(&f.source.join("Documents"))), None);
    ensure!(f.window.pages() == vec![first.clone(), middle.clone(), last.clone()]);
    f.window.notebook.set_current_page(Some(1));
    settle(0.05);
    let plus = tile::descendants(f.window.window.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some("New tab")).unwrap();
    f.pointer_click(&plus, "left", false)?;
    let added = f.window.current_page().unwrap();
    ensure!(f.window.pages() == vec![first.clone(), middle.clone(), added.clone(), last.clone()]);
    ensure_eq!(added.left.current().path().unwrap(), f.target);
    ensure!(f.window.active_pane() == Some(added.left.clone()));
    f.window.notebook.set_current_page(Some(0));
    f.window.action("new-tab");
    let shortcut = f.window.current_page().unwrap();
    ensure!(f.window.pages() == vec![first.clone(), shortcut.clone(), middle.clone(), added.clone(), last.clone()]);
    let folder = f.window.new_tab(Some(&file(&f.source.join("Documents"))), None);
    ensure!(f.window.pages() == vec![first.clone(), shortcut.clone(), folder.clone(), middle.clone(), added.clone(), last.clone()]);
    let expected: Vec<Value> = tab_uris(&f.window).into_iter().map(Value::String).collect();
    wait!(|| wayfiles::bridge::pget(&f.app.preferences(), "tabs") == Value::Array(expected.clone()));
    f.window.close_tab(Some(&folder));
    let expected = tab_uris(&f.window);
    f.window.notebook.set_current_page(Some(2));
    f.window.window.close();
    wayfiles::bridge::pset(&f.app.preferences(), "restore_session", Value::Bool(true))?;
    f.app.session_restored.set(false);
    f.window = f.app.open_locations(Vec::new(), true, false, false);
    ensure_eq!(tab_uris(&f.window), expected);
    ensure_eq!(f.window.notebook.current_page(), Some(2));
    // Launchers name a folder: saved tabs still return, an open folder is reused.
    f.window.notebook.set_current_page(Some(0));
    f.window.window.close();
    ensure!(!f.app.session_restored.get());
    f.window = f.app.open_locations(vec![file(&f.target)], false, false, false);
    wait!(|| !f.window.pages().is_empty() && f.window.current_page().unwrap().left.current().path().unwrap() == f.target);
    ensure_eq!(tab_uris(&f.window), expected);
    f.window.window.close();
    let nested = f.source.join("Documents/deeper");
    std::fs::create_dir(&nested).unwrap();
    f.window = f.app.open_locations(vec![file(&nested)], false, false, false);
    wait!(|| f.window.pages().len() == expected.len() + 1);
    ensure_eq!(f.window.current_page().unwrap().left.current().path().unwrap(), nested);
    let current = f.window.current_page().unwrap();
    let others: Vec<String> = f.window.pages().into_iter().filter(|p| *p != current).map(|p| p.left.current().uri().to_string()).collect();
    ensure_eq!(others, expected);
    Ok(())
}

fn test_tab_changes_show_the_selected_tabs_address(f: &mut Fixture) -> Outcome {
    let first = f.window.current_page().unwrap();
    f.window.new_tab(Some(&file(&f.target)), None);
    wait!(|| f.loaded());
    let second = f.window.current_page().unwrap();
    let host: gtk::Widget = f.window.location_host.clone().upcast();
    ensure!(second.left.location_box.parent() == Some(host.clone()));
    ensure!(first.left.location_box.parent().is_none());
    ensure_eq!(f.pane().location.text().as_str(), f.target.to_string_lossy());
    f.window.notebook.set_current_page(Some(0));
    settle(0.05);
    ensure!(f.pane() == first.left);
    ensure!(first.left.location_box.parent() == Some(host));
    ensure!(second.left.location_box.parent().is_none());
    ensure_eq!(f.pane().location.text().as_str(), f.source.to_string_lossy());
    f.window.action("location");
    ensure_eq!(first.left.location_stack.visible_child_name().as_deref(), Some("entry"));
    ensure_eq!(second.left.location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    Ok(())
}

fn test_split_pane_changes_keep_the_address_with_the_active_pane(f: &mut Fixture) -> Outcome {
    f.window.action("split");
    let page = f.window.current_page().unwrap();
    let right = page.right().unwrap();
    wait!(|| !right.loading());
    right.navigate(&file(&f.target), true, None);
    wait!(|| !right.loading());
    let host: gtk::Widget = f.window.location_host.clone().upcast();
    ensure!(f.pane() == right);
    ensure!(right.location_box.parent() == Some(host.clone()));
    ensure_eq!(f.pane().location.text().as_str(), f.target.to_string_lossy());
    f.window.set_active_pane(&page.left);
    ensure!(page.left.location_box.parent() == Some(host.clone()));
    ensure!(right.location_box.parent().is_none());
    ensure_eq!(f.pane().location.text().as_str(), f.source.to_string_lossy());
    f.window.set_active_pane(&right);
    f.window.action("split");
    ensure!(page.right().is_none());
    ensure!(f.pane() == page.left);
    ensure!(page.left.location_box.parent() == Some(host));
    Ok(())
}

fn test_blank_address_space_opens_edit_mode_and_enter_navigates(f: &mut Fixture) -> Outcome {
    let destination = f.target.join("Folder with spaces ");
    std::fs::create_dir(&destination).unwrap();
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    f.pointer_click(&f.pane().location_blank, "left", false)?;
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("entry"));
    ensure!(gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w.is::<gtk::Text>()));
    f.pane().location.set_text(&destination.to_string_lossy());
    f.key("Return")?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == destination);
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    Ok(())
}

fn test_escape_discards_an_edited_address_without_changing_selection(f: &mut Fixture) -> Outcome {
    f.select("Alpha.txt")?;
    f.window.action("location");
    f.pane().location.set_text("/an/unsubmitted/location");
    f.pane().location.grab_focus();
    settle(0.05);
    f.key("Escape")?;
    wait!(|| f.pane().location_stack.visible_child_name().as_deref() == Some("breadcrumbs"));
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    ensure_eq!(f.pane().location.text().as_str(), f.source.to_string_lossy());
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    Ok(())
}

fn test_always_editable_address_does_not_intercept_search_or_filter_escape(f: &mut Fixture) -> Outcome {
    wayfiles::bridge::pset(&f.app.preferences(), "location_editable", Value::Bool(true))?;
    f.pane().refresh();
    wait!(|| f.loaded());
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("entry"));
    for action in ["search", "filter"] {
        f.window.action(action);
        let widget: gtk::Widget = if action == "search" { f.pane().search_box.clone().upcast() } else { f.pane().filter_entry.clone().upcast() };
        ensure!(widget.is_visible());
        f.pane().views.visible_child().and_downcast::<gtk::ScrolledWindow>().unwrap().child().unwrap().grab_focus();
        settle(0.05);
        ensure!(!gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w.is::<gtk::Text>()));
        f.key("Escape")?;
        wait!(|| !widget.is_visible() && f.loaded());
        ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("entry"));
    }
    Ok(())
}

fn test_home_breadcrumbs_are_short_and_click_without_entering_edit_mode(f: &mut Fixture) -> Outcome {
    let videos = suite_core::paths::home().join("Videos");
    let clips = videos.join("Clips");
    std::fs::create_dir_all(&clips).unwrap();
    f.pane().navigate(&file(&clips), true, None);
    wait!(|| f.loaded());
    let buttons: Vec<gtk::Button> = tile::descendants(f.pane().breadcrumbs.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok()).collect();
    ensure_eq!(buttons.iter().map(|b| b.label().unwrap().to_string()).collect::<Vec<_>>(), vec!["Home", "Videos", "Clips"]);
    f.pointer_click(&buttons[1], "left", false)?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == videos);
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    ensure!(!gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w.is::<gtk::Text>()));
    let _ = std::fs::remove_dir_all(&videos);
    Ok(())
}

// --------------------------------------------------------- test_files_monitor

fn store_item(pane: &BrowserPane, name: &str) -> Option<files_model::FileItem> {
    (0..pane.store.n_items()).filter_map(|i| pane.store.item(i).and_downcast::<files_model::FileItem>()).find(|i| i.name() == name)
}

fn append_line(path: &Path, text: &str) {
    use std::io::Write;
    let mut stream = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(stream, "{text}").unwrap();
}

fn prepare_log(f: &Fixture) -> Result<PathBuf, String> {
    let path = f.source.join("steam-4162040.log");
    write(&path, "initial line\n");
    let pane = f.pane();
    wait!(|| store_item(&pane, "steam-4162040.log").is_some());
    settle(0.35);
    Ok(path)
}

fn test_busy_visible_log_updates_without_rebuilding_folder(f: &mut Fixture) -> Outcome {
    let path = prepare_log(f)?;
    let pane = f.pane();
    f.select("Alpha.txt")?;
    let alpha = store_item(&pane, "Alpha.txt").unwrap();
    let documents = store_item(&pane, "Documents").unwrap();
    let initial = store_item(&pane, "steam-4162040.log").unwrap().size();
    let factories = (pane.icons.factory(), pane.compact.factory());
    let empty: Rc<Cell<bool>> = Rc::default();
    let e = empty.clone();
    let handler = pane.store.connect_items_changed(move |store, _, _, _| if store.n_items() == 0 { e.set(true) });
    let (navigations, refreshes) = (pane.navigate_calls(), pane.refresh_calls());
    for index in 0..5 {
        append_line(&path, &format!("frame {index}: continuous game log output"));
        settle(0.32);
    }
    let size = std::fs::metadata(&path).unwrap().len();
    wait!(|| store_item(&pane, "steam-4162040.log").is_some_and(|i| i.size() == size));
    pane.store.disconnect(handler);
    ensure_eq!((pane.navigate_calls(), pane.refresh_calls()), (navigations, refreshes));
    ensure!(store_item(&pane, "steam-4162040.log").unwrap().size() > initial);
    ensure!(store_item(&pane, "Alpha.txt").unwrap() == alpha);
    ensure!(store_item(&pane, "Documents").unwrap() == documents);
    ensure!((pane.icons.factory(), pane.compact.factory()) == factories);
    ensure!(!empty.get(), "A log write emptied the visible file model");
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    ensure!(!pane.loading() && !pane.spinner.is_spinning());
    Ok(())
}

fn test_create_rename_delete_updates_without_losing_other_selection(f: &mut Fixture) -> Outcome {
    let pane = f.pane();
    f.select("Alpha.txt")?;
    let alpha = store_item(&pane, "Alpha.txt").unwrap();
    let (created, renamed) = (f.source.join("Created.txt"), f.source.join("Renamed.txt"));
    let (navigations, refreshes) = (pane.navigate_calls(), pane.refresh_calls());
    write(&created, "new file\n");
    wait!(|| f.names().contains(&"Created.txt".to_string()));
    std::fs::rename(&created, &renamed).unwrap();
    wait!(|| !f.names().contains(&"Created.txt".to_string()) && f.names().contains(&"Renamed.txt".to_string()));
    std::fs::remove_file(&renamed).unwrap();
    wait!(|| !f.names().contains(&"Renamed.txt".to_string()));
    ensure_eq!((pane.navigate_calls(), pane.refresh_calls()), (navigations, refreshes));
    ensure!(store_item(&pane, "Alpha.txt").unwrap() == alpha);
    ensure_eq!(f.selected_names(), vec!["Alpha.txt"]);
    ensure_eq!(f.names(), vec!["Documents", "Alpha.txt", "Beta.txt"]);
    Ok(())
}

fn test_hidden_writes_do_not_churn_visible_models(f: &mut Fixture) -> Outcome {
    let pane = f.pane();
    ensure!(!pane.option_bool("show_hidden"));
    f.select("Beta.txt")?;
    settle(0.35);
    let original: Vec<glib::Object> = (0..pane.sorted.n_items()).filter_map(|i| pane.sorted.item(i)).collect();
    let changed: Rc<Cell<u32>> = Rc::default();
    let c = changed.clone();
    let handler = pane.sorted.connect_items_changed(move |_, _, _, _| c.set(c.get() + 1));
    let navigations = pane.navigate_calls();
    for index in 0..4 {
        append_line(&f.source.join(".secret"), &format!("private background update {index}"));
        settle(0.32);
    }
    settle(0.4);
    pane.sorted.disconnect(handler);
    ensure_eq!(pane.navigate_calls(), navigations);
    ensure_eq!(changed.get(), 0);
    ensure!((0..pane.sorted.n_items()).filter_map(|i| pane.sorted.item(i)).collect::<Vec<_>>() == original);
    ensure_eq!(f.selected_names(), vec!["Beta.txt"]);
    Ok(())
}

fn test_unrelated_log_preserves_details_expansion_and_child_selection(f: &mut Fixture) -> Outcome {
    let path = prepare_log(f)?;
    f.window.action("details");
    let pane = f.pane();
    wait!(|| !pane.loading());
    let directory = pane.tree.item(0).and_downcast::<gtk::TreeListRow>().unwrap();
    directory.set_expanded(true);
    wait!(|| pane.tree.n_items() == 5);
    pane.selection().select_item(1, true);
    ensure_eq!(f.selected_names(), vec!["nested.txt"]);
    settle(0.35);
    let navigations = pane.navigate_calls();
    for index in 0..4 {
        append_line(&path, &format!("detail-view update {index}"));
        settle(0.32);
    }
    let size = std::fs::metadata(&path).unwrap().len();
    wait!(|| store_item(&pane, "steam-4162040.log").is_some_and(|i| i.size() == size));
    ensure_eq!(pane.navigate_calls(), navigations);
    ensure!(pane.tree.item(0).and_downcast::<gtk::TreeListRow>().unwrap() == directory);
    ensure!(directory.is_expanded());
    ensure_eq!(pane.tree.n_items(), 5);
    ensure_eq!(f.selected_names(), vec!["nested.txt"]);
    Ok(())
}

fn test_navigation_drops_queued_changes_from_previous_folder(f: &mut Fixture) -> Outcome {
    let path = prepare_log(f)?;
    let pane = f.pane();
    write(&f.target.join("Only-target.txt"), "new location\n");
    for index in 0..12 {
        write(&f.source.join(format!("Old-{index}.txt")), &index.to_string());
        append_line(&path, &format!("old-folder pending update {index}"));
    }
    pane.navigate(&file(&f.target), true, None);
    wait!(|| !pane.loading() && f.names() == vec!["Only-target.txt"]);
    settle(0.7);
    ensure_eq!(f.names(), vec!["Only-target.txt"]);
    ensure_eq!(pane.current().uri().to_string(), uri(&f.target));
    write(&f.target.join("Cancelled.txt"), "queued before close\n");
    pane.close();
    let snapshot: Vec<glib::Object> = (0..pane.store.n_items()).filter_map(|i| pane.store.item(i)).collect();
    settle(0.5);
    ensure!((0..pane.store.n_items()).filter_map(|i| pane.store.item(i)).collect::<Vec<_>>() == snapshot);
    Ok(())
}

fn test_desktop_entry_icon_appears_in_all_views_and_information(f: &mut Fixture) -> Outcome {
    let desktop = f.source.join("Terminal.desktop");
    write(&desktop, "[Desktop Entry]\nType=Application\nName=Terminal\nIcon=utilities-terminal\nExec=false\n");
    let pane = f.pane();
    wait!(|| store_item(&pane, "Terminal.desktop").is_some());
    for mode in ["icons", "compact", "details"] {
        f.window.action(mode);
        wait!(|| f.loaded());
        let view: gtk::Widget = match mode {
            "icons" => pane.icons.clone().upcast(),
            "compact" => pane.compact.clone().upcast(),
            _ => pane.details.clone().upcast(),
        };
        let first_icon = || tile::tile_for_uri(&view, &uri(&desktop)).and_then(|t| t.picture()).and_then(|p| p.gicon())
            .and_downcast::<gio::ThemedIcon>().map(|i| i.names()[0].to_string());
        wait!(|| first_icon().as_deref() == Some("utilities-terminal"));
    }
    if !f.window.information.widget().is_visible() {
        f.window.action("information");
    }
    f.select("Terminal.desktop")?;
    let icon = f.window.information.icon.gicon().and_downcast::<gio::ThemedIcon>().map(|i| i.names()[0].to_string());
    ensure_eq!(icon.as_deref(), Some("utilities-terminal"));
    Ok(())
}

// --------------------------------------------------------- test_files_sorting

fn sorting_fixture(f: &mut Fixture) -> Outcome {
    let folder = f.source.join("Folder");
    std::fs::create_dir_all(folder.join("Nested folder")).unwrap();
    write(&folder.join("File2.txt"), "22");
    write(&folder.join("File10.txt"), "0123456789");
    write(&f.source.join("Alpha.txt"), "12345678");
    write(&f.source.join("Beta.txt"), "b");
    std::fs::remove_dir_all(f.source.join("Documents")).unwrap();
    wayfiles::bridge::pupdate(&f.app.preferences(), wayfiles::bridge::map(&[("view", Value::String("details".into())), ("restore_session", Value::Bool(false))]))?;
    f.window.window.close();
    f.fixture_window()
}

fn folder_children(f: &Fixture) -> Result<(gtk::TreeListRow, gio::ListModel), String> {
    let pane = f.pane();
    let row = (0..pane.tree.n_items()).filter_map(|i| pane.tree.item(i).and_downcast::<gtk::TreeListRow>())
        .find(|r| wayfiles::pane::item_of(r.upcast_ref()).is_some_and(|i| i.name() == "Folder")).ok_or("no Folder row")?;
    row.set_expanded(true);
    wait!(|| row.children().is_some_and(|c| c.n_items() == 3));
    let children = row.children().unwrap();
    Ok((row, children))
}

fn header(f: &Fixture, key: &str, descending: bool) {
    let pane = f.pane();
    pane.details.sort_by_column(pane.sort_column(key).as_ref(), if descending { gtk::SortType::Descending } else { gtk::SortType::Ascending });
    settle(0.02);
}

fn test_header_sorts_root_and_expanded_children_with_same_policy(f: &mut Fixture) -> Outcome {
    sorting_fixture(f)?;
    let (row, children) = folder_children(f)?;
    header(f, "size", true);
    ensure_eq!(f.names(), vec!["Folder", "Alpha.txt", "Beta.txt"]);
    ensure_eq!(names_of(&children), vec!["Nested folder", "File10.txt", "File2.txt"]);
    let pane = f.pane();
    ensure!(pane.sorted.sorter() == Some(pane.sorter.clone().upcast()));
    ensure!(children.downcast_ref::<gtk::SortListModel>().and_then(|m| m.sorter()) == Some(pane.sorter.clone().upcast()));
    ensure!(row.is_expanded());
    ensure_eq!(wayfiles::bridge::pget(&f.app.preferences(), "sort"), Value::String("size".into()));
    ensure_eq!(wayfiles::bridge::pget(&f.app.preferences(), "descending"), Value::Bool(true));
    header(f, "name", false);
    ensure_eq!(names_of(&children), vec!["Nested folder", "File2.txt", "File10.txt"]);
    wayfiles::bridge::pupdate(&f.app.preferences(), wayfiles::bridge::map(&[("natural_sort", Value::Bool(false)), ("folders_first", Value::Bool(false))]))?;
    pane.refresh();
    wait!(|| f.loaded());
    let (_, children) = folder_children(f)?;
    ensure_eq!(f.names(), vec!["Alpha.txt", "Beta.txt", "Folder"]);
    ensure_eq!(names_of(&children), vec!["File10.txt", "File2.txt", "Nested folder"]);
    Ok(())
}

fn test_folder_header_preferences_and_menu_stay_in_sync_after_navigation(f: &mut Fixture) -> Outcome {
    sorting_fixture(f)?;
    let other = f.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let prefs = f.app.preferences();
    wayfiles::bridge::pset(&prefs, "remember_folders", Value::Bool(true))?;
    header(f, "size", true);
    let options = wayfiles::bridge::pfolder_options(&prefs, &uri(&f.source));
    ensure_eq!((options["sort"].clone(), options["descending"].clone()), (Value::String("size".into()), Value::Bool(true)));
    ensure_eq!((wayfiles::bridge::pget(&prefs, "sort"), wayfiles::bridge::pget(&prefs, "descending")), (Value::String("name".into()), Value::Bool(false)));
    let pane = f.pane();
    pane.refresh();
    wait!(|| f.loaded());
    let sorter = pane.details.sorter().and_downcast::<gtk::ColumnViewSorter>().unwrap();
    ensure!(sorter.primary_sort_column() == pane.sort_column("size"));
    ensure_eq!(sorter.primary_sort_order(), gtk::SortType::Descending);
    pane.navigate(&file(&other), true, None);
    wait!(|| f.loaded());
    ensure!(sorter.primary_sort_column() == pane.sort_column("name"));
    ensure_eq!(sorter.primary_sort_order(), gtk::SortType::Ascending);
    pane.navigate(&file(&f.source), true, None);
    wait!(|| f.loaded());
    ensure!(sorter.primary_sort_column() == pane.sort_column("size"));
    f.window.option("sort", Value::String("name".into()));
    wait!(|| f.loaded());
    ensure!(sorter.primary_sort_column() == pane.sort_column("name"));
    ensure_eq!(sorter.primary_sort_order(), gtk::SortType::Descending);
    let (_, children) = folder_children(f)?;
    ensure_eq!(names_of(&children), vec!["Nested folder", "File10.txt", "File2.txt"]);
    Ok(())
}

// ---------------------------------------------------------- test_files_memory

fn view_bound(view: &gtk::Widget) -> bool {
    tile::descendants(view).iter().any(|w| w.downcast_ref::<FileTile>().is_some_and(|t| t.bound_uri().is_some()))
}

fn test_only_displayed_layout_binds_rows(f: &mut Fixture) -> Outcome {
    for mode in ["icons", "compact", "details", "icons"] {
        f.window.action(mode);
        wait!(|| f.loaded());
        settle(0.05);
        let pane = f.pane();
        for (name, view) in [("icons", pane.icons.clone().upcast::<gtk::Widget>()), ("compact", pane.compact.clone().upcast()), ("details", pane.details.clone().upcast())] {
            let model = BrowserPane::single_click(&view); // touch
            let _ = model;
            let has_model = match name {
                "icons" => pane.icons.model().is_some(),
                "compact" => pane.compact.model().is_some(),
                _ => pane.details.model().is_some(),
            };
            if name == mode {
                ensure!(has_model, "{mode}: displayed view has no model");
                ensure!(view_bound(&view), "{mode}: no bound rows");
            } else {
                ensure!(!has_model, "{mode}: hidden {name} keeps its model");
                ensure!(!view_bound(&view), "{mode}: hidden {name} keeps rows");
            }
        }
    }
    Ok(())
}

fn test_hidden_tab_keeps_tree_selection_scroll_and_live_updates(f: &mut Fixture) -> Outcome {
    for index in 0..180 {
        write(&f.source.join(format!("file-{index:03}.txt")), "fixture");
    }
    f.window.action("details");
    wait!(|| f.loaded() && f.names().len() >= 183);
    let pane = f.pane();
    pane.tree.item(0).and_downcast::<gtk::TreeListRow>().unwrap().set_expanded(true);
    wait!(|| names_of(&pane.tree).contains(&"nested.txt".to_string()));
    let selected = names_of(&pane.tree).iter().position(|n| n == "file-130.txt").unwrap() as u32;
    pane.selection().select_item(selected, true);
    settle(0.15);
    let adjustment = pane.views.visible_child().and_downcast::<gtk::ScrolledWindow>().unwrap().vadjustment();
    adjustment.set_value(500.);
    settle(0.1);
    let position = adjustment.value();
    ensure!(position > 100.);
    let other = f.window.new_tab(Some(&file(&f.target)), None);
    wait!(|| f.loaded());
    ensure!(pane.icons.model().is_none() && pane.compact.model().is_none() && pane.details.model().is_none());
    write(&f.source.join("zzz-created-while-hidden.txt"), "live update");
    wait!(|| Fixture::pane_names(&pane).contains(&"zzz-created-while-hidden.txt".to_string()));
    f.window.notebook.set_current_page(Some(0));
    wait!(|| pane.widget().is_mapped() && pane.state.borrow().scroll_restore.is_none());
    ensure!(pane.tree.item(0).and_downcast::<gtk::TreeListRow>().unwrap().is_expanded());
    ensure_eq!(pane.selected().iter().map(|i| i.name()).collect::<Vec<_>>(), vec!["file-130.txt"]);
    ensure!((adjustment.value() - position).abs() <= 2., "scroll {} vs {position}", adjustment.value());
    f.window.close_tab(Some(&other));
    Ok(())
}

fn test_navigation_reuses_factories_and_closed_tabs_release_objects(f: &mut Fixture) -> Outcome {
    let page = f.window.new_tab(Some(&file(&f.source)), None);
    wait!(|| f.loaded());
    let pane = page.left.clone();
    let original = (pane.icons.factory(), pane.compact.factory());
    for directory in [&f.target, &f.source, &f.target, &f.source] {
        pane.navigate(&file(directory), true, None);
        wait!(|| !pane.loading());
        ensure!((pane.icons.factory(), pane.compact.factory()) == original);
    }
    drop(original);
    let page_ref = Rc::downgrade(&page.0);
    let pane_ref = Rc::downgrade(&pane.0);
    let rows: Vec<glib::WeakRef<FileTile>> = tile::descendants(pane.views.upcast_ref()).into_iter()
        .filter_map(|w| w.downcast::<FileTile>().ok()).filter(|t| t.bound_uri().is_some()).map(|t| t.downgrade()).collect();
    ensure!(!rows.is_empty());
    f.window.close_tab(Some(&page));
    drop(page);
    drop(pane);
    settle(0.2);
    ensure!(page_ref.upgrade().is_none(), "closed tab retained");
    ensure!(pane_ref.upgrade().is_none(), "closed pane retained");
    ensure!(rows.iter().all(|r| r.upgrade().is_none()), "file row retained");
    Ok(())
}

fn test_reveal_in_hidden_tab_scrolls_on_return(f: &mut Fixture) -> Outcome {
    for index in 0..180 {
        write(&f.source.join(format!("file-{index:03}.txt")), "fixture");
    }
    f.window.action("details");
    wait!(|| f.loaded() && f.names().len() >= 183);
    let pane = f.pane();
    f.window.new_tab(Some(&file(&f.target)), None);
    wait!(|| f.loaded());
    pane.navigate(&file(&f.source), true, Some(vec![uri(&f.source.join("file-179.txt"))]));
    wait!(|| !pane.loading());
    ensure!(pane.state.borrow().reveal_selection);
    f.window.notebook.set_current_page(Some(0));
    wait!(|| !pane.state.borrow().reveal_selection);
    settle(0.2);
    ensure_eq!(pane.selected().iter().map(|i| i.name()).collect::<Vec<_>>(), vec!["file-179.txt"]);
    let adjustment = pane.views.visible_child().and_downcast::<gtk::ScrolledWindow>().unwrap().vadjustment();
    ensure!(adjustment.value() > 100.);
    Ok(())
}

fn test_split_panes_both_render_and_closed_split_is_released(f: &mut Fixture) -> Outcome {
    let page = f.window.current_page().unwrap();
    page.set_split(true, Some(&file(&f.source)));
    let right = page.right().unwrap();
    wait!(|| !right.loading());
    settle(0.1);
    ensure!(page.left.icons.model().is_some());
    ensure!(right.icons.model().is_some());
    let weak = Rc::downgrade(&right.0);
    drop(right);
    page.set_split(false, None);
    settle(0.2);
    ensure!(weak.upgrade().is_none(), "closed split pane retained");
    Ok(())
}

// ------------------------------------------------------ test_files_thumbnails

/// A JPEG/PNG fixture (Pillow in Python); `orientation` adds an EXIF APP1 segment.
fn make_image(path: &Path, width: i32, height: i32, orientation: Option<u16>) -> files_model::FileItem {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, width, height).unwrap();
    pixbuf.fill(0xbe4628ff);
    let png = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let bytes = pixbuf.save_to_bufferv(if png { "png" } else { "jpeg" }, &[]).unwrap();
    let bytes = match orientation {
        Some(value) if !png => {
            // TIFF little endian, IFD0 with one Orientation (0x0112) entry.
            let mut tiff = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            tiff.extend_from_slice(&value.to_le_bytes());
            tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
            let mut app1 = b"Exif\0\0".to_vec();
            app1.extend_from_slice(&tiff);
            let mut out = vec![0xff, 0xd8, 0xff, 0xe1];
            out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(&app1);
            out.extend_from_slice(&bytes[2..]);
            out
        }
        _ => bytes,
    };
    std::fs::write(path, bytes).unwrap();
    files_model::FileItem::query(&file(path), None::<&gio::Cancellable>).unwrap()
}

fn test_jpg_jpeg_png_render_in_all_folder_views_and_information(f: &mut Fixture) -> Outcome {
    let items: Vec<_> = ["photo.jpg", "photo.JPEG", "graphic.png"].iter().map(|n| make_image(&f.source.join(n), 320, 180, None)).collect();
    let pane = f.pane();
    wait!(|| pane.store.n_items() >= 6);
    let uris: Vec<String> = items.iter().map(|i| i.uri()).collect();
    for view in ["icons", "compact", "details"] {
        f.window.action(view);
        wait!(|| {
            let visible = f.pane().views.visible_child().unwrap();
            let tiles: Vec<FileTile> = tile::descendants(&visible).into_iter().filter_map(|w| w.downcast::<FileTile>().ok())
                .filter(|t| t.bound_uri().is_some_and(|u| uris.contains(&u))).collect();
            tiles.len() == uris.len() && tiles.iter().all(|t| t.picture().and_then(|p| p.paintable()).is_some_and(|p| p.is::<gdk::Texture>()))
        });
    }
    if !f.window.information.widget().is_visible() {
        f.window.action("information");
    }
    f.select("photo.jpg")?;
    wait!(|| f.window.information.picture.paintable().is_some_and(|p| p.is::<gdk::Texture>()));
    Ok(())
}

fn collect_textures() -> (Rc<RefCell<Vec<Option<gdk::Texture>>>>, wayfiles::thumbnails::Notify) {
    let results: Rc<RefCell<Vec<Option<gdk::Texture>>>> = Rc::default();
    let sink = results.clone();
    (results, Rc::new(move |texture| sink.borrow_mut().push(texture)))
}

fn test_large_visible_batch_does_not_drop_previews(f: &mut Fixture) -> Outcome {
    let service = wayfiles::thumbnails::Thumbnails::new();
    let (results, notify) = collect_textures();
    let mut cancels = Vec::new();
    for index in 0..40 {
        let item = make_image(&f.source.join(format!("photo-{index:02}.jpg")), 320, 180, None);
        cancels.push(service.request(&item, 64, notify.clone()));
    }
    wait!(10.0, || results.borrow().len() == 40);
    ensure!(results.borrow().iter().all(Option::is_some));
    service.close();
    Ok(())
}

fn test_broken_cached_thumbnail_falls_back_to_jpeg_original(f: &mut Fixture) -> Outcome {
    let service = wayfiles::thumbnails::Thumbnails::new();
    let (results, notify) = collect_textures();
    let item = make_image(&f.source.join("recover.jpg"), 320, 180, None);
    let broken = f.root.join("broken-cache.png");
    std::fs::write(&broken, b"incomplete cached thumbnail").unwrap();
    let key = wayfiles::thumbnails::Key { paths: vec![], size: 64, modified: item.modified(), usec: 0, nsec: 0, bytes: item.size() };
    let paths = vec![broken.to_string_lossy().into_owned(), item.path().unwrap().to_string_lossy().into_owned()];
    let _cancel = service.request_key(wayfiles::thumbnails::Key { paths: paths.clone(), ..key }, paths, 64, notify);
    wait!(|| !results.borrow().is_empty());
    ensure!(results.borrow()[0].is_some());
    service.close();
    Ok(())
}

fn test_invalid_cache_is_ignored_and_camera_orientation_is_respected(f: &mut Fixture) -> Outcome {
    let service = wayfiles::thumbnails::Thumbnails::new();
    let (results, notify) = collect_textures();
    // thumbnail_valid=False: only the original is a candidate (Thumbnails.request).
    let item = make_image(&f.source.join("portrait.jpeg"), 320, 180, Some(6));
    ensure!(!item.thumbnail_valid() || item.thumbnail_path().is_none());
    let _cancel = service.request(&item, 64, notify);
    wait!(|| !results.borrow().is_empty());
    let texture = results.borrow()[0].clone().ok_or("no texture")?;
    ensure_eq!((texture.width(), texture.height()), (36, 64));
    service.close();
    Ok(())
}

fn test_scrolled_away_requests_cancel_without_decoding_backlog(f: &mut Fixture) -> Outcome {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    let service = wayfiles::thumbnails::Thumbnails::new();
    let release = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let (r, c) = (release.clone(), calls.clone());
    service.set_decoder(Some(Arc::new(move |paths: &[String], size: i32| {
        c.fetch_add(1, Ordering::SeqCst);
        let end = Instant::now() + Duration::from_secs(5);
        while !r.load(Ordering::SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        wayfiles::thumbnails::decode(paths, size)
    })));
    let (results, notify) = collect_textures();
    let items: Vec<_> = (0..30).map(|i| make_image(&f.source.join(format!("cancel-{i:02}.jpg")), 320, 180, None)).collect();
    let mut handles: Vec<_> = items.iter().map(|item| service.request(item, 64, notify.clone())).collect();
    ensure_eq!(service.running_len(), 2);
    for handle in handles.drain(2..).flatten() {
        handle.cancel();
    }
    ensure_eq!(service.queued_len(), 0);
    release.store(true, Ordering::SeqCst);
    wait!(|| service.running_len() == 0);
    settle(0.1);
    ensure_eq!(results.borrow().len(), 2);
    ensure_eq!(calls.load(Ordering::SeqCst), 2);
    ensure!(results.borrow().iter().all(Option::is_some));
    service.close();
    Ok(())
}

fn test_corrupt_image_keeps_icon_and_does_not_cache_failed_decode(f: &mut Fixture) -> Outcome {
    let service = wayfiles::thumbnails::Thumbnails::new();
    let (results, notify) = collect_textures();
    let path = f.source.join("corrupt.jpg");
    std::fs::write(&path, b"not a JPEG image").unwrap();
    let item = files_model::FileItem::query(&file(&path), None::<&gio::Cancellable>).unwrap();
    let _cancel = service.request(&item, 64, notify);
    wait!(|| !results.borrow().is_empty());
    ensure!(results.borrow().len() == 1 && results.borrow()[0].is_none());
    ensure_eq!(service.cache_len(), 0);
    let pane = f.pane();
    wait!(|| tile::tile_for_uri(pane.icons.upcast_ref(), &item.uri()).is_some());
    let tile = tile::tile_for_uri(pane.icons.upcast_ref(), &item.uri()).unwrap();
    ensure!(tile.picture().unwrap().gicon().is_some());
    service.close();
    Ok(())
}

// --------------------------------------------------------- test_files_launch

struct LaunchDirs {
    left: PathBuf,
    right: PathBuf,
    third: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
    other: PathBuf,
}

fn launch_dirs(f: &mut Fixture) -> Result<LaunchDirs, String> {
    let dirs = LaunchDirs {
        left: f.root.join("Left folder with spaces"), right: f.root.join("Right folder"), third: f.root.join("Third folder"),
        alpha: f.root.join("Left folder with spaces/Alpha note.txt"), beta: f.root.join("Left folder with spaces/Beta note.txt"),
        other: f.root.join("Right folder/Different folder.txt"),
    };
    for folder in [&dirs.left, &dirs.right, &dirs.third] {
        std::fs::create_dir_all(folder).unwrap();
    }
    for path in [&dirs.alpha, &dirs.beta, &dirs.other] {
        write(path, "Read-only launch fixture\n");
    }
    wayfiles::bridge::pset(&f.app.preferences(), "restore_session", Value::Bool(false))?;
    // The launch tests start without a browser window.
    f.window.window.close();
    settle(0.05);
    Ok(dirs)
}

fn window_loaded(window: &FilesWindow, count: usize) -> bool {
    window.pages().len() == count && window.pages().iter().all(|p| p.panes().iter().all(|pane| !pane.loading()))
}

fn selected_uris(pane: &BrowserPane) -> std::collections::BTreeSet<String> {
    pane.selected().iter().map(|i| i.uri()).collect()
}

fn test_split_opens_one_tab_with_two_distinct_folders(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    let window = f.app.open_locations(vec![file(&d.left), file(&d.right)], true, false, true);
    wait!(|| window_loaded(&window, 1));
    let page = window.pages()[0].clone();
    ensure_eq!(page.left.current().path().unwrap(), d.left);
    ensure_eq!(page.right().ok_or("no split")?.current().path().unwrap(), d.right);
    ensure_eq!(f.browser_windows().len(), 1);
    Ok(())
}

fn test_reveal_scrolls_to_capture_in_all_presentations(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    for index in 0..260 {
        write(&d.left.join(format!("A file {index:04}.txt")), "fixture");
    }
    for (mode, suffix) in [("icons", ".png"), ("compact", ".mp4"), ("details", ".mkv")] {
        let target = d.left.join(format!("ZZZ capture{suffix}"));
        std::fs::write(&target, b"fixture").unwrap();
        wayfiles::bridge::pset_folder_options(&f.app.preferences(), &uri(&d.left), wayfiles::bridge::map(&[("view", Value::String(mode.into()))]))?;
        let window = f.app.open_locations(vec![file(&target)], true, true, false);
        wait!(|| window_loaded(&window, 1));
        let pane = window.active_pane().unwrap();
        ensure_eq!(selected_uris(&pane), [uri(&target)].into_iter().collect());
        let scroll = pane.views.visible_child().and_downcast::<gtk::ScrolledWindow>().unwrap();
        wait!(|| scroll.vadjustment().value() > 0.);
        window.window.close();
        settle(0.05);
    }
    Ok(())
}

fn test_reveal_groups_same_parent_without_losing_selected_items(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    let window = f.app.open_locations(vec![file(&d.alpha), file(&d.other), file(&d.beta)], true, true, false);
    wait!(|| window_loaded(&window, 2));
    let pages = window.pages();
    ensure_eq!(pages.iter().map(|p| p.left.current().path().unwrap()).collect::<Vec<_>>(), vec![d.left.clone(), d.right.clone()]);
    ensure_eq!(selected_uris(&pages[0].left), [uri(&d.alpha), uri(&d.beta)].into_iter().collect());
    ensure_eq!(selected_uris(&pages[1].left), [uri(&d.other)].into_iter().collect());
    Ok(())
}

fn test_split_reveal_keeps_independent_selections(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    let window = f.app.open_locations(vec![file(&d.alpha), file(&d.other)], true, true, true);
    wait!(|| window_loaded(&window, 1));
    let page = window.pages()[0].clone();
    ensure_eq!(selected_uris(&page.left), [uri(&d.alpha)].into_iter().collect());
    ensure_eq!(selected_uris(&page.right().unwrap()), [uri(&d.other)].into_iter().collect());
    Ok(())
}

fn test_async_classification_preserves_argument_order(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    let delays = vec![(d.left.clone(), 180u64), (d.right.clone(), 80), (d.third.clone(), 5)];
    let arrivals: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let sink = arrivals.clone();
    *f.app.hooks.classify_delay.borrow_mut() = Some(Rc::new(move |location: &gio::File| {
        let path = location.path().unwrap();
        let delay = delays.iter().find(|(p, _)| *p == path).map(|(_, d)| *d).unwrap_or(0);
        let (sink, path2) = (sink.clone(), path.clone());
        glib::timeout_add_local_once(Duration::from_millis(delay), move || sink.borrow_mut().push(path2));
        delay
    }));
    let window = f.app.open_locations(vec![file(&d.left), file(&d.right), file(&d.third)], true, false, false);
    wait!(|| window_loaded(&window, 3));
    ensure_eq!(*arrivals.borrow(), vec![d.third.clone(), d.right.clone(), d.left.clone()]);
    ensure_eq!(window.pages().iter().map(|p| p.left.current().path().unwrap()).collect::<Vec<_>>(), vec![d.left, d.right, d.third]);
    Ok(())
}

/// Run the real `wayfiles` binary as a secondary instance on the private bus.
fn invoke(f: &Fixture, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wayfiles")).args(args).current_dir(&f.root)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_secs(6);
    loop {
        settle(0.01);
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let output = child.wait_with_output().map_err(|e| e.to_string())?;
            ensure!(status.success(), "wayfiles {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("wayfiles {args:?} timed out"));
        }
    }
}

fn test_real_secondary_invocations_reuse_primary_and_preserve_arguments(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    invoke(f, &["--new-window", &d.left.to_string_lossy()])?;
    wait!(|| f.browser_windows().len() == 1 && window_loaded(&f.browser_windows()[0], 1));
    let original = f.browser_windows()[0].clone();
    ensure_eq!(original.pages()[0].left.current().path().unwrap(), d.left);
    // Relative arguments resolve from the invoking process's cwd.
    invoke(f, &["--select", "Left folder with spaces/Alpha note.txt", "Left folder with spaces/Beta note.txt"])?;
    wait!(|| window_loaded(&original, 2));
    ensure_eq!(f.browser_windows().len(), 1);
    ensure_eq!(selected_uris(&original.pages()[1].left), [uri(&d.alpha), uri(&d.beta)].into_iter().collect());
    invoke(f, &["--new-window", "--split", &d.right.to_string_lossy(), &d.third.to_string_lossy()])?;
    wait!(|| f.browser_windows().len() == 2 && f.browser_windows().iter().filter(|w| **w != original).all(|w| window_loaded(w, 1)));
    let second = f.browser_windows().into_iter().find(|w| *w != original).unwrap();
    ensure_eq!(second.pages()[0].left.current().path().unwrap(), d.right);
    ensure_eq!(second.pages()[0].right().unwrap().current().path().unwrap(), d.third);
    ensure!(!f.app.app.is_remote());
    Ok(())
}

fn test_dependency_check_does_not_open_a_window(f: &mut Fixture) -> Outcome {
    launch_dirs(f)?;
    let status: Value = serde_json::from_str(&invoke(f, &["--check"])?).map_err(|e| e.to_string())?;
    ensure_eq!(status["ready"], Value::Bool(true));
    ensure_eq!(status["gtk"], Value::from(4));
    ensure_eq!(status["kde_components"], Value::Bool(false));
    ensure_eq!(status["embedded_terminal"], Value::Bool(false));
    ensure!(f.browser_windows().is_empty());
    Ok(())
}

fn test_home_shortcut_opens_home_instead_of_reusing_current_folder(f: &mut Fixture) -> Outcome {
    let d = launch_dirs(f)?;
    invoke(f, &["--new-window", &d.left.to_string_lossy()])?;
    wait!(|| f.browser_windows().len() == 1 && window_loaded(&f.browser_windows()[0], 1));
    let window = f.browser_windows()[0].clone();
    invoke(f, &["--home"])?;
    wait!(|| window_loaded(&window, 2));
    ensure_eq!(window.current_page().unwrap().left.current().path().unwrap(), suite_core::paths::home());
    Ok(())
}

// ------------------------------------------------------------ test_file_dnd

fn test_external_sources_are_copy_only_and_choice_rules(_f: &mut Fixture) -> Outcome {
    use wayfiles::pane::{drop_acceptable, drop_actions, external_drop_choice, internal_drop_kind};
    let (copy, mv, link) = (gdk::DragAction::COPY, gdk::DragAction::MOVE, gdk::DragAction::LINK);
    ensure_eq!(drop_actions(false), copy);
    ensure!(drop_acceptable(copy | mv, drop_actions(false), true, false));
    ensure!(!drop_acceptable(mv, drop_actions(false), true, false));
    ensure!(!drop_acceptable(copy | mv, drop_actions(false), true, true));
    ensure!(!drop_acceptable(copy, drop_actions(false), false, false));
    ensure_eq!(drop_actions(true), copy | mv | link);
    for (action, name) in [(copy, "copy"), (mv, "move"), (link, "link")] {
        ensure_eq!(internal_drop_kind(action), name);
    }
    let (shift, control) = (gdk::ModifierType::SHIFT_MASK, gdk::ModifierType::CONTROL_MASK);
    let none = gdk::ModifierType::empty();
    ensure_eq!(external_drop_choice(copy | mv, none), "ask");
    ensure_eq!(external_drop_choice(copy | mv, shift), "move");
    ensure_eq!(external_drop_choice(copy | mv, control), "copy");
    ensure_eq!(external_drop_choice(copy | mv, shift | control), "link");
    ensure_eq!(external_drop_choice(copy, none), "copy");
    ensure_eq!(external_drop_choice(copy, shift), "copy");
    Ok(())
}

fn test_external_drop_menu_runs_the_chosen_operation(f: &mut Fixture) -> Outcome {
    let calls: Rc<RefCell<Vec<(String, Option<String>)>>> = Rc::default();
    let sink = calls.clone();
    *f.app.hooks.run_operation.borrow_mut() = Some(Rc::new(move |kind, _files, destination| {
        sink.borrow_mut().push((kind.to_string(), destination.and_then(|d| d.path()).map(|p| p.to_string_lossy().into_owned())));
        Some(Rc::new(files_widgets::jobs::JobEntry::new(0, "Fake", None, None)))
    }));
    let pane = f.pane();
    let both = gdk::DragAction::COPY | gdk::DragAction::MOVE;
    let view = pane.view_widget(None);
    let files = vec![file(&f.root.join("private-source"))];
    // A job is running elsewhere: drops are still accepted.
    ensure!(pane.drop_files(None, files.clone(), None, both, gdk::ModifierType::empty(), Some(view.clone()), 4., 4.));
    ensure!(calls.borrow().is_empty());
    wayfiles::pane::last_drop_menu().unwrap().choose(Some("move"));
    wait!(|| !calls.borrow().is_empty());
    ensure_eq!(calls.borrow()[0].clone(), ("move".to_string(), Some(f.source.to_string_lossy().into_owned())));
    ensure!(pane.drop_files(None, files.clone(), None, both, gdk::ModifierType::empty(), Some(view.clone()), 4., 4.));
    let menu = wayfiles::pane::last_drop_menu().unwrap();
    menu.choose(None);
    wait!(|| menu.popover.parent().is_none());
    ensure_eq!(calls.borrow().len(), 1);
    ensure!(pane.drop_files(None, files.clone(), None, both, gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK, Some(view), 4., 4.));
    ensure_eq!(calls.borrow().last().unwrap().0, "link");
    // A regular file row refuses drops; internal drops use the drag's action.
    let alpha = store_item(&pane, "Alpha.txt").unwrap();
    ensure!(!pane.drop_files(Some(&alpha), files.clone(), Some(gdk::DragAction::MOVE), both, gdk::ModifierType::empty(), None, 0., 0.));
    let documents = store_item(&pane, "Documents").unwrap();
    pane.drop_files(Some(&documents), files, Some(gdk::DragAction::MOVE), both, gdk::ModifierType::empty(), None, 0., 0.);
    ensure_eq!(calls.borrow().last().unwrap().clone(), ("move".to_string(), Some(f.source.join("Documents").to_string_lossy().into_owned())));
    Ok(())
}

fn test_item_press_suspends_rubberband_so_quick_drags_start(f: &mut Fixture) -> Outcome {
    let pane = f.pane();
    let tile = f.tile("Alpha.txt").ok_or("no tile")?;
    let controllers = tile.observe_controllers();
    let primary = (0..controllers.n_items()).filter_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
        .find(|g| g.button() == 1).ok_or("no primary gesture")?;
    for ending in ["released", "stopped"] {
        primary.emit_by_name::<()>("pressed", &[&1i32, &0f64, &0f64]);
        ensure!(!pane.icons.enables_rubberband() && !pane.compact.enables_rubberband() && !pane.details.enables_rubberband());
        if ending == "released" {
            primary.emit_by_name::<()>("released", &[&1i32, &0f64, &0f64]);
        } else {
            primary.emit_by_name::<()>("stopped", &[]);
        }
        ensure!(pane.icons.enables_rubberband() && pane.compact.enables_rubberband() && pane.details.enables_rubberband());
    }
    Ok(())
}

struct FakeDrop {
    values: std::collections::HashMap<&'static str, Vec<u8>>,
    finished: Rc<RefCell<Vec<gdk::DragAction>>>,
}

impl wayfiles::ark_drop::DropSource for FakeDrop {
    fn read(&self, mime: &'static str, _cancel: &gio::Cancellable, done: Box<dyn FnOnce(Result<(gio::InputStream, String), glib::Error>)>) {
        let bytes = glib::Bytes::from_owned(self.values[mime].clone());
        glib::idle_add_local_once(move || done(Ok((gio::MemoryInputStream::from_bytes(&bytes).upcast(), mime.to_string()))));
    }
    fn finish(&self, action: gdk::DragAction) {
        self.finished.borrow_mut().push(action);
    }
}

fn ark_request(values: Vec<(&'static str, Vec<u8>)>, destination: &str) -> Result<(Vec<gdk::DragAction>, Vec<String>), String> {
    let finished: Rc<RefCell<Vec<gdk::DragAction>>> = Rc::default();
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = errors.clone();
    let drop = FakeDrop { values: values.into_iter().collect(), finished: finished.clone() };
    let request = wayfiles::ark_drop::ExtractionDrop::new(Rc::new(drop), file(Path::new(destination)), Rc::new(move |e| sink.borrow_mut().push(e)));
    if !wait_for(3.0, || !finished.borrow().is_empty()) {
        request.finish(false, "");
        return Err("Ark drop did not complete".into());
    }
    let result = (finished.borrow().clone(), errors.borrow().clone());
    Ok(result)
}

fn test_ark_drop_negotiation_and_extraction_request(_f: &mut Fixture) -> Outcome {
    use wayfiles::ark_drop::{OBJECT, SERVICE};
    let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).map_err(|e| e.to_string())?;
    let info = gio::DBusNodeInfo::for_xml("<node><interface name='org.kde.ark.DndExtract'><method name='extractSelectedFilesTo'>\
        <arg name='destination' type='s' direction='in'/></method></interface></node>").unwrap();
    let calls: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = calls.clone();
    let registration = bus.register_object("/DndExtract/1", &info.lookup_interface("org.kde.ark.DndExtract").unwrap())
        .method_call(move |_, _, _, _, method, parameters, invocation| {
            sink.borrow_mut().push(format!("{method} {}", parameters.child_value(0).str().unwrap_or_default()));
            invocation.return_value(None);
        }).build().map_err(|e| e.to_string())?;
    let unique = bus.unique_name().unwrap().to_string();
    let result = ark_request(vec![(SERVICE, unique.into_bytes()), (OBJECT, b"/DndExtract/1".to_vec())], "/private/archive destination");
    let _ = bus.unregister_object(registration);
    let (finished, errors) = result?;
    ensure_eq!(finished, vec![gdk::DragAction::COPY]);
    ensure!(errors.is_empty(), "{errors:?}");
    ensure_eq!(*calls.borrow(), vec!["extractSelectedFilesTo /private/archive destination"]);
    for (service, path) in [(b"org.unrelated.Service".to_vec(), b"/DndExtract/1".to_vec()), (b":1.123".to_vec(), b"/Other/Path".to_vec()),
                            (vec![b'x'; 5000], b"/DndExtract/1".to_vec())] {
        let (finished, errors) = ark_request(vec![(SERVICE, service), (OBJECT, path)], "/private/x")?;
        ensure_eq!(finished, vec![gdk::DragAction::empty()]);
        ensure_eq!(errors.len(), 1);
    }
    Ok(())
}

// -------------------------------------------------- test_location_completion

fn enter(f: &Fixture, text: &str) {
    f.window.action("location");
    f.pane().location.set_text(text);
    f.pane().location.set_position(-1);
}

fn suggestions(f: &Fixture) -> Result<Vec<String>, String> {
    let completion = f.pane().location_completion();
    wait!(|| !completion.items.borrow().is_empty());
    settle(0.08);
    let items = completion.items.borrow().clone();
    Ok(items)
}

fn completion_focused(f: &Fixture) -> bool {
    gtk::prelude::GtkWindowExt::focus(&f.window.window).is_some_and(|w| w == *f.pane().location.upcast_ref::<gtk::Widget>() || w.is_ancestor(&f.pane().location))
}

fn test_hidden_prefix_offers_only_real_directories_including_symlinks(f: &mut Fixture) -> Outcome {
    std::fs::create_dir(f.source.join(".steam")).unwrap();
    std::os::unix::fs::symlink(&f.target, f.source.join(".steam library")).unwrap();
    write(&f.source.join(".steps.txt"), "not a directory");
    std::os::unix::fs::symlink(f.source.join("absent"), f.source.join(".steam-broken")).unwrap();
    std::fs::create_dir(f.source.join(".something")).unwrap();
    ensure!(!f.pane().option_bool("show_hidden"));
    enter(f, &format!("{}/.ste", f.source.display()));
    ensure_eq!(suggestions(f)?, vec![format!("{}/.steam/", f.source.display()), format!("{}/.steam library/", f.source.display())]);
    ensure!(completion_focused(f), "Suggestions must not steal typing focus");
    Ok(())
}

fn test_tab_completes_a_path_and_arrow_enter_opens_a_child(f: &mut Fixture) -> Outcome {
    std::fs::create_dir_all(f.source.join(".steam/Games")).unwrap();
    std::fs::create_dir(f.source.join(".steam-library")).unwrap();
    enter(f, &format!("{}/.ste", f.source.display()));
    suggestions(f)?;
    f.key("Tab")?;
    ensure_eq!(f.pane().location.text().to_string(), format!("{}/.steam/", f.source.display()));
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    ensure_eq!(suggestions(f)?, vec![format!("{}/.steam/Games/", f.source.display())]);
    f.key("Down")?;
    f.key("Return")?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == f.source.join(".steam/Games"));
    ensure!(!f.pane().location_completion().popover.is_visible());
    Ok(())
}

fn test_mouse_selects_folder_without_losing_the_edited_address(f: &mut Fixture) -> Outcome {
    let destination = f.source.join("Folder with spaces");
    std::fs::create_dir(&destination).unwrap();
    f.maximize()?;
    enter(f, &format!("{}/Folder w", f.source.display()));
    suggestions(f)?;
    let row = f.pane().location_completion().rows.row_at_index(0).unwrap();
    f.pointer_click(&row, "left", false)?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == destination);
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    Ok(())
}

fn test_fast_tab_waits_for_completion_instead_of_leaving_the_address(f: &mut Fixture) -> Outcome {
    enter(f, "Doc");
    ensure!(f.pane().location_completion().items.borrow().is_empty());
    f.key("Tab")?;
    wait!(|| f.pane().location.text() == "Documents/");
    ensure!(completion_focused(f));
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    Ok(())
}

fn test_escape_then_escape_dismisses_suggestions_then_the_editor(f: &mut Fixture) -> Outcome {
    enter(f, &format!("{}/Doc", f.source.display()));
    suggestions(f)?;
    f.key("Escape")?;
    ensure!(!f.pane().location_completion().popover.is_visible());
    ensure_eq!(f.pane().location.text().to_string(), format!("{}/Doc", f.source.display()));
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("entry"));
    f.key("Escape")?;
    ensure_eq!(f.pane().location_stack.visible_child_name().as_deref(), Some("breadcrumbs"));
    ensure_eq!(f.pane().current().path().unwrap(), f.source);
    Ok(())
}

fn test_relative_paths_and_uri_escaping_navigate_to_the_suggested_folder(f: &mut Fixture) -> Outcome {
    use files_model::completion::{folder_query, location_file};
    let name = "My folder #1";
    let destination = f.source.join(name);
    std::fs::create_dir(&destination).unwrap();
    enter(f, "My f");
    ensure_eq!(suggestions(f)?, vec![format!("{name}/")]);
    f.key("Down")?;
    f.key("Return")?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == destination);
    f.pane().navigate(&file(&f.source), true, None);
    wait!(|| f.loaded());
    enter(f, &format!("{}/My%20f", uri(&f.source)));
    ensure_eq!(suggestions(f)?, vec![format!("{}/", uri(&destination))]);
    f.key("Tab")?;
    f.key("Return")?;
    wait!(|| f.loaded() && f.pane().current().path().unwrap() == destination);
    let query = folder_query("~/Doc", &f.pane().current()).ok_or("no query")?;
    ensure_eq!(query.parent.path().unwrap(), suite_core::paths::home());
    ensure_eq!(query.complete("Documents"), "~/Documents/");
    ensure_eq!(location_file("../Documents", &f.pane().current()).path().unwrap(), f.source.join("Documents"));
    ensure!(folder_query("smb://server", &f.pane().current()).is_none());
    Ok(())
}

fn test_stale_lookup_invalid_path_and_tab_switch_never_restore_old_suggestions(f: &mut Fixture) -> Outcome {
    enter(f, &format!("{}/Doc", f.source.display()));
    settle(0.3);
    f.pane().location.set_text(&format!("{}/absent/Doc", f.source.display()));
    settle(0.3);
    let completion = f.pane().location_completion();
    ensure!(completion.items.borrow().is_empty());
    ensure!(!completion.popover.is_visible());
    enter(f, &format!("{}/Doc", f.source.display()));
    suggestions(f)?;
    let old = f.pane().location_completion();
    f.window.new_tab(Some(&file(&f.target)), None);
    wait!(|| f.loaded());
    ensure!(!old.popover.is_visible());
    ensure!(old.items.borrow().is_empty());
    Ok(())
}

// --------------------------------------------- test_files_information (window)

fn show_preview(f: &Fixture) -> Outcome {
    if !f.window.information.widget().is_visible() {
        f.window.information_toggle.emit_clicked();
    }
    ensure!(f.window.information.widget().is_visible());
    ensure!(f.window.information_toggle.is_active());
    Ok(())
}

fn test_both_panes_and_preview_width_restore_after_reopening(f: &mut Fixture) -> Outcome {
    show_preview(f)?;
    settle(0.15);
    let split = f.window.content_split.clone();
    split.set_position(split.width() - 420 - 5);
    settle(0.4);
    let width = wayfiles::bridge::pget(&f.app.preferences(), "information_width").as_i64().unwrap_or(0);
    ensure!((width - 420).abs() <= 3, "saved width {width}");
    f.window.sidebar_toggle.emit_clicked();
    ensure!(!f.window.sidebar_scroll.is_visible());
    ensure!(!f.window.sidebar_toggle.is_active());
    f.window.information.collapse.emit_clicked();
    ensure!(!f.window.information.widget().is_visible());
    f.window.window.close();
    let prefs = FilePreferences::new(Some(f.root.join("prefs.json")));
    ensure_eq!(prefs.get("show_sidebar"), Value::Bool(false));
    ensure_eq!(prefs.get("show_information"), Value::Bool(false));
    f.app.set_preferences(prefs);
    f.fixture_window()?;
    ensure!(!f.window.sidebar_scroll.is_visible());
    ensure!(!f.window.information.widget().is_visible());
    f.window.sidebar_toggle.emit_clicked();
    show_preview(f)?;
    settle(0.3);
    ensure!((f.window.information.widget().width() - 420).abs() <= 3, "width {}", f.window.information.widget().width());
    f.window.window.close();
    f.app.set_preferences(FilePreferences::new(Some(f.root.join("prefs.json"))));
    f.fixture_window()?;
    settle(0.3);
    ensure!(f.window.sidebar_scroll.is_visible());
    ensure!(f.window.information.widget().is_visible());
    ensure!((f.window.information.widget().width() - 420).abs() <= 3, "width {}", f.window.information.widget().width());
    Ok(())
}

fn test_selection_title_follows_folder_and_multi_selection(f: &mut Fixture) -> Outcome {
    show_preview(f)?;
    let panel = f.window.information.clone();
    f.select("Alpha.txt")?;
    ensure_eq!(panel.title.text().as_str(), "Alpha.txt");
    f.pane().selection().unselect_all();
    ensure_eq!(panel.title.text().as_str(), f.source.file_name().unwrap().to_string_lossy());
    f.window.action("select-all");
    ensure!(panel.title.text().contains("items"));
    ensure_eq!(panel.rows().iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), vec!["Total size"]);
    Ok(())
}

// ---------------------------------------------------------- test_git_refresh

fn test_git_status_labels_appear_and_refresh_is_throttled(f: &mut Fixture) -> Outcome {
    if suite_core::shell::which("git").is_none() {
        println!("  (skipped: git not installed)");
        return Ok(());
    }
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(&f.source).args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").output().map(|o| o.status.success()).unwrap_or(false);
    ensure!(git(&["init", "-q"]));
    ensure!(git(&["-c", "user.name=t", "-c", "user.email=t@example.invalid", "add", "Alpha.txt"]));
    ensure!(git(&["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "x"]));
    let pane = f.pane();
    pane.refresh();
    wait!(|| f.loaded());
    wait!(|| !pane.git_status().is_empty());
    ensure!(pane.git_column().unwrap().is_visible());
    let beta = pane.git_status().get(&f.source.join("Beta.txt").to_string_lossy().into_owned()).cloned();
    ensure!(beta.is_some(), "untracked file has no label: {:?}", pane.git_status());
    // A burst of writes schedules at most one refresh, ≥ 2 s after the last run.
    let factories = pane.icons.factory();
    write(&f.source.join("Alpha.txt"), "changed\n");
    let alpha = f.source.join("Alpha.txt").to_string_lossy().into_owned();
    wait!(6.0, || pane.git_status().get(&alpha).is_some());
    ensure!(pane.icons.factory() == factories);
    Ok(())
}
