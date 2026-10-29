//! Parity with the Python originals (read-only, `python3 -I -B`): transform()
//! output byte-for-byte (so the here-doc delimiter is identical and the swap
//! never rewrites rc.xml), delimiter seeds, the desktop entry, and the argparse
//! grammar (namespace, exit status, stdout/stderr). Skips without the sources.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use wayshell_prompt::cli::{self, Parsed};
use wayshell_prompt::install;

const SCRIPT: &str = r#"
import contextlib, hashlib, io, json, os, sys
home = sys.argv[1]
sys.path[:0] = [home + "/btmanager/wayshell", home + "/btmanager/shared", home + "/btmanager/waydock"]
os.environ["COLUMNS"] = "80"
import install_prompts
import wayshell_prompt
data = json.load(sys.stdin)
out = {"transform": [], "delimiter": [], "cli": []}
for case in data["transform"]:
    try:
        import pathlib
        result = install_prompts.transform(case["xml"].encode(), pathlib.Path(case["launcher"]), case["home"] and pathlib.Path(case["home"]))
        out["transform"].append({"ok": result.decode()})
    except Exception as error:
        out["transform"].append({"error": type(error).__name__, "message": str(error)})
for launcher, messages in data["delimiter"]:
    seed = launcher + "\0" + "\0".join(messages)
    d = "WAYSHELL_PROMPT_" + hashlib.sha256(seed.encode()).hexdigest()
    while any(d in m.splitlines() for m in messages):
        d += "_END"
    out["delimiter"].append(d)
out["desktop"] = install_prompts.prompt_desktop_entry().decode()
sys.argv[0] = "wayshell-prompt"
for argv in data["cli"]:
    stdout, stderr = io.StringIO(), io.StringIO()
    code, ns = None, None
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        try:
            ns = vars(wayshell_prompt.arguments(argv))
        except SystemExit as exit:
            code = exit.code
    out["cli"].append({"code": code, "ns": ns, "out": stdout.getvalue(), "err": stderr.getvalue()})
json.dump(out, sys.stdout)
"#;

fn source_home() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.join("btmanager/wayshell/install_prompts.py").is_file().then_some(home)
}

fn python(home: &Path, input: &Value) -> Option<Value> {
    let tmp = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let script = tmp.join(format!("ws-prompt-parity-{}.py", std::process::id()));
    std::fs::write(&script, SCRIPT).ok()?;
    let mut child = Command::new("python3").arg("-I").arg("-B").arg(&script).arg(home).current_dir(&tmp)
        .env("TMPDIR", &tmp).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().ok()?;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().ok()?;
    let _ = std::fs::remove_file(&script);
    assert!(output.status.success(), "python helper failed");
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn rc(body: &str) -> String {
    format!("<?xml version=\"1.0\"?>\n<labwc_config>\n{body}\n</labwc_config>\n")
}

#[test]
fn transform_delimiter_desktop_entry_and_cli_match_python() {
    let Some(home) = source_home() else {
        eprintln!("skipped: ~/btmanager/wayshell not available");
        return;
    };
    let user = home.to_string_lossy().into_owned();
    let launcher = format!("{user}/.local/bin/wayshell-prompt");
    let keybinds = r#"  <keyboard>
    <keybind key="W-q"><action name="If"><prompt message="Really quit?"/><then><action name="Exit"/></then></action></keybind>
    <keybind key="W-r"><action name="If"><prompt message="Reboot&#10;now? $(x) `y` 'z' &amp; &lt;b&gt;"/></action></keybind>
    <keybind key="W-s"><action name="If"><prompt message="tab	and
newline literal"/></action></keybind>
    <keybind key="W-t"><action name="If"><prompt/></action></keybind>
    <keybind key="W-u"><action name="If"><prompt message="Ünïcödé — ✓"/></action></keybind>
  </keyboard>"#;
    let bodies = [
        String::new(),
        "  <core/>".into(),
        "  <core>\n    <gap>10</gap>\n  </core>".into(),
        "  <core><gap>10</gap><promptCommand>labnag</promptCommand></core>".into(),
        "  <!-- <core><promptCommand>old</promptCommand></core> -->\n  <core>\n    <promptCommand>x</promptCommand>\n    <errorCommand/>\n  </core>".into(),
        "  <core decorations=\"server\">\n    <!-- <errorCommand>c</errorCommand> -->\n    <gap>4</gap>  \n\n  </core >".into(),
        format!("  <core><gap>1</gap></core>\n{keybinds}"),
        keybinds.to_owned(),
        "  <core><promptCommand/><promptCommand/></core>".into(),
        "  <core/><core/>".into(),
    ];
    let mut transform_cases = Vec::new();
    for body in &bodies {
        for (launcher, home_arg) in [(launcher.clone(), Some(user.clone())), (launcher.clone(), None),
                                      ("/opt/My Tools/it's here/prompt".to_owned(), Some(user.clone())),
                                      ("/tmp//odd/./path/".to_owned(), None)] {
            transform_cases.push(json!({"xml": rc(body), "launcher": launcher, "home": home_arg}));
        }
    }
    for raw in ["<other/>", "<labwc_config>", "<labwc_config/>", "<labwc_config></labwc_config  >"] {
        transform_cases.push(json!({"xml": raw, "launcher": launcher, "home": user}));
    }
    transform_cases.push(json!({"xml": rc(""), "launcher": "/tmp/50%/p", "home": null}));
    let delimiter_cases = json!([
        [launcher, []], [launcher, [""]], [launcher, ["Really quit?"]], ["/x", ["a\nb", "c\r\nd", "e\u{2028}f"]],
        ["", ["\0inside", "ünï"]], [launcher, ["WAYSHELL_PROMPT_0", "line\nWAYSHELL_PROMPT_\n"]],
    ]);
    let cli_cases: Vec<Vec<&str>> = vec![
        vec!["--help"], vec![], vec!["--message", "a", "--error"], vec!["--mess", "x"], vec!["--message"],
        vec!["--message", "-x"], vec!["--message", "a", "extra"], vec!["--bogus"], vec!["--message=a", "--message-stdin"],
        vec!["--message", "a", "--", "b"], vec!["-x"], vec!["--message", "a", "--bogus"], vec!["--message", "a", "--message", "b"],
        vec!["--message="], vec!["--message-stdin=x"], vec!["--err"], vec!["--d"], vec!["--message", "-5"],
        vec!["--message", "--", "x"], vec!["--message", "a", "--"], vec!["--error", "-x"], vec!["--message", "a", "--accept-label"],
        vec!["--message", "a", "--acc", "Go", "--dec=No way"], vec!["--error", "--error"], vec!["-"], vec!["--message", "-"],
        vec!["--message", "a b", "-"], vec!["--message", "x", "--message-stdin", "--error"], vec!["--bogus=1", "--message", "a"],
        vec!["--message", "a", "-hx"], vec!["--=x"], vec!["--message", "-1x"], vec!["--decline-label", "x"],
        vec!["--message", "a", "--accept-label", "--error"], vec!["-h=x"], vec!["--he=x"], vec!["--h"], vec!["--error="],
        vec!["--message-stdin", "--destructive", "--accept-label", "Restart"], vec!["--mess=x"], vec!["-h-"],
        vec!["--message", "-.5"], vec!["--message", "-a b"], vec!["--message", "a", "-m"],
    ];
    let input = json!({"transform": transform_cases, "delimiter": delimiter_cases, "cli": cli_cases});
    let Some(expected) = python(&home, &input) else {
        eprintln!("skipped: python3 unavailable");
        return;
    };
    for (case, wanted) in transform_cases.iter().zip(expected["transform"].as_array().unwrap()) {
        let ours = install::transform(case["xml"].as_str().unwrap().as_bytes(), Path::new(case["launcher"].as_str().unwrap()),
                                      case["home"].as_str().map(Path::new));
        match (&ours, wanted.get("ok")) {
            (Ok(text), Some(python)) => assert_eq!(std::str::from_utf8(text).unwrap(), python.as_str().unwrap(), "case {case}"),
            (Err(error), None) => {
                if wanted["error"] == "ValueError" {
                    assert_eq!(error, wanted["message"].as_str().unwrap(), "case {case}");
                } else {
                    assert!(error.starts_with("ParseError"), "case {case}: {error} vs {wanted}");
                }
            }
            _ => panic!("mismatch for {case}: {ours:?} vs {wanted}"),
        }
    }
    for (case, wanted) in delimiter_cases.as_array().unwrap().iter().zip(expected["delimiter"].as_array().unwrap()) {
        let messages: Vec<String> = case[1].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_owned()).collect();
        assert_eq!(install::delimiter(case[0].as_str().unwrap(), &messages), wanted.as_str().unwrap());
    }
    let locale = home.join("btmanager/shared/locale");
    assert_eq!(install::prompt_desktop_entry_from(&locale), expected["desktop"].as_str().unwrap());
    for (argv, wanted) in cli_cases.iter().zip(expected["cli"].as_array().unwrap()) {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        match cli::parse_with_width("wayshell-prompt", &args, 78) {
            Parsed::Run(options) => {
                let ns = &wanted["ns"];
                assert!(wanted["code"].is_null(), "{argv:?}: python exited {wanted}");
                assert_eq!(options.message.as_deref(), ns["message"].as_str(), "{argv:?}");
                assert_eq!(json!(options.message_stdin), ns["message_stdin"]);
                assert_eq!(json!(options.error), ns["error"]);
                assert_eq!(json!(options.accept_label), ns["accept_label"], "{argv:?}");
                assert_eq!(json!(options.decline_label), ns["decline_label"], "{argv:?}");
                assert_eq!(json!(options.destructive), ns["destructive"]);
            }
            Parsed::Exit { code, stdout, stderr } => {
                assert_eq!(json!(code), wanted["code"], "{argv:?}");
                assert_eq!(stdout, wanted["out"].as_str().unwrap(), "{argv:?}");
                assert_eq!(stderr, wanted["err"].as_str().unwrap(), "{argv:?}");
            }
        }
    }
}
