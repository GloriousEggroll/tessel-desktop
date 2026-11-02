//! The D-Bus side: org.freedesktop.Secret.{Service,Collection,Item,Session,Prompt}
//! (Secret Service API 0.2), org.freedesktop.impl.portal.Secret (Flatpak
//! apps through xdg-desktop-portal) and org.tessel.Keyring (Global Settings).
//!
//! The whole keyring is locked or unlocked at once. Unlocking takes the
//! master key from the kernel keyring (stored there by the login), else asks
//! once with the prompt program (`tessel-keyring-prompt`).

use crate::{gnome_import, imports, kwallet_import};
use crate::store::{now, Keyring, LOGIN};
use crate::{dh, kernel, CONTROL_INTERFACE, CONTROL_PATH};
use gio::prelude::*;
use glib::variant::ObjectPath;
use glib::{Variant, VariantTy};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use zeroize::Zeroizing;

const ROOT: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_IFACE: &str = "org.freedesktop.Secret.Session";
const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";
const PORTAL_IFACE: &str = "org.freedesktop.impl.portal.Secret";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_SCHEMA: &str = "org.freedesktop.impl.portal.Secret";
const LABEL_PROP: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES_PROP: &str = "org.freedesktop.Secret.Item.Attributes";
const COLLECTION_LABEL_PROP: &str = "org.freedesktop.Secret.Collection.Label";
const MAX_TRIES: u32 = 3;

const XML: &str = r#"<node>
<interface name="org.freedesktop.Secret.Service">
  <method name="OpenSession"><arg name="algorithm" type="s" direction="in"/><arg name="input" type="v" direction="in"/>
    <arg name="output" type="v" direction="out"/><arg name="result" type="o" direction="out"/></method>
  <method name="CreateCollection"><arg name="properties" type="a{sv}" direction="in"/><arg name="alias" type="s" direction="in"/>
    <arg name="collection" type="o" direction="out"/><arg name="prompt" type="o" direction="out"/></method>
  <method name="SearchItems"><arg name="attributes" type="a{ss}" direction="in"/>
    <arg name="unlocked" type="ao" direction="out"/><arg name="locked" type="ao" direction="out"/></method>
  <method name="Unlock"><arg name="objects" type="ao" direction="in"/>
    <arg name="unlocked" type="ao" direction="out"/><arg name="prompt" type="o" direction="out"/></method>
  <method name="Lock"><arg name="objects" type="ao" direction="in"/>
    <arg name="locked" type="ao" direction="out"/><arg name="Prompt" type="o" direction="out"/></method>
  <method name="GetSecrets"><arg name="items" type="ao" direction="in"/><arg name="session" type="o" direction="in"/>
    <arg name="secrets" type="a{o(oayays)}" direction="out"/></method>
  <method name="ReadAlias"><arg name="name" type="s" direction="in"/><arg name="collection" type="o" direction="out"/></method>
  <method name="SetAlias"><arg name="name" type="s" direction="in"/><arg name="collection" type="o" direction="in"/></method>
  <signal name="CollectionCreated"><arg name="collection" type="o"/></signal>
  <signal name="CollectionDeleted"><arg name="collection" type="o"/></signal>
  <signal name="CollectionChanged"><arg name="collection" type="o"/></signal>
  <property name="Collections" type="ao" access="read"/>
</interface>
<interface name="org.freedesktop.Secret.Collection">
  <method name="Delete"><arg name="prompt" type="o" direction="out"/></method>
  <method name="SearchItems"><arg name="attributes" type="a{ss}" direction="in"/><arg name="results" type="ao" direction="out"/></method>
  <method name="CreateItem"><arg name="properties" type="a{sv}" direction="in"/><arg name="secret" type="(oayays)" direction="in"/>
    <arg name="replace" type="b" direction="in"/><arg name="item" type="o" direction="out"/><arg name="prompt" type="o" direction="out"/></method>
  <signal name="ItemCreated"><arg name="item" type="o"/></signal>
  <signal name="ItemDeleted"><arg name="item" type="o"/></signal>
  <signal name="ItemChanged"><arg name="item" type="o"/></signal>
  <property name="Items" type="ao" access="read"/>
  <property name="Label" type="s" access="readwrite"/>
  <property name="Locked" type="b" access="read"/>
  <property name="Created" type="t" access="read"/>
  <property name="Modified" type="t" access="read"/>
</interface>
<interface name="org.freedesktop.Secret.Item">
  <method name="Delete"><arg name="Prompt" type="o" direction="out"/></method>
  <method name="GetSecret"><arg name="session" type="o" direction="in"/><arg name="secret" type="(oayays)" direction="out"/></method>
  <method name="SetSecret"><arg name="secret" type="(oayays)" direction="in"/></method>
  <property name="Locked" type="b" access="read"/>
  <property name="Attributes" type="a{ss}" access="readwrite"/>
  <property name="Label" type="s" access="readwrite"/>
  <property name="Created" type="t" access="read"/>
  <property name="Modified" type="t" access="read"/>
</interface>
<interface name="org.freedesktop.Secret.Session"><method name="Close"/></interface>
<interface name="org.freedesktop.Secret.Prompt">
  <method name="Prompt"><arg name="window-id" type="s" direction="in"/></method>
  <method name="Dismiss"/>
  <signal name="Completed"><arg name="dismissed" type="b"/><arg name="result" type="v"/></signal>
</interface>
<interface name="org.freedesktop.impl.portal.Secret">
  <method name="RetrieveSecret"><arg name="handle" type="o" direction="in"/><arg name="app_id" type="s" direction="in"/>
    <arg name="fd" type="h" direction="in"/><arg name="options" type="a{sv}" direction="in"/>
    <arg name="response" type="u" direction="out"/><arg name="results" type="a{sv}" direction="out"/></method>
  <property name="version" type="u" access="read"/>
</interface>
<interface name="org.tessel.Keyring">
  <method name="Status"><arg name="status" type="a{sv}" direction="out"/></method>
  <method name="Unlock"><arg name="unlocked" type="b" direction="out"/></method>
  <method name="Lock"/>
  <method name="RemoveLegacyFiles"><arg name="removed" type="as" direction="out"/></method>
  <method name="ImportKWallet"><arg name="path" type="s" direction="in"/><arg name="password" type="ay" direction="in"/>
    <arg name="added" type="u" direction="out"/><arg name="skipped" type="u" direction="out"/></method>
  <method name="ImportGnomeKeyring"><arg name="path" type="s" direction="in"/>
    <arg name="added" type="u" direction="out"/><arg name="skipped" type="u" direction="out"/></method>
  <signal name="Changed"/>
</interface>
</node>"#;

/// Where things live; tests point everything at a scratch directory.
#[derive(Clone, Debug)]
pub struct Config {
    pub dir: PathBuf,
    /// GNOME keyrings to import once (`~/.local/share/keyrings`).
    pub gnome_dir: PathBuf,
    /// KDE wallets to import (`~/.local/share/kwalletd`).
    pub kwallet_dir: PathBuf,
    /// The GTK password prompt (`tessel-keyring-prompt` next to the binary).
    pub prompt: PathBuf,
    /// Take the master key from the kernel keyring (off in tests).
    pub kernel: bool,
    /// `unix_chkpwd`: checks that a new keyring password is the login password.
    pub chkpwd: Option<PathBuf>,
}

impl Config {
    pub fn default_for(exe: &std::path::Path) -> Self {
        let chkpwd = ["/usr/sbin/unix_chkpwd", "/sbin/unix_chkpwd"].iter().map(PathBuf::from).find(|p| p.exists());
        Config {
            dir: crate::store::default_dir(),
            gnome_dir: gnome_import::gnome_dir(),
            kwallet_dir: kwallet_import::kwallet_dir(),
            prompt: exe.parent().map(|d| d.join("tessel-keyring-prompt")).unwrap_or_else(|| "tessel-keyring-prompt".into()),
            kernel: true,
            chkpwd,
        }
    }
}

struct Session {
    owner: String,
    /// `None`: plain transport.
    key: Option<Zeroizing<[u8; 16]>>,
}

/// What a completed Secret Service prompt does.
enum PromptAction {
    Unlock(Vec<String>),
    CreateCollection { label: String, alias: String },
    CreateItem { collection: String, label: String, attributes: BTreeMap<String, String>,
                 secret: Zeroizing<Vec<u8>>, content_type: String, replace: bool },
}

type Waiter = Box<dyn FnOnce(&Rc<Daemon>, bool)>;

#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    prompts: HashMap<String, PromptAction>,
    next_session: u64,
    next_prompt: u64,
    /// Callbacks waiting for the running unlock prompt.
    waiters: Vec<Waiter>,
    prompting: bool,
    unlocked_by_login: bool,
}

pub struct Daemon {
    conn: gio::DBusConnection,
    keyring: RefCell<Keyring>,
    state: RefCell<State>,
    registrations: RefCell<HashMap<(String, &'static str), gio::RegistrationId>>,
    config: Config,
    me: RefCell<Weak<Daemon>>,
    _watch: RefCell<Option<gio::SignalSubscriptionId>>,
}

fn path(text: &str) -> ObjectPath {
    ObjectPath::try_from(text).unwrap_or_else(|_| ObjectPath::try_from("/").unwrap())
}

fn collection_path(id: &str) -> String {
    format!("{ROOT}/collection/{id}")
}

fn item_path(collection: &str, item: u64) -> String {
    format!("{ROOT}/collection/{collection}/{item}")
}

fn paths(list: &[String]) -> Vec<ObjectPath> {
    list.iter().map(|p| path(p)).collect()
}

/// (collection id, item id) of an item path.
fn parse_item(object: &str) -> Option<(String, u64)> {
    let rest = object.strip_prefix(&format!("{ROOT}/collection/"))?;
    let (collection, item) = rest.split_once('/')?;
    Some((collection.to_owned(), item.parse().ok()?))
}

fn interface_info(name: &str) -> gio::DBusInterfaceInfo {
    thread_local! {
        static NODE: gio::DBusNodeInfo = gio::DBusNodeInfo::for_xml(XML).expect("introspection XML");
    }
    NODE.with(|node| node.lookup_interface(name).expect("interface in XML"))
}

fn error(invocation: gio::DBusMethodInvocation, name: &str, message: &str) {
    invocation.return_dbus_error(name, message);
}

fn no_such_object(invocation: gio::DBusMethodInvocation) {
    error(invocation, "org.freedesktop.Secret.Error.NoSuchObject", "No such object");
}

fn is_locked(invocation: gio::DBusMethodInvocation) {
    error(invocation, "org.freedesktop.Secret.Error.IsLocked", "The keyring is locked");
}

/// The result of a dismissed prompt, typed as the completed one would be (libsecret checks it).
fn empty_result(action: &PromptAction) -> Variant {
    match action {
        PromptAction::Unlock(_) => Vec::<ObjectPath>::new().to_variant(),
        _ => path("/").to_variant(),
    }
}

fn attributes_of(value: &Variant) -> BTreeMap<String, String> {
    value.get::<HashMap<String, String>>().unwrap_or_default().into_iter().collect()
}

impl Daemon {
    pub fn new(conn: gio::DBusConnection, config: Config) -> Rc<Self> {
        let keyring = Keyring::open(config.dir.clone());
        let daemon = Rc::new(Daemon {
            conn, keyring: RefCell::new(keyring), state: RefCell::new(State::default()),
            registrations: RefCell::new(HashMap::new()), config, me: RefCell::new(Weak::new()),
            _watch: RefCell::new(None),
        });
        *daemon.me.borrow_mut() = Rc::downgrade(&daemon);
        daemon
    }

    fn rc(&self) -> Rc<Daemon> {
        self.me.borrow().upgrade().expect("daemon alive")
    }

    /// Export the fixed objects and every collection and item.
    pub fn export(self: &Rc<Self>) -> Result<(), String> {
        self.register(ROOT, SERVICE_IFACE)?;
        self.register(PORTAL_PATH, PORTAL_IFACE)?;
        self.register(CONTROL_PATH, CONTROL_INTERFACE)?;
        self.try_kernel_unlock();
        self.sync_objects();
        let weak = Rc::downgrade(self);
        #[allow(deprecated)]
        let watch = self.conn.signal_subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"), Some("/org/freedesktop/DBus"), None, gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, params| {
                let Some(daemon) = weak.upgrade() else { return };
                if let Some((name, _, new)) = params.get::<(String, String, String)>() {
                    if new.is_empty() {
                        daemon.drop_sessions_of(&name);
                    }
                }
            });
        *self._watch.borrow_mut() = Some(watch);
        Ok(())
    }

    fn drop_sessions_of(&self, owner: &str) {
        let gone: Vec<String> = self.state.borrow().sessions.iter().filter(|(_, s)| s.owner == owner)
            .map(|(p, _)| p.clone()).collect();
        for p in gone {
            self.state.borrow_mut().sessions.remove(&p);
            self.unregister(&p, SESSION_IFACE);
        }
    }

    fn register(self: &Rc<Self>, object: &str, interface: &'static str) -> Result<(), String> {
        let key = (object.to_owned(), interface);
        if self.registrations.borrow().contains_key(&key) {
            return Ok(());
        }
        let info = interface_info(interface);
        let (call, get, set) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
        let id = self.conn.register_object(object, &info)
            .method_call(move |_, sender, object, interface, method, params, invocation| {
                if let Some(daemon) = call.upgrade() {
                    daemon.call(sender.unwrap_or(""), object, interface.unwrap_or(""), method, params, invocation);
                }
            })
            .property(move |_, _, object, interface, name| {
                get.upgrade().and_then(|d| d.get_property(object, interface, name)).unwrap_or_else(|| "".to_variant())
            })
            .set_property(move |_, _, object, interface, name, value| {
                set.upgrade().is_some_and(|d| d.set_property(object, interface, name, &value))
            })
            .build().map_err(|e| format!("cannot export {object}: {e}"))?;
        self.registrations.borrow_mut().insert(key, id);
        Ok(())
    }

    fn unregister(&self, object: &str, interface: &'static str) {
        if let Some(id) = self.registrations.borrow_mut().remove(&(object.to_owned(), interface)) {
            let _ = self.conn.unregister_object(id);
        }
    }

    /// Register collection/alias/item objects that exist, unregister the rest.
    fn sync_objects(self: &Rc<Self>) {
        let mut wanted: Vec<(String, &'static str)> = Vec::new();
        {
            let keyring = self.keyring.borrow();
            for c in &keyring.collections {
                wanted.push((collection_path(&c.id), COLLECTION_IFACE));
                for item in c.item_ids() {
                    wanted.push((item_path(&c.id, item), ITEM_IFACE));
                }
            }
            for alias in keyring.aliases.keys().chain(std::iter::once(&"default".to_owned())) {
                if keyring.resolve_alias(alias).is_some() {
                    wanted.push((format!("{ROOT}/aliases/{alias}"), COLLECTION_IFACE));
                }
            }
        }
        let stale: Vec<(String, &'static str)> = self.registrations.borrow().keys()
            .filter(|(p, i)| (*i == COLLECTION_IFACE || *i == ITEM_IFACE) && !wanted.contains(&(p.clone(), *i)))
            .cloned().collect();
        for (p, i) in stale {
            self.unregister(&p, i);
        }
        for (p, i) in wanted {
            if let Err(e) = self.register(&p, i) {
                eprintln!("tessel-keyring: {e}");
            }
        }
    }

    /// The collection id an object path (collection or alias) stands for.
    fn collection_of(&self, object: &str) -> Option<String> {
        let keyring = self.keyring.borrow();
        if let Some(alias) = object.strip_prefix(&format!("{ROOT}/aliases/")) {
            return keyring.resolve_alias(alias);
        }
        let id = object.strip_prefix(&format!("{ROOT}/collection/"))?;
        keyring.collection(id).map(|c| c.id.clone())
    }

    fn emit(&self, object: &str, interface: &str, signal: &str, args: Option<&Variant>) {
        let _ = self.conn.emit_signal(None, object, interface, signal, args);
    }

    fn properties_changed(&self, object: &str, interface: &str, names: &[&str]) {
        let mut changed = HashMap::new();
        for name in names {
            if let Some(value) = self.get_property(object, interface, name) {
                changed.insert(name.to_string(), value);
            }
        }
        let args = (interface, changed, Vec::<String>::new()).to_variant();
        self.emit(object, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&args));
    }

    fn changed(&self) {
        self.emit(CONTROL_PATH, CONTROL_INTERFACE, "Changed", None);
    }

    // ---- unlocking -----------------------------------------------------------------------

    /// Unlock with the key the login left in the kernel keyring.
    fn try_kernel_unlock(self: &Rc<Self>) -> bool {
        if self.keyring.borrow().unlocked() {
            return true;
        }
        if !self.config.kernel || !self.keyring.borrow().is_set_up() {
            return false;
        }
        let Some(master) = kernel::load() else { return false };
        // A stale key from an older keyring (re-created since) must not count as unlocked.
        let opened = {
            let mut keyring = self.keyring.borrow_mut();
            let login_opens = keyring.collection_mut(LOGIN).is_none_or(|c| c.unlock(&master));
            if login_opens {
                keyring.unlock_with(master);
            }
            login_opens
        };
        if opened {
            self.state.borrow_mut().unlocked_by_login = true;
            self.after_unlock();
        }
        opened
    }

    fn after_unlock(self: &Rc<Self>) {
        self.import_gnome();
        self.import_pending();
        self.sync_objects();
        self.notify_locked_changed();
    }

    fn notify_locked_changed(&self) {
        let ids: Vec<(String, Vec<u64>)> = self.keyring.borrow().collections.iter()
            .map(|c| (c.id.clone(), c.item_ids())).collect();
        for (id, items) in ids {
            self.properties_changed(&collection_path(&id), COLLECTION_IFACE, &["Locked"]);
            self.emit(ROOT, SERVICE_IFACE, "CollectionChanged", Some(&(path(&collection_path(&id)),).to_variant()));
            for item in items {
                self.properties_changed(&item_path(&id, item), ITEM_IFACE, &["Locked", "Label", "Attributes"]);
            }
        }
        self.changed();
    }

    /// Run `then(true)` once unlocked (now, or after the password prompt).
    fn ensure_unlocked(self: &Rc<Self>, then: Waiter) {
        if self.try_kernel_unlock() {
            then(self, true);
            return;
        }
        self.state.borrow_mut().waiters.push(then);
        if !self.state.borrow().prompting {
            self.state.borrow_mut().prompting = true;
            self.run_prompt(0);
        }
    }

    fn finish_prompting(self: &Rc<Self>, unlocked: bool) {
        let waiters = {
            let mut state = self.state.borrow_mut();
            state.prompting = false;
            std::mem::take(&mut state.waiters)
        };
        if unlocked {
            self.after_unlock();
        }
        for waiter in waiters {
            waiter(self, unlocked);
        }
    }

    fn run_prompt(self: &Rc<Self>, attempt: u32) {
        let setup = !self.keyring.borrow().is_set_up();
        let mut argv: Vec<std::ffi::OsString> = vec![self.config.prompt.clone().into(),
                                                     if setup { "--setup" } else { "--unlock" }.into()];
        if attempt > 0 {
            argv.push("--retry".into());
        }
        let argv_ref: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
        let process = match gio::Subprocess::newv(&argv_ref, gio::SubprocessFlags::STDOUT_PIPE) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("tessel-keyring: cannot run {}: {e}", self.config.prompt.display());
                self.finish_prompting(false);
                return;
            }
        };
        let daemon = self.clone();
        process.clone().communicate_async(None, None::<&gio::Cancellable>, move |result| {
            let entered = match result {
                Ok((Some(out), _)) if process.exit_status() == 0 => {
                    let mut bytes = Zeroizing::new(out.to_vec());
                    if bytes.last() == Some(&b'\n') {
                        bytes.pop();
                    }
                    Some(bytes)
                }
                _ => None,
            };
            let Some(password) = entered else {
                daemon.finish_prompting(false);
                return;
            };
            let accepted = if setup {
                daemon.login_password_ok(&password) && daemon.keyring.borrow_mut().set_up(&password).is_ok()
            } else {
                daemon.keyring.borrow_mut().unlock_with_password(&password).unwrap_or(false)
            };
            if accepted {
                daemon.finish_prompting(true);
            } else if attempt + 1 < MAX_TRIES {
                daemon.run_prompt(attempt + 1);
            } else {
                daemon.finish_prompting(false);
            }
        });
    }

    /// The keyring password must be the login password (checked with unix_chkpwd).
    fn login_password_ok(&self, password: &[u8]) -> bool {
        let Some(chkpwd) = &self.config.chkpwd else { return true };
        let user = std::env::var("USER").unwrap_or_default();
        let Ok(mut child) = std::process::Command::new(chkpwd).arg(&user).arg("nullok")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).spawn() else { return true };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(password);
            let _ = stdin.write_all(&[0]);
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    // ---- GNOME import ----------------------------------------------------------------------

    fn imported(&self) -> serde_json::Value {
        imports::record(&self.config.dir)
    }

    /// Add items to the login collection and save; (added, skipped).
    fn add_to_login(self: &Rc<Self>, items: Vec<gnome_import::Imported>) -> Result<(u32, u32), String> {
        let counts = {
            let mut keyring = self.keyring.borrow_mut();
            let login = keyring.collection_mut(LOGIN).filter(|c| !c.locked()).ok_or("the keyring is locked")?;
            let counts = imports::add_items(login, items);
            keyring.save(LOGIN)?;
            counts
        };
        self.sync_objects();
        self.properties_changed(&collection_path(LOGIN), COLLECTION_IFACE, &["Items", "Modified"]);
        self.changed();
        Ok(counts)
    }

    /// Copy every item of unencrypted GNOME keyrings into the login collection, once per file.
    fn import_gnome(self: &Rc<Self>) {
        let record = self.imported();
        for file in gnome_import::plaintext_files(&self.config.gnome_dir) {
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !gnome_import::is_keyring_file(&file) || imports::is_marked(&record, "gnome", &name) {
                continue;
            }
            let _ = self.import_gnome_file(&file);
        }
    }

    fn import_gnome_file(self: &Rc<Self>, file: &std::path::Path) -> Result<(u32, u32), String> {
        if !gnome_import::is_plaintext(file) {
            return Err("encrypted GNOME keyrings cannot be imported".into());
        }
        let text = Zeroizing::new(std::fs::read_to_string(file).map_err(|e| e.to_string())?);
        let counts = self.add_to_login(gnome_import::parse(&text))?;
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        imports::mark(&self.config.dir, "gnome", &name, counts.0)?;
        Ok(counts)
    }

    /// KDE wallets the login helper decrypted with the login password.
    fn import_pending(self: &Rc<Self>) {
        let Some(master) = self.keyring.borrow().master().cloned() else { return };
        for (name, items) in imports::take_pending(&self.config.dir, &master) {
            if let Ok((added, _)) = self.add_to_login(items) {
                if let Some((kind, file)) = name.split_once(':') {
                    let _ = imports::mark(&self.config.dir, kind, file, added);
                }
            }
        }
    }

    fn import_kwallet_file(self: &Rc<Self>, file: &std::path::Path, password: &[u8]) -> Result<(u32, u32), String> {
        let items = kwallet_import::import_file(file, password).map_err(|e| e.to_string())?;
        let counts = self.add_to_login(items)?;
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        imports::mark(&self.config.dir, "kwallet", &name, counts.0)?;
        Ok(counts)
    }

    /// KDE wallets in KWallet's directory not imported yet.
    fn kwallets_pending(&self) -> Vec<String> {
        let record = self.imported();
        kwallet_import::wallets(&self.config.kwallet_dir).into_iter()
            .filter(|p| !p.file_name().is_some_and(|n| imports::is_marked(&record, "kwallet", &n.to_string_lossy())))
            .map(|p| p.display().to_string()).collect()
    }

    /// Unencrypted GNOME keyring files (and temporary copies) still on disk.
    fn legacy_files(&self) -> Vec<PathBuf> {
        gnome_import::plaintext_files(&self.config.gnome_dir)
    }

    /// Remove them, but only once every keyring file among them was imported.
    fn remove_legacy_files(&self) -> Vec<String> {
        let record = self.imported();
        let done: Vec<String> = record.get("gnome").and_then(|g| g.as_object())
            .map(|o| o.keys().cloned().collect()).unwrap_or_default();
        let files = self.legacy_files();
        let all_imported = files.iter().filter(|p| gnome_import::is_keyring_file(p))
            .all(|p| p.file_name().is_some_and(|n| done.contains(&n.to_string_lossy().into_owned())));
        if !all_imported {
            return Vec::new();
        }
        let mut removed = Vec::new();
        for file in files {
            if std::fs::remove_file(&file).is_ok() {
                removed.push(file.display().to_string());
            }
        }
        removed
    }

    // ---- secrets in transit ----------------------------------------------------------------

    fn encode_secret(&self, session: &str, secret: &[u8], content_type: &str) -> Option<Variant> {
        let state = self.state.borrow();
        let s = state.sessions.get(session)?;
        let (params, value) = match &s.key {
            None => (Vec::new(), secret.to_vec()),
            Some(key) => dh::encrypt(key, secret).ok()?,
        };
        Some((path(session), params, value, content_type.to_owned()).to_variant())
    }

    fn decode_secret(&self, value: &Variant) -> Option<(Zeroizing<Vec<u8>>, String)> {
        let (session, params, data, content_type) = value.get::<(ObjectPath, Vec<u8>, Vec<u8>, String)>()?;
        let state = self.state.borrow();
        let s = state.sessions.get(session.as_str())?;
        let plain = match &s.key {
            None => Zeroizing::new(data),
            Some(key) => dh::decrypt(key, &params, &data)?,
        };
        Some((plain, if content_type.is_empty() { "text/plain".into() } else { content_type }))
    }

    // ---- method calls -----------------------------------------------------------------------

    fn call(self: &Rc<Self>, sender: &str, object: &str, interface: &str, method: &str, params: Variant,
            invocation: gio::DBusMethodInvocation) {
        match interface {
            SERVICE_IFACE => self.service_call(sender, method, &params, invocation),
            COLLECTION_IFACE => self.collection_call(object, method, &params, invocation),
            ITEM_IFACE => self.item_call(object, method, &params, invocation),
            SESSION_IFACE => {
                self.state.borrow_mut().sessions.remove(object);
                self.unregister(object, SESSION_IFACE);
                invocation.return_value(None);
            }
            PROMPT_IFACE => self.prompt_call(object, method, invocation),
            PORTAL_IFACE => self.portal_call(&params, invocation),
            CONTROL_INTERFACE => self.control_call(method, &params, invocation),
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownInterface", "Unknown interface"),
        }
    }

    fn new_prompt(self: &Rc<Self>, action: PromptAction) -> String {
        let object = {
            let mut state = self.state.borrow_mut();
            state.next_prompt += 1;
            let object = format!("{ROOT}/prompt/p{}", state.next_prompt);
            state.prompts.insert(object.clone(), action);
            object
        };
        let _ = self.register(&object, PROMPT_IFACE);
        object
    }

    fn service_call(self: &Rc<Self>, sender: &str, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        match method {
            "OpenSession" => {
                let Some((algorithm, input)) = params.get::<(String, Variant)>() else {
                    return error(invocation, "org.freedesktop.DBus.Error.InvalidArgs", "Bad arguments");
                };
                let (output, key) = match algorithm.as_str() {
                    "plain" => ("".to_variant(), None),
                    dh::ALGORITHM => {
                        let client = input.get::<Vec<u8>>()
                            .or_else(|| input.is_type(VariantTy::VARIANT).then(|| input.as_variant())
                                .flatten().and_then(|v| v.get::<Vec<u8>>()))
                            .unwrap_or_default();
                        match dh::negotiate(&client) {
                            Ok((public, key)) => (public.to_variant(), Some(key)),
                            Err(e) => return error(invocation, "org.freedesktop.DBus.Error.InvalidArgs", &e),
                        }
                    }
                    _ => return error(invocation, "org.freedesktop.DBus.Error.NotSupported", "Algorithm not supported"),
                };
                let object = {
                    let mut state = self.state.borrow_mut();
                    state.next_session += 1;
                    let object = format!("{ROOT}/session/s{}", state.next_session);
                    state.sessions.insert(object.clone(), Session { owner: sender.to_owned(), key });
                    object
                };
                let _ = self.register(&object, SESSION_IFACE);
                invocation.return_value(Some(&(output, path(&object)).to_variant()));
            }
            "CreateCollection" => {
                let Some((props, alias)) = params.get::<(HashMap<String, Variant>, String)>() else {
                    return error(invocation, "org.freedesktop.DBus.Error.InvalidArgs", "Bad arguments");
                };
                if !alias.is_empty() {
                    if let Some(id) = self.keyring.borrow().resolve_alias(&alias) {
                        return invocation.return_value(Some(&(path(&collection_path(&id)), path("/")).to_variant()));
                    }
                }
                let label = props.get(COLLECTION_LABEL_PROP).and_then(|v| v.get::<String>()).unwrap_or_default();
                if self.keyring.borrow().unlocked() {
                    match self.create_collection(&label, &alias) {
                        Ok(object) => invocation.return_value(Some(&(path(&object), path("/")).to_variant())),
                        Err(e) => error(invocation, "org.freedesktop.DBus.Error.Failed", &e),
                    }
                } else {
                    let prompt = self.new_prompt(PromptAction::CreateCollection { label, alias });
                    invocation.return_value(Some(&(path("/"), path(&prompt)).to_variant()));
                }
            }
            "SearchItems" => {
                let query = params.child_value(0);
                let (unlocked, locked) = self.search_all(&attributes_of(&query));
                invocation.return_value(Some(&(paths(&unlocked), paths(&locked)).to_variant()));
            }
            "Unlock" => {
                let objects: Vec<String> = params.child_value(0).get::<Vec<ObjectPath>>().unwrap_or_default()
                    .iter().map(|p| p.to_string()).collect();
                if self.try_kernel_unlock() {
                    return invocation.return_value(Some(&(paths(&objects), path("/")).to_variant()));
                }
                let prompt = self.new_prompt(PromptAction::Unlock(objects));
                invocation.return_value(Some(&(paths(&[]), path(&prompt)).to_variant()));
            }
            "Lock" => {
                let objects: Vec<String> = params.child_value(0).get::<Vec<ObjectPath>>().unwrap_or_default()
                    .iter().map(|p| p.to_string()).collect();
                self.lock();
                invocation.return_value(Some(&(paths(&objects), path("/")).to_variant()));
            }
            "GetSecrets" => {
                let Some((items, session)) = params.get::<(Vec<ObjectPath>, ObjectPath)>() else {
                    return error(invocation, "org.freedesktop.DBus.Error.InvalidArgs", "Bad arguments");
                };
                if !self.state.borrow().sessions.contains_key(session.as_str()) {
                    return error(invocation, "org.freedesktop.Secret.Error.NoSession", "No such session");
                }
                let mut entries = Vec::new();
                for item in items {
                    let Some((c, i)) = parse_item(&item) else { continue };
                    let found = self.keyring.borrow().collection(&c).and_then(|c| c.item(i))
                        .map(|it| (it.secret.clone(), it.content_type.clone()));
                    if let Some((secret, content_type)) = found {
                        if let Some(value) = self.encode_secret(session.as_str(), &secret, &content_type) {
                            entries.push(Variant::from_dict_entry(&item.to_variant(), &value));
                        }
                    }
                }
                let dict = Variant::array_from_iter_with_type(VariantTy::new("{o(oayays)}").unwrap(), entries);
                invocation.return_value(Some(&Variant::tuple_from_iter([dict])));
            }
            "ReadAlias" => {
                let name = params.child_value(0).get::<String>().unwrap_or_default();
                let found = self.keyring.borrow().resolve_alias(&name).map(|id| collection_path(&id));
                invocation.return_value(Some(&(path(found.as_deref().unwrap_or("/")),).to_variant()));
            }
            "SetAlias" => {
                let Some((name, target)) = params.get::<(String, ObjectPath)>() else {
                    return error(invocation, "org.freedesktop.DBus.Error.InvalidArgs", "Bad arguments");
                };
                let result = {
                    let mut keyring = self.keyring.borrow_mut();
                    if target.as_str() == "/" {
                        keyring.aliases.remove(&name);
                        keyring.save_aliases()
                    } else {
                        drop(keyring);
                        match self.collection_of(target.as_str()) {
                            Some(id) => {
                                let mut keyring = self.keyring.borrow_mut();
                                keyring.aliases.insert(name.clone(), id);
                                keyring.save_aliases()
                            }
                            None => return no_such_object(invocation),
                        }
                    }
                };
                self.sync_objects();
                match result {
                    Ok(()) => invocation.return_value(None),
                    Err(e) => error(invocation, "org.freedesktop.DBus.Error.Failed", &e),
                }
            }
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    fn search_all(&self, query: &BTreeMap<String, String>) -> (Vec<String>, Vec<String>) {
        let keyring = self.keyring.borrow();
        let (mut unlocked, mut locked) = (Vec::new(), Vec::new());
        for c in &keyring.collections {
            let found = c.search(query).into_iter().map(|i| item_path(&c.id, i));
            if c.locked() { locked.extend(found) } else { unlocked.extend(found) }
        }
        (unlocked, locked)
    }

    fn create_collection(self: &Rc<Self>, label: &str, alias: &str) -> Result<String, String> {
        let id = self.keyring.borrow_mut().create_collection(label)?;
        if !alias.is_empty() {
            let mut keyring = self.keyring.borrow_mut();
            keyring.aliases.insert(alias.to_owned(), id.clone());
            keyring.save_aliases()?;
        }
        self.sync_objects();
        let object = collection_path(&id);
        self.emit(ROOT, SERVICE_IFACE, "CollectionCreated", Some(&(path(&object),).to_variant()));
        self.properties_changed(ROOT, SERVICE_IFACE, &["Collections"]);
        Ok(object)
    }

    fn create_item(self: &Rc<Self>, collection: &str, label: &str, attributes: BTreeMap<String, String>,
                   secret: Zeroizing<Vec<u8>>, content_type: &str, replace: bool) -> Result<String, String> {
        let existed = self.keyring.borrow().collection(collection).map(|c| c.search(&attributes).len()).unwrap_or(0);
        let id = {
            let mut keyring = self.keyring.borrow_mut();
            let c = keyring.collection_mut(collection).ok_or("no such collection")?;
            let id = c.store(label, attributes, secret, content_type, replace).ok_or("the keyring is locked")?;
            keyring.save(collection)?;
            id
        };
        self.sync_objects();
        let object = item_path(collection, id);
        let collection_object = collection_path(collection);
        let signal = if replace && existed > 0 { "ItemChanged" } else { "ItemCreated" };
        self.emit(&collection_object, COLLECTION_IFACE, signal, Some(&(path(&object),).to_variant()));
        self.properties_changed(&collection_object, COLLECTION_IFACE, &["Items", "Modified"]);
        self.changed();
        Ok(object)
    }

    fn collection_call(self: &Rc<Self>, object: &str, method: &str, params: &Variant,
                       invocation: gio::DBusMethodInvocation) {
        let Some(id) = self.collection_of(object) else { return no_such_object(invocation) };
        match method {
            "Delete" => {
                let result = self.keyring.borrow_mut().delete_collection(&id);
                if let Err(e) = result {
                    return error(invocation, "org.freedesktop.DBus.Error.Failed", &e);
                }
                self.sync_objects();
                self.emit(ROOT, SERVICE_IFACE, "CollectionDeleted", Some(&(path(&collection_path(&id)),).to_variant()));
                self.properties_changed(ROOT, SERVICE_IFACE, &["Collections"]);
                self.changed();
                invocation.return_value(Some(&(path("/"),).to_variant()));
            }
            "SearchItems" => {
                let query = attributes_of(&params.child_value(0));
                let found: Vec<String> = self.keyring.borrow().collection(&id)
                    .map(|c| c.search(&query).into_iter().map(|i| item_path(&id, i)).collect()).unwrap_or_default();
                invocation.return_value(Some(&(paths(&found),).to_variant()));
            }
            "CreateItem" => {
                let props = params.child_value(0).get::<HashMap<String, Variant>>().unwrap_or_default();
                let Some((secret, content_type)) = self.decode_secret(&params.child_value(1)) else {
                    return error(invocation, "org.freedesktop.Secret.Error.NoSession", "No such session");
                };
                let replace = params.child_value(2).get::<bool>().unwrap_or(false);
                let label = props.get(LABEL_PROP).and_then(|v| v.get::<String>()).unwrap_or_default();
                let attributes = props.get(ATTRIBUTES_PROP).map(attributes_of).unwrap_or_default();
                if self.keyring.borrow().collection(&id).is_some_and(|c| !c.locked()) {
                    match self.create_item(&id, &label, attributes, secret, &content_type, replace) {
                        Ok(item) => invocation.return_value(Some(&(path(&item), path("/")).to_variant())),
                        Err(e) => error(invocation, "org.freedesktop.DBus.Error.Failed", &e),
                    }
                } else {
                    let prompt = self.new_prompt(PromptAction::CreateItem {
                        collection: id, label, attributes, secret, content_type, replace });
                    invocation.return_value(Some(&(path("/"), path(&prompt)).to_variant()));
                }
            }
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    fn item_call(self: &Rc<Self>, object: &str, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        let Some((c, i)) = parse_item(object) else { return no_such_object(invocation) };
        let locked = match self.keyring.borrow().collection(&c) {
            Some(col) if col.item_ids().contains(&i) => col.locked(),
            _ => return no_such_object(invocation),
        };
        if locked {
            return is_locked(invocation);
        }
        match method {
            "Delete" => {
                let saved = {
                    let mut keyring = self.keyring.borrow_mut();
                    keyring.collection_mut(&c).map(|col| col.remove(i));
                    keyring.save(&c)
                };
                if let Err(e) = saved {
                    return error(invocation, "org.freedesktop.DBus.Error.Failed", &e);
                }
                self.sync_objects();
                let collection_object = collection_path(&c);
                self.emit(&collection_object, COLLECTION_IFACE, "ItemDeleted", Some(&(path(object),).to_variant()));
                self.properties_changed(&collection_object, COLLECTION_IFACE, &["Items", "Modified"]);
                self.changed();
                invocation.return_value(Some(&(path("/"),).to_variant()));
            }
            "GetSecret" => {
                let session = params.child_value(0).get::<ObjectPath>().map(|p| p.to_string()).unwrap_or_default();
                let found = self.keyring.borrow().collection(&c).and_then(|col| col.item(i))
                    .map(|it| (it.secret.clone(), it.content_type.clone()));
                let Some((secret, content_type)) = found else { return no_such_object(invocation) };
                match self.encode_secret(&session, &secret, &content_type) {
                    Some(value) => invocation.return_value(Some(&Variant::tuple_from_iter([value]))),
                    None => error(invocation, "org.freedesktop.Secret.Error.NoSession", "No such session"),
                }
            }
            "SetSecret" => {
                let Some((secret, content_type)) = self.decode_secret(&params.child_value(0)) else {
                    return error(invocation, "org.freedesktop.Secret.Error.NoSession", "No such session");
                };
                let saved = {
                    let mut keyring = self.keyring.borrow_mut();
                    keyring.collection_mut(&c).map(|col| col.update(i, |it| {
                        it.secret = secret;
                        it.content_type = content_type;
                    }));
                    keyring.save(&c)
                };
                match saved {
                    Ok(()) => {
                        self.emit(&collection_path(&c), COLLECTION_IFACE, "ItemChanged", Some(&(path(object),).to_variant()));
                        invocation.return_value(None);
                    }
                    Err(e) => error(invocation, "org.freedesktop.DBus.Error.Failed", &e),
                }
            }
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    fn prompt_call(self: &Rc<Self>, object: &str, method: &str, invocation: gio::DBusMethodInvocation) {
        if !self.state.borrow().prompts.contains_key(object) {
            return no_such_object(invocation);
        }
        let object = object.to_owned();
        match method {
            "Dismiss" => {
                let action = self.state.borrow_mut().prompts.remove(&object);
                invocation.return_value(None);
                if let Some(action) = action {
                    self.complete_prompt(&object, true, empty_result(&action));
                }
            }
            "Prompt" => {
                invocation.return_value(None);
                self.ensure_unlocked(Box::new(move |daemon, unlocked| {
                    let Some(action) = daemon.state.borrow_mut().prompts.remove(&object) else { return };
                    if !unlocked {
                        return daemon.complete_prompt(&object, true, empty_result(&action));
                    }
                    let result = match action {
                        PromptAction::Unlock(objects) => paths(&objects).to_variant(),
                        PromptAction::CreateCollection { label, alias } => {
                            path(&daemon.create_collection(&label, &alias).unwrap_or_else(|_| "/".into())).to_variant()
                        }
                        PromptAction::CreateItem { collection, label, attributes, secret, content_type, replace } => {
                            path(&daemon.create_item(&collection, &label, attributes, secret, &content_type, replace)
                                .unwrap_or_else(|_| "/".into())).to_variant()
                        }
                    };
                    daemon.complete_prompt(&object, false, result);
                }));
            }
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    fn complete_prompt(&self, object: &str, dismissed: bool, result: Variant) {
        let args = Variant::tuple_from_iter([dismissed.to_variant(), Variant::from_variant(&result)]);
        self.emit(object, PROMPT_IFACE, "Completed", Some(&args));
        self.unregister(object, PROMPT_IFACE);
    }

    /// xdg-desktop-portal: write the app's own random key into `fd` (created on first use).
    fn portal_call(self: &Rc<Self>, params: &Variant, invocation: gio::DBusMethodInvocation) {
        let app_id = params.child_value(1).get::<String>().unwrap_or_default();
        let handle = params.child_value(2).get::<glib::variant::Handle>().map(|h| h.0).unwrap_or(-1);
        let fd = invocation.message().unix_fd_list().and_then(|list| list.get(handle).ok());
        let Some(fd) = fd else {
            return invocation.return_value(Some(&(2u32, HashMap::<String, Variant>::new()).to_variant()));
        };
        self.ensure_unlocked(Box::new(move |daemon, unlocked| {
            let respond = |code: u32| invocation.return_value(Some(&(code, HashMap::<String, Variant>::new()).to_variant()));
            if !unlocked {
                return respond(1);
            }
            let query: BTreeMap<String, String> = [("app_id".to_owned(), app_id.clone()),
                                                   ("xdg:schema".to_owned(), PORTAL_SCHEMA.to_owned())].into();
            let existing = daemon.keyring.borrow().collection(LOGIN).and_then(|c| {
                c.search(&query).first().and_then(|i| c.item(*i)).map(|it| it.secret.clone())
            });
            let secret = match existing {
                Some(secret) => secret,
                None => {
                    let Ok(fresh) = crate::crypto::random_vec(64) else { return respond(2) };
                    let fresh = Zeroizing::new(fresh);
                    let label = format!("Application key for {app_id}");
                    if daemon.create_item(LOGIN, &label, query, fresh.clone(), "application/octet-stream", true).is_err() {
                        return respond(2);
                    }
                    fresh
                }
            };
            use std::io::Write;
            use std::os::fd::{FromRawFd, IntoRawFd};
            // SAFETY: the fd list handed us a new descriptor we now own.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd.into_raw_fd()) };
            respond(if file.write_all(&secret).is_ok() { 0 } else { 2 });
        }));
    }

    fn control_call(self: &Rc<Self>, method: &str, params: &Variant, invocation: gio::DBusMethodInvocation) {
        match method {
            "ImportKWallet" | "ImportGnomeKeyring" => {
                let file = PathBuf::from(params.child_value(0).get::<String>().unwrap_or_default());
                let password = Zeroizing::new(if method == "ImportKWallet" {
                    params.child_value(1).get::<Vec<u8>>().unwrap_or_default()
                } else {
                    Vec::new()
                });
                let kwallet = method == "ImportKWallet";
                self.ensure_unlocked(Box::new(move |daemon, unlocked| {
                    if !unlocked {
                        return error(invocation, "org.tessel.Keyring.Error.Locked", "The keyring is locked");
                    }
                    let result = if kwallet { daemon.import_kwallet_file(&file, &password) }
                                 else { daemon.import_gnome_file(&file) };
                    match result {
                        Ok((added, skipped)) => invocation.return_value(Some(&(added, skipped).to_variant())),
                        Err(e) if e == kwallet_import::Error::WrongPassword.to_string() =>
                            error(invocation, "org.tessel.Keyring.Error.WrongPassword", &e),
                        Err(e) => error(invocation, "org.tessel.Keyring.Error.Failed", &e),
                    }
                }));
            }
            "Status" => invocation.return_value(Some(&(self.status(),).to_variant())),
            "Unlock" => self.ensure_unlocked(Box::new(move |_, unlocked| {
                invocation.return_value(Some(&(unlocked,).to_variant()));
            })),
            "Lock" => {
                self.lock();
                invocation.return_value(None);
            }
            "RemoveLegacyFiles" => {
                let removed = self.remove_legacy_files();
                self.changed();
                invocation.return_value(Some(&(removed,).to_variant()));
            }
            _ => error(invocation, "org.freedesktop.DBus.Error.UnknownMethod", "Unknown method"),
        }
    }

    pub fn status(&self) -> HashMap<String, Variant> {
        let keyring = self.keyring.borrow();
        let items: u32 = keyring.collections.iter().filter(|c| c.persistent).map(|c| c.item_ids().len() as u32).sum();
        let imported = imports::total(&self.imported());
        let legacy: Vec<String> = self.legacy_files().iter().map(|p| p.display().to_string()).collect();
        HashMap::from([
            ("set_up".to_owned(), keyring.is_set_up().to_variant()),
            ("unlocked".to_owned(), keyring.unlocked().to_variant()),
            ("unlocked_by_login".to_owned(), self.state.borrow().unlocked_by_login.to_variant()),
            ("items".to_owned(), items.to_variant()),
            ("imported".to_owned(), imported.to_variant()),
            ("legacy_files".to_owned(), legacy.to_variant()),
            ("kwallets".to_owned(), self.kwallets_pending().to_variant()),
        ])
    }

    fn lock(&self) {
        self.keyring.borrow_mut().lock();
        self.state.borrow_mut().unlocked_by_login = false;
        if self.config.kernel {
            kernel::forget();
        }
        self.notify_locked_changed();
    }

    // ---- properties -------------------------------------------------------------------------

    fn get_property(&self, object: &str, interface: &str, name: &str) -> Option<Variant> {
        match (interface, name) {
            (SERVICE_IFACE, "Collections") => {
                let list: Vec<String> = self.keyring.borrow().collections.iter().map(|c| collection_path(&c.id)).collect();
                Some(paths(&list).to_variant())
            }
            (COLLECTION_IFACE, _) => {
                let id = self.collection_of(object)?;
                let keyring = self.keyring.borrow();
                let c = keyring.collection(&id)?;
                Some(match name {
                    "Items" => paths(&c.item_ids().into_iter().map(|i| item_path(&id, i)).collect::<Vec<_>>()).to_variant(),
                    "Label" => c.label.to_variant(),
                    "Locked" => c.locked().to_variant(),
                    "Created" => c.created.to_variant(),
                    "Modified" => c.modified.to_variant(),
                    _ => return None,
                })
            }
            (ITEM_IFACE, _) => {
                let (c, i) = parse_item(object)?;
                let keyring = self.keyring.borrow();
                let col = keyring.collection(&c)?;
                let item = col.item(i);
                Some(match name {
                    "Locked" => col.locked().to_variant(),
                    "Label" => item.map(|it| it.label.clone()).unwrap_or_default().to_variant(),
                    "Attributes" => item.map(|it| it.attributes.clone().into_iter().collect::<HashMap<_, _>>())
                        .unwrap_or_default().to_variant(),
                    "Created" => item.map(|it| it.created).unwrap_or(0).to_variant(),
                    "Modified" => item.map(|it| it.modified).unwrap_or(0).to_variant(),
                    _ => return None,
                })
            }
            (PORTAL_IFACE, "version") => Some(1u32.to_variant()),
            _ => None,
        }
    }

    fn set_property(&self, object: &str, interface: &str, name: &str, value: &Variant) -> bool {
        let rc = self.rc();
        match (interface, name) {
            (COLLECTION_IFACE, "Label") => {
                let Some(id) = self.collection_of(object) else { return false };
                let Some(label) = value.get::<String>() else { return false };
                let ok = {
                    let mut keyring = self.keyring.borrow_mut();
                    match keyring.collection_mut(&id) {
                        Some(c) if !c.locked() => {
                            c.label = label;
                            c.modified = now();
                            keyring.save(&id).is_ok()
                        }
                        _ => false,
                    }
                };
                if ok {
                    rc.properties_changed(&collection_path(&id), COLLECTION_IFACE, &["Label"]);
                }
                ok
            }
            (ITEM_IFACE, "Label" | "Attributes") => {
                let Some((c, i)) = parse_item(object) else { return false };
                let ok = {
                    let mut keyring = self.keyring.borrow_mut();
                    let updated = keyring.collection_mut(&c).is_some_and(|col| col.update(i, |it| {
                        if name == "Label" {
                            if let Some(label) = value.get::<String>() {
                                it.label = label;
                            }
                        } else {
                            it.attributes = attributes_of(value);
                        }
                    }));
                    updated && keyring.save(&c).is_ok()
                };
                if ok {
                    rc.emit(&collection_path(&c), COLLECTION_IFACE, "ItemChanged", Some(&(path(object),).to_variant()));
                    rc.properties_changed(object, ITEM_IFACE, &[name]);
                }
                ok
            }
            _ => false,
        }
    }
}

/// Own org.freedesktop.secrets on the session bus and serve until the name is lost.
pub fn run(config: Config) -> Result<(), String> {
    let main_loop = glib::MainLoop::new(None, false);
    let conn = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        .map_err(|e| format!("no session bus: {e}"))?;
    conn.set_exit_on_close(false);
    let daemon = Daemon::new(conn.clone(), config);
    daemon.export()?;
    // RequestName with DO_NOT_QUEUE (4): 1 = primary owner.
    let reply = conn.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
        "RequestName", Some(&(crate::BUS_NAME, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, -1,
        None::<&gio::Cancellable>).map_err(|e| format!("RequestName failed: {e}"))?;
    if reply.child_value(0).get::<u32>() != Some(1) {
        return Err(format!("{} is owned by another Secret Service (gnome-keyring or KWallet?)", crate::BUS_NAME));
    }
    let lost_loop = main_loop.clone();
    #[allow(deprecated)]
    let _lost = conn.signal_subscribe(Some("org.freedesktop.DBus"), Some("org.freedesktop.DBus"), Some("NameLost"),
        Some("/org/freedesktop/DBus"), Some(crate::BUS_NAME), gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, _| lost_loop.quit());
    let closed_loop = main_loop.clone();
    conn.connect_closed(move |_, _, _| closed_loop.quit());
    let quit = main_loop.clone();
    tessel_core::glib_compat::unix_signal_add_local(libc::SIGTERM, move || {
        quit.quit();
        glib::ControlFlow::Break
    });
    main_loop.run();
    drop(daemon);
    Ok(())
}
