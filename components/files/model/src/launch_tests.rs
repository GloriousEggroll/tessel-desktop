//! test_file_launch.py: launch lifetimes/argument preservation with every real
//! launch replaced by a fake spawner/backend.

use super::*;
use std::cell::RefCell;
use tessel_dock::core::spawn::{error as spawn_error, FakeSpawner};

fn env(pairs: &[(&str, &str)]) -> OrderedMap<String> {
    let mut map = OrderedMap::new();
    for (k, v) in pairs {
        map.insert(*k, (*v).to_owned());
    }
    map
}

fn launcher(spawner: &FakeSpawner, environment: OrderedMap<String>, token: Option<&str>) -> FileLauncher {
    let mut launcher = FileLauncher::new(Rc::new(spawner.clone()));
    let token = token.map(str::to_owned);
    launcher.environment = Box::new(move |_, _, _| (environment.clone(), token.clone()));
    launcher.helper = vec!["/usr/local/bin/tessel-files".into(), HELPER_FLAG.into()];
    launcher
}

fn done_log() -> (Rc<RefCell<Vec<Option<String>>>>, DoneFn) {
    let log: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let sink = log.clone();
    (log, Box::new(move |error| sink.borrow_mut().push(error)))
}

fn env_vec(map: &OrderedMap<String>) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect()
}

#[test]
fn terminal_launch_is_independent_and_preserves_literal_directory() {
    crate::test_env();
    let path = "/tmp/résumé 'two words' $(do-not-execute)";
    let environment = env(&[("TERMINAL", "kitty --class \"Files terminal\""), ("PATH", "/test/bin")]);
    let spawner = FakeSpawner::new();
    let mut launch = launcher(&spawner, environment.clone(), None);
    launch.which = Box::new(|name, _path| (name == "kitty").then(|| "/test/bin/kitty".to_owned()));
    let (done, on_done) = done_log();
    launch.launch_terminal(&gio::File::for_path(path), None, Some(on_done)).unwrap();
    let argv = spawner.last_argv();
    assert_eq!(argv[argv.len() - 7..], ["--terminal", path, "/test/bin/kitty", "--class", "Files terminal", "--directory", path]);
    for argument in ["--user", "--collect", "--service-type=forking", "--property=ExitType=cgroup", "--expand-environment=no"] {
        assert!(argv.iter().any(|a| a == argument), "{argument}");
    }
    for argument in ["--wait", "--pipe", "--scope", "sh", "-c"] {
        assert!(!argv.iter().any(|a| a == argument), "{argument}");
    }
    assert_eq!(&argv[argv.len() - 9..argv.len() - 7], ["/usr/local/bin/tessel-files", "--launch-helper"]);
    assert_eq!(spawner.last_environment().unwrap(), env_vec(&environment));
    assert!(done.borrow().is_empty());
    spawner.finish_last(Ok((true, String::new())));
    assert_eq!(*done.borrow(), [None]);
}

#[test]
fn xdg_terminal_preference_is_used_before_fallback() {
    let calls: Rc<RefCell<Vec<(String, Option<String>)>>> = Rc::default();
    let record = calls.clone();
    let which = move |name: &str, path: Option<&str>| {
        record.borrow_mut().push((name.to_owned(), path.map(str::to_owned)));
        Some("/test/xdg-terminal-exec".to_owned())
    };
    assert_eq!(
        terminal_command_with("/tmp/folder with spaces", &env(&[("TERMINAL", "kitty")]), &which).unwrap(),
        ["/test/xdg-terminal-exec", "--dir=/tmp/folder with spaces"]
    );
    assert_eq!(*calls.borrow(), [("xdg-terminal-exec".to_owned(), None)]);
}

#[test]
fn missing_configured_terminal_uses_existing_shell_ptyxis_fallback() {
    let which = |name: &str, _: Option<&str>| (name == "ptyxis").then(|| "/usr/bin/ptyxis".to_owned());
    assert_eq!(
        terminal_command_with("/home/example", &env(&[("TERMINAL", "missing-terminal")]), &which).unwrap(),
        ["/usr/bin/ptyxis", "--new-window", "--working-directory", "/home/example"]
    );
}

#[test]
fn terminal_directory_flags_and_unknown_emulator_cwd_fallback() {
    let cases: &[(&str, &[&str])] = &[
        ("kitty", &["--directory", "/example"]),
        ("konsole", &["--workdir", "/example"]),
        ("gnome-terminal", &["--working-directory", "/example"]),
        ("ghostty", &["--working-directory=/example"]),
        ("wezterm", &["start", "--cwd", "/example"]),
        ("custom-terminal", &[]),
    ];
    for (name, suffix) in cases {
        let which = |value: &str, _: Option<&str>| (value == *name).then(|| format!("/bin/{name}"));
        let mut expected = vec![format!("/bin/{name}")];
        expected.extend(suffix.iter().map(|s| (*s).to_owned()));
        assert_eq!(terminal_command_with("/example", &env(&[("TERMINAL", name)]), &which).unwrap(), expected, "{name}");
    }
}

#[test]
fn remote_terminal_without_local_mount_path_is_rejected() {
    crate::test_env();
    let spawner = FakeSpawner::new();
    let launch = launcher(&spawner, env(&[]), None);
    let remote = gio::File::for_uri("sftp://example.invalid/folder");
    let error = launch.launch_terminal(&remote, None, None).unwrap_err();
    assert!(error.to_string().contains("Mount it locally"), "{error}");
    assert_eq!(spawner.count(), 0);
}

#[test]
fn default_terminal_alternatives_alias_retains_directory_flag() {
    let root = std::env::temp_dir().join(format!("files-model-launch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("ptyxis");
    std::fs::write(&executable, "").unwrap();
    let alias = root.join("x-terminal-emulator");
    std::os::unix::fs::symlink(&executable, &alias).unwrap();
    let alias_text = alias.to_string_lossy().into_owned();
    let which = |name: &str, _: Option<&str>| (name == "x-terminal-emulator").then(|| alias_text.clone());
    assert_eq!(
        terminal_command_with("/folder", &env(&[]), &which).unwrap(),
        [alias_text.as_str(), "--new-window", "--working-directory", "/folder"]
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[derive(Default)]
struct FakeHelper {
    terminal: RefCell<Option<(String, Vec<String>)>>,
    terminal_error: Option<String>,
    installed: bool,
    finish: Option<Result<bool, String>>,
    requests: RefCell<Vec<(Vec<String>, Option<String>)>>,
    lookups: RefCell<Vec<String>>,
}

impl HelperBackend for FakeHelper {
    fn spawn_terminal(&self, cwd: &str, argv: &[String]) -> Result<(), glib::Error> {
        self.terminal.replace(Some((cwd.to_owned(), argv.to_vec())));
        match &self.terminal_error {
            Some(message) => Err(spawn_error(message)),
            None => Ok(()),
        }
    }
    fn launch_application(&self, id: &str, uris: &[String], token: Option<&str>) -> Option<Result<bool, glib::Error>> {
        self.lookups.borrow_mut().push(id.to_owned());
        if !self.installed {
            return None;
        }
        self.requests.borrow_mut().push((uris.to_vec(), token.map(str::to_owned)));
        Some(match self.finish.clone().unwrap_or(Ok(true)) {
            Ok(value) => Ok(value),
            Err(message) => Err(spawn_error(&message)),
        })
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}

#[test]
fn terminal_helper_sets_cwd_and_does_not_wait_for_child() {
    let path = "/run/user/1000/gvfs/smb-share:server=test,share=résumé/sub folder";
    let backend = FakeHelper::default();
    let argv = strings(&["/test/terminal", "--directory", path]);
    let mut args = strings(&["--terminal", path]);
    args.extend(argv.clone());
    assert_eq!(helper_main_with(&args, &backend, || panic!("token is only for applications")), 0);
    assert_eq!(backend.terminal.borrow().clone().unwrap(), (path.to_owned(), argv));
}

#[test]
fn terminal_failures_are_reported_without_fallback_to_home() {
    let none = |_: &str, _: Option<&str>| None;
    assert!(terminal_command_with("/example", &env(&[]), &none).unwrap_err().contains("No terminal application"));
    assert!(terminal_command_with("/example", &env(&[("TERMINAL", "'unterminated")]), &none)
        .unwrap_err()
        .contains("configured terminal command is invalid"));
    let backend = FakeHelper { terminal_error: Some("Invalid directory".into()), ..Default::default() };
    assert_eq!(helper_main_with(&strings(&["--terminal", "/missing", "/test/terminal"]), &backend, || None), 1);
}

#[test]
fn selected_app_and_file_uris_are_literal_independent_service_arguments() {
    crate::test_env();
    let mut files: Vec<gio::File> =
        ["/tmp/a space.txt", "/tmp/quote'$(no command).txt", "/tmp/résumé.txt"].iter().map(gio::File::for_path).collect();
    files.push(gio::File::for_uri("sftp://host.example/folder/with%20spaces.txt"));
    let environment = env(&[("WAYLAND_DISPLAY", "wayland-private"), ("A_LITERAL_VALUE", "$(do-not-execute) ' \"")]);
    let spawner = FakeSpawner::new();
    let launch = launcher(&spawner, environment.clone(), None);
    let (done, on_done) = done_log();
    launch.launch_with_id(Some("test.desktop"), None, &files, None, Some(on_done)).unwrap();
    let argv = spawner.last_argv();
    for argument in [
        "--user",
        "--collect",
        "--service-type=forking",
        "--property=ExitType=cgroup",
        "--property=GuessMainPID=no",
        "--slice=app.slice",
        "--expand-environment=no",
    ] {
        assert!(argv.iter().any(|a| a == argument), "{argument}");
    }
    for argument in ["--scope", "--wait", "--pipe"] {
        assert!(!argv.iter().any(|a| a == argument));
    }
    let mut tail = strings(&["--application", "test.desktop"]);
    tail.extend(files.iter().map(|f| f.uri().to_string()));
    assert_eq!(argv[argv.len() - 6..], tail[..]);
    assert_eq!(spawner.last_environment().unwrap(), env_vec(&environment));
    assert!(done.borrow().is_empty());
    spawner.finish_last(Ok((true, String::new())));
    assert_eq!(*done.borrow(), [None]);
    // An application without a desktop id is refused before spawning.
    let error = launch.launch_with_id(None, None, &files, None, None).unwrap_err();
    assert_eq!(error.to_string(), "The selected application does not have a desktop entry.");
    assert_eq!(spawner.count(), 1);
}

struct Context {
    retired: Rc<RefCell<Vec<String>>>,
}

impl LaunchContext for Context {
    fn environment(&self) -> Vec<String> {
        Vec::new()
    }
    fn native_wayland(&self) -> bool {
        true
    }
    fn display(&self, _: &gio::AppInfo, _: &[gio::File]) -> Option<String> {
        None
    }
    fn startup_notify_id(&self, _: &gio::AppInfo, _: &[gio::File]) -> Option<String> {
        None
    }
    fn launch_failed(&self, token: &str) {
        self.retired.borrow_mut().push(token.to_owned());
    }
}

#[test]
fn launch_failure_retires_activation_token() {
    for spawn_fails in [false, true] {
        let retired: Rc<RefCell<Vec<String>>> = Rc::default();
        let context: Rc<dyn LaunchContext> = Rc::new(Context { retired: retired.clone() });
        let spawner = FakeSpawner::new();
        let launch = launcher(&spawner, env(&[]), Some("activation-token"));
        let (done, on_done) = done_log();
        if spawn_fails {
            spawner.fail_next.replace(Some(spawn_error("spawn failed")));
            let error = launch.launch_with_id(Some("test.desktop"), None, &[], Some(context), Some(on_done)).unwrap_err();
            assert!(matches!(error, LaunchError::Glib(_)));
        } else {
            launch.launch_with_id(Some("test.desktop"), None, &[], Some(context), Some(on_done)).unwrap();
            spawner.finish_last(Ok((false, "Unit launch failed\n".into())));
            assert_eq!(*done.borrow(), [Some("Unit launch failed".to_owned())]);
        }
        assert_eq!(*retired.borrow(), ["activation-token"]);
    }
    // Empty stderr reports the operation's own message; communicate errors their message.
    let spawner = FakeSpawner::new();
    let launch = launcher(&spawner, env(&[]), None);
    let (done, on_done) = done_log();
    launch.launch_with_id(Some("a.desktop"), None, &[], None, Some(on_done)).unwrap();
    spawner.finish_last(Ok((false, String::new())));
    let (done2, on_done2) = done_log();
    launch.launch_with_id(Some("a.desktop"), None, &[], None, Some(on_done2)).unwrap();
    spawner.finish_last(Err(spawn_error("pipe broke")));
    assert_eq!(*done.borrow(), [Some("Could not launch the selected application.".to_owned())]);
    assert_eq!(*done2.borrow(), [Some("pipe broke".to_owned())]);
}

#[test]
fn helper_preserves_activation_token_and_uris_without_any_real_launch() {
    let uris = strings(&["file:///tmp/two%20words.txt", "sftp://example.invalid/file"]);
    let backend = FakeHelper { installed: true, ..Default::default() };
    let mut args = strings(&["--application", "test.desktop"]);
    args.extend(uris.clone());
    assert_eq!(helper_main_with(&args, &backend, || Some("passed-token".into())), 0);
    assert_eq!(*backend.requests.borrow(), [(uris, Some("passed-token".to_owned()))]);
}

#[test]
fn synchronous_completion_and_false_is_error() {
    for (finish, code) in [(Ok(true), 0), (Ok(false), 1), (Err("boom".to_owned()), 1)] {
        let backend = FakeHelper { installed: true, finish: Some(finish), ..Default::default() };
        assert_eq!(helper_main_with(&strings(&["--application", "test.desktop", "file:///tmp/example"]), &backend, || None), code);
    }
}

#[test]
fn bad_or_missing_application_does_not_launch() {
    let backend = FakeHelper::default();
    assert_eq!(helper_main_with(&[], &backend, || None), 2);
    assert_eq!(helper_main_with(&strings(&["--application", "only-id"]), &backend, || None), 2);
    assert!(backend.lookups.borrow().is_empty());
    assert_eq!(helper_main_with(&strings(&["--application", "removed.desktop", "file:///tmp/example"]), &backend, || None), 1);
    assert_eq!(*backend.lookups.borrow(), ["removed.desktop"]);
}

#[test]
fn source_helper_rejects_unknown_operation() {
    // The real entry point: no lookup, no spawn, no environment change.
    assert_eq!(helper_main(&strings(&["--invalid"])), 2);
    assert_eq!(default_helper()[1], "--launch-helper");
}

#[test]
fn program_launch_runs_literal_argv_in_folder_through_independent_service() {
    crate::test_env();
    let folder = "/srv/two words $(no-shell)";
    let spawner = FakeSpawner::new();
    let launch = launcher(&spawner, env(&[("PATH", "/test/bin")]), None);
    let program = strings(&["/test/bin/tessel-archive", "--batch", "--destination", folder, "/srv/a b.zip"]);
    let (done, on_done) = done_log();
    launch.launch_program(Path::new(folder), &program, None, Some(on_done)).unwrap();
    let argv = spawner.last_argv();
    let tail = argv.len() - program.len();
    assert_eq!(argv[tail..], program[..]);
    assert_eq!(argv[tail - 4..tail], ["/usr/local/bin/tessel-files", HELPER_FLAG, PROGRAM_FLAG, folder]);
    assert!(argv.iter().any(|a| a == "--service-type=forking"));
    for argument in ["--wait", "--pipe", "--scope", "sh", "-c"] {
        assert!(!argv.iter().any(|a| a == argument), "{argument}");
    }
    assert!(done.borrow().is_empty());
    spawner.finish_last(Ok((true, String::new())));
    assert_eq!(*done.borrow(), [None]);
    assert!(launch.launch_program(Path::new(folder), &[], None, None).is_err());
}

#[test]
fn program_helper_spawns_argv_in_cwd_without_waiting() {
    let backend = FakeHelper::default();
    let args = strings(&["--program", "/srv/x", "/usr/bin/tessel-archive", "--add", "a"]);
    assert_eq!(helper_main_with(&args, &backend, || panic!("token is only for applications")), 0);
    assert_eq!(backend.terminal.borrow().clone().unwrap(), ("/srv/x".to_owned(), strings(&["/usr/bin/tessel-archive", "--add", "a"])));
    assert_eq!(helper_main_with(&strings(&["--program", "/srv/x"]), &backend, || None), 2);
}
