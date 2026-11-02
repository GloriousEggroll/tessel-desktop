//! Port of test_wayshell_prompt.py `PromptConfigTests` plus protocol checks
//! (plain `cargo test`; files only under $TMPDIR).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tessel_prompt::install::{self, transform};
use tessel_prompt::{arguments, cli, Parsed, ACCEPT, DECLINE, DISMISS, TEXT_LIMIT};

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ws-prompt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn doc(text: &[u8]) -> roxmltree::Document<'_> {
    roxmltree::Document::parse(std::str::from_utf8(text).unwrap()).unwrap()
}

/// ElementTree `findtext("a/b")` on the root.
fn findtext(text: &[u8], path: &str) -> Option<String> {
    let document = doc(text);
    let mut node = document.root_element();
    for part in path.split('/') {
        node = node.children().find(|c| c.is_element() && c.tag_name().name() == part)?;
    }
    Some(node.text().unwrap_or("").to_owned())
}

fn findall_count(text: &[u8], path: &str) -> usize {
    let document = doc(text);
    let (first, second) = path.split_once('/').unwrap();
    document.root_element().children().filter(|c| c.is_element() && c.tag_name().name() == first)
        .flat_map(|c| c.children().filter(|g| g.is_element() && g.tag_name().name() == second).collect::<Vec<_>>())
        .count()
}

#[test]
fn test_protocol_constants() {
    assert_eq!((ACCEPT, DECLINE, DISMISS, TEXT_LIMIT), (0, 1, 254, 65536));
}

#[test]
fn test_only_commands_change_and_reinstall_is_idempotent() {
    let original = br#"<?xml version="1.0"?>
<labwc_config>
  <!-- example <core><promptCommand>old</promptCommand></core> -->
  <core><gap>10</gap><promptCommand>labnag</promptCommand></core>
  <keyboard><keybind key="W-q"><action name="If"><prompt message="Really quit?"/>
  <then><action name="Exit"/></then></action></keybind></keyboard>
</labwc_config>
"#;
    let launcher = Path::new("/home/User Name/.local/bin/tessel-prompt");
    let result = transform(original, launcher, None).unwrap();
    assert_eq!(transform(&result, launcher, None).unwrap(), result);
    assert_eq!(findtext(&result, "core/gap").as_deref(), Some("10"));
    let keyboard = |text: &[u8]| {
        let text = std::str::from_utf8(text).unwrap().to_owned();
        let start = text.find("<keyboard>").unwrap();
        let end = text.find("</keyboard>").unwrap();
        text[start..end].to_owned()
    };
    assert_eq!(keyboard(&result), keyboard(original));
    let text = String::from_utf8(result.clone()).unwrap();
    assert!(text.contains("<!-- example <core><promptCommand>old</promptCommand></core> -->"));
    assert!(findtext(&result, "core/promptCommand").unwrap().contains("--message-stdin"));
    assert_eq!(findtext(&result, "core/errorCommand").unwrap(), "'/home/User Name/.local/bin/tessel-prompt' --error");
}

#[test]
fn test_launcher_in_home_is_written_portably() {
    let home = Path::new("/home/User Name");
    let launcher = home.join(".local/bin/tessel-prompt");
    let result = transform(b"<labwc_config><core/></labwc_config>", &launcher, Some(home)).unwrap();
    assert!(!String::from_utf8_lossy(&result).contains("/home/User Name"));
    assert!(findtext(&result, "core/promptCommand").unwrap().starts_with("exec \"$HOME\"/.local/bin/tessel-prompt --message-stdin"));
    assert_eq!(findtext(&result, "core/errorCommand").unwrap(), r#"sh -c 'exec "$HOME"/.local/bin/tessel-prompt --error'"#);
    assert_eq!(transform(&result, &launcher, Some(home)).unwrap(), result);
}

#[test]
fn test_core_creation_and_self_closing_commands() {
    for source in [&b"<labwc_config></labwc_config>"[..], b"<labwc_config><core/></labwc_config>",
                   b"<labwc_config><core><promptCommand/><errorCommand/></core></labwc_config>"] {
        let result = transform(source, Path::new("/tmp/helper"), None).unwrap();
        assert_eq!(findall_count(&result, "core/promptCommand"), 1);
        assert_eq!(transform(&result, Path::new("/tmp/helper"), None).unwrap(), result);
    }
}

#[test]
fn test_review_errors() {
    let helper = Path::new("/tmp/helper");
    assert_eq!(transform(b"<other/>", helper, None).unwrap_err(), "Expected labwc_config root");
    assert_eq!(transform(b"<labwc_config><core/><core/></labwc_config>", helper, None).unwrap_err(),
               "Multiple core sections require review");
    assert_eq!(transform(b"<labwc_config/>", Path::new("/tmp/50%"), None).unwrap_err(),
               "A labwc prompt command path cannot contain percent signs");
    assert_eq!(transform(b"<labwc_config><core><promptCommand/><promptCommand/></core></labwc_config>", helper, None).unwrap_err(),
               "Multiple promptCommand entries require review");
    assert!(transform(b"<labwc_config>", helper, None).is_err());
}

#[test]
fn delimiter_format_and_splitlines() {
    let launcher = "/x";
    let plain = install::delimiter(launcher, &["a".into()]);
    assert!(plain.starts_with("WAYSHELL_PROMPT_") && plain.len() == 16 + 64);
    assert_eq!(install::py_splitlines("a\r\nb\rc\u{2028}d\n"), vec!["a", "b", "c", "d"]);
    assert_eq!(install::py_splitlines(""), Vec::<&str>::new());
}

fn xml_attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\n', "&#10;")
}

#[test]
fn test_prompt_punctuation_is_literal_to_shell() {
    let root = scratch("prompt quote '");
    let marker = root.join("should-not-exist");
    let received = root.join("received.txt");
    let launcher = root.join("capture prompt");
    std::fs::write(&launcher, "#!/bin/sh\nexec cat > \"$PROMPT_RECEIVED\"\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let message = format!("Don't execute $(touch {}) or `echo surprise`; \"quotes\" <b>text</b> &\nsecond line", marker.display());
    let source = format!("<labwc_config><prompt message=\"{}\" /></labwc_config>", xml_attribute(&message));
    let result = transform(source.as_bytes(), &launcher, None).unwrap();
    let command = findtext(&result, "core/promptCommand").unwrap();
    let output = std::process::Command::new("/bin/sh").arg("-c").arg(command.replace("%m", &message))
        .env("PROMPT_RECEIVED", &received).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(std::fs::read_to_string(&received).unwrap(), format!("{message}\n"));
    assert!(!marker.exists());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn test_plan_is_read_only_and_has_no_services() {
    let home = scratch("plan");
    let config = home.join(".config/labwc/rc.xml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "<labwc_config><core><gap>7</gap></core></labwc_config>").unwrap();
    let before = std::fs::read(&config).unwrap();
    let files = install::plan(&home, None, None).unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert!(!home.join(".local").exists());
    assert!(files.iter().any(|f| f.path == home.join(".local/bin/tessel-prompt")));
    assert!(!files.iter().any(|f| f.path.components().any(|c| c.as_os_str() == "systemd") || f.path.file_name().unwrap() == "autostart"));
    let entry = files.iter().find(|f| f.path == home.join(".local/share/applications/org.tessel.Prompt.desktop")).unwrap();
    let entry = String::from_utf8(entry.data.clone()).unwrap();
    assert!(entry.contains("\nIcon=dialog-question-symbolic\n"));
    assert!(entry.contains("\nNoDisplay=true\n"));
    assert!(entry.contains("\nName=Confirmation\n"));
    let launcher = files.iter().find(|f| f.path == home.join(".local/bin/tessel-prompt")).unwrap();
    assert_eq!(launcher.mode, 0o755);
    assert_eq!(String::from_utf8(launcher.data.clone()).unwrap(),
               format!("#!/bin/sh\nexec {} \"$@\"\n", home.join(".local/lib/tessel/tessel-prompt").display()));
    assert_eq!(install::pending(&files).len(), 3);
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn cli_grammar() {
    let options = arguments(&["--message", "Perform fixture action?", "--decline-label", "Later"]);
    assert_eq!(options.message.as_deref(), Some("Perform fixture action?"));
    assert_eq!(options.decline_label, "Later");
    assert_eq!(options.accept_label, "Yes");
    assert!(arguments(&["--err"]).error);
    assert!(arguments(&["--message-stdin", "--destructive"]).destructive);
    let fail = |args: &[&str]| match cli::parse_with_width("p", &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), 78) {
        Parsed::Exit { code, stderr, .. } => (code, stderr.lines().last().unwrap_or("").to_owned()),
        Parsed::Run(options) => panic!("{options:?}"),
    };
    assert_eq!(fail(&[]), (2, "p: error: one of the arguments --message --message-stdin --error is required".into()));
    assert_eq!(fail(&["--message", "a", "--error"]), (2, "p: error: argument --error: not allowed with argument --message".into()));
    assert_eq!(fail(&["--mess", "x"]), (2, "p: error: ambiguous option: --mess could match --message, --message-stdin".into()));
    assert_eq!(fail(&["--message-stdin=x"]), (2, "p: error: argument --message-stdin: ignored explicit argument 'x'".into()));
    assert_eq!(fail(&["--message", "a", "--", "b"]), (2, "p: error: unrecognized arguments: -- b".into()));
}
