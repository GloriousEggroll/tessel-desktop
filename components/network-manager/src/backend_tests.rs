//! Port of test_network_backend.py BackendTests/ProfileConstructionTests on a
//! FakeBus: every call is recorded and answered by hand.

use super::*;
use std::collections::BTreeMap;
use suite_core::bus::{FakeBus, Reply};
use suite_core::scheduler::FakeScheduler;

type Props = Vec<(&'static str, Variant)>;

pub(crate) fn o(path: &str) -> Variant {
    path_variant(Some(path))
}

pub(crate) fn ao(paths: &[&str]) -> Variant {
    let items: Vec<glib::variant::ObjectPath> = paths.iter().map(|p| glib::variant::ObjectPath::try_from(*p).unwrap()).collect();
    items.to_variant()
}

#[derive(Clone)]
pub(crate) struct World {
    pub objects: BTreeMap<String, BTreeMap<String, Props>>,
    pub settings: BTreeMap<String, Variant>,
}

impl World {
    pub fn new() -> Self {
        let mut world = Self { objects: BTreeMap::new(), settings: BTreeMap::new() };
        world.set(NM_PATH, NM_IFACE, vec![
            ("NetworkingEnabled", true.to_variant()), ("WirelessEnabled", true.to_variant()),
            ("WirelessHardwareEnabled", true.to_variant()), ("WwanEnabled", true.to_variant()),
            ("WwanHardwareEnabled", true.to_variant()), ("Connectivity", 4u32.to_variant()),
            ("ConnectivityCheckUri", "http://check.invalid/".to_variant()), ("Devices", ao(&[])), ("ActiveConnections", ao(&[]))]);
        world.set(SETTINGS_PATH, SETTINGS_IFACE, vec![("Connections", ao(&[])), ("Hostname", "test-host".to_variant())]);
        world
    }

    pub fn set(&mut self, path: &str, iface: &str, props: Props) {
        let entry = self.objects.entry(path.into()).or_default().entry(iface.into()).or_default();
        for (k, v) in props {
            entry.retain(|(n, _)| *n != k);
            entry.push((k, v));
        }
    }

    pub fn list(&mut self, path: &str, iface: &str, name: &'static str, paths: &[String]) {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        self.set(path, iface, vec![(name, ao(&refs))]);
    }

    pub fn get_list(&self, path: &str, iface: &str, name: &str) -> Vec<String> {
        self.objects.get(path).and_then(|o| o.get(iface)).and_then(|p| p.iter().find(|(k, _)| *k == name))
            .map(|(_, v)| v.iter().map(|x| x.str().unwrap().to_owned()).collect()).unwrap_or_default()
    }

    pub fn push(&mut self, path: &str, iface: &str, name: &'static str, item: &str) {
        let mut items = self.get_list(path, iface, name);
        items.push(item.into());
        self.list(path, iface, name, &items);
    }

    pub fn device(&mut self, path: &str, iface: &str, device_type: u32) {
        self.set(path, DEVICE_IFACE, vec![("Interface", iface.to_variant()), ("DeviceType", device_type.to_variant()),
            ("State", 30u32.to_variant()), ("StateReason", (30u32, 0u32).to_variant()), ("HwAddress", "00:11:22:33:44:66".to_variant()),
            ("Driver", "test-driver".to_variant()), ("Mtu", 1500u32.to_variant()), ("Managed", true.to_variant()),
            ("Ip4Config", o("/")), ("Ip6Config", o("/")), ("Ifindex", 0u32.to_variant())]);
        if device_type == 2 {
            self.set(path, WIRELESS_IFACE, vec![("AccessPoints", ao(&[])), ("ActiveAccessPoint", o("/")),
                ("Bitrate", 433000u32.to_variant()), ("LastScan", 100i64.to_variant()), ("WirelessCapabilities", 0x3fu32.to_variant()), ("PermHwAddress", "00:11:22:33:44:66".to_variant())]);
        }
        if device_type == 1 {
            self.set(path, WIRED_IFACE, vec![("Carrier", true.to_variant()), ("Speed", 1000u32.to_variant())]);
        }
        self.push(NM_PATH, NM_IFACE, "Devices", path);
    }

    pub fn ap(&mut self, device: &str, path: &str, ssid: &[u8], strength: u8, rsn: u32) {
        self.set(path, AP_IFACE, vec![("Ssid", ssid.to_vec().to_variant()), ("Strength", strength.to_variant()),
            ("Frequency", 5180u32.to_variant()), ("HwAddress", "00:11:22:33:44:55".to_variant()),
            ("Flags", (if rsn != 0 { AP_PRIVACY } else { 0 }).to_variant()), ("WpaFlags", 0u32.to_variant()),
            ("RsnFlags", rsn.to_variant()), ("Mode", 2u32.to_variant())]);
        self.push(device, WIRELESS_IFACE, "AccessPoints", path);
    }

    pub fn profile(&mut self, path: &str, connection: &Connection, flags: u32) {
        self.settings.insert(path.into(), connection.to_dbus(libnm::WITH_NON_SECRET));
        self.set(path, CONNECTION_IFACE, vec![("Flags", flags.to_variant()), ("VersionId", 7u64.to_variant()), ("Unsaved", false.to_variant())]);
        self.push(SETTINGS_PATH, SETTINGS_IFACE, "Connections", path);
    }

    pub fn active(&mut self, path: &str, uuid: &str, kind: &str, devices: &[&str], vpn: bool, flags: u32) {
        self.set(path, ACTIVE_IFACE, vec![("Id", (if vpn { "Work VPN" } else { "Example" }).to_variant()), ("Uuid", uuid.to_variant()),
            ("Type", kind.to_variant()), ("Devices", ao(devices)), ("State", 2u32.to_variant()), ("StateFlags", flags.to_variant()),
            ("Default", (!vpn).to_variant()), ("Default6", false.to_variant()), ("Vpn", vpn.to_variant()),
            ("Ip4Config", o("/")), ("Ip6Config", o("/"))]);
        self.push(NM_PATH, NM_IFACE, "ActiveConnections", path);
    }

    pub fn variant(&self) -> Variant {
        let entries: Vec<Variant> = self.objects.iter().map(|(path, ifaces)| {
            let inner: Vec<Variant> = ifaces.iter().map(|(iface, props)| {
                let dict = glib::VariantDict::new(None);
                for (k, v) in props {
                    dict.insert_value(k, v);
                }
                Variant::from_dict_entry(&iface.to_variant(), &dict.end())
            }).collect();
            Variant::from_dict_entry(&o(path), &Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), inner))
        }).collect();
        Variant::tuple_from_iter([Variant::array_from_iter_with_type(glib::VariantTy::new("{oa{sa{sv}}}").unwrap(), entries)])
    }
}

pub(crate) struct Harness {
    pub backend: NetworkBackend,
    pub bus: Rc<FakeBus>,
    pub scheduler: Rc<FakeScheduler>,
    pub errors: Rc<RefCell<Vec<String>>>,
    pub changes: Rc<Cell<usize>>,
    pub world: World,
}

impl Harness {
    pub fn new(world: World) -> Self {
        let bus = FakeBus::new();
        let scheduler = Rc::new(FakeScheduler::default());
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let changes: Rc<Cell<usize>> = Rc::default();
        let (e, c) = (errors.clone(), changes.clone());
        let fake: Rc<dyn Bus> = bus.clone();
        let fake = RefCell::new(Some(fake));
        let backend = NetworkBackend::new(move || c.set(c.get() + 1), move |m| e.borrow_mut().push(m.to_owned()),
            Options { connector: Box::new(move |done| done(Ok(fake.borrow_mut().take().unwrap()))), scheduler: scheduler.clone(),
                      matcher: Rc::new(FakeMatcher) });
        let mut harness = Self { backend, bus, scheduler, errors, changes, world };
        harness.backend.start();
        harness.bus.reply("GetNameOwner", Ok((":1.7",).to_variant()), None, None, 0);
        harness.serve();
        harness
    }

    /// Answer the pending full load from the current world.
    pub fn serve(&mut self) {
        self.bus.reply("GetManagedObjects", Ok(self.world.variant()), None, None, 0);
        while self.bus.has_pending("GetSettings") {
            let path = self.bus.pending.borrow().iter().find(|(c, _)| c.method == "GetSettings").unwrap().0.path.clone();
            let reply: Reply = match self.world.settings.get(&path) {
                Some(v) => Ok(Variant::tuple_from_iter([v.clone()])),
                None => Err(BusError::remote("org.freedesktop.NetworkManager.Settings.PermissionDenied", "no")),
            };
            self.bus.reply("GetSettings", reply, Some(&path), None, 0);
        }
    }

    /// Reload after changing the world (NetworkManager restart style).
    pub fn sync(&mut self) {
        self.signal("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged",
                    (NM_BUS, ":1.7", ":1.7").to_variant());
        self.serve();
    }

    pub fn signal(&self, sender: &str, path: &str, iface: &str, member: &str, params: Variant) {
        let handlers: Vec<suite_core::bus::SignalHandler> = self.bus.signals.borrow().iter()
            .filter(|s| s.1.as_deref().is_none_or(|i| i == iface) && s.2.as_deref().is_none_or(|m| m == member)
                    && s.0.as_deref().is_none_or(|x| x == sender || (x == NM_BUS && sender != "org.freedesktop.DBus")))
            .map(|s| s.5.clone()).collect();
        for handler in handlers {
            handler(&Signal { sender: sender.into(), path: path.into(), interface: iface.into(), member: member.into(), params: params.clone() });
        }
    }

    pub fn mutations(&self) -> Vec<Call> {
        self.bus.calls.borrow().iter().filter(|c| c.mutation).cloned().collect()
    }

    pub fn reply_ok(&self, method: &str) {
        self.bus.reply(method, Ok(().to_variant()), None, None, 0);
    }

    pub fn reply_err(&self, method: &str, name: &str, message: &str) {
        self.bus.reply(method, Err(BusError::remote(name, message)), None, None, 0);
    }

    pub fn busy(&self) -> HashSet<String> {
        self.backend.state().busy.clone()
    }
}

/// Python's FakeDevice.connection_compatible / FakeAP.connection_valid.
pub(crate) struct FakeMatcher;

impl Matcher for FakeMatcher {
    fn compatible(&self, objects: &Objects, device: &str, _connection: &str, settings: &Settings) -> Option<bool> {
        Some(prop_u32(objects, device, DEVICE_IFACE, "DeviceType") == 2
             && get_str(settings, "connection", "type").as_deref() == Some("802-11-wireless"))
    }
    fn ap_valid(&self, objects: &Objects, _device: &str, ap: &str, _connection: &str, settings: &Settings) -> Option<bool> {
        let security = ap_security(prop_u32(objects, ap, AP_IFACE, "Flags"), prop_u32(objects, ap, AP_IFACE, "WpaFlags"),
                                   prop_u32(objects, ap, AP_IFACE, "RsnFlags"));
        Some(settings.contains_key("802-11-wireless")
             && get_bytes(settings, "802-11-wireless", "ssid").unwrap_or_default() == prop_bytes(objects, ap, AP_IFACE, "Ssid")
             && profile_security(settings) == security)
    }
    fn description(&self, _objects: &Objects, _device: &str) -> Option<String> {
        Some("Test adapter".into())
    }
}

pub(crate) fn changed(iface: &str, name: &str, value: Variant) -> Variant {
    let dict = glib::VariantDict::new(None);
    dict.insert_value(name, &value);
    tuple(&[iface.to_variant(), dict.end(), Vec::<String>::new().to_variant()])
}

pub(crate) fn aasv(items: Vec<Variant>) -> Variant {
    Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, items)
}

pub(crate) fn wifi(security: &str, ssid: &[u8]) -> WifiNetwork {
    WifiNetwork::new("net", "/wifi1", ssid, "Example", security, 80, "/ap1")
}

pub(crate) fn base_world() -> (World, Connection) {
    let mut world = World::new();
    world.device("/wifi1", "wlan0", 2);
    world.ap("/wifi1", "/ap1", b"Example", 40, KEY_MGMT_PSK);
    world.ap("/wifi1", "/ap2", b"Example", 90, KEY_MGMT_PSK);
    world.set("/wifi1", WIRELESS_IFACE, vec![("ActiveAccessPoint", o("/ap1"))]);
    let profile = make_wifi_connection(&wifi("wpa-personal", b"Example"), Some("test-secret")).unwrap();
    world.profile("/profiles/1", &profile, 0);
    (world, profile)
}

pub(crate) fn wireguard_profile() -> Connection {
    let connection = Connection::new().unwrap();
    let general = Setting::new("connection").unwrap();
    general.set_str("id", Some("External tunnel"));
    general.set_str("uuid", Some(&libnm::uuid_generate()));
    general.set_str("type", Some("wireguard"));
    general.set_str("interface-name", Some("wg-example"));
    connection.add_setting(&general);
    connection.add_setting(&Setting::new("wireguard").unwrap());
    connection
}

#[test]
fn refresh_reads_without_mutations_and_groups_strongest_ap() {
    let (world, profile) = base_world();
    let h = Harness::new(world);
    assert!(h.backend.ready());
    assert!(h.mutations().is_empty());
    assert_eq!(h.bus.methods().into_iter().collect::<Vec<_>>(), ["GetManagedObjects", "GetNameOwner", "GetSettings"]);
    let s = h.backend.state();
    assert_eq!(s.networks.len(), 1);
    let network = s.networks.values().next().unwrap();
    assert_eq!(network.ap_path, "/ap2");
    assert_eq!(network.strength, 90);
    assert!(network.active);
    assert_eq!(network.profile_ids, [profile.uuid()]);
    assert_eq!(s.profiles.get(&profile.uuid()).unwrap().device_ids, ["/wifi1"]);
    assert_eq!(s.hostname, "test-host");
    assert_eq!(s.connectivity, "Online");
}

#[test]
fn networks_are_separate_for_adapter_security_and_hidden_beacons() {
    let (mut world, _) = base_world();
    world.ap("/wifi1", "/open", b"Example", 50, 0);
    world.ap("/wifi1", "/hidden1", b"", 50, KEY_MGMT_PSK);
    world.ap("/wifi1", "/hidden2", b"", 50, KEY_MGMT_PSK);
    world.device("/wifi2", "wlan1", 2);
    world.ap("/wifi2", "/other", b"Example", 50, KEY_MGMT_PSK);
    let h = Harness::new(world);
    assert_eq!(h.backend.state().networks.len(), 5);
    assert_eq!(h.backend.state().devices.len(), 2);
    let hidden = h.backend.state().networks.values().find(|n| n.ap_path == "/hidden1").unwrap().label.clone();
    assert_eq!(hidden, "Hidden network");
}

#[test]
fn virtual_devices_with_same_missing_hardware_path_stay_separate() {
    let mut world = World::new();
    world.device("/devices/lo", "lo", 32);
    world.device("/devices/p2p", "p2p-dev-wlan0", 30);
    world.device("/devices/vpn", "wg-mullvad", 29);
    let address = glib::VariantDict::new(None);
    address.insert_value("address", &"127.0.0.1".to_variant());
    address.insert_value("prefix", &8u32.to_variant());
    world.set("/ip_loopback", IP4_IFACE, vec![("AddressData", aasv(vec![address.end()]))]);
    world.set("/devices/lo", DEVICE_IFACE, vec![("State", 100u32.to_variant()), ("Ip4Config", o("/ip_loopback"))]);
    world.active("/active/lo", "loopback-uuid", "loopback", &["/devices/lo"], false, 0);
    let h = Harness::new(world);
    let s = h.backend.state();
    let mut keys: Vec<&String> = s.devices.keys().collect();
    keys.sort();
    assert_eq!(keys, ["/devices/lo", "/devices/p2p", "/devices/vpn"]);
    assert_eq!(s.active.get("/active/lo").unwrap().device_ids, ["/devices/lo"]);
    assert_eq!(s.devices.get("/devices/lo").unwrap().active_ids, ["/active/lo"]);
    assert!(s.devices.get("/devices/p2p").unwrap().active_ids.is_empty());
    assert!(s.devices.get("/devices/vpn").unwrap().active_ids.is_empty());
    assert_eq!(s.devices.get("/devices/lo").unwrap().detail("IPv4 addresses"), Some("127.0.0.1/8"));
    assert!(s.devices.get("/devices/p2p").unwrap().detail("IPv4 addresses").is_none());
    assert_eq!(s.devices.get("/devices/p2p").unwrap().kind, "wifi-p2p");
}

pub(crate) fn ip_config(world: &mut World, path: &str) {
    let address = glib::VariantDict::new(None);
    address.insert_value("address", &"192.0.2.5".to_variant());
    address.insert_value("prefix", &24u32.to_variant());
    let dns = glib::VariantDict::new(None);
    dns.insert_value("address", &"192.0.2.53".to_variant());
    let route = glib::VariantDict::new(None);
    route.insert_value("dest", &"0.0.0.0".to_variant());
    route.insert_value("prefix", &0u32.to_variant());
    route.insert_value("next-hop", &"192.0.2.1".to_variant());
    route.insert_value("metric", &600u32.to_variant());
    world.set(path, IP4_IFACE, vec![("AddressData", aasv(vec![address.end()])), ("Gateway", "192.0.2.1".to_variant()),
        ("NameserverData", aasv(vec![dns.end()])), ("Domains", vec!["example.invalid"].to_variant()),
        ("RouteData", aasv(vec![route.end()]))]);
}

#[test]
fn active_details_include_ipv4_dns_routes_speed_and_vpn() {
    let (mut world, profile) = base_world();
    ip_config(&mut world, "/ip");
    world.set("/wifi1", DEVICE_IFACE, vec![("Ip4Config", o("/ip"))]);
    world.active("/active", &profile.uuid(), "802-11-wireless", &["/wifi1"], false, 0);
    world.set("/active", ACTIVE_IFACE, vec![("Ip4Config", o("/ip"))]);
    world.active("/vpn", "vpn-uuid", "vpn", &[], true, 0);
    let h = Harness::new(world);
    let s = h.backend.state();
    let active = s.active.get("/active").unwrap();
    let detail = |k: &str| active.details.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(detail("IPv4 addresses"), "192.0.2.5/24");
    assert_eq!(detail("IPv4 DNS"), "192.0.2.53");
    assert_eq!(detail("IPv4 search domains"), "example.invalid");
    assert!(detail("IPv4 routes").contains("0.0.0.0/0 via 192.0.2.1 (metric 600)"));
    assert_eq!(active.kind, "wifi");
    assert_eq!(detail("Type"), "Wifi");
    assert_eq!(s.devices.get("/wifi1").unwrap().detail("Link speed"), Some("433 Mb/s"));
    assert!(s.active.get("/vpn").unwrap().vpn);
    assert_eq!(s.profiles.get(&profile.uuid()).unwrap().active_ids, ["/active"]);
}

#[test]
fn snapshot_reads_explicit_external_ownership_without_writes() {
    let (mut world, profile) = base_world();
    let wg = wireguard_profile();
    world.profile("/profiles/wg", &wg, 0x8 | 0x2 | 0x1);
    world.active("/wg_active", &wg.uuid(), "wireguard", &[], false, ACTIVATION_FLAG_EXTERNAL);
    let mut h = Harness::new(world);
    assert!(h.backend.state().profiles.get(&wg.uuid()).unwrap().external);
    assert!(h.backend.state().active.get("/wg_active").unwrap().external);
    assert!(!h.backend.state().profiles.get(&profile.uuid()).unwrap().external);
    assert!(h.mutations().is_empty());
    h.world.set("/profiles/wg", CONNECTION_IFACE, vec![("Flags", 0x3u32.to_variant())]);
    h.world.set("/wg_active", ACTIVE_IFACE, vec![("StateFlags", 0x10u32.to_variant())]);
    h.sync();
    assert!(!h.backend.state().profiles.get(&wg.uuid()).unwrap().external);
    assert!(!h.backend.state().active.get("/wg_active").unwrap().external);
}

#[test]
fn external_vpn_actions_are_rejected_before_any_native_action() {
    let (mut world, _) = base_world();
    let wg = wireguard_profile();
    world.profile("/profiles/wg", &wg, SETTINGS_FLAG_EXTERNAL);
    world.active("/wg_active", &wg.uuid(), "wireguard", &[], false, 0);
    let h = Harness::new(world);
    assert!(!h.backend.activate_profile(&wg.uuid(), None));
    assert!(!h.backend.deactivate("/wg_active"));
    assert!(h.backend.error().contains("managed externally"));
    assert!(h.mutations().is_empty());
    assert!(h.busy().is_empty());
}

#[test]
fn vpn_actions_recheck_ownership_after_snapshot_before_queueing() {
    let (mut world, _) = base_world();
    let wg = wireguard_profile();
    world.profile("/profiles/wg", &wg, 0);
    world.active("/wg_active", &wg.uuid(), "wireguard", &[], false, 0);
    let h = Harness::new(world);
    // Ownership changes arrive as property updates; the models are not rebuilt yet.
    h.signal(":1.7", "/profiles/wg", "org.freedesktop.DBus.Properties", "PropertiesChanged",
             changed(CONNECTION_IFACE, "Flags", SETTINGS_FLAG_EXTERNAL.to_variant()));
    assert!(!h.backend.activate_profile(&wg.uuid(), None));
    assert!(!h.backend.deactivate("/wg_active"));
    h.signal(":1.7", "/profiles/wg", "org.freedesktop.DBus.Properties", "PropertiesChanged",
             changed(CONNECTION_IFACE, "Flags", 0u32.to_variant()));
    h.signal(":1.7", "/wg_active", "org.freedesktop.DBus.Properties", "PropertiesChanged",
             changed(ACTIVE_IFACE, "StateFlags", ACTIVATION_FLAG_EXTERNAL.to_variant()));
    assert!(!h.backend.activate_profile(&wg.uuid(), None));
    assert!(!h.backend.deactivate("/wg_active"));
    assert!(h.mutations().is_empty());
}

#[test]
fn saved_managed_wireguard_uses_profile_activation_and_active_deactivation() {
    let (mut world, _) = base_world();
    let wg = wireguard_profile();
    world.profile("/profiles/wg", &wg, 0x1);
    world.active("/wg_active", &wg.uuid(), "wireguard", &[], false, 0);
    let h = Harness::new(world);
    assert!(h.backend.activate_profile(&wg.uuid(), None));
    let call = h.mutations()[0].clone();
    assert_eq!(call.method, "ActivateConnection");
    let params = call.params.unwrap();
    assert_eq!(params.type_().as_str(), "(ooo)");
    assert_eq!(params.print(false).as_str(), "('/profiles/wg', '/', '/')");
    h.reply_ok("ActivateConnection");
    assert!(h.backend.deactivate("/wg_active"));
    h.reply_ok("DeactivateConnection");
    assert_eq!(h.mutations().iter().map(|c| c.method.clone()).collect::<Vec<_>>(), ["ActivateConnection", "DeactivateConnection"]);
    assert!(h.mutations().iter().all(|c| c.interactive));
}

#[test]
fn saved_wifi_activates_existing_profile_without_creating_duplicate() {
    let (world, profile) = base_world();
    let h = Harness::new(world);
    let network_id = h.backend.state().networks.keys().next().unwrap().clone();
    assert!(h.backend.connect_wifi(&network_id, None));
    let call = h.mutations()[0].clone();
    assert_eq!(call.method, "ActivateConnection");
    assert_eq!(call.params.unwrap().child_value(0).str(), Some("/profiles/1"));
    assert!(h.busy().contains(&format!("activate:{}", profile.uuid())));
    h.reply_ok("ActivateConnection");
    assert!(h.busy().is_empty());
}

#[test]
fn new_wifi_profile_targets_requested_adapter_and_ap() {
    let (mut world, _) = base_world();
    world.settings.clear();
    world.list(SETTINGS_PATH, SETTINGS_IFACE, "Connections", &[]);
    let h = Harness::new(world);
    let network_id = h.backend.state().networks.keys().next().unwrap().clone();
    assert!(h.backend.connect_wifi(&network_id, Some("test-secret")));
    let call = h.mutations()[0].clone();
    assert_eq!(call.method, "AddAndActivateConnection");
    let params = call.params.unwrap();
    assert_eq!(params.child_value(1).str(), Some("/wifi1"));
    assert_eq!(params.child_value(2).str(), Some("/ap2"));
    let sent = Connection::from_dbus(&params.child_value(0)).unwrap();
    assert_eq!(sent.setting("802-11-wireless-security").unwrap().get_string("psk").as_deref(), Some("test-secret"));
    assert!(!format!("{:?}", h.backend.state().networks).contains("test-secret"));
    let permissions = sent.setting("connection").unwrap().get_strv("permissions");
    assert_eq!(permissions, [format!("user:{}:", current_user())]);
}

#[test]
fn stale_network_device_or_profile_rejected_without_action() {
    let (world, profile) = base_world();
    let h = Harness::new(world);
    assert!(!h.backend.connect_wifi("missing", None));
    assert!(!h.backend.activate_profile("missing", None));
    assert!(!h.backend.activate_profile(&profile.uuid(), Some("/missing")));
    assert!(!h.backend.disconnect_device("missing"));
    assert!(!h.backend.deactivate("missing"));
    assert!(!h.backend.delete_profile("missing"));
    assert!(h.mutations().is_empty());
}

#[test]
fn radio_actions_are_async_and_do_not_optimistically_change_state() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    assert!(h.backend.set_wireless(false));
    assert!(h.backend.state().wireless_enabled);
    let call = h.mutations().last().unwrap().clone();
    assert_eq!((call.method.as_str(), call.interface.as_str()), ("Set", "org.freedesktop.DBus.Properties"));
    assert_eq!(call.params.unwrap().print(false).as_str(), "('org.freedesktop.NetworkManager', 'WirelessEnabled', <false>)");
    h.reply_ok("Set");
    assert!(h.backend.set_wwan(false));
    assert_eq!(h.mutations().last().unwrap().params.clone().unwrap().child_value(1).str(), Some("WwanEnabled"));
    h.reply_ok("Set");
    assert!(h.backend.set_networking(false));
    let call = h.mutations().last().unwrap().clone();
    assert_eq!((call.method.as_str(), call.params.unwrap().print(false).as_str()), ("Enable", "(false,)"));
    assert!(call.interactive && call.mutation);
}

#[test]
fn device_enabled_tracks_management_instead_of_connection_state() {
    let (world, _) = base_world();
    let mut h = Harness::new(world);
    for state in [30u32, 20, 40, 100] {
        h.world.set("/wifi1", DEVICE_IFACE, vec![("State", state.to_variant())]);
        h.sync();
        assert!(h.backend.state().devices.get("/wifi1").unwrap().enabled());
    }
    h.world.set("/wifi1", DEVICE_IFACE, vec![("Managed", false.to_variant())]);
    h.sync();
    assert!(!h.backend.state().devices.get("/wifi1").unwrap().enabled());
}

#[test]
fn adapter_toggle_targets_only_its_canonical_device_and_administrative_state() {
    let (mut world, profile) = base_world();
    world.device("/wired1", "eth0", 1);
    world.active("/active/wifi", &profile.uuid(), "802-11-wireless", &["/wifi1"], false, 0);
    world.active("/active/wired", "wired-profile", "802-3-ethernet", &["/wired1"], false, 0);
    let mut h = Harness::new(world);
    assert!(h.backend.set_device_enabled("/wifi1", false));
    let calls = h.mutations();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].path.as_str(), calls[0].method.as_str()), ("/wifi1", "SetManaged"));
    assert_eq!(calls[0].params.clone().unwrap().get::<(u32, u32)>(), Some((MANAGED_NO, MANAGED_RUNTIME | MANAGED_SET_ADMIN_STATE)));
    assert!(h.busy().contains("device:/wifi1"));
    assert!(h.backend.state().devices.get("/wifi1").unwrap().enabled());
    assert!(h.backend.state().networking_enabled && h.backend.state().wireless_enabled);
    h.reply_ok("SetManaged");
    assert!(!h.busy().contains("device:/wifi1"));
    h.world.set("/wifi1", DEVICE_IFACE, vec![("Managed", false.to_variant())]);
    h.world.list(NM_PATH, NM_IFACE, "ActiveConnections", &["/active/wired".into()]);
    h.sync();
    assert!(!h.backend.state().devices.get("/wifi1").unwrap().enabled());
    assert!(h.backend.state().devices.get("/wired1").unwrap().enabled());
    assert_eq!(h.backend.state().devices.get("/wired1").unwrap().active_ids, ["/active/wired"]);
    assert_eq!(h.mutations().len(), 1);
}

#[test]
fn adapter_enable_does_not_change_shared_global_radio_or_networking() {
    let (mut world, _) = base_world();
    world.set("/wifi1", DEVICE_IFACE, vec![("Managed", false.to_variant())]);
    world.set(NM_PATH, NM_IFACE, vec![("NetworkingEnabled", false.to_variant()), ("WirelessEnabled", false.to_variant()),
                                      ("WirelessHardwareEnabled", false.to_variant())]);
    let h = Harness::new(world);
    assert!(h.backend.set_device_enabled("/wifi1", true));
    assert_eq!(h.mutations()[0].params.clone().unwrap().get::<(u32, u32)>(), Some((MANAGED_YES, 17)));
    assert_eq!(h.mutations().len(), 1);
    h.reply_ok("SetManaged");
    assert!(!h.backend.state().networking_enabled);
}

#[test]
fn adapter_toggle_rejects_stale_unsupported_and_invalid_requests() {
    let (mut world, _) = base_world();
    world.device("/loopback", "lo", 32);
    world.device("/p2p", "p2p-wlan0", 30);
    let mut h = Harness::new(world);
    for (path, enabled) in [("/missing", false), ("/loopback", false), ("/p2p", true), ("", false)] {
        assert!(!h.backend.set_device_enabled(path, enabled), "{path}");
    }
    h.world.list(NM_PATH, NM_IFACE, "Devices", &[]);
    h.sync();
    assert!(!h.backend.set_device_enabled("/wifi1", false));
    assert!(h.mutations().is_empty());
}

#[test]
fn adapter_operations_are_independent_and_repeated_requests_are_blocked() {
    let (mut world, _) = base_world();
    world.device("/wired1", "eth0", 1);
    let h = Harness::new(world);
    assert!(h.backend.set_device_enabled("/wifi1", false));
    assert!(!h.backend.set_device_enabled("/wifi1", true));
    assert!(h.backend.set_device_enabled("/wired1", false));
    assert_eq!(h.busy(), HashSet::from(["device:/wifi1".to_string(), "device:/wired1".to_string()]));
    assert_eq!(h.mutations().len(), 2);
    h.bus.reply("SetManaged", Ok(().to_variant()), Some("/wifi1"), None, 0);
    assert_eq!(h.busy(), HashSet::from(["device:/wired1".to_string()]));
    h.reply_ok("SetManaged");
    assert!(h.busy().is_empty());
}

#[test]
fn adapter_permission_failure_keeps_native_state_and_clears_pending() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    h.backend.set_device_enabled("/wifi1", false);
    h.bus.reply("SetManaged", Err(BusError::local("Permission denied")), None, None, 0);
    assert!(h.backend.state().devices.get("/wifi1").unwrap().enabled());
    assert!(h.busy().is_empty());
    assert_eq!(h.errors.borrow().last().unwrap(), "Permission denied");
}

#[test]
fn adapter_unknown_method_failure_does_not_fall_back_to_managed_property() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    assert!(h.backend.set_device_enabled("/wifi1", false));
    h.reply_err("SetManaged", "org.freedesktop.DBus.Error.UnknownMethod", "SetManaged is unavailable");
    assert_eq!(h.errors.borrow().last().unwrap(), "SetManaged is unavailable");
    assert!(h.busy().is_empty());
    assert_eq!(h.mutations().len(), 1, "no legacy Managed property fallback");
}

#[test]
fn hardware_block_and_disabled_networking_reject_actions() {
    let (world, profile) = base_world();
    let h = Harness::new(world);
    {
        let mut s = h.backend.state_mut();
        s.wireless_hardware_enabled = false;
        s.wwan_hardware_enabled = false;
    }
    let network = h.backend.state().networks.keys().next().unwrap().clone();
    assert!(!h.backend.set_wireless(true));
    assert!(!h.backend.set_wwan(true));
    assert!(!h.backend.scan(None));
    assert!(!h.backend.activate_profile(&profile.uuid(), None));
    assert!(!h.backend.connect_wifi(&network, None));
    assert!(!h.backend.activate_connection(&profile, Some("/wifi1"), Some("/ap1")));
    h.backend.state_mut().networking_enabled = false;
    assert!(!h.backend.activate_profile(&profile.uuid(), None));
    assert!(!h.backend.scan(None));
    assert!(h.mutations().is_empty());
}

#[test]
fn unmanaged_adapter_rejects_saved_new_and_dialog_activation() {
    let (world, profile) = base_world();
    let h = Harness::new(world);
    h.backend.state_mut().devices.get_mut("/wifi1").unwrap().managed = false;
    let network = h.backend.state().networks.keys().next().unwrap().clone();
    assert!(!h.backend.activate_profile(&profile.uuid(), Some("/wifi1")));
    assert!(!h.backend.connect_wifi(&network, None));
    assert!(!h.backend.activate_connection(&profile, Some("/wifi1"), Some("/ap1")));
    assert!(!h.backend.scan(Some("/wifi1")));
    assert!(h.mutations().is_empty());
}

#[test]
fn scan_stays_busy_until_scan_timestamp_advances() {
    let (world, _) = base_world();
    let mut h = Harness::new(world);
    assert!(h.backend.scan(Some("/wifi1")));
    assert!(h.busy().contains("scan:/wifi1"));
    let call = h.mutations()[0].clone();
    assert_eq!((call.interface.as_str(), call.method.as_str(), call.params.unwrap().print(false).as_str()),
               (WIRELESS_IFACE, "RequestScan", "({},)"));
    h.reply_ok("RequestScan");
    assert!(h.busy().contains("scan:/wifi1"));
    assert!(h.scheduler.delays().contains(&12000));
    h.world.set("/wifi1", WIRELESS_IFACE, vec![("LastScan", 200i64.to_variant())]);
    h.sync();
    assert!(!h.busy().contains("scan:/wifi1"));
}

#[test]
fn async_denial_clears_busy_and_leaves_actual_state_unchanged() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    h.backend.set_wireless(false);
    h.reply_err("Set", "org.freedesktop.NetworkManager.PermissionDenied", "Not authorized to control networking.");
    assert!(h.busy().is_empty());
    assert!(h.backend.state().wireless_enabled);
    assert_eq!(h.errors.borrow().last().unwrap(), "You are not allowed to change this network setting.");
}

#[test]
fn daemon_loss_and_close_ignore_old_replies() {
    let (world, _) = base_world();
    let mut h = Harness::new(world);
    h.backend.set_wireless(false);
    h.signal("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged",
             (NM_BUS, ":1.7", "").to_variant());
    assert!(!h.backend.ready());
    assert!(h.busy().is_empty());
    assert!(h.backend.state().devices.is_empty());
    assert_eq!(h.backend.error(), "NetworkManager is not running.");
    let count = h.changes.get();
    h.reply_ok("Set");
    assert_eq!(h.changes.get(), count);
    h.sync();
    assert!(h.backend.ready());
    h.backend.set_wireless(false);
    h.backend.close();
    let count = h.changes.get();
    h.reply_ok("Set");
    assert_eq!(h.changes.get(), count);
}

#[test]
fn vpn_disconnect_and_transient_failures_are_reported() {
    let (mut world, _) = base_world();
    world.active("/vpn", "vpn-uuid", "vpn", &[], true, 0);
    let h = Harness::new(world);
    assert!(h.backend.deactivate("/vpn"));
    assert_eq!(h.mutations().last().unwrap().method, "DeactivateConnection");
    h.signal(":1.7", "/vpn", VPN_IFACE, "VpnStateChanged", (6u32, 10u32).to_variant());
    assert!(h.errors.borrow().last().unwrap().contains("login failed"), "{:?}", h.errors.borrow());
    h.signal(":1.7", "/wifi1", DEVICE_IFACE, "StateChanged", (120u32, 30u32, 7u32).to_variant());
    assert_eq!(h.errors.borrow().last().unwrap(), "Connection on wlan0 failed: no secrets.");
    h.signal(":1.7", "/vpn", ACTIVE_IFACE, "StateChanged", (4u32, 2u32).to_variant());
    assert_eq!(h.errors.borrow().len(), 2, "user disconnect is not a failure");
}

#[test]
fn enterprise_connection_builder_defers_credentials_to_our_form() {
    let mut world = World::new();
    world.device("/wifi1", "wlan0", 2);
    world.ap("/wifi1", "/enterprise", b"Example", 50, KEY_MGMT_802_1X);
    let h = Harness::new(world);
    let network_id = h.backend.state().networks.keys().next().unwrap().clone();
    let connection = h.backend.build_wifi_connection(&network_id).unwrap();
    assert_eq!(connection.setting("802-11-wireless-security").unwrap().get_string("key-mgmt").as_deref(), Some("wpa-eap"));
    assert!(connection.setting("802-1x").is_some());
    assert_eq!(h.backend.get_wifi_objects(&network_id), (Some("/wifi1".into()), Some("/enterprise".into())));
    assert!(h.mutations().is_empty());
}

#[test]
fn incremental_signals_update_the_cache_without_reloading() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    let loads = h.bus.calls.borrow().iter().filter(|c| c.method == "GetManagedObjects").count();
    h.signal(":1.7", "/ap2", "org.freedesktop.DBus.Properties", "PropertiesChanged", changed(AP_IFACE, "Strength", 20u8.to_variant()));
    assert!(h.scheduler.delays().contains(&60), "coalesced 60 ms refresh");
    h.backend.refresh();
    assert_eq!(h.backend.state().networks.values().next().unwrap().ap_path, "/ap1");
    assert_eq!(h.bus.calls.borrow().iter().filter(|c| c.method == "GetManagedObjects").count(), loads);
}

#[test]
fn native_wifi_profiles_validate_and_keep_secrets_agent_owned() {
    for (security, password) in [("open", None), ("owe", None), ("wpa-personal", Some("test-secret")), ("sae", Some("test-secret")),
                                 ("wep", Some("abcde"))] {
        let profile = make_wifi_connection(&wifi(security, b"Example"), password).unwrap();
        profile.verify().unwrap();
        let settings = parse_settings(&profile.to_dbus(libnm::ALL));
        assert_eq!(profile_security(&settings), security);
        assert_eq!(get_bytes(&settings, "802-11-wireless", "ssid").unwrap(), b"Example");
        let wsec = profile.setting("802-11-wireless-security");
        if security == "sae" || security == "wpa-personal" {
            let wsec = wsec.unwrap();
            assert_eq!(wsec.get_string("psk").as_deref(), password);
            assert_eq!(wsec.get_u32("psk-flags"), libnm::AGENT_OWNED);
        } else if security == "wep" {
            assert_eq!(wsec.unwrap().get_u32("wep-key-flags"), libnm::AGENT_OWNED);
        }
    }
}

#[test]
fn invalid_wifi_credentials_and_enterprise_require_configuration() {
    for (network, password) in [(wifi("wpa-personal", b"Example"), Some("short")), (wifi("sae", b"Example"), Some("")),
                                (wifi("wep", b"Example"), Some("")), (wifi("wpa-enterprise", b"Example"), Some("test-secret")),
                                (wifi("wpa-personal", b""), None)] {
        assert!(make_wifi_connection(&network, password).is_err(), "{}", network.security);
    }
}

#[test]
fn device_names_come_from_libnm_descriptions() {
    let (world, _) = base_world();
    let h = Harness::new(world);
    assert_eq!(h.backend.state().devices.get("/wifi1").unwrap().name, "Test adapter");
}

struct LaggingMatcher;

impl Matcher for LaggingMatcher {
    fn compatible(&self, _: &Objects, _: &str, _: &str, _: &Settings) -> Option<bool> {
        None
    }
    fn ap_valid(&self, _: &Objects, _: &str, _: &str, _: &str, _: &Settings) -> Option<bool> {
        None
    }
    fn description(&self, _: &Objects, _: &str) -> Option<String> {
        None
    }
}

#[test]
fn libnm_cache_lag_is_treated_as_unknown_and_rechecked() {
    let (world, profile) = base_world();
    let bus = FakeBus::new();
    let scheduler = Rc::new(FakeScheduler::default());
    let fake: Rc<dyn Bus> = bus.clone();
    let fake = RefCell::new(Some(fake));
    let backend = NetworkBackend::new(|| {}, |_| {}, Options { connector: Box::new(move |done| done(Ok(fake.borrow_mut().take().unwrap()))),
                                                         scheduler: scheduler.clone(), matcher: Rc::new(LaggingMatcher) });
    backend.start();
    bus.reply("GetNameOwner", Ok((":1.7",).to_variant()), None, None, 0);
    bus.reply("GetManagedObjects", Ok(world.variant()), None, None, 0);
    bus.reply("GetSettings", Ok(Variant::tuple_from_iter([world.settings["/profiles/1"].clone()])), None, None, 0);
    let state = backend.state().clone();
    assert!(state.profiles.get(&profile.uuid()).unwrap().device_ids.is_empty());
    assert_eq!(state.devices.get("/wifi1").unwrap().name, "wlan0", "falls back to the interface like `get_description() or iface`");
    assert!(scheduler.delays().contains(&250), "re-checked once libnm caught up");
}
