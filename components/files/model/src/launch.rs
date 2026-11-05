//! `file_launch.py`: launch a selected application, or a terminal in a folder,
//! outside the file manager's process lifetime.
//!
//! Both go through Tessel Dock's independent `systemd-run --user` service
//! ([`tessel_dock::core::app_catalog::launch_command`], `--service-type=forking`).
//! The service runs this binary's helper mode: `[current_exe, "--launch-helper",
//! "--application", <desktop id>, <uris>...]` or `[current_exe, "--launch-helper",
//! "--terminal", <dir>, <terminal argv>...]` (Python: `python3 file_launch.py …`),
//! or `[current_exe, "--launch-helper", "--program", <dir>, <argv>...]` (another
//! program, e.g. the archive manager's service-menu commands; Rust only).
//! The application's `main()` dispatches to [`helper_main`] with the arguments
//! after `--launch-helper`.
//!
//! Error texts are untranslated, exactly as in Python (the window shows them as given).

use std::path::Path;
use std::rc::Rc;

use gio::prelude::*;
use tessel_dock::core::app_catalog::{launch_command, launch_environment, LaunchContext, LaunchError};
use tessel_dock::core::ordered::OrderedMap;
use tessel_dock::core::spawn::{GioSpawner, Spawner};

/// Completion callback: `None` on success, else the error text.
pub type DoneFn = Box<dyn FnOnce(Option<String>)>;
/// `_launch_environment(context, info, files)` → (environment, startup token).
pub type EnvironmentFn = Box<dyn Fn(Option<&dyn LaunchContext>, Option<&gio::AppInfo>, &[gio::File]) -> (OrderedMap<String>, Option<String>)>;
/// `shutil.which(name, path=…)`.
pub type WhichFn = Box<dyn Fn(&str, Option<&str>) -> Option<String>>;

/// The first argument of the helper mode.
pub const HELPER_FLAG: &str = "--launch-helper";
/// Helper mode that runs a program argv in a working directory.
pub const PROGRAM_FLAG: &str = "--program";
const TERMINALS: &[&str] = &[
    "x-terminal-emulator", "ptyxis", "kgx", "gnome-terminal", "konsole", "xfce4-terminal", "tilix", "alacritty", "kitty",
    "wezterm", "foot", "ghostty", "xterm",
];

fn process_environment() -> Vec<(String, String)> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
}

/// `[current_exe, "--launch-helper"]`.
pub fn default_helper() -> Vec<String> {
    let exe = tessel_core::paths::own_program("tessel-files").to_string_lossy().into_owned();
    vec![exe, HELPER_FLAG.into()]
}

/// The launcher with injectable process spawning, environment and `which`
/// (Python tests patch these module globals).
pub struct FileLauncher {
    pub spawner: Rc<dyn Spawner>,
    /// argv before `--application …` / `--terminal …` (default [`default_helper`]).
    pub helper: Vec<String>,
    pub environment: EnvironmentFn,
    pub which: WhichFn,
}

impl Default for FileLauncher {
    fn default() -> Self {
        Self::new(Rc::new(GioSpawner))
    }
}

impl FileLauncher {
    pub fn new(spawner: Rc<dyn Spawner>) -> Self {
        Self {
            spawner,
            helper: default_helper(),
            environment: Box::new(|context, info, files| {
                let base = if context.is_none() { process_environment() } else { Vec::new() };
                launch_environment(context, info, files, &base)
            }),
            which: Box::new(crate::util::which),
        }
    }

    /// `launch_with(app_info, files, context, on_done)`.
    pub fn launch_with(
        &self,
        app_info: &gio::AppInfo,
        files: &[gio::File],
        context: Option<Rc<dyn LaunchContext>>,
        on_done: Option<DoneFn>,
    ) -> Result<(), LaunchError> {
        let ident = app_info.id().map(|s| s.to_string());
        self.launch_with_id(ident.as_deref(), Some(app_info), files, context, on_done)
    }

    /// [`Self::launch_with`] with the desktop id given separately.
    pub fn launch_with_id(
        &self,
        ident: Option<&str>,
        app_info: Option<&gio::AppInfo>,
        files: &[gio::File],
        context: Option<Rc<dyn LaunchContext>>,
        on_done: Option<DoneFn>,
    ) -> Result<(), LaunchError> {
        let Some(ident) = ident.filter(|i| !i.is_empty()) else {
            return Err(LaunchError::Value("The selected application does not have a desktop entry.".into()));
        };
        let (environment, token) = (self.environment)(context.as_deref(), app_info, files);
        let mut helper = self.helper.clone();
        helper.extend(["--application".to_owned(), ident.to_owned()]);
        helper.extend(files.iter().map(|f| f.uri().to_string()));
        self.spawn_detached(helper, environment, context, token, on_done, "Could not launch the selected application.")
    }

    /// `_spawn_detached(helper, environment, context, token, on_done, failure_message)`.
    fn spawn_detached(
        &self,
        helper: Vec<String>,
        environment: OrderedMap<String>,
        context: Option<Rc<dyn LaunchContext>>,
        token: Option<String>,
        on_done: Option<DoneFn>,
        failure_message: &'static str,
    ) -> Result<(), LaunchError> {
        let command = launch_command(&helper, environment.keys(), None, "forking", None);
        let environ: Vec<(String, String)> = environment.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let (finish_context, finish_token) = (context.clone(), token.clone());
        let spawned = self.spawner.spawn(
            &command,
            Some(&environ),
            Box::new(move |result| {
                let error = match result {
                    Ok((true, _)) => None,
                    Ok((false, stderr)) => {
                        let text = if stderr.is_empty() { failure_message.to_owned() } else { stderr };
                        Some(tessel_core::pyjson::py_strip(&text).to_owned())
                    }
                    Err(error) => Some(error.message().to_owned()),
                };
                if let (Some(_), Some(token), Some(context)) = (&error, &finish_token, &finish_context) {
                    context.launch_failed(token);
                }
                if let Some(on_done) = on_done {
                    on_done(error);
                }
            }),
        );
        spawned.map_err(|error| {
            if let (Some(token), Some(context)) = (&token, &context) {
                context.launch_failed(token);
            }
            LaunchError::Glib(error)
        })
    }

    /// `_terminal_command(directory, environment)` with this launcher's `which`.
    pub fn terminal_command(&self, directory: &str, environment: &OrderedMap<String>) -> Result<Vec<String>, String> {
        terminal_command_with(directory, environment, &*self.which)
    }

    /// `launch_terminal(directory, context, on_done)`: open a folder in the
    /// default terminal, independently of Files. Locations without a local
    /// path are rejected (never Home, never a remote URI in a shell).
    pub fn launch_terminal(&self, directory: &gio::File, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) -> Result<(), LaunchError> {
        let Some(path) = directory.path() else {
            return Err(LaunchError::Value(
                "This location has no local folder path. Mount it locally before opening it in a terminal.".into(),
            ));
        };
        let path = path.to_string_lossy().into_owned();
        let (environment, token) = (self.environment)(context.as_deref(), None, std::slice::from_ref(directory));
        let command = self.terminal_command(&path, &environment).map_err(LaunchError::Value)?;
        let mut helper = self.helper.clone();
        helper.extend(["--terminal".to_owned(), path]);
        helper.extend(command);
        self.spawn_detached(helper, environment, context, token, on_done, "Could not open a terminal in this folder.")
    }

    /// Run `argv` (no shell; `argv[0]` should be an absolute path) in
    /// `directory`, in its own independent app service like a terminal.
    pub fn launch_program(
        &self,
        directory: &Path,
        argv: &[String],
        context: Option<Rc<dyn LaunchContext>>,
        on_done: Option<DoneFn>,
    ) -> Result<(), LaunchError> {
        if argv.is_empty() {
            return Err(LaunchError::Value("Nothing to launch.".into()));
        }
        let folder = gio::File::for_path(directory);
        let (environment, token) = (self.environment)(context.as_deref(), None, std::slice::from_ref(&folder));
        let mut helper = self.helper.clone();
        helper.extend([PROGRAM_FLAG.to_owned(), directory.to_string_lossy().into_owned()]);
        helper.extend(argv.iter().cloned());
        self.spawn_detached(helper, environment, context, token, on_done, "Could not start the program.")
    }
}

/// `launch_with(app_info, files, context, on_done)` with the real spawner.
pub fn launch_with(app_info: &gio::AppInfo, files: &[gio::File], context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) -> Result<(), LaunchError> {
    FileLauncher::default().launch_with(app_info, files, context, on_done)
}

/// `launch_terminal(directory, context, on_done)` with the real spawner.
pub fn launch_terminal(directory: &gio::File, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) -> Result<(), LaunchError> {
    FileLauncher::default().launch_terminal(directory, context, on_done)
}

/// `_terminal_command(directory, environment)`.
pub fn terminal_command(directory: &str, environment: &OrderedMap<String>) -> Result<Vec<String>, String> {
    terminal_command_with(directory, environment, &crate::util::which)
}

/// `_terminal_command` with an explicit `which(name, path)`: resolve the
/// desktop's terminal preference without a shell. `Err` is the ValueError text.
pub fn terminal_command_with(
    directory: &str,
    environment: &OrderedMap<String>,
    which: &dyn Fn(&str, Option<&str>) -> Option<String>,
) -> Result<Vec<String>, String> {
    let search = environment.get("PATH").map(String::as_str);
    if let Some(executable) = which("xdg-terminal-exec", search) {
        // The XDG terminal interface takes its directory as one argument.
        return Ok(vec![executable, format!("--dir={directory}")]);
    }
    let configured = crate::util::shlex_split(environment.get("TERMINAL").map(String::as_str).unwrap_or(""))
        .map_err(|_| "The configured terminal command is invalid.".to_owned())?;
    let mut candidates: Vec<Vec<String>> = Vec::new();
    if !configured.is_empty() {
        candidates.push(configured);
    }
    candidates.extend(TERMINALS.iter().map(|name| vec![(*name).to_owned()]));
    for arguments in candidates {
        let Some(executable) = which(&arguments[0], search) else { continue };
        let mut command = vec![executable.clone()];
        command.extend(arguments[1..].iter().cloned());
        // Alternatives symlinks (x-terminal-emulator) still need the real
        // emulator's directory flag.
        let resolved = std::fs::canonicalize(&executable).unwrap_or_else(|_| Path::new(&executable).to_path_buf());
        let name = resolved.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let directory = directory.to_owned();
        match name.as_str() {
            "ptyxis" => command.extend(["--new-window".into(), "--working-directory".into(), directory]),
            "kgx" | "gnome-terminal" | "xfce4-terminal" | "tilix" | "alacritty" | "foot" | "footclient" => {
                command.extend(["--working-directory".into(), directory])
            }
            "konsole" => command.extend(["--workdir".into(), directory]),
            "kitty" => command.extend(["--directory".into(), directory]),
            "wezterm" => command.extend(["start".into(), "--cwd".into(), directory]),
            "ghostty" => command.push(format!("--working-directory={directory}")),
            _ => {}
        }
        return Ok(command);
    }
    Err("No terminal application was found. Install a terminal or set the TERMINAL environment variable.".into())
}

// ---- The helper (runs inside the independent service) ---------------------

/// What the helper does to the outside world (tests substitute a fake).
pub trait HelperBackend {
    /// `Gio.SubprocessLauncher(STDOUT_SILENCE | STDERR_SILENCE)` with `cwd`,
    /// `spawnv(argv)`; never waits for the child.
    fn spawn_terminal(&self, cwd: &str, argv: &[String]) -> Result<(), glib::Error>;
    /// `DesktopAppInfo.new(id)` + `launch_uris_async` with a transferred
    /// launch context carrying `token`, waiting for the result. `None` when
    /// the application is not installed; `Ok(false)` is a false finish.
    fn launch_application(&self, id: &str, uris: &[String], token: Option<&str>) -> Option<Result<bool, glib::Error>>;
}

/// The real backend.
pub struct GioHelper;

impl HelperBackend for GioHelper {
    fn spawn_terminal(&self, cwd: &str, argv: &[String]) -> Result<(), glib::Error> {
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE);
        launcher.set_cwd(cwd);
        let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| std::ffi::OsStr::new(a.as_str())).collect();
        launcher.spawn(&args).map(|_| ())
    }

    fn launch_application(&self, id: &str, uris: &[String], token: Option<&str>) -> Option<Result<bool, glib::Error>> {
        let info = tessel_dock::core::app_catalog::desktop_info(id)?;
        let context = tessel_dock::core::app_catalog::transferred_context(token);
        let main_context = glib::MainContext::new();
        let outcome: Rc<std::cell::RefCell<Option<Result<bool, glib::Error>>>> = Rc::default();
        let result = main_context.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&main_context), false);
            let (slot, quit) = (outcome.clone(), main_loop.clone());
            let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
            info.launch_uris_async(&uris, Some(&context), None::<&gio::Cancellable>, move |result| {
                slot.replace(Some(result.map(|()| true)));
                quit.quit();
            });
            if outcome.borrow().is_none() {
                main_loop.run();
            }
        });
        if let Err(error) = result {
            return Some(Err(glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string())));
        }
        let value = outcome.borrow_mut().take();
        Some(value.unwrap_or(Ok(false)))
    }
}

/// `file_launch.main(arguments)` with an explicit backend and startup token
/// (`token` is only consulted for `--application`). Returns the exit status:
/// 0 ok, 1 failure (message on stderr), 2 bad usage.
pub fn helper_main_with(args: &[String], backend: &dyn HelperBackend, token: impl FnOnce() -> Option<String>) -> i32 {
    if args.len() >= 3 && (args[0] == "--terminal" || args[0] == PROGRAM_FLAG) {
        // The child stays in the independent app service; returning lets its
        // Type=forking startup complete without waiting for the terminal.
        return match backend.spawn_terminal(&args[1], &args[2..]) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{}", error.message());
                1
            }
        };
    }
    if args.len() < 3 || args[0] != "--application" {
        return 2;
    }
    let token = token();
    match backend.launch_application(&args[1], &args[2..], token.as_deref()) {
        None => {
            eprintln!("Application no longer installed.");
            1
        }
        Some(Ok(true)) => 0,
        Some(Ok(false)) => {
            eprintln!("The selected application could not be launched.");
            1
        }
        Some(Err(error)) => {
            eprintln!("{}", error.message());
            1
        }
    }
}

/// The helper entry point: `args` are the arguments after `--launch-helper`.
/// The startup token is taken (and removed) from `WAYDOCK_LAUNCH_STARTUP_ID`.
pub fn helper_main(args: &[String]) -> i32 {
    helper_main_with(args, &GioHelper, || {
        let token = std::env::var("WAYDOCK_LAUNCH_STARTUP_ID").ok();
        std::env::remove_var("WAYDOCK_LAUNCH_STARTUP_ID");
        token
    })
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
