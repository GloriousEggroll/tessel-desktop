//! Port of test_connection_profiles.py (catalogue, drafts and StoreTests) on
//! the FakeBus backend harness.

use super::*;
use crate::backend::tests::{base_world, changed, wireguard_profile, Harness, World};

struct S {
    h: Harness,
    store: ProfileStore,
    errors: Rc<RefCell<Vec<String>>>,
    saved: Rc<RefCell<Vec<Remote>>>,
}

impl S {
    fn new(world: World) -> Self {
        let h = Harness::new(world);
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let e = errors.clone();
        let store = ProfileStore::new(&h.backend, || {}, move |m| e.borrow_mut().push(m.to_owned()));
        Self { h, store, errors, saved: Rc::default() }
    }

    fn on_saved(&self) -> Saved {
        let saved = self.saved.clone();
        Rc::new(move |r: &Remote| saved.borrow_mut().push(r.clone()))
    }

    fn calls(&self) -> Vec<String> {
        self.h.bus.calls.borrow().iter().filter(|c| c.method == "GetSecrets" || c.mutation).map(|c| c.method.clone()).collect()
    }
}

fn wifi_draft() -> ProfileDraft {
    let draft = new_draft("802-11-wireless", Some("Example")).unwrap();
    let wireless = draft.connection.setting("802-11-wireless").unwrap();
    wireless.set_bytes("ssid", Some(b"Example"));
    let security = Setting::new("802-11-wireless-security").unwrap();
    security.set_str("key-mgmt", Some("wpa-psk"));
    security.set_int("psk-flags", libnm::AGENT_OWNED as i64);
    draft.connection.add_setting(&security);
    draft
}

fn attached(world: &mut World, connection: &Connection) {
    world.profile("/profiles/edit", connection, 0);
}

#[test]
fn catalogue_lists_installed_base_types_and_vpn_plugins() {
    let types = connection_types();
    assert!(types.iter().any(|t| t.id == "802-3-ethernet" && t.category == "Hardware"));
    assert!(types.iter().any(|t| t.id == "wireguard" && t.category == "VPN"));
    assert!(types.iter().filter(|t| t.id.starts_with("vpn:")).all(|t| t.setting == "vpn" && !t.vpn_service.is_empty()));
    assert!(new_draft("not-a-type", None).is_err());
}

#[test]
fn schema_describes_writable_properties_and_secret_flags() {
    let security = Setting::new("802-11-wireless-security").unwrap();
    let schema = setting_schema(&security);
    let psk = schema.iter().find(|p| p.name == "psk").unwrap();
    assert!(psk.secret);
    assert_eq!(psk.label, "Password");
    assert_eq!(psk.value_type, "gchararray");
    let wireless = setting_schema(&Setting::new("802-11-wireless").unwrap());
    let ssid = wireless.iter().find(|p| p.name == "ssid").unwrap();
    assert_eq!((ssid.value_type.as_str(), ssid.dbus_type.as_str()), ("GBytes", "ay"));
    let mtu = wireless.iter().find(|p| p.name == "mtu").unwrap();
    assert_eq!(mtu.minimum, Some(0.0));
}

#[test]
fn drafts_have_type_defaults() {
    let draft = new_draft("wireguard", None).unwrap();
    assert_eq!(draft.connection.id(), "New WireGuard");
    assert_eq!(draft.connection.setting("ipv4").unwrap().ip_method().as_deref(), Some("disabled"));
    assert_eq!(draft.connection.setting("ipv6").unwrap().ip_method().as_deref(), Some("disabled"));
    let pppoe = new_draft("pppoe", None).unwrap();
    assert!(pppoe.connection.setting("ppp").is_some() && pppoe.connection.setting("802-3-ethernet").is_some());
    let vlan = new_draft("vlan", None).unwrap();
    assert_eq!(vlan.connection.setting("vlan").unwrap().get_u32("id"), 1);
    assert!(new_draft("ovs-bridge", None).unwrap().connection.setting("ipv4").is_none());
}

#[test]
fn new_save_validates_then_adds_to_disk_with_autoconnect_blocked() {
    let s = S::new(World::new());
    let draft = Rc::new(RefCell::new(new_draft("802-3-ethernet", Some("Office")).unwrap()));
    assert!(s.store.save(&draft, s.on_saved()));
    assert!(s.store.busy());
    let call = s.h.mutations()[0].clone();
    assert_eq!(call.method, "AddConnection2");
    let params = call.params.unwrap();
    assert_eq!(params.child_value(1).get::<u32>(), Some(ADD2_TO_DISK | ADD2_BLOCK_AUTOCONNECT));
    Connection::from_dbus(&params.child_value(0)).unwrap().verify().unwrap();
    s.h.bus.reply("AddConnection2", Ok(tuple(&[crate::backend::path_variant(Some("/profiles/9")), glib::VariantDict::new(None).end()])),
                  None, None, 0);
    assert!(!s.store.busy());
    assert_eq!(s.saved.borrow().len(), 1);
    assert_eq!(draft.borrow().original.as_ref().unwrap().path, "/profiles/9");
    assert!(s.errors.borrow().is_empty());
}

#[test]
fn invalid_draft_does_not_queue_native_save_or_modify_input() {
    let s = S::new(World::new());
    let draft = Rc::new(RefCell::new(new_draft("802-11-wireless", None).unwrap()));
    let before = draft.borrow().connection.to_dbus(libnm::WITH_NON_SECRET);
    assert!(!s.store.save(&draft, s.on_saved()));
    assert!(s.h.mutations().is_empty());
    assert_eq!(before, draft.borrow().connection.to_dbus(libnm::WITH_NON_SECRET));
    assert!(s.store.error().contains("ssid"), "{}", s.store.error());
}

#[test]
fn ordinary_update_omits_secrets_and_preserves_version() {
    let mut world = World::new();
    attached(&mut world, &wifi_draft().connection);
    let s = S::new(world);
    let remote = s.h.backend.remote(&s.h.backend.state().profiles.keys().next().unwrap().clone()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("connection").unwrap().set_str("id", Some("Renamed"));
    assert!(s.store.save(&draft, s.on_saved()));
    assert_eq!(s.calls(), ["Update2"]);
    let params = s.h.mutations()[0].params.clone().unwrap();
    assert_eq!(params.child_value(1).get::<u32>(), Some(UPDATE2_TO_DISK | UPDATE2_NO_REAPPLY));
    assert_eq!(params.child_value(2).print(false).as_str(), "{'version-id': <uint64 7>}");
    let settings = parse_settings(&params.child_value(0));
    assert!(!settings["802-11-wireless-security"].contains_key("psk"));
    assert_eq!(get_str(&settings, "connection", "id").as_deref(), Some("Renamed"));
    s.h.bus.reply("Update2", Ok(tuple(&[glib::VariantDict::new(None).end()])), None, None, 0);
    assert_eq!(s.saved.borrow().len(), 1);
}

fn secrets_variant(setting: &str, values: &[(&str, &str)]) -> Variant {
    let values: Vec<(String, String)> = values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    tuple(&[crate::keyring::variant_secrets(setting, &values)])
}

#[test]
fn changed_wifi_secret_fetches_existing_values_then_overrides_only_entered_value() {
    let mut world = World::new();
    attached(&mut world, &wifi_draft().connection);
    let s = S::new(world);
    let remote = s.h.backend.remote(&s.h.backend.state().profiles.keys().next().unwrap().clone()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("802-11-wireless-security").unwrap().set_str("psk", Some("synthetic-new-password"));
    assert!(s.store.save(&draft, s.on_saved()));
    assert_eq!(s.calls(), ["GetSecrets"]);
    assert_eq!(s.h.bus.last_call().params.unwrap().child_value(0).str(), Some("802-11-wireless-security"));
    s.h.bus.reply("GetSecrets", Ok(secrets_variant("802-11-wireless-security", &[("psk", "synthetic-old-password"), ("wep-key0", "abcde")])),
                  None, None, 0);
    assert_eq!(s.calls(), ["GetSecrets", "Update2"]);
    let settings = parse_settings(&s.h.mutations()[0].params.clone().unwrap().child_value(0));
    assert_eq!(get_str(&settings, "802-11-wireless-security", "psk").as_deref(), Some("synthetic-new-password"));
    assert_eq!(get_str(&settings, "802-11-wireless-security", "wep-key0").as_deref(), Some("abcde"));
    s.h.bus.reply("Update2", Ok(tuple(&[glib::VariantDict::new(None).end()])), None, None, 0);
    assert!(draft.borrow().connection.setting("802-11-wireless-security").unwrap().get_string("psk").is_none());
}

#[test]
fn credential_preservation_failure_never_submits_a_partial_update() {
    let mut world = World::new();
    attached(&mut world, &wifi_draft().connection);
    let s = S::new(world);
    let remote = s.h.backend.remote(&s.h.backend.state().profiles.keys().next().unwrap().clone()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("802-11-wireless-security").unwrap().set_str("psk", Some("synthetic-new-password"));
    assert!(s.store.save(&draft, s.on_saved()));
    s.h.reply_err("GetSecrets", "org.freedesktop.NetworkManager.PermissionDenied", "Not authorized");
    assert!(!s.store.busy());
    assert!(s.store.error().contains("preserve existing credentials"));
    assert_eq!(s.calls(), ["GetSecrets"]);
    assert!(s.saved.borrow().is_empty());
}

#[test]
fn wireguard_private_key_change_preserves_peer_secret_and_routes() {
    let wg = wireguard_profile();
    let setting = wg.setting("wireguard").unwrap();
    let b64 = |b: u8| tessel_core::base64::encode(&[b; 32]);
    let peer = libnm::Peer::new();
    assert!(peer.set_public_key(&b64(22)));
    assert!(peer.append_allowed_ip("192.0.2.0/24"));
    setting.append_peer(&peer);
    let mut world = World::new();
    attached(&mut world, &wg);
    let s = S::new(world);
    let remote = s.h.backend.remote(&wg.uuid()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("wireguard").unwrap().set_str("private-key", Some(&b64(23)));
    assert!(s.store.save(&draft, s.on_saved()));
    assert_eq!(s.calls(), ["GetSecrets"]);
    let peer_dict = glib::VariantDict::new(None);
    peer_dict.insert_value("public-key", &b64(22).to_variant());
    peer_dict.insert_value("preshared-key", &b64(25).to_variant());
    let wireguard = glib::VariantDict::new(None);
    wireguard.insert_value("private-key", &b64(24).to_variant());
    wireguard.insert_value("peers", &Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, [peer_dict.end()]));
    let entry = Variant::from_dict_entry(&"wireguard".to_variant(), &wireguard.end());
    s.h.bus.reply("GetSecrets", Ok(tuple(&[Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), [entry])])), None, None, 0);
    assert_eq!(s.calls(), ["GetSecrets", "Update2"]);
    let saved = Connection::from_dbus(&s.h.mutations()[0].params.clone().unwrap().child_value(0)).unwrap();
    let wireguard = saved.setting("wireguard").unwrap();
    assert_eq!(wireguard.get_string("private-key"), Some(b64(23)));
    assert_eq!(wireguard.peers()[0].preshared_key(), Some(b64(25)));
    assert_eq!(wireguard.peers()[0].allowed_ips(), ["192.0.2.0/24"]);
}

#[test]
fn external_deleted_changed_and_uuid_replaced_profiles_are_rejected() {
    for scenario in ["external", "deleted", "changed", "uuid"] {
        let mut world = World::new();
        let connection = new_draft("802-3-ethernet", None).unwrap().connection;
        attached(&mut world, &connection);
        let mut s = S::new(world);
        let remote = s.h.backend.remote(&connection.uuid()).unwrap();
        let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
        match scenario {
            "external" => s.h.signal(":1.7", "/profiles/edit", "org.freedesktop.DBus.Properties", "PropertiesChanged",
                                     changed(CONNECTION_IFACE, "Flags", SETTINGS_FLAG_EXTERNAL.to_variant())),
            "deleted" => {
                s.h.world.list(SETTINGS_PATH, SETTINGS_IFACE, "Connections", &[]);
                s.h.sync();
            }
            "changed" => s.h.signal(":1.7", "/profiles/edit", "org.freedesktop.DBus.Properties", "PropertiesChanged",
                                    changed(CONNECTION_IFACE, "VersionId", 8u64.to_variant())),
            _ => draft.borrow().connection.setting("connection").unwrap().set_str("uuid", Some(&libnm::uuid_generate())),
        }
        assert!(!s.store.save(&draft, s.on_saved()), "{scenario}");
        assert!(s.calls().is_empty(), "{scenario}");
    }
}

#[test]
fn external_ownership_change_during_secret_fetch_prevents_update() {
    let mut world = World::new();
    attached(&mut world, &wifi_draft().connection);
    let s = S::new(world);
    let remote = s.h.backend.remote(&s.h.backend.state().profiles.keys().next().unwrap().clone()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("802-11-wireless-security").unwrap().set_str("psk", Some("synthetic-new-password"));
    assert!(s.store.save(&draft, s.on_saved()));
    s.h.signal(":1.7", "/profiles/edit", "org.freedesktop.DBus.Properties", "PropertiesChanged",
               changed(CONNECTION_IFACE, "Flags", SETTINGS_FLAG_EXTERNAL.to_variant()));
    s.h.bus.reply("GetSecrets", Ok(secrets_variant("802-11-wireless-security", &[("psk", "synthetic-old-password")])), None, None, 0);
    assert_eq!(s.calls(), ["GetSecrets"]);
    assert!(!s.store.busy());
}

#[test]
fn duplicate_save_is_not_queued_and_async_denial_is_reported() {
    let s = S::new(World::new());
    let draft = Rc::new(RefCell::new(new_draft("802-3-ethernet", None).unwrap()));
    assert!(s.store.save(&draft, s.on_saved()));
    assert!(!s.store.save(&draft, s.on_saved()));
    assert_eq!(s.h.mutations().len(), 1);
    s.h.reply_err("AddConnection2", "org.freedesktop.NetworkManager.PermissionDenied", "Not authorized");
    assert!(!s.store.busy());
    assert_eq!(*s.errors.borrow(), ["Not authorized"]);
}

#[test]
fn close_during_secret_fetch_prevents_any_write_or_late_callback() {
    let mut world = World::new();
    attached(&mut world, &wifi_draft().connection);
    let s = S::new(world);
    let remote = s.h.backend.remote(&s.h.backend.state().profiles.keys().next().unwrap().clone()).unwrap();
    let draft = Rc::new(RefCell::new(edit_draft(&remote).unwrap()));
    draft.borrow().connection.setting("802-11-wireless-security").unwrap().set_str("psk", Some("synthetic-new-password"));
    assert!(s.store.save(&draft, s.on_saved()));
    s.store.close();
    s.h.bus.reply("GetSecrets", Ok(secrets_variant("802-11-wireless-security", &[("psk", "synthetic-old-password")])), None, None, 0);
    assert_eq!(s.calls(), ["GetSecrets"]);
    assert!(s.saved.borrow().is_empty() && s.errors.borrow().is_empty());
}

#[test]
fn backend_restart_clears_busy_and_suppresses_success_callback() {
    let mut s = S::new(World::new());
    let draft = Rc::new(RefCell::new(new_draft("802-3-ethernet", None).unwrap()));
    assert!(s.store.save(&draft, s.on_saved()));
    s.h.signal("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged",
               (NM_BUS, ":1.7", "").to_variant());
    s.h.sync();
    s.h.bus.reply("AddConnection2", Ok(tuple(&[crate::backend::path_variant(Some("/profiles/9")), glib::VariantDict::new(None).end()])),
                  None, None, 0);
    assert!(!s.store.busy());
    assert!(s.saved.borrow().is_empty());
    assert!(s.store.error().contains("NetworkManager changed"));
    let _ = base_world;
}
