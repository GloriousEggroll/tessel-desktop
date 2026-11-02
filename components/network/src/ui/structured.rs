//! Structured list properties of the generic editor (`PropertyField._array_parser`
//! and the simple-array branch of connection_forms.py): tc `qdiscs`/`tfilters`,
//! SR-IOV `vfs`, OVS `trunks`, bridge `vlans`, team `link-watchers` (libnm's own
//! string parsers/renderers, or Python's formats) and plain `au/ai/ax/at/ab/as`
//! arrays (dcb priorities). One line per item; parse everything first, then
//! replace the property in one step.

use crate::libnm::{api, cstr, opt_string, Setting};
use glib::translate::*;
use glib::{ffi as gffi, gobject_ffi as gobj};
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Qdiscs,
    Tfilters,
    Vfs,
    Trunks,
    Vlans,
    LinkWatchers,
    /// GArray of 32/64-bit integers or booleans (`ai au ax at ab`).
    Ints(&'static str),
    /// GPtrArray of strings (`as`).
    Strings,
}

/// `_array_parser()` (by property name, as Python) or a simple D-Bus array type.
pub fn kind(name: &str, dbus_type: &str) -> Option<Kind> {
    Some(match name {
        "qdiscs" => Kind::Qdiscs,
        "tfilters" => Kind::Tfilters,
        "vfs" => Kind::Vfs,
        "trunks" => Kind::Trunks,
        "vlans" => Kind::Vlans,
        "link-watchers" => Kind::LinkWatchers,
        _ => match dbus_type {
            "ai" => Kind::Ints("ai"),
            "au" => Kind::Ints("au"),
            "ax" => Kind::Ints("ax"),
            "at" => Kind::Ints("at"),
            "ab" => Kind::Ints("ab"),
            "as" => Kind::Strings,
            _ => return None,
        },
    })
}

struct Item(*mut c_void, unsafe extern "C" fn(*mut c_void));

impl Drop for Item {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0) }
    }
}

unsafe fn take_string(raw: *mut c_char) -> Option<String> {
    let out = opt_string(raw);
    if !raw.is_null() {
        gffi::g_free(raw as *mut c_void);
    }
    out
}

/// Python `int(text)` (no base prefixes).
fn int(text: &str) -> Result<i64, ()> {
    let t = text.trim().replace('_', "");
    t.parse::<i64>().map_err(|_| ())
}

fn bounds(text: &str) -> Result<(i64, i64), ()> {
    let parts: Vec<&str> = text.split('-').collect();
    Ok((int(parts[0])?, int(parts[parts.len() - 1])?))
}

/// Parse one line into a libnm boxed item.
fn parse(kind: Kind, line: &str) -> Result<Item, ()> {
    let nm = api().ok_or(())?;
    let text = cstr(line);
    unsafe {
        let mut error = ptr::null_mut();
        let (raw, unref): (*mut c_void, unsafe extern "C" fn(*mut c_void)) = match kind {
            Kind::Qdiscs => ((nm.nm_utils_tc_qdisc_from_str)(text.as_ptr(), &mut error), nm.nm_tc_qdisc_unref),
            Kind::Tfilters => ((nm.nm_utils_tc_tfilter_from_str)(text.as_ptr(), &mut error), nm.nm_tc_tfilter_unref),
            Kind::Vfs => ((nm.nm_utils_sriov_vf_from_str)(text.as_ptr(), &mut error), nm.nm_sriov_vf_unref),
            Kind::Trunks => {
                let (first, last) = bounds(line)?;
                if !(0 <= first && first <= last && last <= 4095) {
                    return Err(());
                }
                ((nm.nm_range_new)(first as u64, last as u64), nm.nm_range_unref)
            }
            Kind::Vlans => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let (first, last) = bounds(parts.first().ok_or(())?)?;
                if !(1 <= first && first <= last && last <= 4094) {
                    return Err(());
                }
                let vlan = (nm.nm_bridge_vlan_new)(first as u16, last as u16);
                (nm.nm_bridge_vlan_set_pvid)(vlan, parts[1..].contains(&"pvid") as i32);
                (nm.nm_bridge_vlan_set_untagged)(vlan, parts[1..].contains(&"untagged") as i32);
                (vlan, nm.nm_bridge_vlan_unref)
            }
            Kind::LinkWatchers => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let mut options: Vec<(&str, &str)> = Vec::new();
                for part in parts.iter().skip(1) {
                    options.push(part.split_once('=').ok_or(())?);
                }
                let get = |key: &str| options.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v);
                let number = |key: &str, default: i64| -> Result<c_int, ()> {
                    let value = match get(key) { Some(v) => int(v)?, None => default };
                    c_int::try_from(value).map_err(|_| ())
                };
                let host = |key: &str| -> Result<std::ffi::CString, ()> { Ok(cstr(get(key).ok_or(())?)) };
                let raw = match *parts.first().ok_or(())? {
                    "ethtool" => (nm.nm_team_link_watcher_new_ethtool)(number("delay-up", 0)?, number("delay-down", 0)?, &mut error),
                    "nsna_ping" => {
                        let target = host("target-host")?;
                        (nm.nm_team_link_watcher_new_nsna_ping)(number("init-wait", 0)?, number("interval", 0)?, number("missed-max", 3)?,
                                                               target.as_ptr(), &mut error)
                    }
                    "arp_ping" => {
                        let (target, source) = (host("target-host")?, host("source-host")?);
                        (nm.nm_team_link_watcher_new_arp_ping2)(number("init-wait", 0)?, number("interval", 0)?, number("missed-max", 3)?,
                            number("vlanid", -1)?, target.as_ptr(), source.as_ptr(), number("flags", 0)? as u32, &mut error)
                    }
                    _ => return Err(()),
                };
                (raw, nm.nm_team_link_watcher_unref)
            }
            Kind::Ints(_) | Kind::Strings => return Err(()),
        };
        if !error.is_null() {
            gffi::g_error_free(error);
        }
        if raw.is_null() { Err(()) } else { Ok(Item(raw, unref)) }
    }
}

/// Render one libnm item like Python's `render`.
unsafe fn render(kind: Kind, item: *mut c_void) -> Option<String> {
    let nm = api()?;
    let mut error = ptr::null_mut();
    let out = match kind {
        Kind::Qdiscs => take_string((nm.nm_utils_tc_qdisc_to_str)(item, &mut error)),
        Kind::Tfilters => take_string((nm.nm_utils_tc_tfilter_to_str)(item, &mut error)),
        Kind::Vfs => take_string((nm.nm_utils_sriov_vf_to_str)(item, 0, &mut error)),
        Kind::Trunks => {
            let (mut first, mut last) = (0u64, 0u64);
            (nm.nm_range_get_range)(item, &mut first, &mut last);
            Some(if first == last { first.to_string() } else { format!("{first}-{last}") })
        }
        Kind::Vlans => {
            let (mut first, mut last) = (0u16, 0u16);
            (nm.nm_bridge_vlan_get_vid_range)(item, &mut first, &mut last);
            let mut text = if first == last { first.to_string() } else { format!("{first}-{last}") };
            if (nm.nm_bridge_vlan_is_pvid)(item) != 0 {
                text += " pvid";
            }
            if (nm.nm_bridge_vlan_is_untagged)(item) != 0 {
                text += " untagged";
            }
            Some(text)
        }
        Kind::LinkWatchers => {
            let name = opt_string((nm.nm_team_link_watcher_get_name)(item)).unwrap_or_default();
            if name == "ethtool" {
                Some(format!("ethtool delay-up={} delay-down={}", (nm.nm_team_link_watcher_get_delay_up)(item),
                             (nm.nm_team_link_watcher_get_delay_down)(item)))
            } else {
                let mut text = format!("{name} init-wait={} interval={} missed-max={} target-host={}",
                    (nm.nm_team_link_watcher_get_init_wait)(item), (nm.nm_team_link_watcher_get_interval)(item),
                    (nm.nm_team_link_watcher_get_missed_max)(item),
                    opt_string((nm.nm_team_link_watcher_get_target_host)(item)).unwrap_or_else(|| "None".into()));
                if name == "arp_ping" {
                    text += &format!(" source-host={} vlanid={} flags={}",
                        opt_string((nm.nm_team_link_watcher_get_source_host)(item)).unwrap_or_else(|| "None".into()),
                        (nm.nm_team_link_watcher_get_vlanid)(item), (nm.nm_team_link_watcher_get_flags)(item));
                }
                Some(text)
            }
        }
        Kind::Ints(_) | Kind::Strings => None,
    };
    if !error.is_null() {
        gffi::g_error_free(error);
    }
    out
}

fn element_size(signature: &str) -> usize {
    match signature {
        "ax" | "at" => 8,
        _ => 4,
    }
}

/// Current items as text lines (`_array_text`).
pub fn read(setting: &Setting, name: &str, kind: Kind) -> Vec<String> {
    let Some(value) = setting.get(name) else { return Vec::new() };
    unsafe {
        let boxed = gobj::g_value_get_boxed(value.to_glib_none().0);
        if boxed.is_null() {
            return Vec::new();
        }
        match kind {
            Kind::Ints(signature) => {
                let array = boxed as *mut gffi::GArray;
                let size = element_size(signature);
                (0..(*array).len as usize).map(|i| {
                    let p = (*array).data.add(i * size);
                    match signature {
                        "ai" => (*(p as *const i32)).to_string(),
                        "au" => (*(p as *const u32)).to_string(),
                        "ax" => (*(p as *const i64)).to_string(),
                        "at" => (*(p as *const u64)).to_string(),
                        _ => (if *(p as *const i32) != 0 { "True" } else { "False" }).to_string(),
                    }
                }).collect()
            }
            _ => {
                let array = boxed as *mut gffi::GPtrArray;
                (0..(*array).len as usize).filter_map(|i| {
                    let item = *(*array).pdata.add(i);
                    match kind {
                        Kind::Strings => opt_string(item as *const c_char),
                        _ => render(kind, item),
                    }
                }).collect()
            }
        }
    }
}

/// Validate every line without touching the setting (`_parse`).
pub fn validate(kind: Kind, lines: &[String]) -> Result<(), ()> {
    for line in lines {
        match kind {
            Kind::Ints("ab") => { int(line)?; }
            Kind::Ints(signature) => {
                let v = int(line)?;
                let ok = match signature {
                    "ai" => i32::try_from(v).is_ok(),
                    "au" => u32::try_from(v).is_ok(),
                    "at" => v >= 0,
                    _ => true,
                };
                if !ok {
                    return Err(());
                }
            }
            Kind::Strings => {}
            _ => { parse(kind, line)?; }
        }
    }
    Ok(())
}

/// Replace the property with the parsed lines.
pub fn write(setting: &Setting, name: &str, kind: Kind, lines: &[String]) -> Result<(), ()> {
    validate(kind, lines)?;
    let spec = setting.find_property(name).ok_or(())?;
    unsafe {
        let mut value = glib::Value::from_type(spec.value_type());
        match kind {
            Kind::Ints(signature) => {
                let size = element_size(signature);
                let array = gffi::g_array_sized_new(0, 1, size as u32, lines.len() as u32);
                for line in lines {
                    let v = int(line)?;
                    let bytes: Vec<u8> = if size == 8 { v.to_ne_bytes().to_vec() } else { (v as i32).to_ne_bytes().to_vec() };
                    gffi::g_array_append_vals(array, bytes.as_ptr() as *const c_void, 1);
                }
                gobj::g_value_take_boxed(value.to_glib_none_mut().0, array as *const c_void);
            }
            Kind::Strings => {
                let array = gffi::g_ptr_array_new_with_free_func(Some(gffi::g_free));
                for line in lines {
                    let text = cstr(line);
                    gffi::g_ptr_array_add(array, gffi::g_strdup(text.as_ptr()) as *mut c_void);
                }
                gobj::g_value_take_boxed(value.to_glib_none_mut().0, array as *const c_void);
            }
            _ => {
                let items: Vec<Item> = lines.iter().map(|l| parse(kind, l)).collect::<Result<_, _>>()?;
                let array = gffi::g_ptr_array_new_with_free_func(Some(items.first().map(|i| i.1).unwrap_or(noop)));
                for item in items {
                    gffi::g_ptr_array_add(array, item.0);
                    std::mem::forget(item);
                }
                gobj::g_value_take_boxed(value.to_glib_none_mut().0, array as *const c_void);
            }
        }
        setting.0.set_property_from_value(name, &value);
    }
    Ok(())
}

unsafe extern "C" fn noop(_: *mut c_void) {}

use glib::prelude::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_structured_list_property_is_editable_and_round_trips() {
        let cases: [(&str, &str, Vec<String>); 7] = [
            ("tc", "qdiscs", lines(&["root sfq", "parent 1234: fq_codel"])),
            ("tc", "tfilters", lines(&["parent ffff: matchall action simple sdata Hello"])),
            ("sriov", "vfs", lines(&["2 mac=00:11:22:33:44:55 trust=true"])),
            ("ovs-port", "trunks", lines(&["10", "20-30"])),
            ("bridge", "vlans", lines(&["1 pvid untagged", "100-200"])),
            ("team", "link-watchers", lines(&["ethtool delay-up=1 delay-down=2",
                "arp_ping init-wait=1 interval=2 missed-max=3 target-host=192.0.2.1 source-host=192.0.2.2 vlanid=-1 flags=0"])),
            ("dcb", "priority-flow-control", lines(&["0", "1", "0", "1", "0", "0", "0", "1"])),
        ];
        for (setting_name, property, wanted) in cases {
            let setting = Setting::new(setting_name).unwrap();
            let dbus = setting.dbus_type(property);
            let kind = kind(property, &dbus).unwrap_or_else(|| panic!("{setting_name}.{property} {dbus}"));
            write(&setting, property, kind, &wanted).unwrap_or_else(|_| panic!("{setting_name}.{property}"));
            let got = read(&setting, property, kind);
            assert_eq!(got.len(), wanted.len(), "{setting_name}.{property}: {got:?}");
            // libnm's own renderers may normalize spacing: parse again must agree.
            let setting2 = Setting::new(setting_name).unwrap();
            write(&setting2, property, kind, &got).unwrap();
            assert_eq!(read(&setting2, property, kind), got, "{setting_name}.{property} is stable");
        }
        let bridge = Setting::new("bridge").unwrap();
        write(&bridge, "vlans", Kind::Vlans, &lines(&["1 pvid untagged", "100-200"])).unwrap();
        assert_eq!(read(&bridge, "vlans", Kind::Vlans), ["1 pvid untagged", "100-200"]);
        let port = Setting::new("ovs-port").unwrap();
        write(&port, "trunks", Kind::Trunks, &lines(&["10", "20-30"])).unwrap();
        assert_eq!(read(&port, "trunks", Kind::Trunks), ["10", "20-30"]);
    }

    #[test]
    fn invalid_lines_are_rejected_without_changing_the_setting() {
        let bridge = Setting::new("bridge").unwrap();
        write(&bridge, "vlans", Kind::Vlans, &lines(&["5"])).unwrap();
        for bad in ["0", "4095", "20-10", "x"] {
            assert!(write(&bridge, "vlans", Kind::Vlans, &lines(&[bad])).is_err(), "{bad}");
        }
        assert_eq!(read(&bridge, "vlans", Kind::Vlans), ["5"]);
        assert!(validate(Kind::Trunks, &lines(&["4096"])).is_err());
        assert!(validate(Kind::Qdiscs, &lines(&["not a qdisc"])).is_err());
        assert!(validate(Kind::LinkWatchers, &lines(&["nsna_ping interval=1"])).is_err(), "target-host is required");
        assert!(validate(Kind::Ints("au"), &lines(&["-1"])).is_err());
        assert_eq!(kind("routing-rules", "aa{sv}"), None, "unparsed structures stay preserved");
    }
}
