//! Port of test_secret_agent.py AgentTests: an unregistered agent core with a
//! fake keyring and fake prompts; never touches real networks or the keyring.

use super::*;
use crate::keyring::storable_secrets;
use crate::libnm::Setting;

#[derive(Default)]
struct FakeKeyring {
    values: RefCell<Vec<(String, String)>>,
    delayed: Cell<bool>,
    pending: RefCell<Vec<Box<dyn FnOnce(Vec<(String, String)>, Option<String>)>>>,
    saved: RefCell<Vec<Vec<(String, String, String)>>>,
    deleted: RefCell<Vec<String>>,
    lookups: RefCell<Vec<(String, bool)>>,
}

impl Keyring for FakeKeyring {
    fn lookup(&self, _uuid: &str, setting: &str, interactive: bool, done: Box<dyn FnOnce(Vec<(String, String)>, Option<String>)>) {
        self.lookups.borrow_mut().push((setting.into(), interactive));
        if self.delayed.get() {
            self.pending.borrow_mut().push(done);
        } else {
            done(self.values.borrow().clone(), None);
        }
    }
    fn save(&self, connection: &Connection, setting_names: Option<Vec<String>>, done: Box<dyn FnOnce(Option<String>)>) {
        let mut values = storable_secrets(connection);
        if let Some(names) = setting_names {
            values.retain(|v| names.contains(&v.0));
        }
        self.saved.borrow_mut().push(values);
        done(None);
    }
    fn delete(&self, uuid: &str, done: Box<dyn FnOnce(Option<String>)>) {
        self.deleted.borrow_mut().push(uuid.into());
        done(None);
    }
}

struct Prompt {
    fields: Vec<Field>,
    remember: Option<bool>,
    done: RefCell<Option<Box<dyn FnOnce(Option<(Vec<(String, String)>, bool)>)>>>,
    destroyed: Rc<Cell<bool>>,
}

#[derive(Default)]
struct FakePrompter {
    prompts: RefCell<Vec<Rc<Prompt>>>,
    helpers: Cell<u32>,
}

struct Handle(Rc<Cell<bool>>);

impl PromptHandle for Handle {
    fn close(&self) {
        self.0.set(true);
    }
}

impl Prompter for FakePrompter {
    fn fields(&self, _title: &str, _message: &str, fields: &[Field], remember: Option<bool>,
              done: Box<dyn FnOnce(Option<(Vec<(String, String)>, bool)>)>) -> Result<Box<dyn PromptHandle>, String> {
        let destroyed = Rc::new(Cell::new(false));
        self.prompts.borrow_mut().push(Rc::new(Prompt { fields: fields.to_vec(), remember, done: RefCell::new(Some(done)), destroyed: destroyed.clone() }));
        Ok(Box::new(Handle(destroyed)))
    }
    fn before_helper(&self) {
        self.helpers.set(self.helpers.get() + 1);
    }
}

impl Prompt {
    fn respond(&self, values: Option<Vec<(&str, &str)>>) {
        let remember = self.remember.unwrap_or(false);
        let done = self.done.borrow_mut().take().unwrap();
        done(values.map(|v| (v.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect(), remember)));
    }
    fn keys(&self) -> Vec<String> {
        self.fields.iter().map(|f| f.0.clone()).collect()
    }
}

type Replies = Rc<RefCell<Vec<Result<Variant, Failure>>>>;

struct T {
    _context: crate::testing::TestContext,
    core: Rc<AgentCore>,
    keyring: Rc<FakeKeyring>,
    prompter: Rc<FakePrompter>,
    replies: Replies,
}

const ALLOW: u32 = ALLOW_INTERACTION;

fn iterate() {
    let context = glib::MainContext::ref_thread_default();
    for _ in 0..100 {
        if !context.pending() {
            break;
        }
        context.iteration(false);
    }
}

fn connection(kind: &str, flags: u32) -> Connection {
    let profile = Connection::new().unwrap();
    let identity = Setting::new("connection").unwrap();
    identity.set_str("id", Some("Synthetic credential test"));
    identity.set_str("uuid", Some(&libnm::uuid_generate()));
    identity.set_str("type", Some(kind));
    if kind == "wireguard" {
        identity.set_str("interface-name", Some("wg-test"));
    }
    profile.add_setting(&identity);
    match kind {
        "802-11-wireless" => {
            let wireless = Setting::new("802-11-wireless").unwrap();
            wireless.set_bytes("ssid", Some(b"Synthetic network"));
            wireless.set_str("mode", Some("infrastructure"));
            profile.add_setting(&wireless);
            let security = Setting::new("802-11-wireless-security").unwrap();
            security.set_str("key-mgmt", Some("wpa-psk"));
            security.set_int("psk-flags", flags as i64);
            profile.add_setting(&security);
        }
        "vpn" => {
            let vpn = Setting::new("vpn").unwrap();
            vpn.set_str("service-type", Some("org.freedesktop.NetworkManager.openvpn"));
            vpn.vpn_add_data_item("remote", "test.invalid");
            vpn.vpn_add_data_item("password-flags", &flags.to_string());
            profile.add_setting(&vpn);
        }
        "wireguard" => {
            let wireguard = Setting::new("wireguard").unwrap();
            wireguard.set_int("private-key-flags", flags as i64);
            profile.add_setting(&wireguard);
        }
        "802-3-ethernet" => {
            profile.add_setting(&Setting::new("802-3-ethernet").unwrap());
            let dot1x = Setting::new("802-1x").unwrap();
            dot1x.set_strv("eap", &["peap".into()]);
            dot1x.set_str("identity", Some("synthetic-identity"));
            dot1x.set_str("phase2-auth", Some("mschapv2"));
            dot1x.set_int("password-flags", flags as i64);
            profile.add_setting(&dot1x);
        }
        _ => {}
    }
    profile
}

fn wifi() -> Connection {
    connection("802-11-wireless", libnm::AGENT_OWNED)
}

impl T {
    fn new() -> Self {
        let context = crate::testing::TestContext::new();
        let keyring = Rc::new(FakeKeyring::default());
        let prompter = Rc::new(FakePrompter::default());
        let core = AgentCore::new(keyring.clone(), prompter.clone(), |_| {});
        Self { _context: context, core, keyring, prompter, replies: Rc::default() }
    }

    fn request(&self, profile: &Connection, path: &str, setting: &str, hints: &[&str], flags: u32) {
        let replies = self.replies.clone();
        self.core.get(&profile.to_dbus(libnm::ALL), path, setting, hints.iter().map(|h| h.to_string()).collect(), flags,
                      Box::new(move |r| replies.borrow_mut().push(r)));
    }

    fn wifi_request(&self, flags: u32) -> Connection {
        let profile = wifi();
        self.request(&profile, "/org/freedesktop/NetworkManager/Settings/99999", "802-11-wireless-security", &[], flags);
        profile
    }

    fn prompt(&self, index: usize) -> Rc<Prompt> {
        self.prompter.prompts.borrow()[index].clone()
    }

    fn secret(&self, index: usize, setting: &str, key: &str) -> Option<String> {
        let replies = self.replies.borrow();
        let value = replies[index].as_ref().ok()?;
        let values = libnm::lookup(value, setting, "a{sv}")?;
        libnm::lookup(&values, key, "s").and_then(|v| v.str().map(String::from))
    }

    fn error(&self, index: usize) -> Option<AgentError> {
        self.replies.borrow()[index].as_ref().err().map(|e| e.0)
    }
}

impl Drop for T {
    fn drop(&mut self) {
        self.core.close();
        iterate();
    }
}

#[test]
fn callbacks_work_without_registration() {
    let t = T::new();
    let profile = t.wifi_request(0);
    assert_eq!(t.error(0), Some(AgentError::NoSecrets));
    let done: Rc<RefCell<Vec<Option<Failure>>>> = Rc::default();
    let (a, b) = (done.clone(), done.clone());
    t.core.save_delete(true, &profile.to_dbus(libnm::ALL), Box::new(move |e| a.borrow_mut().push(e)));
    t.core.save_delete(false, &profile.to_dbus(libnm::ALL), Box::new(move |e| b.borrow_mut().push(e)));
    assert_eq!(*done.borrow(), [None, None]);
    assert_eq!(*t.keyring.deleted.borrow(), [profile.uuid()]);
}

#[test]
fn invalid_requests_are_rejected() {
    let t = T::new();
    t.request(&wifi(), "/p", "802-1x", &[], ALLOW);
    assert_eq!(t.error(0), Some(AgentError::InvalidConnection));
}

#[test]
fn noninteractive_does_not_prompt_or_unlock_keyring() {
    let t = T::new();
    t.wifi_request(0);
    assert!(t.prompter.prompts.borrow().is_empty());
    assert_eq!(*t.keyring.lookups.borrow(), [("802-11-wireless-security".to_string(), false)]);
}

#[test]
fn saved_secret_returns_without_prompt() {
    let t = T::new();
    t.keyring.values.replace(vec![("psk".into(), "synthetic-passphrase".into())]);
    t.wifi_request(ALLOW);
    assert!(t.prompter.prompts.borrow().is_empty());
    assert_eq!(t.secret(0, "802-11-wireless-security", "psk").as_deref(), Some("synthetic-passphrase"));
}

#[test]
fn request_new_prompts_even_with_stored_secret_and_clears_old_value() {
    let t = T::new();
    t.keyring.values.replace(vec![("psk".into(), "synthetic-old-password".into())]);
    t.wifi_request(ALLOW | REQUEST_NEW);
    assert_eq!(t.prompter.prompts.borrow().len(), 1);
    assert_eq!(t.prompt(0).fields[0].2, "", "REQUEST_NEW clears the old secret before the prompt");
    t.prompt(0).respond(None);
    assert_eq!(t.error(0), Some(AgentError::UserCanceled));
}

#[test]
fn no_interaction_request_new_never_reuses_secret() {
    let t = T::new();
    t.keyring.values.replace(vec![("psk".into(), "synthetic-passphrase".into())]);
    t.wifi_request(REQUEST_NEW);
    assert!(t.prompter.prompts.borrow().is_empty());
    assert_eq!(t.error(0), Some(AgentError::NoSecrets));
}

#[test]
fn concurrent_requests_queue_and_cancel_once() {
    let t = T::new();
    t.request(&wifi(), "/org/freedesktop/NetworkManager/Settings/99999", "802-11-wireless-security", &[], ALLOW);
    t.request(&wifi(), "/org/freedesktop/NetworkManager/Settings/99998", "802-11-wireless-security", &[], ALLOW);
    assert_eq!(t.prompter.prompts.borrow().len(), 1);
    t.core.cancel("/org/freedesktop/NetworkManager/Settings/99999", "802-11-wireless-security");
    iterate();
    assert!(t.prompt(0).destroyed.get());
    assert_eq!(t.prompter.prompts.borrow().len(), 2);
    assert_eq!(t.error(0), Some(AgentError::AgentCanceled));
    t.prompt(0).respond(None);
    assert_eq!(t.replies.borrow().len(), 1, "a late response of a cancelled prompt is ignored");
    t.prompt(1).respond(None);
    assert_eq!(t.replies.borrow().len(), 2);
    assert_eq!(t.error(1), Some(AgentError::UserCanceled));
}

#[test]
fn cancel_while_keyring_is_pending_ignores_late_callback() {
    let t = T::new();
    t.keyring.delayed.set(true);
    t.wifi_request(ALLOW);
    t.core.cancel("/org/freedesktop/NetworkManager/Settings/99999", "802-11-wireless-security");
    let pending = t.keyring.pending.borrow_mut().pop().unwrap();
    pending(vec![("psk".into(), "synthetic-passphrase".into())], None);
    assert_eq!(t.replies.borrow().len(), 1);
    assert!(t.prompter.prompts.borrow().is_empty());
}

#[test]
fn psk_prompt_returns_valid_secret_saves_owned_secret_and_leaves_original() {
    let t = T::new();
    let original = t.wifi_request(ALLOW);
    let prompt = t.prompt(0);
    assert_eq!(prompt.keys(), ["psk"]);
    assert!(prompt.fields.iter().all(|f| f.3));
    assert_eq!(prompt.remember, Some(true));
    prompt.respond(Some(vec![("psk", "synthetic-passphrase")]));
    assert_eq!(t.secret(0, "802-11-wireless-security", "psk").as_deref(), Some("synthetic-passphrase"));
    assert_eq!(*t.keyring.saved.borrow(), [vec![("802-11-wireless-security".to_string(), "psk".to_string(), "synthetic-passphrase".to_string())]]);
    assert!(original.setting("802-11-wireless-security").unwrap().get_string("psk").is_none());
    assert!(prompt.destroyed.get());
}

#[test]
fn psk_prompt_rejects_invalid_credentials_before_saving_or_returning() {
    let t = T::new();
    t.wifi_request(ALLOW);
    t.prompt(0).respond(Some(vec![("psk", "short")]));
    assert_eq!(t.error(0), Some(AgentError::Failed));
    assert!(t.keyring.saved.borrow().is_empty());
}

#[test]
fn close_cancels_active_and_queued_requests() {
    let t = T::new();
    t.wifi_request(ALLOW);
    t.wifi_request(ALLOW);
    t.core.close();
    assert_eq!(t.replies.borrow().len(), 2);
    assert!(t.prompt(0).destroyed.get());
    assert!(t.core.closed());
    assert_eq!(t.error(1), Some(AgentError::AgentCanceled));
}

#[test]
fn wired_8021x_fields_follow_hints() {
    let profile = connection("802-3-ethernet", libnm::AGENT_OWNED);
    let fields = AgentCore::connection_fields(&profile, "802-1x", &["password".into(), "private-key-password".into(), "identity".into()]);
    assert_eq!(fields.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["password", "private-key-password"]);
}

#[test]
fn wep_prompt_uses_selected_key_index_and_shared_flags() {
    let t = T::new();
    let profile = wifi();
    let security = profile.setting("802-11-wireless-security").unwrap();
    security.set_str("key-mgmt", Some("none"));
    security.set_int("wep-tx-keyidx", 2);
    security.set_int("wep-key-type", 1);
    security.set_int("wep-key-flags", libnm::AGENT_OWNED as i64);
    t.request(&profile, "/p/1", "802-11-wireless-security", &[], ALLOW);
    assert_eq!(t.prompt(0).keys(), ["wep-key2"]);
    t.prompt(0).respond(Some(vec![("wep-key2", "abcde")]));
    assert_eq!(t.secret(0, "802-11-wireless-security", "wep-key2").as_deref(), Some("abcde"));
    assert_eq!(*t.keyring.saved.borrow(), [vec![("802-11-wireless-security".to_string(), "wep-key2".to_string(), "abcde".to_string())]]);
}

#[test]
fn leap_prompt_uses_leap_password_and_respects_do_not_remember() {
    let t = T::new();
    let profile = wifi();
    let security = profile.setting("802-11-wireless-security").unwrap();
    security.set_str("key-mgmt", Some("ieee8021x"));
    security.set_str("auth-alg", Some("leap"));
    security.set_str("leap-username", Some("synthetic-user"));
    security.set_int("leap-password-flags", libnm::NOT_SAVED as i64);
    t.request(&profile, "/p/1", "802-11-wireless-security", &[], ALLOW);
    assert_eq!(t.prompt(0).keys(), ["leap-password"]);
    assert_eq!(t.prompt(0).remember, Some(false));
    t.prompt(0).respond(Some(vec![("leap-password", "synthetic-password")]));
    assert_eq!(t.secret(0, "802-11-wireless-security", "leap-password").as_deref(), Some("synthetic-password"));
    assert_eq!(*t.keyring.saved.borrow(), [Vec::<(String, String, String)>::new()]);
}

#[test]
fn hints_map_only_secret_properties_and_return_requested_fields() {
    let t = T::new();
    let profile = connection("802-3-ethernet", libnm::AGENT_OWNED);
    t.request(&profile, "/p/1", "802-1x", &["password", "private-key-password", "identity", "unknown"], ALLOW);
    assert_eq!(t.prompt(0).keys(), ["password", "private-key-password"]);
    t.prompt(0).respond(Some(vec![("password", "synthetic-password"), ("private-key-password", "synthetic-key-password")]));
    assert_eq!(t.secret(0, "802-1x", "password").as_deref(), Some("synthetic-password"));
    assert_eq!(t.secret(0, "802-1x", "private-key-password").as_deref(), Some("synthetic-key-password"));
    assert_eq!(profile.setting("802-1x").unwrap().get_string("identity").as_deref(), Some("synthetic-identity"));
}

#[test]
fn peer_preshared_key_prompt_updates_peer_flags() {
    let t = T::new();
    let profile = connection("wireguard", libnm::AGENT_OWNED);
    let b64 = |b: u8| tessel_core::base64::encode(&[b; 32]);
    let (public, private, preshared) = (b64(17), b64(18), b64(19));
    let setting = profile.setting("wireguard").unwrap();
    setting.set_str("private-key", Some(&private));
    let peer = libnm::Peer::new();
    assert!(peer.set_public_key(&public));
    peer.set_preshared_key_flags(libnm::AGENT_OWNED);
    setting.append_peer(&peer);
    let key = format!("peers.{public}.preshared-key");
    t.request(&profile, "/p/1", "wireguard", &[&key], ALLOW);
    assert_eq!(t.prompt(0).keys(), [key.clone()]);
    t.prompt(0).respond(Some(vec![(&key, &preshared)]));
    let reply = t.replies.borrow()[0].clone().unwrap();
    let peers = libnm::lookup(&libnm::lookup(&reply, "wireguard", "a{sv}").unwrap(), "peers", "aa{sv}").unwrap();
    assert_eq!(libnm::lookup(&peers.child_value(0), "preshared-key", "s").unwrap().str(), Some(preshared.as_str()));
    assert!(t.keyring.saved.borrow()[0].contains(&("wireguard".to_string(), key, preshared)));
}

#[test]
fn vpn_helper_reply_is_returned_without_prompts_and_saved() {
    let t = T::new();
    let argv: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = argv.clone();
    t.core.plugin_lookup.replace(Rc::new(|_| Some(PluginMeta { auth_dialog: Some("/usr/libexec/fake-auth".into()), supports_hints: false, external_ui: false })));
    t.core.trusted.replace(Rc::new(|_| true));
    t.core.spawn.replace(Rc::new(move |args, payload, done| {
        seen.replace(args);
        assert!(String::from_utf8(payload).unwrap().ends_with("DONE\n\nQUIT\n\n"));
        let process = VpnProcess::spawn(vec!["/bin/true".into()], Vec::new(), Box::new(|_, _| {}), Duration::from_secs(5));
        done(b"password\nsynthetic-vpn\n\n".to_vec(), None);
        process
    }));
    let profile = connection("vpn", libnm::AGENT_OWNED);
    t.request(&profile, "/p/vpn", "vpn", &["password"], ALLOW);
    assert_eq!(t.prompter.helpers.get(), 1, "popup dismissed before the helper maps a window");
    assert!(argv.borrow().contains(&"-i".to_string()));
    let reply = t.replies.borrow()[0].clone().unwrap();
    let secrets = libnm::lookup(&libnm::lookup(&reply, "vpn", "a{sv}").unwrap(), "secrets", "a{ss}").unwrap();
    assert_eq!(libnm::lookup(&secrets, "password", "s").unwrap().str(), Some("synthetic-vpn"));
    assert_eq!(*t.keyring.saved.borrow(), [vec![("vpn".to_string(), "password".to_string(), "synthetic-vpn".to_string())]]);
}

#[test]
fn untrusted_vpn_helper_fails_the_request() {
    let t = T::new();
    t.core.plugin_lookup.replace(Rc::new(|_| Some(PluginMeta { auth_dialog: Some("/tmp/evil".into()), supports_hints: false, external_ui: false })));
    t.core.trusted.replace(Rc::new(|_| false));
    t.request(&connection("vpn", 0), "/p/vpn", "vpn", &[], ALLOW);
    assert_eq!(t.error(0), Some(AgentError::Failed));
}
