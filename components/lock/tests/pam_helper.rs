//! Real `tessel-lock-pam` framing against a FAKE libpam.so.0 (test_lock_backend.py
//! PamTests). The fake is compiled with `cc` into this test's own directory and
//! loaded through LD_LIBRARY_PATH. Before any conversation, `ld.so` tracing
//! proves the helper resolves libpam.so.0 to the fake: the system PAM stack and
//! the user's password are never used. No headers needed (types are inlined).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const FAKE_PAM: &str = include_str!("support/fake_pam.c");

fn fake_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("fake-pam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fake_pam.c");
        std::fs::write(&source, FAKE_PAM).unwrap();
        // Same symbol version node as Linux-PAM, so ld.so binds silently.
        let map = dir.join("libpam.map");
        std::fs::write(&map, "LIBPAM_1.0 { global: pam_*; fake_pam_marker; local: *; };\n").unwrap();
        let status = Command::new("cc").args(["-shared", "-fPIC", "-Wall", "-Werror", "-Wl,-soname,libpam.so.0"])
            .arg(format!("-Wl,--version-script={}", map.display())).arg("-o")
            .arg(dir.join("libpam.so.0")).arg(&source).status().expect("cc");
        assert!(status.success(), "compiling the fake libpam failed");
        write_scripts(&dir);
        dir
    })
}

fn rust_helper() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_tessel-lock-pam")) }

fn command(binary: &Path, env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(binary);
    command.env_clear().env("LD_LIBRARY_PATH", fake_dir()).env("PATH", "/usr/bin:/bin");
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// Refuse to run any conversation unless ld.so resolves libpam.so.0 to the fake.
fn assert_fake_loaded(binary: &Path) {
    let output = command(binary, &[("LD_TRACE_LOADED_OBJECTS", "1")]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|l| l.contains("libpam.so.0")).unwrap_or_default().to_string();
    let expected = fake_dir().join("libpam.so.0");
    assert!(line.contains(expected.to_str().unwrap()), "libpam.so.0 not the fake: {line:?}");
}

type Packets = Vec<(u8, Vec<u8>)>;

fn frame(responses: &[&[u8]]) -> Vec<u8> {
    responses.iter().flat_map(|r| (r.len() as u32).to_be_bytes().into_iter().chain(r.iter().copied())).collect()
}

fn run_raw(binary: &Path, input: &[u8], args: &[&str], env: &[(&str, &str)]) -> (i32, Vec<u8>) {
    assert_fake_loaded(binary);
    let mut child = command(binary, env).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || { let _ = stdin.write_all(&data); });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "helper timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = writer.join();
    let output = child.wait_with_output().unwrap();
    assert!(output.stderr.is_empty(), "helper wrote to stderr");
    (output.status.code().unwrap_or(-1), output.stdout)
}

fn parse(mut output: &[u8]) -> Packets {
    let mut packets = Vec::new();
    while output.len() >= 5 {
        let size = u32::from_be_bytes(output[1..5].try_into().unwrap()) as usize;
        packets.push((output[0], output[5..5 + size].to_vec()));
        output = &output[5 + size..];
    }
    assert!(output.is_empty(), "trailing bytes");
    packets
}

fn authenticate(binary: &Path, responses: &[&[u8]], args: &[&str], env: &[(&str, &str)]) -> (i32, Packets) {
    let (code, out) = run_raw(binary, &frame(responses), args, env);
    (code, parse(&out))
}

fn ok() -> (u8, Vec<u8>) { (5, b"ok".to_vec()) }

#[test]
fn success_requires_password_and_second_factor() {
    let (code, packets) = authenticate(&rust_helper(), &[b"fixture password", b"123456"], &[], &[]);
    assert_eq!(code, 0);
    assert_eq!(packets.iter().map(|p| p.0).collect::<Vec<_>>(), [4, 1, 2, 5]);
    assert_eq!(packets[0].1, b"Fixture only");
    assert_eq!(packets[1].1, b"Password:");
    assert_eq!(packets[2].1, b"Second factor:");
    assert_eq!(packets.last().unwrap(), &ok());
}

#[test]
fn password_only_and_fingerprint_use_fixed_policies_and_reject_other_arguments() {
    let helper = rust_helper();
    let env = [("FIXTURE_SERVICE", "tessel-lock-password")];
    let (code, packets) = authenticate(&helper, &[b"fixture password", b"123456"], &["--password-only"], &env);
    assert_eq!((code, packets.last().cloned()), (0, Some(ok())));
    let (code, _) = authenticate(&helper, &[b"fixture password", b"123456"], &[], &env);
    assert_ne!(code, 0, "default remains the tessel-lock policy");
    let env = [("FIXTURE_SERVICE", "tessel-lock-fingerprint")];
    let (code, packets) = authenticate(&helper, &[b"fixture password", b"123456"], &["--fingerprint"], &env);
    assert_eq!((code, packets.last().cloned()), (0, Some(ok())));
    let env = [("FIXTURE_SERVICE", "tessel-lock-fingerprint"), ("FIXTURE_ACCOUNT_DENIED", "1")];
    let (code, packets) = authenticate(&helper, &[b"fixture password", b"123456"], &["--fingerprint"], &env);
    assert_ne!(code, 0, "fingerprint success still needs account validity");
    assert!(!packets.contains(&ok()));
    for args in [&["--service", "login"][..], &["login"], &["--password-only", "x"], &["--fingerprint", "--password-only"],
                 &["--password"], &["--PASSWORD-ONLY"], &[""], &["--fingerprint="]] {
        let (code, out) = run_raw(&helper, &frame(&[b"fixture password", b"123456"]), args, &[]);
        assert_eq!(code, 1, "{args:?}");
        assert!(out.is_empty(), "{args:?} must exit before any PAM output");
    }
}

/// Regression: like the C helper, a vanished client (closed stdout) kills the
/// helper with SIGPIPE (the Rust runtime would otherwise ignore SIGPIPE, and
/// PAM modules' children would inherit SIG_IGN).
#[test]
fn closed_stdout_kills_the_helper_with_sigpipe_like_c() {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::ExitStatusExt;
    for helper in [rust_helper()] {
        assert_fake_loaded(&helper);
        let mut fds = [0; 2];
        // SAFETY: pipe(2) into a 2-element array.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: both fds were just created and are owned here.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        drop(read);
        let status = command(&helper, &[]).stdin(Stdio::null()).stdout(Stdio::from(write)).stderr(Stdio::null())
            .status().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGPIPE), "{helper:?}: {status:?}");
    }
}

#[test]
fn authenticates_only_the_invoking_uid() {
    let (_, packets) = authenticate(&rust_helper(), &[], &[], &[("FIXTURE_MODE", "user"), ("USER", "root"), ("LOGNAME", "root")]);
    // SAFETY: getpwuid on the current uid.
    let name = unsafe { std::ffi::CStr::from_ptr((*libc::getpwuid(libc::getuid())).pw_name) }.to_bytes().to_vec();
    assert_eq!(packets, vec![(4, name)], "username comes from getuid(), never the environment");
}

#[test]
fn wrong_missing_and_malformed_responses_fail_closed() {
    let big = vec![b'x'; 4097];
    let cases: Vec<Vec<&[u8]>> = vec![vec![b"wrong"], vec![b"fixture password", b"bad token"], vec![],
        vec![b"fixture password"], vec![b"fixture password\0extra"], vec![&big]];
    for values in cases {
        let (code, packets) = authenticate(&rust_helper(), &values, &[], &[]);
        assert_ne!(code, 0, "{:?}", values.iter().map(|v| v.len()).collect::<Vec<_>>());
        assert!(!packets.contains(&ok()));
    }
    // Truncated length header and truncated body.
    for input in [vec![0u8, 0], vec![0, 0, 0, 16, b'f', b'i']] {
        let (code, out) = run_raw(&rust_helper(), &input, &[], &[]);
        assert_ne!(code, 0);
        assert!(!parse(&out).contains(&ok()));
    }
}

#[test]
fn a_4096_byte_secret_is_the_maximum() {
    let max = vec![b'x'; 4096];
    let (code, packets) = authenticate(&rust_helper(), &[&max], &[], &[("FIXTURE_MODE", "max-secret")]);
    assert_eq!((code, packets.last().cloned()), (0, Some(ok())));
}

#[test]
fn correct_password_does_not_unlock_a_disabled_account() {
    let (code, packets) = authenticate(&rust_helper(), &[b"fixture password", b"123456"], &[], &[("FIXTURE_ACCOUNT_DENIED", "1")]);
    assert_ne!(code, 0);
    assert!(!packets.contains(&ok()));
}

#[test]
fn message_count_and_style_limits() {
    let helper = rust_helper();
    let (code, packets) = authenticate(&helper, &[], &[], &[("FIXTURE_MODE", "too-many")]);
    assert_ne!(code, 0);
    assert!(packets.is_empty(), "33 messages are rejected before any is sent");
    let (code, packets) = authenticate(&helper, &[], &[], &[("FIXTURE_MODE", "zero")]);
    assert_ne!(code, 0);
    assert!(packets.is_empty());
    let (code, packets) = authenticate(&helper, &[], &[], &[("FIXTURE_MODE", "max")]);
    assert_eq!(code, 0, "PAM_MAX_NUM_MSG (32) is allowed");
    assert_eq!(packets.len(), 33);
    let (code, packets) = authenticate(&helper, &[], &[], &[("FIXTURE_MODE", "bad-style")]);
    assert_ne!(code, 0);
    assert!(packets.is_empty(), "invalid styles are never forwarded");
}

#[test]
fn long_messages_are_truncated_to_4096_bytes() {
    let (code, packets) = authenticate(&rust_helper(), &[], &[], &[("FIXTURE_MODE", "long")]);
    assert_eq!(code, 0);
    assert_eq!(packets[0].0, 3);
    assert_eq!(packets[0].1, vec![b'm'; 4096]);
}

#[test]
fn core_limit_is_zero_and_helper_is_not_setuid() {
    let helper = rust_helper();
    assert_fake_loaded(&helper);
    let mut child = command(&helper, &[]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    // Wait until the helper is blocked reading the first response.
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 5];
    use std::io::Read;
    stdout.read_exact(&mut first).unwrap();
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap_or_default();
    let core = limits.lines().find(|l| l.starts_with("Max core file size")).unwrap_or_default().to_string();
    let _ = child.kill();
    let _ = child.wait();
    let fields: Vec<&str> = core.split_whitespace().collect();
    assert_eq!(fields.get(4..6), Some(&["0", "0"][..]), "{core}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&helper).unwrap().permissions().mode();
    assert_eq!(mode & 0o6000, 0, "helper must never be setuid/setgid");
}

#[test]
fn dumpable_flag_is_cleared() {
    let helper = rust_helper();
    assert_fake_loaded(&helper);
    let mut child = command(&helper, &[]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 5];
    use std::io::Read;
    stdout.read_exact(&mut first).unwrap();
    // Non-dumpable processes get root-owned /proc/<pid> entries.
    use std::os::unix::fs::MetadataExt;
    let owner = std::fs::metadata(format!("/proc/{}/mem", child.id())).map(|m| m.uid()).unwrap_or(u32::MAX);
    let environ = std::fs::read(format!("/proc/{}/environ", child.id()));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(owner, 0, "PR_SET_DUMPABLE 0 makes /proc/<pid>/mem root-owned");
    assert!(environ.is_err(), "environment of the helper must not be readable");
}

/// Every executable script is written once inside the serialized fixture
/// init: writing an exec'd file while another test thread forks causes ETXTBSY.
fn write_scripts(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut scripts: Vec<(String, String)> = Vec::new();
    let wrap = |binary: &Path, env: &[(&str, &str)]| {
        let exports: String = env.iter().map(|(k, v)| format!("export {k}='{v}'\n")).collect();
        format!("#!/bin/sh\nexport LD_LIBRARY_PATH='{}'\n{exports}exec '{}' \"$@\"\n", dir.display(), binary.display())
    };
    let helpers = [("rust", rust_helper())];
    for (name, binary) in &helpers {
        scripts.push((format!("{name}-plain"), wrap(binary, &[])));
        scripts.push((format!("{name}-denied"), wrap(binary, &[("FIXTURE_ACCOUNT_DENIED", "1")])));
        scripts.push((format!("{name}-password"), wrap(binary, &[("FIXTURE_SERVICE", "tessel-lock-password")])));
        scripts.push((format!("{name}-fingerprint"), wrap(binary, &[("FIXTURE_SERVICE", "tessel-lock-fingerprint")])));
    }
    let packet = |body: &str, code: i32| format!("#!/bin/sh\nprintf '{body}'\nexit {code}\n");
    scripts.push(("liar-status".into(), packet("\\005\\000\\000\\000\\002ok", 1)));
    scripts.push(("liar-oversize".into(), packet("\\004\\000\\000\\020\\001", 0)));
    scripts.push(("liar-type".into(), packet("\\007\\000\\000\\000\\000\\005\\000\\000\\000\\002ok", 0)));
    scripts.push(("liar-payload".into(), packet("\\005\\000\\000\\000\\003okk", 0)));
    scripts.push(("honest".into(), packet("\\005\\000\\000\\000\\002ok", 0)));
    for (name, text) in scripts {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn script(name: &str) -> PathBuf {
    let path = fake_dir().join(name);
    assert!(path.is_file(), "{name}");
    path
}

fn drive(helper: PathBuf, answers: fn(&str, bool) -> &'static str, configure: impl FnOnce(&dyn tessel_lock::auth::Transaction))
    -> (Vec<bool>, Vec<(String, bool)>, Vec<String>) {
    use tessel_lock::auth::{Authentication, Callbacks, Transaction};
    use std::cell::RefCell;
    use std::rc::Rc;
    let results = Rc::new(RefCell::new(Vec::new()));
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let messages = Rc::new(RefCell::new(Vec::new()));
    let client: Rc<RefCell<Option<Authentication>>> = Rc::new(RefCell::new(None));
    let (r, p, m, c) = (results.clone(), prompts.clone(), messages.clone(), client.clone());
    let auth = Authentication::with_helper(helper, Callbacks {
        prompt: Box::new(move |message, echo| {
            p.borrow_mut().push((message.to_string(), echo));
            let answer = answers(message, echo);
            let client = c.borrow().clone().unwrap();
            client.respond(answer);
        }),
        message: Box::new(move |message| m.borrow_mut().push(message.to_string())),
        complete: Box::new(move |success| r.borrow_mut().push(success)),
    });
    *client.borrow_mut() = Some(auth.clone());
    configure(&auth);
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    let _default = context.with_thread_default(|| auth.start()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while results.borrow().is_empty() && Instant::now() < deadline {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
    client.borrow_mut().take();
    let out = (results.borrow().clone(), prompts.borrow().clone(), messages.borrow().clone());
    out
}

fn right(_: &str, echo: bool) -> &'static str { if echo { "123456" } else { "fixture password" } }
fn wrong(_: &str, _: bool) -> &'static str { "wrong" }
fn nul(_: &str, _: bool) -> &'static str { "fixture\0password" }

#[test]
fn rust_client_completes_real_native_conversation() {
    let helpers = [("rust", rust_helper())];
    for (name, binary) in helpers {
        assert_fake_loaded(&binary);
        let (results, prompts, messages) = drive(script(&format!("{name}-plain")), right, |_| {});
        assert_eq!(results, [true], "{name}");
        assert_eq!(prompts, [("Password:".to_string(), false), ("Second factor:".to_string(), true)], "{name}");
        assert_eq!(messages, ["Fixture only"], "{name}");
        let (results, _, _) = drive(script(&format!("{name}-plain")), wrong, |_| {});
        assert_eq!(results, [false]);
        let (results, prompts, _) = drive(script(&format!("{name}-plain")), nul, |_| {});
        assert_eq!(results, [false], "NUL secrets are refused client-side");
        assert_eq!(prompts.len(), 1);
        let (results, _, _) = drive(script(&format!("{name}-denied")), right, |_| {});
        assert_eq!(results, [false], "account checks gate success");
    }
}

#[test]
fn rust_client_selects_the_password_only_and_fingerprint_policies() {
    assert_fake_loaded(&rust_helper());
    let (results, _, _) = drive(script("rust-password"), right, |a| a.set_password_only(true));
    assert_eq!(results, [true]);
    let (results, _, _) = drive(script("rust-password"), right, |_| {});
    assert_eq!(results, [false], "default policy is tessel-lock");
    let (results, _, _) = drive(script("rust-fingerprint"), right, |a| { a.set_fingerprint(true); a.set_password_only(true) });
    assert_eq!(results, [true], "--fingerprint wins over --password-only");
}

#[test]
fn ok_packet_without_exit_status_zero_is_not_success() {
    // (5,"ok") with exit status 1 must never count as success.
    assert_eq!(drive(script("liar-status"), right, |_| {}).0, [false]);
    // Oversized, unknown and malformed packets fail closed.
    assert_eq!(drive(script("liar-oversize"), right, |_| {}).0, [false]);
    assert_eq!(drive(script("liar-type"), right, |_| {}).0, [false]);
    assert_eq!(drive(script("liar-payload"), right, |_| {}).0, [false]);
    assert_eq!(drive(script("honest"), right, |_| {}).0, [true], "control: ok + status 0");
}

#[test]
fn cancel_terminates_the_helper_and_suppresses_completion() {
    use tessel_lock::auth::{Authentication, Callbacks, Transaction};
    use std::cell::RefCell;
    use std::rc::Rc;
    let results = Rc::new(RefCell::new(Vec::new()));
    let prompted = Rc::new(std::cell::Cell::new(false));
    let (r, p) = (results.clone(), prompted.clone());
    let auth = Authentication::with_helper(script("rust-plain"), Callbacks {
        prompt: Box::new(move |_, _| p.set(true)), message: Box::new(|_| {}),
        complete: Box::new(move |s| r.borrow_mut().push(s)) });
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    context.with_thread_default(|| auth.start()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !prompted.get() && Instant::now() < deadline {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(prompted.get() && auth.waiting());
    auth.cancel();
    assert!(!auth.waiting());
    auth.respond("fixture password"); // ignored once cancelled
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(results.borrow().is_empty(), "cancelled transactions never complete");
}
