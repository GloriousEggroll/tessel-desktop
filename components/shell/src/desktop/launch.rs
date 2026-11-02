//! Launching from the desktop, outside tessel-shell's cgroup:
//! - tessel-shell's `labwc_launch_environment()` (tessel-shell:110-185), and
//! - the launch half of tessel-dock/app_catalog.py (`launch_command` systemd-run
//!   argv, `_launch_environment`, `_launch_separately`, `_start_service`,
//!   `launch_process`, `launch_desktop_file`, `launch_uri`, `AppCatalog.launch`,
//!   `_TransferredLaunchContext`, `_run_launch_helper`).
//!
//! The in-service helper is this executable itself: `[exe, mode, target]`
//! (Python: `[python3, app_catalog.py, mode, target]`); `main` must dispatch
//! `<exe> --launch-desktop|--launch-file|--open-uri <target>` to
//! [`helper_main`] before any GTK initialisation.

use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type Environment = BTreeMap<String, String>;
pub type OnDone = Box<dyn FnOnce(Option<String>)>;

pub const HELPER_MODES: [&str; 3] = ["--launch-desktop", "--launch-file", "--open-uri"];

/// Execution metadata belongs to tessel-shell's service, not to a newly started app.
pub const SERVICE_ENV: [&str; 19] = [
    "INVOCATION_ID", "JOURNAL_STREAM", "NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC", "WATCHDOG_DEVICE", "LISTEN_PID",
    "LISTEN_FDS", "LISTEN_FDNAMES", "SYSTEMD_EXEC_PID", "MAINPID", "MANAGERPID", "PIDFILE", "EXIT_CODE", "EXIT_STATUS",
    "SERVICE_RESULT", "GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID", "WAYDOCK_LAUNCH_STARTUP_ID",
];

pub const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";

thread_local! {
    static SYSTEMD_RUN_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static HELPER_OVERRIDE: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Tests only: spawn this program instead of `/usr/bin/systemd-run` (the argv
/// keeps the real path as its first element only in [`launch_command`]).
pub fn set_systemd_run_path(path: Option<PathBuf>) {
    SYSTEMD_RUN_OVERRIDE.with(|cell| cell.replace(path));
}

/// Tests only: helper prefix instead of `[current_exe]`.
pub fn set_helper_prefix(prefix: Option<Vec<String>>) {
    HELPER_OVERRIDE.with(|cell| cell.replace(prefix));
}

// ---------------------------------------------------------------- labwc env

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '{' || c == '}'
}

/// Expand a Labwc environment value using Labwc's parsing rules.
pub fn expand_labwc_value(value: &str, environment: &Environment) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut expanded = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$' && index + 1 < chars.len() && is_name_char(chars[index + 1]) {
            let mut end = index + 1;
            while end < chars.len() && is_name_char(chars[end]) {
                end += 1;
            }
            let mut name: String = chars[index + 1..end].iter().collect();
            if name.starts_with('{') && name.ends_with('}') && name.chars().count() >= 2 {
                name = name[1..name.len() - 1].to_owned();
            }
            expanded.push_str(environment.get(&name).map(String::as_str).unwrap_or(""));
            index = end;
            continue;
        }
        expanded.push(chars[index]);
        index += 1;
    }
    // Labwc expands every tilde, not only one at the start of a path.
    expanded.replace('~', environment.get("HOME").map(String::as_str).unwrap_or(""))
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

pub fn read_labwc_environment_file(path: &Path, environment: &mut Environment) {
    // A missing/unreadable file has the same effect as no overrides.
    let Ok(bytes) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let (name, value) = line.split_once('=').unwrap();
        let (name, value) = (py_strip(name), py_strip(value));
        if name.is_empty() || name.contains('\0') {
            continue;
        }
        // Match Labwc's recursion guard before expanding the value.
        if value.len() > 1024 {
            continue;
        }
        let expanded = expand_labwc_value(value, environment);
        environment.insert(name.to_owned(), expanded);
    }
}

fn expanduser(path: &str, environment: &Environment) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        let home = environment.get("HOME").cloned().unwrap_or_else(|| tessel_core::paths::home().to_string_lossy().into_owned());
        return PathBuf::from(format!("{home}{}", &path[1..]));
    }
    PathBuf::from(path)
}

pub fn process_environment() -> Environment {
    std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
}

/// A fresh child environment from Labwc's current config files.
pub fn labwc_launch_environment() -> Environment {
    labwc_environment_from(process_environment())
}

pub fn labwc_environment_from(mut environment: Environment) -> Environment {
    let config_home = match environment.get("XDG_CONFIG_HOME") {
        Some(value) => expanduser(value, &environment),
        None => tessel_core::paths::home().join(".config"),
    };
    let environment_file = config_home.join("labwc").join("environment");
    if environment_file.is_file() {
        read_labwc_environment_file(&environment_file, &mut environment);
    }
    let directory = PathBuf::from(format!("{}.d", environment_file.display()));
    if directory.is_dir() {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
            .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".env"))).collect())
            .unwrap_or_default();
        paths.sort();
        for path in paths {
            if path.is_file() {
                read_labwc_environment_file(&path, &mut environment);
            }
        }
    }
    environment
}

/// `apply_launch_environment(context, environment)`.
pub fn apply_launch_environment(context: &impl IsA<gio::AppLaunchContext>, environment: &Environment) {
    for (name, value) in environment {
        context.setenv(name, value);
    }
}

/// `shutil.which(name, path=PATH)`.
pub fn which_in(name: &str, path: Option<&str>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let candidate = PathBuf::from(name);
        return tessel_core::shell::is_executable(&candidate).then_some(candidate);
    }
    let path = path.map(str::to_owned).or_else(|| std::env::var("PATH").ok()).unwrap_or_else(|| "/bin:/usr/bin".into());
    path.split(':').map(|dir| Path::new(if dir.is_empty() { "." } else { dir }).join(name)).find(|c| tessel_core::shell::is_executable(c))
}

// -------------------------------------------------------------- systemd-run

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `app-tessel-dock-<uuid4 hex>.service`.
pub fn unit_name() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: fills the local buffer.
    unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!("app-tessel-dock-{}.service", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Build a separate app service; never tie its lifetime to tessel-shell.service.
pub fn launch_command(helper: &[String], environment: &Environment, unit: Option<&str>, service_type: &str, cwd: Option<&str>) -> Vec<String> {
    let unit = unit.map(str::to_owned).unwrap_or_else(unit_name);
    let mut command: Vec<String> = [SYSTEMD_RUN, "--user", "--collect", "--quiet"].iter().map(|s| s.to_string()).collect();
    command.push(format!("--service-type={service_type}"));
    command.extend(["--property=GuessMainPID=no", "--property=ExitType=cgroup", "--property=TimeoutStartSec=90s", "--slice=app.slice", "--expand-environment=no"].map(String::from));
    command.push(format!("--unit={unit}"));
    if let Some(cwd) = cwd {
        command.push(format!("--working-directory={cwd}"));
    }
    command.extend(environment.keys().filter(|k| valid_env_name(k)).map(|k| format!("--setenv={k}")));
    command.push("--".into());
    command.extend(helper.iter().cloned());
    command
}

fn without_service_env(environment: impl IntoIterator<Item = (String, String)>) -> Environment {
    environment.into_iter().filter(|(k, _)| !SERVICE_ENV.contains(&k.as_str())).collect()
}

/// `_start_service`: spawn systemd-run with exactly `environment`, report its result.
pub fn start_service(command: &[String], environment: &Environment, complete: OnDone) -> Result<gio::Subprocess, String> {
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE);
    let environ: Vec<std::ffi::OsString> = environment.iter().map(|(k, v)| format!("{k}={v}").into()).collect();
    launcher.set_environ(&environ);
    let mut argv: Vec<std::ffi::OsString> = command.iter().map(Into::into).collect();
    if argv.first().is_some_and(|a| a == SYSTEMD_RUN) {
        if let Some(path) = SYSTEMD_RUN_OVERRIDE.with(|c| c.borrow().clone()) {
            argv[0] = path.into();
        }
    }
    let process = launcher.spawn(&argv.iter().map(|a| a.as_os_str()).collect::<Vec<_>>()).map_err(|e| tessel_core::pyerr::python_error_text(&e))?;
    let watched = process.clone();
    // Do not cancel this on shutdown: the service belongs to the app.
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| match result {
        Ok((_stdout, stderr)) => {
            if watched.is_successful() {
                complete(None)
            } else {
                let detail = stderr.map(|s| s.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "The application service could not start.".into());
                complete(Some(format!("Could not launch the application: {}", detail.trim())))
            }
        }
        Err(error) => complete(Some(format!("Could not launch the application: {}", tessel_core::pyerr::python_error_text(&error)))),
    });
    Ok(process)
}

/// `launch_process(argv, environment=, cwd=, on_done=)`: a command outside
/// the calling desktop service's cgroup.
pub fn launch_process(argv: &[String], environment: Option<&Environment>, cwd: Option<&str>, on_done: Option<OnDone>) -> Result<gio::Subprocess, String> {
    if argv.is_empty() {
        return Err("An application command is required.".into());
    }
    let environment = without_service_env(environment.cloned().unwrap_or_else(process_environment));
    let command = launch_command(argv, &environment, None, "exec", cwd);
    start_service(&command, &environment, on_done.unwrap_or_else(|| Box::new(|_| {})))
}

/// `_launch_environment(context, info, files)` -> (environment, startup token).
pub fn launch_environment(context: Option<&gio::AppLaunchContext>, info: Option<&gio::AppInfo>, files: &[gio::File]) -> (Environment, Option<String>) {
    let base: Environment = match context {
        None => process_environment(),
        Some(context) => context
            .environment()
            .iter()
            .filter_map(|item| item.to_str().and_then(|s| s.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect(),
    };
    let mut environment = without_service_env(base);
    let mut token = None;
    if let (Some(context), Some(info)) = (context, info) {
        // GDK's Wayland launch context answers get_display() with the Wayland
        // socket; that must never overwrite DISPLAY (Xwayland).
        let is_wayland = context.find_property("display").is_some_and(|_| {
            context.property_value("display").get::<Option<glib::Object>>().ok().flatten().is_some_and(|d| d.type_().name().starts_with("GdkWayland"))
        });
        if !is_wayland {
            if let Some(display) = context.display(info, files) {
                if !display.is_empty() {
                    environment.insert("DISPLAY".into(), display.to_string());
                }
            }
        }
        let startup_notify = info.downcast_ref::<gio_unix::DesktopAppInfo>().is_some_and(|d| d.boolean("StartupNotify"));
        if startup_notify {
            token = context.startup_notify_id(Some(info), files).map(|t| t.to_string()).filter(|t| !t.is_empty());
        }
    }
    if let Some(token) = &token {
        environment.insert("WAYDOCK_LAUNCH_STARTUP_ID".into(), token.clone());
    }
    (environment, token)
}

pub fn helper_argv(mode: &str, target: &str) -> Vec<String> {
    let mut argv = HELPER_OVERRIDE.with(|c| c.borrow().clone()).unwrap_or_else(|| {
        vec![std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "tessel-shell".into())]
    });
    argv.push(mode.into());
    argv.push(target.into());
    argv
}

/// `_launch_separately(mode, target, info, context, on_done)`.
pub fn launch_separately(mode: &str, target: &str, info: Option<&gio::AppInfo>, context: Option<&gio::AppLaunchContext>, on_done: Option<OnDone>) -> Result<gio::Subprocess, String> {
    let files: Vec<gio::File> = if mode == "--open-uri" { vec![gio::File::for_uri(target)] } else { vec![] };
    let (environment, token) = launch_environment(context, info, &files);
    let command = launch_command(&helper_argv(mode, target), &environment, None, "forking", None);
    let failed_context = context.cloned();
    let failed_token = token.clone();
    let complete: OnDone = Box::new(move |error| {
        if error.is_some() {
            if let (Some(token), Some(context)) = (&failed_token, &failed_context) {
                context.launch_failed(token);
            }
        }
        if let Some(on_done) = on_done {
            on_done(error);
        }
    });
    start_service(&command, &environment, complete).inspect_err(|_| {
        if let (Some(token), Some(context)) = (&token, context) {
            context.launch_failed(token);
        }
    })
}

/// `launch_desktop_file(path, context, on_done)`.
pub fn launch_desktop_file(path: &str, context: Option<&gio::AppLaunchContext>, on_done: Option<OnDone>) -> Result<gio::Subprocess, String> {
    let info = gio_unix::DesktopAppInfo::from_filename(path).ok_or_else(|| "That desktop file is not a valid application.".to_owned())?;
    launch_separately("--launch-file", path, Some(info.upcast_ref()), context, on_done)
}

/// `launch_uri(uri, context, on_done)`: open Home/Trash/files through Gio.
pub fn launch_uri(uri: &str, context: Option<&gio::AppLaunchContext>, on_done: Option<OnDone>) -> Result<gio::Subprocess, String> {
    let scheme = glib::Uri::peek_scheme(uri).map(|s| s.to_string()).unwrap_or_default();
    let info = gio::AppInfo::default_for_uri_scheme(&scheme).or_else(|| gio::AppInfo::default_for_type("inode/directory", true));
    launch_separately("--open-uri", uri, info.as_ref(), context, on_done)
}

/// `AppCatalog().launch(desktop_id, context, on_done)`.
pub fn launch_desktop_id(catalog: &crate::app_catalog::AppCatalog, desktop_id: &str, context: Option<&gio::AppLaunchContext>, on_done: Option<OnDone>) -> Result<gio::Subprocess, String> {
    let entry = catalog.get(desktop_id).ok_or_else(|| "That application is no longer installed.".to_owned())?;
    launch_separately("--launch-desktop", &entry.id, entry.info.as_ref(), context, on_done)
}

// ------------------------------------------------------------ launch helper

mod imp {
    use gio::subclass::prelude::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct TransferredLaunchContext {
        pub token: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TransferredLaunchContext {
        const NAME: &'static str = "WayshellTransferredLaunchContext";
        type Type = super::TransferredLaunchContext;
        type ParentType = gio::AppLaunchContext;
    }

    impl ObjectImpl for TransferredLaunchContext {}
}

glib::wrapper! {
    /// `_TransferredLaunchContext`: carries the original GDK startup token to
    /// both Exec and D-Bus launches; DISPLAY stays X11's address.
    pub struct TransferredLaunchContext(ObjectSubclass<imp::TransferredLaunchContext>) @extends gio::AppLaunchContext;
}

unsafe impl glib::subclass::types::IsSubclassable<imp::TransferredLaunchContext> for gio::AppLaunchContext {
    fn class_init(class: &mut glib::Class<Self>) {
        use glib::subclass::types::IsSubclassableExt;
        Self::parent_class_init::<imp::TransferredLaunchContext>(class);
        let klass = class.as_mut();
        klass.get_display = Some(get_display);
        klass.get_startup_notify_id = Some(get_startup_notify_id);
    }
}

unsafe fn strdup_opt(value: Option<String>) -> *mut std::os::raw::c_char {
    match value.and_then(|v| std::ffi::CString::new(v).ok()) {
        Some(text) => unsafe { glib::ffi::g_strdup(text.as_ptr()) },
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn get_display(context: *mut gio::ffi::GAppLaunchContext, _info: *mut gio::ffi::GAppInfo, _files: *mut glib::ffi::GList) -> *mut std::os::raw::c_char {
    use glib::translate::FromGlibPtrNone;
    let context: gio::AppLaunchContext = unsafe { gio::AppLaunchContext::from_glib_none(context) };
    let display = context.environment().iter().filter_map(|item| item.to_str()).find_map(|item| item.strip_prefix("DISPLAY=").map(str::to_owned));
    unsafe { strdup_opt(display) }
}

unsafe extern "C" fn get_startup_notify_id(context: *mut gio::ffi::GAppLaunchContext, _info: *mut gio::ffi::GAppInfo, _files: *mut glib::ffi::GList) -> *mut std::os::raw::c_char {
    use glib::subclass::prelude::ObjectSubclassIsExt;
    use glib::translate::FromGlibPtrNone;
    let context: gio::AppLaunchContext = unsafe { gio::AppLaunchContext::from_glib_none(context) };
    let token = context.downcast_ref::<TransferredLaunchContext>().and_then(|c| c.imp().token.borrow().clone());
    unsafe { strdup_opt(token) }
}

impl TransferredLaunchContext {
    pub fn new(token: Option<String>) -> Self {
        let context: Self = glib::Object::new();
        use glib::subclass::prelude::ObjectSubclassIsExt;
        context.imp().token.replace(token);
        context
    }
}

/// `_run_launch_helper(mode, target)`; Err is the message Python prints.
pub fn run_launch_helper(mode: &str, target: &str) -> Result<(), String> {
    let token = std::env::var("WAYDOCK_LAUNCH_STARTUP_ID").ok();
    #[allow(unused_unsafe)]
    // SAFETY: the helper is single-threaded at this point.
    unsafe {
        std::env::remove_var("WAYDOCK_LAUNCH_STARTUP_ID")
    };
    let context = TransferredLaunchContext::new(token);
    let main_loop = glib::MainLoop::new(Some(&glib::MainContext::ref_thread_default()), false);
    let failure: std::rc::Rc<RefCell<Option<Option<String>>>> = Default::default();
    let finished = {
        let failure = failure.clone();
        let main_loop = main_loop.clone();
        move |result: Result<(), glib::Error>| {
            failure.replace(Some(result.err().map(|e| tessel_core::pyerr::python_error_text(&e))));
            main_loop.quit();
        }
    };
    match mode {
        "--launch-desktop" | "--launch-file" => {
            let info = if mode == "--launch-file" { gio_unix::DesktopAppInfo::from_filename(target) } else { gio_unix::DesktopAppInfo::new(target) };
            let info = info.ok_or_else(|| "That application is no longer installed.".to_owned())?;
            info.launch_uris_async(&[], Some(&context), gio::Cancellable::NONE, finished);
        }
        "--open-uri" => gio::AppInfo::launch_default_for_uri_async(target, Some(&context), gio::Cancellable::NONE, finished),
        _ => return Err("Unknown launch operation.".into()),
    }
    if failure.borrow().is_none() {
        main_loop.run();
    }
    match failure.take().flatten() {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Entry point for `<exe> <mode> <target>` inside the app service.
/// `args` excludes the program name. Returns the exit code.
pub fn helper_main(args: &[String]) -> i32 {
    let result = if args.len() != 2 { Err("Expected a launch operation and its target.".to_owned()) } else { run_launch_helper(&args[0], &args[1]) };
    match result {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("Tessel Dock launch: {message}");
            1
        }
    }
}

/// True when argv (without program name) is a launch-helper invocation.
pub fn is_helper_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|a| HELPER_MODES.contains(&a.as_str()))
}

// ------------------------------------------------------------- terminal

/// Preferred terminal order (tessel-shell:1085).
pub fn terminal_candidates(environment: &Environment) -> Vec<String> {
    let mut list = vec![environment.get("TERMINAL").cloned().unwrap_or_default()];
    list.extend(
        ["xdg-terminal", "ptyxis", "kgx", "gnome-terminal", "konsole", "xfce4-terminal", "tilix", "alacritty", "kitty", "wezterm", "xterm", "ghostty"]
            .map(String::from),
    );
    list
}

/// Per-app working-directory flags.
pub fn terminal_command(name: &str, cwd: &str) -> Vec<String> {
    let v = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match name {
        "xdg-terminal" => v(&[name, "--working-directory", cwd]),
        "ptyxis" => v(&[name, "--new-window", "--working-directory", cwd]),
        "ghostty" => v(&[name, "+new-window"]),
        "kgx" | "gnome-terminal" | "xfce4-terminal" | "tilix" | "alacritty" => v(&[name, "--working-directory", cwd]),
        "konsole" => v(&[name, "--workdir", cwd]),
        "kitty" => v(&[name, "--directory", cwd]),
        "wezterm" => v(&[name, "start", "--cwd", cwd]),
        _ => v(&[name]),
    }
}

/// What `open_default_terminal` resolves: (argv, cwd) or None (no terminal).
pub fn resolve_terminal(environment: &Environment, cwd: &str) -> Option<(Vec<String>, PathBuf)> {
    let path = environment.get("PATH").map(String::as_str);
    terminal_candidates(environment).into_iter().filter(|c| !c.is_empty()).find_map(|candidate| {
        let exe = which_in(&candidate, path)?;
        Some((terminal_command(&candidate, cwd), exe))
    })
}

/// `open_default_terminal(cwd)`: Err(()) means "No terminal app was found."
pub fn open_default_terminal(environment: &Environment, cwd: &str, on_done: impl Fn(Option<String>) + 'static) -> Result<(), ()> {
    let on_done = std::rc::Rc::new(on_done);
    let path = environment.get("PATH").map(String::as_str);
    for candidate in terminal_candidates(environment) {
        if candidate.is_empty() {
            continue;
        }
        let Some(exe) = which_in(&candidate, path) else { continue };
        let done = on_done.clone();
        if launch_process(&terminal_command(&candidate, cwd), Some(environment), Some(cwd), Some(Box::new(move |e| done(e)))).is_ok() {
            return Ok(());
        }
        let done = on_done.clone();
        if launch_process(&[exe.to_string_lossy().into_owned()], Some(environment), Some(cwd), Some(Box::new(move |e| done(e)))).is_ok() {
            return Ok(());
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Environment {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn labwc_values_expand_like_labwc() {
        let e = env(&[("HOME", "/h"), ("A", "x"), ("B_C", "y")]);
        assert_eq!(expand_labwc_value("$A/${B_C}/$MISSING/~/a~b", &e), "x/y///h/a/hb"); // verified against the Python function
        assert_eq!(expand_labwc_value("cost $ 5 $", &e), "cost $ 5 $");
        assert_eq!(expand_labwc_value("$A-$A", &e), "x-x");
        // `{}` are name characters: "${A}x" reads name "{A}x" (no stripping).
        assert_eq!(expand_labwc_value("${A}x", &e), "");
    }

    #[test]
    fn labwc_files_parse_and_override_in_order() {
        let root = std::env::temp_dir().join(format!("ws-desk-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("labwc/environment.d")).unwrap();
        std::fs::write(root.join("labwc/environment"), "# comment\nXKB=us\n FOO = bar \nnoequals\n=empty\nPATHX=$HOME/bin:~/x\r\nLONG=".to_owned() + &"a".repeat(1025) + "\n").unwrap();
        std::fs::write(root.join("labwc/environment.d/20-b.env"), "FOO=second\n").unwrap();
        std::fs::write(root.join("labwc/environment.d/10-a.env"), "FOO=first\nNEW=$FOO\n").unwrap();
        std::fs::write(root.join("labwc/environment.d/ignored.txt"), "FOO=never\n").unwrap();
        let result = labwc_environment_from(env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", root.to_str().unwrap()), ("LONG", "kept")]));
        assert_eq!(result["XKB"], "us");
        assert_eq!(result["FOO"], "second");
        assert_eq!(result["NEW"], "first");
        assert_eq!(result["PATHX"], "/home/u/bin:/home/u/x");
        assert_eq!(result["LONG"], "kept", "values over 1024 bytes are skipped");
        assert!(!result.contains_key(""));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn command_matches_tessel_dock_launcher_exactly() {
        let environment = without_service_env(env(&[("WAYLAND_DISPLAY", "wayland-1"), ("INVOCATION_ID", "x"), ("BAD-NAME", "y"), ("A_1", "z")]));
        let command = launch_command(&["/x/tessel-shell".into(), "--launch-file".into(), "/d/a.desktop".into()], &environment, Some("app-tessel-dock-0.service"), "forking", None);
        assert_eq!(command.join(" "), "/usr/bin/systemd-run --user --collect --quiet --service-type=forking --property=GuessMainPID=no --property=ExitType=cgroup --property=TimeoutStartSec=90s --slice=app.slice --expand-environment=no --unit=app-tessel-dock-0.service --setenv=A_1 --setenv=WAYLAND_DISPLAY -- /x/tessel-shell --launch-file /d/a.desktop");
        let exec = launch_command(&["ptyxis".into()], &Environment::new(), Some("u.service"), "exec", Some("/home/u"));
        assert_eq!(exec[4], "--service-type=exec");
        assert_eq!(exec[11], "--working-directory=/home/u");
        let unit = unit_name();
        assert!(unit.starts_with("app-tessel-dock-") && unit.ends_with(".service") && unit.len() == "app-tessel-dock-.service".len() + 32);
        assert_ne!(unit, unit_name());
    }

    #[test]
    fn terminal_flags_and_order() {
        assert_eq!(terminal_command("ptyxis", "/h"), ["ptyxis", "--new-window", "--working-directory", "/h"]);
        assert_eq!(terminal_command("konsole", "/h"), ["konsole", "--workdir", "/h"]);
        assert_eq!(terminal_command("kitty", "/h"), ["kitty", "--directory", "/h"]);
        assert_eq!(terminal_command("wezterm", "/h"), ["wezterm", "start", "--cwd", "/h"]);
        assert_eq!(terminal_command("ghostty", "/h"), ["ghostty", "+new-window"]);
        assert_eq!(terminal_command("xterm", "/h"), ["xterm"]);
        assert_eq!(terminal_command("foot", "/h"), ["foot"]);
        let list = terminal_candidates(&env(&[("TERMINAL", "foot")]));
        assert_eq!(list[..4], ["foot", "xdg-terminal", "ptyxis", "kgx"]);
        assert_eq!(list.last().unwrap(), "ghostty");
    }

    #[test]
    fn helper_invocation_detection() {
        assert!(is_helper_invocation(&["--open-uri".into(), "file:///".into()]));
        assert!(!is_helper_invocation(&["set".into()]));
        assert_eq!(helper_main(&["--open-uri".into()]), 1);
    }
}
