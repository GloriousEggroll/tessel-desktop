//! test_login.py ProtocolTests + extra fail-closed cases. greetd is only ever a
//! fake socket server in a private temporary directory.
use super::*;
use serde_json::json;
use std::os::unix::net::UnixListener;
use std::time::Instant;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reference frames produced by Python's `exchange()` (json.dumps(ensure_ascii=False)
/// + struct '=I'), captured with CPython 3.14 in an isolated env.
const PY_CANCEL: &str = "1a0000007b2274797065223a202263616e63656c5f73657373696f6e227d";
const PY_CREATE: &str = "410000007b2274797065223a20226372656174655f73657373696f6e222c2022757365726e616d65223a20226a6f73c3a9205c22715c225c5c5c75303030317fe280a8227d";
const PY_RESPOND: &str = "500000007b2274797065223a2022706f73745f617574685f6d6573736167655f726573706f6e7365222c2022726573706f6e7365223a202270c3a473735c74775c6e5c725c625c665c7530303166f09f9880227d";
const PY_ACK: &str = "260000007b2274797065223a2022706f73745f617574685f6d6573736167655f726573706f6e7365227d";
const PY_START_LABWC: &str = "1f0100007b2274797065223a202273746172745f73657373696f6e222c2022636d64223a205b222f7573722f6c6f63616c2f6c69622f7761796c6f67696e2f73746172742d73657373696f6e206c61627763202d2d7374617274757020272f7573722f62696e2f707974686f6e33202d49202f7573722f6c6f63616c2f6c69622f7761796c6f67696e2f73657373696f6e5f7472616e736974696f6e2e707927225d2c2022656e76223a205b225844475f53455353494f4e5f545950453d7761796c616e64222c20225844475f53455353494f4e5f4445534b544f503d6c61627763222c20225844475f43555252454e545f4445534b544f503d6c616277633a776c726f6f7473222c20224445534b544f505f53455353494f4e3d6c61627763225d7d";
const PY_START_PLASMA: &str = "ec0000007b2274797065223a202273746172745f73657373696f6e222c2022636d64223a205b222f7573722f6c6f63616c2f6c69622f7761796c6f67696e2f73746172742d73657373696f6e20272f7573722f62696e2f737461727420706c61736d612720272d2d783d24484f4d45272027c3bc27225d2c2022656e76223a205b225844475f53455353494f4e5f545950453d7761796c616e64222c20225844475f53455353494f4e5f4445534b544f503d706c61736d61222c20225844475f43555252454e545f4445534b544f503d4b4445222c20224445534b544f505f53455353494f4e3d706c61736d61225d7d";

fn session(ident: &str, name: &str, argv: &[&str], desktops: &str) -> Session {
    Session { ident: ident.into(), name: name.into(), argv: argv.iter().map(|s| s.to_string()).collect(), desktops: desktops.into() }
}

fn labwc() -> Session {
    session("labwc", "labwc", &["labwc"], "labwc:wlroots")
}

#[test]
fn frames_are_byte_identical_to_python_exchange() {
    let frame = |request: Request| hex(&request.frame().unwrap());
    assert_eq!(frame(Request::CancelSession), PY_CANCEL);
    assert_eq!(frame(Request::CreateSession("josé \"q\"\\\u{1}\u{7f}\u{2028}".into())), PY_CREATE);
    assert_eq!(frame(Request::Respond(Zeroizing::new("päss\tw\n\r\u{8}\u{c}\u{1f}😀".into()))), PY_RESPOND);
    assert_eq!(frame(Request::Acknowledge), PY_ACK);
    assert_eq!(frame(Request::StartSession(labwc().request())), PY_START_LABWC);
    assert_eq!(frame(Request::StartSession(session("plasma", "Plasma ü", &["/usr/bin/start plasma", "--x=$HOME", "ü"], "KDE").request())),
               PY_START_PLASMA);
    // Exact capacity: no reallocation copied the secret.
    let payload = Request::Respond(Zeroizing::new("x\u{1}".repeat(700))).payload();
    assert_eq!(payload.len(), payload.capacity());
    assert_eq!(format!("{:?}", Request::Respond(Zeroizing::new("secret".into()))), "post_auth_message_response(<redacted>)");
}

#[test]
fn oversized_request_is_never_sent() {
    let huge = session("x", "x", &["/bin/true", &"a".repeat(MAX_FRAME)], "x");
    let error = Request::StartSession(huge.request()).frame().unwrap_err();
    assert_eq!(error.to_string(), "Login request is too large");
    let (mut left, mut right) = UnixStream::pair().unwrap();
    assert!(exchange(&mut left, &Request::StartSession(huge.request())).is_err());
    drop(left);
    let mut sent = Vec::new();
    right.read_to_end(&mut sent).unwrap();
    assert!(sent.is_empty());
}

#[test]
fn malformed_backend_frames_fail_closed() {
    let frame = |payload: &[u8]| [&(payload.len() as u32).to_ne_bytes()[..], payload].concat();
    let cases: Vec<(Vec<u8>, &str)> = vec![
        ((1u32 << 30).to_ne_bytes().to_vec(), "Invalid login backend message size"),
        (((MAX_FRAME + 1) as u32).to_ne_bytes().to_vec(), "Invalid login backend message size"),
        (0u32.to_ne_bytes().to_vec(), "Invalid login backend message size"),
        (frame(b"[]"), "Invalid login backend response"),
        (frame(b"!"), "Invalid login backend response"),
        (frame(b"{\"type\": \"other\"}"), "Invalid login backend response"),
        (frame(b"{\"type\": 1}"), "Invalid login backend response"),
        (frame(b"{\"kind\": \"success\"}"), "Invalid login backend response"),
        (frame(b"\"success\""), "Invalid login backend response"),
        (vec![1, 0], "Login backend disconnected"),
        ([&20u32.to_ne_bytes()[..], b"{\"type\": \"succ"].concat(), "Login backend disconnected"),
        ([&8u32.to_ne_bytes()[..], b"{}"].concat(), "Login backend disconnected"),
    ];
    for (data, message) in cases {
        let (mut left, mut right) = UnixStream::pair().unwrap();
        right.write_all(&data).unwrap();
        right.shutdown(std::net::Shutdown::Write).unwrap();
        let error = exchange(&mut left, &Request::CancelSession).unwrap_err();
        assert_eq!(error.to_string(), message, "{data:?}");
        drop(left);
        let mut sent = Vec::new();
        right.read_to_end(&mut sent).unwrap();
        assert_eq!(hex(&sent), PY_CANCEL);
    }
    // A valid fragmented reply still parses.
    let (mut left, mut right) = UnixStream::pair().unwrap();
    let writer = std::thread::spawn(move || {
        for chunk in frame(br#"{"type": "success"}"#).chunks(3) {
            right.write_all(chunk).unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        right
    });
    assert_eq!(exchange(&mut left, &Request::CancelSession).unwrap()["type"], "success");
    drop(writer.join());
}

enum Reply {
    Json(Value),
    Raw(Vec<u8>),
    Hangup,
}

/// Fake greetd: accepts connections, records each request frame and answers
/// from the script, fragmenting header and payload into 3-byte writes.
struct Server {
    path: PathBuf,
    root: PathBuf,
    thread: Option<std::thread::JoinHandle<Result<Vec<(usize, Vec<u8>)>, String>>>,
}

fn scratch(name: &str) -> PathBuf {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("wl-{}-{}-{name}", std::process::id(), COUNT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

impl Server {
    fn new(name: &str, answers: Vec<Reply>) -> Server {
        let root = scratch(name);
        let path = root.join("greetd.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            let mut requests = Vec::new();
            let mut connection = 0;
            let deadline = Instant::now() + Duration::from_secs(4);
            'accept: while Instant::now() < deadline {
                let mut channel = match listener.accept() {
                    Ok((channel, _)) => channel,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error.to_string()),
                };
                connection += 1;
                channel.set_nonblocking(false).unwrap();
                channel.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
                loop {
                    let mut header = [0u8; 4];
                    if channel.read_exact(&mut header).is_err() {
                        if answers.len() == 0 {
                            return Ok(requests);
                        }
                        continue 'accept;
                    }
                    let mut payload = vec![0u8; u32::from_ne_bytes(header) as usize];
                    channel.read_exact(&mut payload).map_err(|e| e.to_string())?;
                    requests.push((connection, [&header[..], &payload].concat()));
                    let packet = match answers.next() {
                        None => return Ok(requests),
                        Some(Reply::Hangup) => continue 'accept,
                        Some(Reply::Raw(bytes)) => bytes,
                        Some(Reply::Json(value)) => {
                            let data = serde_json::to_vec(&value).unwrap();
                            [&(data.len() as u32).to_ne_bytes()[..], &data].concat()
                        }
                    };
                    for chunk in packet.chunks(3) {
                        if channel.write_all(chunk).is_err() {
                            continue 'accept;
                        }
                    }
                }
            }
            Ok(requests)
        });
        Server { path, root, thread: Some(thread) }
    }

    fn finish(mut self) -> Vec<(usize, Value)> {
        let requests = self.thread.take().unwrap().join().unwrap().unwrap();
        requests.into_iter().map(|(connection, frame)| (connection, serde_json::from_slice(&frame[4..]).unwrap())).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn success() -> Reply {
    Reply::Json(json!({"type": "success"}))
}

fn prompt(kind: &str, message: &str) -> Reply {
    Reply::Json(json!({"type": "auth_message", "auth_message_type": kind, "auth_message": message}))
}

type Events = Rc<RefCell<Vec<(String, String)>>>;

/// `ProtocolTests.exercise`: answers prompts from `inputs`, starts the session
/// on `authenticated`, runs until error/started.
fn exercise(name: &str, replies: Vec<Reply>, inputs: &[&str]) -> (Vec<(String, String)>, Vec<Value>) {
    let server = Server::new(name, replies);
    let (sender, receiver) = mpsc::channel::<Completion>();
    let sender = Mutex::new(sender);
    let events: Events = Rc::default();
    let slot: Rc<RefCell<Option<Client>>> = Rc::default();
    let answers = Rc::new(RefCell::new(inputs.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()));
    let client = {
        let (events, slot, answers) = (events.clone(), slot.clone(), answers.clone());
        Client::new(&server.path, move |completion| { let _ = sender.lock().unwrap().send(completion); }, move |kind, text| {
            events.borrow_mut().push((kind.into(), text.into()));
            let client = slot.borrow().clone().unwrap();
            if kind == "secret" || kind == "visible" {
                client.respond(Zeroizing::new(answers.borrow_mut().next().unwrap()));
            } else if kind == "authenticated" {
                client.start_session(client.generation());
            }
        })
    };
    *slot.borrow_mut() = Some(client.clone());
    client.begin("fixture", Some(labwc()));
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline && !matches!(client.state(), State::Error | State::Started) {
        let completion = receiver.recv_timeout(Duration::from_secs(4)).unwrap();
        client.result(completion);
    }
    client.close();
    slot.borrow_mut().take();
    let requests = server.finish().into_iter().map(|(_, value)| value).collect();
    let events = events.borrow().clone();
    (events, requests)
}

fn kinds(requests: &[Value]) -> Vec<&str> {
    requests.iter().map(|r| r["type"].as_str().unwrap()).collect()
}

#[test]
fn password_second_factor_and_info_before_session_start() {
    let (events, requests) = exercise("2fa", vec![success(), prompt("info", "Sign in"), prompt("secret", "Password:"),
        prompt("visible", "Verification code:"), success(), success()], &["fixture password", "123456"]);
    assert_eq!(kinds(&requests), ["cancel_session", "create_session", "post_auth_message_response", "post_auth_message_response",
                                  "post_auth_message_response", "start_session"]);
    assert_eq!(requests[1]["username"], "fixture");
    assert!(requests[2].get("response").is_none());
    assert_eq!(requests[3]["response"], "fixture password");
    assert_eq!(requests[4]["response"], "123456");
    assert_eq!(Value::Object(labwc().request()), requests[5]);
    assert_eq!(events, [("busy", "Preparing sign-in…"), ("message", "Sign in"), ("secret", "Password:"), ("busy", "Signing in…"),
                        ("visible", "Verification code:"), ("busy", "Signing in…"), ("authenticated", "Starting your session…"),
                        ("started", "")].map(|(a, b)| (a.to_string(), b.to_string())));
}

#[test]
fn wrong_password_cannot_start_session() {
    let (events, requests) = exercise("wrong", vec![success(), prompt("secret", "Password:"),
        Reply::Json(json!({"type": "error", "error_type": "auth_error", "description": "denied"}))], &["wrong"]);
    assert!(!kinds(&requests).contains(&"start_session"));
    assert_eq!(events.last().unwrap(), &("error".to_string(), "Sign-in failed. Check your credentials and try again.".to_string()));
}

#[test]
fn backend_error_description_is_shown_without_session() {
    let (events, requests) = exercise("descr", vec![success(), Reply::Json(json!({"type": "error", "error_type": "error",
        "description": "Account locked"}))], &[]);
    assert_eq!(kinds(&requests), ["cancel_session", "create_session"]);
    assert_eq!(events.last().unwrap().1, "Account locked");
    let (events, _) = exercise("nodescr", vec![success(), Reply::Json(json!({"type": "error"}))], &[]);
    assert_eq!(events.last().unwrap().1, "Sign-in could not be completed.");
    let (events, _) = exercise("nulldescr", vec![success(), Reply::Json(json!({"type": "error", "description": null}))], &[]);
    assert_eq!(events.last().unwrap().1, "None");
}

#[test]
fn backend_disconnect_cannot_start_session() {
    let (events, requests) = exercise("disconnect", vec![success(), prompt("secret", "Password:"), Reply::Hangup], &["wrong"]);
    assert_eq!(events.last().unwrap(), &("error".to_string(), UNAVAILABLE.to_string()));
    assert!(!kinds(&requests).contains(&"start_session"));
}

#[test]
fn malformed_reply_mid_conversation_fails_closed() {
    for (name, reply) in [("big", Reply::Raw(((MAX_FRAME + 1) as u32).to_ne_bytes().to_vec())),
                          ("junk", Reply::Raw([&3u32.to_ne_bytes()[..], b"{x}"].concat())),
                          ("type", Reply::Json(json!({"type": "session_started"})))] {
        let (events, requests) = exercise(name, vec![success(), prompt("secret", "Password:"), reply], &["secret"]);
        assert_eq!(events.last().unwrap().1, UNAVAILABLE, "{name}");
        assert!(!kinds(&requests).contains(&"start_session"));
    }
}

#[test]
fn unexpected_and_unsupported_messages_fail_closed() {
    // auth_message in the reset phase.
    let (events, requests) = exercise("unexpected", vec![prompt("secret", "Password:")], &[]);
    assert_eq!(events.last().unwrap().1, "Unexpected response from the login service.");
    assert_eq!(kinds(&requests), ["cancel_session"]);
    // auth_message in the start phase.
    let (events, requests) = exercise("startmsg", vec![success(), success(), prompt("info", "late")], &[]);
    assert_eq!(kinds(&requests), ["cancel_session", "create_session", "start_session"]);
    assert_eq!(events.last().unwrap().1, "Unexpected response from the login service.");
    // Unknown prompt kind: cancel, then error (state stays idle as in Python).
    let server = Server::new("unsupported", vec![success(), prompt("fingerprint", "Touch"), success()]);
    let (sender, receiver) = mpsc::channel::<Completion>();
    let sender = Mutex::new(sender);
    let events: Events = Rc::default();
    let log = events.clone();
    let client = Client::new(&server.path, move |c| { let _ = sender.lock().unwrap().send(c); },
                             move |k, t| log.borrow_mut().push((k.into(), t.into())));
    client.begin("fixture", Some(labwc()));
    for _ in 0..2 {
        client.result(receiver.recv_timeout(Duration::from_secs(4)).unwrap());
    }
    assert_eq!(client.state(), State::Idle);
    assert_eq!(events.borrow().last().unwrap().1, "Unsupported authentication prompt.");
    let _ = receiver.recv_timeout(Duration::from_secs(4)); // the cancel's reply, ignored
    client.close();
    let requests: Vec<Value> = server.finish().into_iter().map(|(_, v)| v).collect();
    assert_eq!(kinds(&requests), ["cancel_session", "create_session", "cancel_session"]);
}

#[test]
fn missing_socket_reports_unavailable() {
    let root = scratch("missing");
    let (sender, receiver) = mpsc::channel::<Completion>();
    let sender = Mutex::new(sender);
    let events: Events = Rc::default();
    let log = events.clone();
    let client = Client::new(root.join("none.sock"), move |c| { let _ = sender.lock().unwrap().send(c); },
                             move |k, t| log.borrow_mut().push((k.into(), t.into())));
    client.begin("fixture", Some(labwc()));
    client.result(receiver.recv_timeout(Duration::from_secs(4)).unwrap());
    assert_eq!(client.state(), State::Error);
    assert_eq!(events.borrow().last().unwrap(), &("error".to_string(), UNAVAILABLE.to_string()));
    client.close();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cancel_race_never_creates_or_starts_a_session() {
    // begin() then cancel() before any result: the reset result is stale, so
    // no create_session follows; every reply is ignored.
    let server = Server::new("race", vec![success(), success(), success()]);
    let (sender, receiver) = mpsc::channel::<Completion>();
    let sender = Mutex::new(sender);
    let events: Events = Rc::default();
    let log = events.clone();
    let client = Client::new(&server.path, move |c| { let _ = sender.lock().unwrap().send(c); },
                             move |k, t| log.borrow_mut().push((k.into(), t.into())));
    client.begin("fixture", Some(labwc()));
    client.cancel();
    let first_generation = client.generation();
    while let Ok(completion) = receiver.recv_timeout(Duration::from_millis(800)) {
        assert!(completion.serial <= first_generation);
        client.result(completion);
    }
    assert_eq!(client.state(), State::Idle);
    assert_eq!(events.borrow().as_slice(), [("busy".to_string(), "Preparing sign-in…".to_string())]);
    client.close();
    let requests: Vec<Value> = server.finish().into_iter().map(|(_, v)| v).collect();
    assert!(kinds(&requests).iter().all(|k| *k == "cancel_session"), "{requests:?}");
}

#[test]
fn stale_authentication_success_after_cancel_is_ignored() {
    let client = Client::new("/no/socket", |_| {}, |_, _| panic!("stale callback delivered"));
    client.force(State::Idle, 2);
    let mut success = Map::new();
    success.insert("type".into(), "success".into());
    client.result(Completion { serial: 1, phase: Phase::Auth, response: Some(success.clone()) });
    client.result(Completion { serial: 2, phase: Phase::Cancel, response: Some(success) });
    client.result(Completion { serial: 2, phase: Phase::Auth, response: None });
    assert_eq!(client.state(), State::Idle);
    client.close();
}

#[test]
fn fade_completion_cannot_launch_without_current_authentication() {
    let client = Client::new("/no/socket", |_| {}, |_, _| {});
    client.set_session(session("test", "Test", &["/bin/true"], "Test"));
    let calls: Rc<RefCell<Vec<(Value, Phase)>>> = Rc::default();
    let log = calls.clone();
    client.set_hook(Some(Box::new(move |request, phase| log.borrow_mut().push((request.to_value().unwrap(), phase)))));
    for (state, generation) in [(State::Idle, 3), (State::Prompt, 3), (State::Error, 3), (State::Authenticating, 3)] {
        client.force(state, 3);
        client.start_session(generation);
    }
    client.force(State::Ready, 3);
    client.start_session(2);
    assert!(calls.borrow().is_empty());
    client.start_session(3);
    client.start_session(3);
    assert_eq!(client.state(), State::Launching);
    assert_eq!(calls.borrow().as_slice(), [(Value::Object(session("test", "Test", &["/bin/true"], "Test").request()), Phase::Start)]);
    // Launching cannot be cancelled or restarted.
    client.cancel();
    client.begin("other", None);
    assert_eq!(client.state(), State::Launching);
    assert_eq!(calls.borrow().len(), 1);
    client.close();
    client.force(State::Ready, client.generation());
    client.start_session(client.generation());
    assert_eq!(calls.borrow().len(), 1);
}

#[test]
fn invalid_credentials_are_rejected_before_transmission() {
    let events: Events = Rc::default();
    let log = events.clone();
    let client = Client::new("/no/socket", |_| {}, move |k, t| log.borrow_mut().push((k.into(), t.into())));
    let calls: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let log = calls.clone();
    client.set_hook(Some(Box::new(move |request, _| log.borrow_mut().push(request.kind()))));
    for username in ["", &"x".repeat(257), "user\0suffix", &"é".repeat(129)] {
        client.begin(username, None);
        assert_eq!(client.state(), State::Error);
        assert_eq!(events.borrow().last().unwrap().1, "Enter a valid username.");
    }
    for secret in ["x".repeat(4097), "password\0suffix".into(), "é".repeat(2049)] {
        client.force(State::Prompt, client.generation());
        client.respond(Zeroizing::new(secret));
        assert_eq!(client.state(), State::Error);
        assert_eq!(events.borrow().last().unwrap().1, "The authentication response is invalid or too long.");
    }
    assert!(calls.borrow().iter().all(|kind| *kind == "cancel_session"));
    // Limits are inclusive: 256-byte usernames and 4096-byte answers are sent.
    calls.borrow_mut().clear();
    client.begin(&"x".repeat(256), None);
    assert_eq!(client.state(), State::Resetting);
    client.force(State::Prompt, client.generation());
    client.respond(Zeroizing::new("é".repeat(2048)));
    assert_eq!(client.state(), State::Authenticating);
    assert_eq!(calls.borrow().as_slice(), ["cancel_session", "post_auth_message_response"]);
    // Answers are ignored outside a prompt.
    client.respond(Zeroizing::new("late".into()));
    assert_eq!(calls.borrow().len(), 2);
    client.close();
}
