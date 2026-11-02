//! Port of test_app_catalog.py, test_app_launch.py and the argv/env parts of
//! test_app_lifetime.py / test_launch_lifetime.py (fake spawner and a fake
//! `systemd-run` script in $TMPDIR; nothing real is launched), plus Python
//! parity of launch argv and environment.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;


use super::app_catalog::*;
use super::spawn::{error, FakeSpawner, GioSpawner};
use super::testutil::TempDir;

fn info(id: &str, name: &str) -> AppInfoData {
    AppInfoData::fake(id, name)
}

fn with(id: &str, f: impl FnOnce(&mut AppInfoData)) -> AppInfoData {
    let mut data = info(id, "Application");
    f(&mut data);
    data
}

fn ids(apps: &[AppEntry]) -> Vec<&str> {
    apps.iter().map(|a| a.id.as_str()).collect()
}

#[test]
fn exact_desktop_ids_and_unique_wm_class_resolve() {
    let catalog = AppCatalog::from_infos(vec![with("org.example.Editor.desktop", |i| i.startup_wm_class = Some("ExampleEdit".into()))]);
    for identity in ["org.example.Editor.desktop", "org.example.Editor", "exampleedit", "Editor"] {
        assert_eq!(catalog.resolve(identity, None).unwrap().id, "org.example.Editor.desktop", "{identity}");
    }
    assert_eq!(catalog.resolve("", Some("ExampleEdit")).unwrap().id, "org.example.Editor.desktop");
    assert_eq!(catalog.resolve("/usr/bin/Editor", None).unwrap().id, "org.example.Editor.desktop");
}

#[test]
fn ambiguous_basenames_or_classes_do_not_merge_unrelated_apps() {
    let catalog = AppCatalog::from_infos(vec![
        with("org.first.Terminal.desktop", |i| i.startup_wm_class = Some("Terminal".into())),
        with("org.second.Terminal.desktop", |i| i.startup_wm_class = Some("Terminal".into())),
    ]);
    assert!(catalog.resolve("Terminal", None).is_none());
    assert!(catalog.resolve("org.unknown.Terminal", None).is_none());
    assert_eq!(catalog.resolve("org.second.Terminal", None).unwrap().id, "org.second.Terminal.desktop");
}

#[test]
fn reverse_dns_unknown_is_not_guessed_from_its_final_word() {
    let catalog = AppCatalog::from_infos(vec![info("org.example.Editor.desktop", "Editor")]);
    assert!(catalog.resolve("org.other.Editor", None).is_none());
    assert!(catalog.resolve("python3", None).is_none());
    assert!(catalog.resolve("", None).is_none());
}

#[test]
fn menu_filters_no_display_without_losing_running_app_identity() {
    let catalog = AppCatalog::from_infos(vec![with("hidden.desktop", |i| i.should_show = false), info("editor.desktop", "Editor")]);
    assert_eq!(ids(&catalog.menu_apps()), ["editor.desktop"]);
    assert_eq!(ids(&catalog.search("edit fixture")), ["editor.desktop"]);
    assert!(catalog.resolve("hidden", None).is_some());
}

#[test]
fn launch_uses_only_known_gio_info_in_separate_service() {
    let catalog = AppCatalog::from_infos(vec![info("app.desktop", "App")]);
    let spawner = FakeSpawner::new();
    let mut launcher = Launcher::new(Rc::new(spawner.clone()));
    launcher.base_environment = Box::new(Vec::new);
    launcher.helper = vec!["/python".into(), "/runtime/app_catalog.py".into()];
    catalog.launch(&launcher, "app.desktop", None, None).unwrap();
    assert_eq!(spawner.last_argv()[spawner.last_argv().len() - 4..], ["/python", "/runtime/app_catalog.py", "--launch-desktop", "app.desktop"]);
    let error = catalog.launch(&launcher, "sh -c something", None, None).unwrap_err();
    assert_eq!(error.to_string(), "That application is no longer installed.");
    assert_eq!(spawner.count(), 1);
}

#[test]
fn search_prefers_name_matches_over_metadata_and_preserves_alphabetical_ties() {
    let mut infos = vec![
        info("game.desktop", "Clair Obscur Expedition 33"),
        info("com.obsproject.Studio.desktop", "OBS Studio"),
        info("a-metadata-obs.desktop", "A metadata match"),
        info("b-metadata-obs.desktop", "B metadata match"),
        info("word.desktop", "Zebra OBS"),
        info("substring.desktop", "Gobservation"),
        info("prefix.desktop", "OBS Tools"),
    ];
    infos.reverse();
    let catalog = AppCatalog::from_infos(infos);
    let expected = [
        "com.obsproject.Studio.desktop", "prefix.desktop", "game.desktop", "word.desktop", "substring.desktop",
        "a-metadata-obs.desktop", "b-metadata-obs.desktop",
    ];
    for query in ["obs", "OBS", "oBs", "  obs  "] {
        assert_eq!(ids(&catalog.search(query)), expected, "{query}");
    }
    assert_eq!(catalog.search("oBS STuDio")[0].id, "com.obsproject.Studio.desktop");
    assert_eq!(catalog.search(""), catalog.menu_apps());
    assert_eq!(catalog.search("  "), catalog.menu_apps());
}

#[test]
fn exact_name_ranks_first_with_unicode_casefold() {
    let catalog = AppCatalog::from_infos(vec![info("long.desktop", "Straße Tools"), info("exact.desktop", "Straße"), info("middle.desktop", "My Straße")]);
    assert_eq!(ids(&catalog.search("STRASSE")), ["exact.desktop", "long.desktop", "middle.desktop"]);
}

fn categorized(id: &str, name: &str, categories: Option<&str>) -> AppInfoData {
    AppInfoData { categories: categories.map(Into::into), ..info(id, name) }
}

#[test]
fn categories_use_desktop_metadata_and_show_only_populated_sections() {
    let catalog = AppCatalog::from_infos(vec![
        categorized("browser.desktop", "Browser", Some("Network;WebBrowser;")),
        categorized("game.desktop", "Game", Some("Game;ActionGame;")),
        categorized("tool.desktop", "Tool", Some("Utility;")),
    ]);
    assert_eq!(
        catalog.menu_categories().iter().map(|c| (c.id, c.name)).collect::<Vec<_>>(),
        vec![("Utility", "Accessories"), ("Game", "Games"), ("Network", "Internet")]
    );
    assert!(catalog.menu_categories().iter().all(|c| !c.icon.is_empty()));
    assert_eq!(ids(&catalog.apps_in_category(Some("Network"))), ["browser.desktop"]);
    assert_eq!(catalog.apps_in_category(None), catalog.menu_apps());
    assert_eq!(catalog.apps_in_category(Some("")), catalog.menu_apps());
    assert!(catalog.apps_in_category(Some("Office")).is_empty());
    assert!(catalog.apps_in_category(Some("WebBrowser")).is_empty());
}

#[test]
fn categories_preserve_multiple_main_sections_without_duplicate_entries() {
    let catalog = AppCatalog::from_infos(vec![
        categorized("zed.desktop", "Zed", Some("Graphics;Office;Graphics;")),
        categorized("alpha.desktop", "alpha", Some("Office;")),
        categorized("audio.desktop", "Audio", Some("AudioVideo;Audio;Video;")),
        categorized("video.desktop", "Video", Some("Video;")),
        categorized("sound.desktop", "Sound", Some("Audio;")),
    ]);
    assert_eq!(catalog.get("zed.desktop").unwrap().categories, ["Graphics", "Office"]);
    assert_eq!(ids(&catalog.apps_in_category(Some("Office"))), ["alpha.desktop", "zed.desktop"]);
    assert_eq!(ids(&catalog.apps_in_category(Some("Graphics"))), ["zed.desktop"]);
    assert_eq!(catalog.get("audio.desktop").unwrap().categories, ["AudioVideo"]);
    assert_eq!(ids(&catalog.apps_in_category(Some("AudioVideo"))), ["audio.desktop", "sound.desktop", "video.desktop"]);
}

#[test]
fn unknown_missing_and_non_main_categories_have_other_fallback() {
    let catalog = AppCatalog::from_infos(vec![
        categorized("empty.desktop", "Application", Some("")),
        categorized("missing.desktop", "Application", None),
        categorized("vendor.desktop", "Application", Some("X-Example;")),
        categorized("secondary.desktop", "Application", Some("TextEditor;")),
        categorized("wrong-case.desktop", "Application", Some("network;")),
        categorized("office.desktop", "Application", Some("X-Example;Office;WordProcessor;")),
    ]);
    assert_eq!(catalog.menu_categories().iter().map(|c| c.id).collect::<Vec<_>>(), ["Office", "Other"]);
    assert_eq!(
        ids(&catalog.apps_in_category(Some("Other"))),
        ["empty.desktop", "missing.desktop", "secondary.desktop", "vendor.desktop", "wrong-case.desktop"]
    );
    assert_eq!(catalog.get("office.desktop").unwrap().categories, ["Office"]);
}

#[test]
fn hidden_apps_never_appear_in_categories_or_create_empty_sections() {
    let catalog = AppCatalog::from_infos(vec![
        AppInfoData { should_show: false, ..categorized("nodisplay.desktop", "Application", Some("Network;")) },
        AppInfoData { hidden: true, ..categorized("hidden.desktop", "Application", Some("Office;")) },
        categorized("shown.desktop", "Application", Some("Game;")),
    ]);
    assert_eq!(catalog.menu_categories().iter().map(|c| c.id).collect::<Vec<_>>(), ["Game"]);
    assert!(catalog.apps_in_category(Some("Network")).is_empty());
    assert!(catalog.apps_in_category(Some("Office")).is_empty());
    assert!(catalog.search("nodisplay").is_empty());
    assert!(catalog.resolve("nodisplay", None).is_some());
    assert!(catalog.resolve("hidden", None).is_none());
}

#[test]
fn refresh_updates_categories_and_search_remains_global() {
    let apps = Rc::new(RefCell::new(vec![
        categorized("obs.desktop", "OBS Studio", Some("AudioVideo;")),
        categorized("game.desktop", "Clair Obscur", Some("Game;")),
    ]));
    let source = apps.clone();
    let mut catalog = AppCatalog::with_source(Box::new(move || source.borrow().clone()));
    assert_eq!(ids(&catalog.apps_in_category(Some("Game"))), ["game.desktop"]);
    assert_eq!(ids(&catalog.search("oBS")), ["obs.desktop", "game.desktop"]);
    apps.borrow_mut().pop();
    apps.borrow_mut()[0].categories = Some("Development;".into());
    catalog.refresh();
    assert_eq!(catalog.menu_categories().iter().map(|c| c.id).collect::<Vec<_>>(), ["Development"]);
    assert!(catalog.apps_in_category(Some("Game")).is_empty());
    assert!(catalog.apps_in_category(Some("AudioVideo")).is_empty());
    assert_eq!(ids(&catalog.apps_in_category(Some("Development"))), ["obs.desktop"]);
}

fn rich(id: &str, name: &str, keywords: &[&str], generic: &str, executable: &str) -> AppInfoData {
    AppInfoData {
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        generic_name: Some(generic.into()),
        executable: Some(executable.into()),
        ..info(id, name)
    }
}

#[test]
fn search_indexes_keywords_generic_name_and_executable() {
    let catalog = AppCatalog::from_infos(vec![
        rich("org.mozilla.firefox.desktop", "Firefox", &["Internet", "WWW"], "Web Browser", "/usr/lib64/firefox/firefox"),
        rich("org.gnome.Ptyxis.desktop", "Ptyxis", &["shell", "prompt", "command"], "Terminal", "ptyxis"),
        rich("nautilus.desktop", "Files", &[], "File Manager", "nautilus"),
        rich("browser-named.desktop", "Browser Tools", &[], "", ""),
    ]);
    let search = |q: &str| catalog.search(q).into_iter().map(|a| a.id).collect::<Vec<_>>();
    assert_eq!(search("terminal"), ["org.gnome.Ptyxis.desktop"]);
    assert_eq!(search("shell"), ["org.gnome.Ptyxis.desktop"]);
    assert_eq!(search("nautilus"), ["nautilus.desktop"]);
    assert_eq!(search("www"), ["org.mozilla.firefox.desktop"]);
    assert_eq!(search("browser"), ["browser-named.desktop", "org.mozilla.firefox.desktop"]);
    assert_eq!(catalog.get("org.mozilla.firefox.desktop").unwrap().executable, "firefox");
}

#[test]
fn typos_match_below_exact_and_prefix_results() {
    let catalog = AppCatalog::from_infos(vec![info("firefox.desktop", "Firefox"), info("fire.desktop", "Fireworks Viewer"), info("calc.desktop", "Calculator")]);
    let search = |q: &str| catalog.search(q).into_iter().map(|a| a.id).collect::<Vec<_>>();
    assert_eq!(search("fierfox"), ["firefox.desktop"]);
    assert_eq!(search("firfox"), ["firefox.desktop"]);
    assert_eq!(search("calcualtor"), ["calc.desktop"]);
    assert_eq!(search("calcultr"), ["calc.desktop"]);
    assert_eq!(search("firew")[0], "fire.desktop");
    assert!(search("zzzz").is_empty());
    assert!(search("xy").is_empty());
}

#[test]
fn launch_history_counts_persist_and_skip_missing_apps() {
    let catalog = AppCatalog::from_infos(vec![info("a.desktop", "Alpha"), info("b.desktop", "Beta"), AppInfoData { should_show: false, ..info("hidden.desktop", "Hidden") }]);
    let dir = TempDir::new();
    let path = dir.join("state/tessel-dock/launch-counts.json");
    let mut history = LaunchHistory::new(Some(path.clone()));
    assert!(history.frequent(&catalog, 6).is_empty());
    for ident in ["b.desktop", "a.desktop", "b.desktop", "gone.desktop", "hidden.desktop"] {
        history.record(ident);
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"a.desktop\": 1, \"b.desktop\": 2, \"gone.desktop\": 1, \"hidden.desktop\": 1}"
    );
    let reloaded = LaunchHistory::new(Some(path.clone()));
    assert_eq!(reloaded.counts.get("b.desktop"), Some(&2));
    assert_eq!(ids(&reloaded.frequent(&catalog, 6)), ["b.desktop", "a.desktop"]);
    assert_eq!(ids(&reloaded.frequent(&catalog, 1)), ["b.desktop"]);
    std::fs::write(&path, "not json").unwrap();
    assert!(LaunchHistory::new(Some(path.clone())).counts.is_empty());
    std::fs::write(&path, "{\"x.desktop\": true, \"y.desktop\": 0, \"z.desktop\": 2.0, \"w.desktop\": 3}").unwrap();
    let odd = LaunchHistory::new(Some(path));
    assert_eq!(odd.counts.iter().map(|(k, v)| (k.to_owned(), *v)).collect::<Vec<_>>(), vec![("x.desktop".into(), 1), ("w.desktop".into(), 3)]);
}

#[test]
fn launch_history_keeps_the_most_used_two_hundred() {
    let dir = TempDir::new();
    let mut history = LaunchHistory::new(Some(dir.join("counts.json")));
    history.record("popular.desktop");
    history.record("popular.desktop");
    for index in 0..LaunchHistory::LIMIT {
        history.record(&format!("app{index}.desktop"));
    }
    assert_eq!(history.counts.len(), LaunchHistory::LIMIT);
    assert_eq!(history.counts.get("popular.desktop"), Some(&2));
    assert!(!history.counts.contains_key("app199.desktop"));
}

#[test]
fn app_entry_defaults() {
    let app = AppEntry::new("app.desktop", "App");
    assert_eq!(app.categories, ["Other"]);
    assert!(app.visible);
    let named = AppCatalog::from_infos(vec![AppInfoData { display_name: None, name: None, ..info("plain.desktop", "") }]);
    assert_eq!(named.get("plain.desktop").unwrap().name, "plain");
    let skipped = AppCatalog::from_infos(vec![info("noext", "X"), AppInfoData { id: None, ..info("x.desktop", "X") }]);
    assert!(skipped.apps().is_empty());
}

#[test]
fn installed_apps_load_from_gio_read_only() {
    let catalog = AppCatalog::new();
    for app in catalog.apps() {
        assert!(app.id.ends_with(".desktop"));
        assert!(!app.categories.is_empty());
    }
}

// ---- launch command / environment ----

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

#[test]
fn service_lifetime_is_independent_and_arguments_remain_literal() {
    let command = launch_command(&s(&["/python", "/a path/helper", "$literal"]), ["CUSTOM", "WAYLAND_DISPLAY"], Some("app-tessel-dock-test.service"), "forking", None);
    assert_eq!(
        command,
        s(&[
            "/usr/bin/systemd-run", "--user", "--collect", "--quiet", "--service-type=forking", "--property=GuessMainPID=no",
            "--property=ExitType=cgroup", "--property=TimeoutStartSec=90s", "--slice=app.slice", "--expand-environment=no",
            "--unit=app-tessel-dock-test.service", "--setenv=CUSTOM", "--setenv=WAYLAND_DISPLAY", "--", "/python", "/a path/helper", "$literal",
        ])
    );
    for flag in ["--wait", "--scope", "--pipe", "--no-block"] {
        assert!(!command.iter().any(|a| a == flag));
    }
    let filtered = launch_command(&s(&["/h"]), ["1BAD", "B", "A_1", "bad-name", "_ok"], Some("u"), "forking", None);
    assert_eq!(filtered[11..14], s(&["--setenv=A_1", "--setenv=B", "--setenv=_ok"]));
}

#[test]
fn each_application_gets_a_unique_unit() {
    let unit = |command: &[String]| command.iter().find(|a| a.starts_with("--unit=")).cloned().unwrap();
    let first = launch_command(&s(&["/helper"]), [], None, "forking", None);
    let second = launch_command(&s(&["/helper"]), [], None, "forking", None);
    assert_ne!(unit(&first), unit(&second));
    let name = unit(&first);
    let hex = name.strip_prefix("--unit=app-tessel-dock-").unwrap().strip_suffix(".service").unwrap();
    assert_eq!(hex.len(), 32);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

fn fake_launcher(spawner: &FakeSpawner) -> Launcher {
    let mut launcher = Launcher::new(Rc::new(spawner.clone()));
    launcher.helper = s(&["/runtime/tessel-dock-launch"]);
    launcher.base_environment = Box::new(|| vec![("HOME".into(), "/home/example".into()), ("INVOCATION_ID".into(), "shell".into())]);
    launcher
}

#[test]
fn direct_command_has_independent_exec_service_and_literal_working_directory() {
    let spawner = FakeSpawner::new();
    let launcher = fake_launcher(&spawner);
    let done = Rc::new(RefCell::new(Vec::new()));
    let sink = done.clone();
    launcher
        .launch_process(
            &s(&["/usr/bin/ptyxis", "--new-window"]),
            Some(vec![("HOME".into(), "/home/example".into()), ("INVOCATION_ID".into(), "shell".into())]),
            Some("/tmp/folder with spaces"),
            Some(Box::new(move |e| sink.borrow_mut().push(e))),
        )
        .unwrap();
    let command = spawner.last_argv();
    assert!(command.contains(&"--service-type=exec".to_string()));
    assert!(command.contains(&"--working-directory=/tmp/folder with spaces".to_string()));
    assert!(command.contains(&"--property=ExitType=cgroup".to_string()));
    assert_eq!(spawner.last_environment().unwrap(), vec![("HOME".to_string(), "/home/example".to_string())]);
    assert_eq!(command[command.len() - 3..], s(&["--", "/usr/bin/ptyxis", "--new-window"]));
    spawner.finish_last(Ok((true, String::new())));
    assert_eq!(*done.borrow(), vec![None]);
    assert_eq!(launcher.launch_process(&[], None, None, None).unwrap_err().to_string(), "An application command is required.");
}

fn startup_info(startup_notify: bool) -> gio::AppInfo {
    let keyfile = glib::KeyFile::new();
    let data = format!("[Desktop Entry]\nType=Application\nName=Fixture\nExec=/usr/bin/true\nStartupNotify={startup_notify}\n");
    keyfile.load_from_data(&data, glib::KeyFileFlags::NONE).unwrap();
    desktop_info_from_keyfile(&keyfile).unwrap()
}

#[derive(Default)]
struct Context {
    failed: RefCell<Vec<String>>,
    requests: RefCell<usize>,
    wayland: bool,
}

impl LaunchContext for Context {
    fn environment(&self) -> Vec<String> {
        s(&[
            "WAYLAND_DISPLAY=wayland-test", "DISPLAY=:1", "CUSTOM=a=b $value", "DBUS_SESSION_BUS_ADDRESS=unix:path=/test/session",
            "INVOCATION_ID=dock", "NOTIFY_SOCKET=/dock/notify", "LISTEN_FDS=1", "WATCHDOG_USEC=1000", "NOEQUALS",
        ])
    }
    fn native_wayland(&self) -> bool {
        self.wayland
    }
    fn display(&self, _info: &gio::AppInfo, _files: &[gio::File]) -> Option<String> {
        Some(":2".into())
    }
    fn startup_notify_id(&self, _info: &gio::AppInfo, _files: &[gio::File]) -> Option<String> {
        *self.requests.borrow_mut() += 1;
        Some("activation-token".into())
    }
    fn launch_failed(&self, token: &str) {
        self.failed.borrow_mut().push(token.into());
    }
}

#[test]
fn environment_and_startup_token_are_forwarded_without_dock_service_metadata() {
    let context = Context::default();
    let (environment, token) = launch_environment(Some(&context), Some(&startup_info(true)), &[], &[]);
    assert_eq!(token.as_deref(), Some("activation-token"));
    assert_eq!(environment.get("WAYDOCK_LAUNCH_STARTUP_ID").map(String::as_str), Some("activation-token"));
    assert_eq!(environment.get("WAYLAND_DISPLAY").map(String::as_str), Some("wayland-test"));
    assert_eq!(environment.get("DISPLAY").map(String::as_str), Some(":2"));
    assert_eq!(environment.get("CUSTOM").map(String::as_str), Some("a=b $value"));
    assert_eq!(environment.get("DBUS_SESSION_BUS_ADDRESS").map(String::as_str), Some("unix:path=/test/session"));
    assert!(!environment.keys().any(|k| SERVICE_ENV.contains(&k) && k != "WAYDOCK_LAUNCH_STARTUP_ID"));
    assert!(!environment.contains_key("NOEQUALS"));
    assert_eq!(*context.requests.borrow(), 1);
    // On Wayland the X11 DISPLAY is kept, never replaced by the socket.
    let wayland = Context { wayland: true, ..Default::default() };
    let (environment, _) = launch_environment(Some(&wayland), Some(&startup_info(true)), &[], &[]);
    assert_eq!(environment.get("DISPLAY").map(String::as_str), Some(":1"));
    // Without a context the process environment is used, minus service variables.
    let base = vec![("A".to_string(), "1".to_string()), ("NOTIFY_SOCKET".to_string(), "x".to_string())];
    let (environment, token) = launch_environment(None, Some(&startup_info(true)), &[], &base);
    assert_eq!(environment.keys().collect::<Vec<_>>(), ["A"]);
    assert_eq!(token, None);
}

#[test]
fn without_startupnotify_no_new_notification_is_created() {
    let context = Context::default();
    let (_environment, token) = launch_environment(Some(&context), Some(&startup_info(false)), &[], &[]);
    assert_eq!(token, None);
    assert_eq!(*context.requests.borrow(), 0);
}

#[test]
fn async_success_returns_after_service_start_and_keeps_context() {
    let spawner = FakeSpawner::new();
    let launcher = fake_launcher(&spawner);
    let context = Rc::new(Context::default());
    let completed = Rc::new(RefCell::new(Vec::new()));
    let sink = completed.clone();
    launcher
        .launch_separately("--launch-desktop", "app.desktop", Some(&startup_info(true)), Some(context.clone()), Some(Box::new(move |e| sink.borrow_mut().push(e))))
        .unwrap();
    assert!(completed.borrow().is_empty());
    let command = spawner.last_argv();
    assert_eq!(command[command.len() - 2..], s(&["--launch-desktop", "app.desktop"]));
    let environment = spawner.last_environment().unwrap();
    assert!(environment.contains(&("WAYDOCK_LAUNCH_STARTUP_ID".into(), "activation-token".into())));
    assert!(!environment.iter().any(|(k, _)| k == "INVOCATION_ID"));
    for (key, _) in &environment {
        assert!(command.contains(&format!("--setenv={key}")));
    }
    assert!(!command.iter().any(|a| a.contains("wayland-test") || a.contains("a=b")));
    spawner.finish_last(Ok((true, String::new())));
    assert_eq!(*completed.borrow(), vec![None]);
    assert!(context.failed.borrow().is_empty());
}

#[test]
fn async_failure_cancels_notification_and_reports_error_once() {
    for (result, expected) in [
        (Ok((false, "Unit startup failed".to_string())), "Could not launch the application: Unit startup failed"),
        (Ok((false, String::new())), "Could not launch the application: The application service could not start."),
        (Err(error("Transport error")), "Could not launch the application: g-io-error-quark: Transport error (0)"),
    ] {
        let spawner = FakeSpawner::new();
        let launcher = fake_launcher(&spawner);
        let context = Rc::new(Context::default());
        let completed = Rc::new(RefCell::new(Vec::new()));
        let sink = completed.clone();
        launcher
            .launch_separately("--launch-desktop", "app.desktop", Some(&startup_info(true)), Some(context.clone()), Some(Box::new(move |e| sink.borrow_mut().push(e))))
            .unwrap();
        spawner.finish_last(result);
        assert_eq!(*completed.borrow(), vec![Some(expected.to_string())]);
        assert_eq!(*context.failed.borrow(), s(&["activation-token"]));
    }
}

#[test]
fn failure_to_start_systemd_run_does_not_launch_directly() {
    let spawner = FakeSpawner::new();
    *spawner.fail_next.borrow_mut() = Some(error("Missing"));
    let launcher = fake_launcher(&spawner);
    let context = Rc::new(Context::default());
    let failure = launcher.launch_separately("--launch-desktop", "app.desktop", Some(&startup_info(true)), Some(context.clone()), None).unwrap_err();
    assert_eq!(failure.message(), "Missing");
    assert_eq!(spawner.count(), 1, "exactly one systemd-run attempt, no direct spawn");
    assert_eq!(*context.failed.borrow(), s(&["activation-token"]));
}

#[test]
fn uri_lookup_and_target_are_forwarded_to_independent_service() {
    let spawner = FakeSpawner::new();
    let mut launcher = fake_launcher(&spawner);
    let lookups = Rc::new(RefCell::new(Vec::<String>::new()));
    let (scheme_log, type_log) = (lookups.clone(), lookups.clone());
    launcher.default_for_uri_scheme = Box::new(move |scheme| {
        scheme_log.borrow_mut().push(format!("scheme:{scheme}"));
        None
    });
    launcher.default_for_type = Box::new(move |kind, uris| {
        type_log.borrow_mut().push(format!("type:{kind}:{uris}"));
        Some(startup_info(false))
    });
    launcher.launch_uri("trash:///", None, None).unwrap();
    assert_eq!(*lookups.borrow(), s(&["scheme:trash", "type:inode/directory:true"]));
    assert_eq!(spawner.last_argv()[spawner.last_argv().len() - 2..], s(&["--open-uri", "trash:///"]));
}

#[test]
fn desktop_file_uses_its_real_path_in_independent_helper() {
    let dir = TempDir::new();
    let path = dir.join("An App.desktop");
    std::fs::write(&path, "[Desktop Entry]\nType=Application\nName=Fixture\nExec=/usr/bin/true\n").unwrap();
    let spawner = FakeSpawner::new();
    let launcher = fake_launcher(&spawner);
    launcher.launch_desktop_file(&path, None, None).unwrap();
    let command = spawner.last_argv();
    assert_eq!(command[command.len() - 2..], ["--launch-file".to_string(), path.to_string_lossy().into_owned()]);
    let invalid = dir.join("bad.desktop");
    std::fs::write(&invalid, "not a desktop file").unwrap();
    assert_eq!(launcher.launch_desktop_file(&invalid, None, None).unwrap_err().to_string(), "That desktop file is not a valid application.");
    assert_eq!(spawner.count(), 1);
}

#[test]
fn transferred_context_returns_token_and_x11_display_only() {
    let info = startup_info(true);
    let context = transferred_context(Some("passed-token"));
    use gio::prelude::AppLaunchContextExt;
    assert_eq!(LaunchContext::startup_notify_id(&context, &info, &[]).as_deref(), Some("passed-token"));
    context.setenv("DISPLAY", ":42");
    context.setenv("WAYLAND_DISPLAY", "wayland-fixture");
    assert_eq!(gio::prelude::AppLaunchContextExt::display(&context, &info, &[]).as_deref(), Some(":42"));
    context.unsetenv("DISPLAY");
    assert_eq!(gio::prelude::AppLaunchContextExt::display(&context, &info, &[]), None);
    assert_eq!(LaunchContext::startup_notify_id(&transferred_context(None), &info, &[]), None);
}

#[test]
fn helper_missing_desktop_or_invalid_mode_fails_without_launch() {
    assert_eq!(launch_helper("--launch-desktop", "tessel-dock-no-such-app-for-tests.desktop", None).unwrap_err(), "That application is no longer installed.");
    assert_eq!(launch_helper("--launch-file", "/nonexistent/tessel-dock.desktop", None).unwrap_err(), "That application is no longer installed.");
    assert_eq!(launch_helper("--invalid", "anything", None).unwrap_err(), "Unknown launch operation.");
    assert_eq!(launch_helper_main(&s(&["only-one"])), 1);
}

/// Child half of `helper_launches_synthetic_desktop_file_with_token`: runs
/// in a separate process with a dead session bus and the token in env.
#[test]
#[ignore]
fn helper_child_process() {
    if std::env::var_os("WAYDOCK_CORE_HELPER_CHILD").is_none() {
        return;
    }
    let target = std::env::var("WAYDOCK_CORE_HELPER_TARGET").unwrap();
    let status = run_launch_helper("--launch-file", &target);
    assert!(std::env::var_os("WAYDOCK_LAUNCH_STARTUP_ID").is_none());
    std::process::exit(status);
}

#[test]
fn helper_launches_synthetic_desktop_file_with_token() {
    let dir = TempDir::new();
    let capture = dir.join("captured.txt");
    let script = dir.join("record.sh");
    std::fs::write(&script, format!("#!/bin/sh\nprintf '%s|%s' \"$DESKTOP_STARTUP_ID\" \"$WAYDOCK_LAUNCH_STARTUP_ID\" > '{}'\n", capture.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let desktop = dir.join("synthetic.desktop");
    std::fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName=Synthetic\nExec={}\nStartupNotify=true\nDBusActivatable=false\n", script.display())).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["core::app_catalog_tests::helper_child_process", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env("WAYDOCK_CORE_HELPER_CHILD", "1")
        .env("WAYDOCK_CORE_HELPER_TARGET", &desktop)
        .env("WAYDOCK_LAUNCH_STARTUP_ID", "synthetic-startup-token")
        .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", dir.join("no-bus").display()))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !capture.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(std::fs::read_to_string(&capture).unwrap(), "synthetic-startup-token|");
}

/// A fake `systemd-run` in $TMPDIR records argv and environment through the
/// real Gio spawner; nothing is started.
#[test]
fn fake_systemd_run_receives_unit_env_names_and_values_via_environment() {
    let dir = TempDir::new();
    let record = dir.join("record");
    let script = dir.join("systemd-run");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{0}.argv'\nenv > '{0}.env'\nif [ \"$FAIL\" = 1 ]; then echo ' Unit failed ' >&2; exit 1; fi\n",
            record.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    for fail in [false, true] {
        let mut launcher = Launcher::new(Rc::new(GioSpawner));
        launcher.systemd_run = Some(script.to_string_lossy().into_owned());
        launcher.helper = s(&["/runtime/tessel-dock-launch"]);
        let environment = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("CUSTOM".to_string(), "$(do-not-run) a=b".to_string()),
            ("INVOCATION_ID".to_string(), "dock".to_string()),
            ("FAIL".to_string(), if fail { "1" } else { "0" }.to_string()),
        ];
        launcher.base_environment = Box::new(move || environment.clone());
        let done = Rc::new(RefCell::new(Vec::new()));
        let sink = done.clone();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                launcher.launch_separately("--launch-desktop", "app.desktop", None, None, Some(Box::new(move |e| sink.borrow_mut().push(e)))).unwrap();
                assert!(tessel_core::timer::wait_for(&context, Duration::from_secs(10), || !done.borrow().is_empty()));
            })
            .unwrap();
        let expected = if fail { Some("Could not launch the application: Unit failed".to_string()) } else { None };
        assert_eq!(*done.borrow(), vec![expected]);
        let argv: Vec<String> = std::fs::read_to_string(format!("{}.argv", record.display())).unwrap().lines().map(String::from).collect();
        assert_eq!(argv[..9], s(&["--user", "--collect", "--quiet", "--service-type=forking", "--property=GuessMainPID=no", "--property=ExitType=cgroup", "--property=TimeoutStartSec=90s", "--slice=app.slice", "--expand-environment=no"]));
        assert!(argv[9].starts_with("--unit=app-tessel-dock-") && argv[9].ends_with(".service"));
        assert_eq!(argv[10..], s(&["--setenv=CUSTOM", "--setenv=FAIL", "--setenv=PATH", "--", "/runtime/tessel-dock-launch", "--launch-desktop", "app.desktop"]));
        let env = std::fs::read_to_string(format!("{}.env", record.display())).unwrap();
        assert!(env.lines().any(|l| l == "CUSTOM=$(do-not-run) a=b"));
        assert!(!env.contains("INVOCATION_ID"));
    }
}

// ---- Python parity ----

