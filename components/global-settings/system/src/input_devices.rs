//! input_devices.py: read-only pointer capability discovery. Reported values
//! are hardware defaults.
use std::path::Path;

use crate::compat::{casefold, strip};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct InputDevice {
    pub name: String,
    pub kind: String,
    pub paths: Vec<String>,
    /// libinput list-devices fields (`key → value`).
    pub properties: Vec<(String, String)>,
}

impl InputDevice {
    pub fn new(name: &str, kind: &str) -> InputDevice {
        InputDevice { name: name.into(), kind: kind.into(), ..Default::default() }
    }

    fn property(&self, key: &str) -> Option<&str> {
        self.properties.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn known(&self) -> bool { !self.properties.is_empty() }

    pub fn supports(&self, key: &str) -> bool {
        if !self.known() {
            return true; // labwc safely ignores unsupported options.
        }
        let prop = match key {
            "pointerSpeed" | "accelProfile" => "Accel profiles",
            "leftHanded" => "Left-handed",
            "naturalScroll" => "Nat.scrolling",
            "middleEmulation" => "Middle emulation",
            "scrollButton" => "Scroll button",
            "tap" | "threeFingerDrag" => "Tap-to-click",
            "tapButtonMap" => "Tap button map",
            "tapAndDrag" => "Tap-and-drag",
            "dragLock" => "Tap drag lock",
            "disableWhileTyping" => "Disable-w-typing",
            "clickMethod" => "Click methods",
            "scrollMethod" => "Scroll methods",
            _ => return true,
        };
        !["n/a", "none", ""].contains(&strip(self.property(prop).unwrap_or("n/a")))
    }

    /// The device's own on/off default for a boolean option, or None if unknown.
    pub fn default_enabled(&self, key: &str) -> Option<bool> {
        if key == "tap" {
            return Some(true); // labwc enables tapping unless a profile turns it off.
        }
        let prop = match key {
            "naturalScroll" => Some("Nat.scrolling"),
            "middleEmulation" => Some("Middle emulation"),
            "tapAndDrag" => Some("Tap-and-drag"),
            "disableWhileTyping" => Some("Disable-w-typing"),
            _ => None,
        };
        let value = prop.map(|p| casefold(strip(self.property(p).unwrap_or("")))).unwrap_or_default();
        match value.as_str() {
            "enabled" => Some(true),
            "disabled" => Some(false),
            _ => None,
        }
    }

    pub fn allowed(&self, key: &str, value: Option<&str>) -> bool {
        let Some(value) = value else { return true };
        if !self.known() {
            return true;
        }
        let prop = match key {
            "scrollMethod" => "Scroll methods",
            "clickMethod" => "Click methods",
            "accelProfile" => "Accel profiles",
            _ => return true,
        };
        if value == "none" {
            return true;
        }
        let token = match value {
            "twofinger" => "two-finger",
            "onbutton" => "button",
            "buttonAreas" => "button-areas",
            other => other,
        };
        crate::compat::split_whitespace(&self.property(prop).unwrap_or("").replace('*', "")).contains(&token)
    }
}

pub fn parse_devices(output: &str) -> Vec<InputDevice> {
    let mut blocks: Vec<Vec<(String, String)>> = Vec::new();
    let mut current = false;
    for line in crate::compat::splitlines(output, false) {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (strip(key).to_owned(), strip(value).to_owned());
        if key == "Device" {
            blocks.push(Vec::new());
            current = true;
        }
        if current {
            let block = blocks.last_mut().unwrap();
            if let Some(slot) = block.iter_mut().find(|(k, _)| *k == key) { slot.1 = value } else { block.push((key, value)) }
        }
    }
    let mut result: Vec<InputDevice> = Vec::new();
    for item in blocks {
        let get = |k: &str| item.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        if !crate::compat::split_whitespace(&get("Capabilities").unwrap_or_default()).contains(&"pointer") {
            continue;
        }
        let name = get("Device").unwrap_or_default();
        let kind = if get("Tap-to-click").unwrap_or_else(|| "n/a".into()) != "n/a" { "touchpad" } else { "non-touch" };
        let key = casefold(&name);
        let index = match result.iter().position(|d| casefold(&d.name) == key) {
            Some(index) => index,
            None => {
                result.push(InputDevice { name: name.clone(), kind: kind.into(), paths: Vec::new(), properties: item.clone() });
                result.len() - 1
            }
        };
        let path = get("Kernel").unwrap_or_default();
        if !path.is_empty() && !result[index].paths.contains(&path) {
            result[index].paths.push(path);
        }
    }
    result
}

/// `udev_devices()` without pyudev: walk /sys/class/input event nodes and the
/// udev database (`/run/udev/data/c<major>:<minor>`) for ID_INPUT_* tags.
pub fn udev_devices(sysfs: &Path, udev_data: &Path) -> Vec<InputDevice> {
    let mut result: Vec<InputDevice> = Vec::new();
    let Ok(entries) = std::fs::read_dir(sysfs.join("class/input")) else { return result };
    let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for sys_name in names.into_iter().filter(|n| n.starts_with("event")) {
        let node = sysfs.join("class/input").join(&sys_name);
        let dev = std::fs::read_to_string(node.join("dev")).unwrap_or_default();
        let properties: Vec<(String, String)> = std::fs::read_to_string(udev_data.join(format!("c{}", dev.trim())))
            .unwrap_or_default().lines()
            .filter_map(|l| l.strip_prefix("E:")).filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        let get = |k: &str| properties.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        let touchpad = get("ID_INPUT_TOUCHPAD") == Some("1");
        if !touchpad && get("ID_INPUT_MOUSE") != Some("1") && get("ID_INPUT_POINTINGSTICK") != Some("1") {
            continue;
        }
        let name = std::fs::read_to_string(node.join("device/name")).map(|n| n.trim_end_matches('\n').to_owned())
            .unwrap_or_else(|_| get("NAME").unwrap_or(&sys_name).trim_matches('"').to_owned());
        let key = casefold(&name);
        let index = match result.iter().position(|d| casefold(&d.name) == key) {
            Some(index) => index,
            None => {
                result.push(InputDevice::new(&name, if touchpad { "touchpad" } else { "non-touch" }));
                result.len() - 1
            }
        };
        result[index].paths.push(format!("/dev/input/{sys_name}"));
    }
    result
}

/// `libinput list-devices` runner: `(stdout, stderr, returncode)` or an error.
pub type ListDevices = dyn Fn() -> Result<(String, String, i32), ()> + Send + Sync;

pub fn run_libinput() -> Result<(String, String, i32), ()> {
    let mut command = std::process::Command::new("libinput");
    command.arg("list-devices").env("LC_ALL", "C").stdin(std::process::Stdio::null());
    let output = run_timeout(command, 8).ok_or(())?;
    Ok((String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1)))
}

/// `subprocess.run(capture_output=True, timeout=…)`; None on OSError or timeout.
pub fn run_timeout(mut command: std::process::Command, seconds: u64) -> Option<std::process::Output> {
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut out, &mut data).ok();
        data
    });
    let error_reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut err, &mut data).ok();
        data
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    Some(std::process::Output { status, stdout: reader.join().unwrap_or_default(), stderr: error_reader.join().unwrap_or_default() })
}

/// `discover()` with injectable libinput runner and sysfs/udev roots.
pub fn discover_with(list: &ListDevices, sysfs: &Path, udev_data: &Path) -> (Vec<InputDevice>, String) {
    let (mut devices, mut detail) = (Vec::new(), String::new());
    match list() {
        Ok((stdout, stderr, code)) => {
            devices = parse_devices(&stdout);
            if code != 0 || stderr.contains("Permission denied") {
                detail = "Some device capabilities could not be read. Unsupported options are ignored by the device.".into();
            }
        }
        Err(()) => detail = "Device capabilities could not be read. Unsupported options are ignored by the device.".into(),
    }
    let known: Vec<String> = devices.iter().map(|d| casefold(&d.name)).collect();
    devices.extend(udev_devices(sysfs, udev_data).into_iter().filter(|d| !known.contains(&casefold(&d.name))));
    devices.sort_by_key(|d| (d.kind != "touchpad", casefold(&d.name)));
    (devices, detail)
}

pub fn discover() -> (Vec<InputDevice>, String) {
    discover_with(&run_libinput, Path::new("/sys"), Path::new("/run/udev/data"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    #[test]
    fn udev_fallback_and_discovery_order() {
        let root = scratch("udev");
        let sys = root.join("sys");
        let data = root.join("udev");
        std::fs::create_dir_all(&data).unwrap();
        for (event, dev, name, tag) in [("event2", "13:66", "Zed Mouse", "ID_INPUT_MOUSE"), ("event3", "13:67", "Kbd", "ID_INPUT_KEY"),
                                        ("event4", "13:68", "Pad", "ID_INPUT_TOUCHPAD")] {
            let node = sys.join("class/input").join(event);
            std::fs::create_dir_all(node.join("device")).unwrap();
            std::fs::write(node.join("dev"), format!("{dev}\n")).unwrap();
            std::fs::write(node.join("device/name"), format!("{name}\n")).unwrap();
            std::fs::write(data.join(format!("c{dev}")), format!("E:{tag}=1\n")).unwrap();
        }
        let list = || Ok((crate::input_config::tests::LAPTOP.to_owned(), String::new(), 1));
        let (devices, detail) = discover_with(&list, &sys, &data);
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Pad", "SNSL002D:00 2C2F:002D Touchpad", "TPPS/2 Elan TrackPoint", "Zed Mouse"]);
        assert!(detail.starts_with("Some device"));
        assert_eq!(devices[3].paths, ["/dev/input/event2"]);
        let (_, detail) = discover_with(&|| Err(()), &sys, &data);
        assert!(detail.starts_with("Device capabilities could not be read"));
    }
}
