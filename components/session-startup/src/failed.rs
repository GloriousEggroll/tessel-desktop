//! Port of `desktop_suite_failed.py`: tell the user a desktop suite service gave up
//! restarting, and offer a restart.
//!
//! Started by desktop-suite-failed@<unit>.service through the components' OnFailure=
//! setting, which systemd only triggers once Restart=on-failure has hit the unit's
//! start limit. The Restart action clears the failed state and starts the unit again.

use std::cell::Cell;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::prelude::*;
use suite_core::tr;

/// Units are named by their component, as on the dock and in Global Settings.
pub const COMPONENTS: [(&str, &str); 11] = [
    ("waydock.service", "Dock"),
    ("wayshell.service", "Desktop shell"),
    ("netman.service", "Network"),
    ("power-manager.service", "Power"),
    ("notifications.service", "Notifications"),
    ("battery-manager.service", "Battery"),
    ("brightness-manager.service", "Brightness"),
    ("polkitman.service", "Authentication agent"),
    ("keep-awake.service", "Keep Awake"),
    ("on-screen-keyboard.service", "On-screen keyboard"),
    ("audio-manager-loopbacks.service", "Audio loopbacks"),
];
/// Keep the offer while the notification may still be listed; then exit quietly.
pub const WAIT_MS: u64 = 30 * 60 * 1000;
pub const USAGE: &str = "usage: desktop_suite_failed.py UNIT.service";

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// `UNIT.fullmatch(name)` with `[A-Za-z0-9:_.\\-]+\.service`.
pub fn valid_unit(name: &str) -> bool {
    name.strip_suffix(".service").is_some_and(|stem| {
        !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric() || ":_.\\-".contains(c))
    })
}

/// The translated component name (unknown units: the unit name without `.service`).
pub fn component_name(unit: &str) -> String {
    let name = COMPONENTS.iter().find(|(u, _)| *u == unit).map(|(_, n)| n.to_string());
    tr!(name.unwrap_or_else(|| unit.strip_suffix(".service").unwrap_or(unit).to_owned()))
}

/// `systemctl --user ARGS` (output captured, 20 s timeout); returns the exit status code.
pub type Systemctl = Rc<dyn Fn(&[&str]) -> Result<i32, String>>;

pub fn real_systemctl() -> Systemctl {
    Rc::new(|arguments: &[&str]| {
        let mut argv = vec!["systemctl", "--user"];
        argv.extend_from_slice(arguments);
        let mut child = Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{}: 'systemctl'", suite_core::pyerr::os_error_text(&error)))?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Ok(status.code().unwrap_or(-1));
            }
            if started.elapsed() >= Duration::from_secs(20) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Command '{}' timed out after 20 seconds", crate::pystr::py_repr_list(&argv)));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })
}

/// A compositor exit fails clients first; do not report that as a crash.
pub fn session_running(systemctl: &Systemctl) -> Result<bool, String> {
    let display = std::env::var_os("WAYLAND_DISPLAY").filter(|v| !v.is_empty());
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty());
    if let (Some(display), Some(runtime)) = (display, runtime) {
        if !Path::new(&runtime).join(display).exists() {
            return Ok(false);
        }
    }
    Ok(systemctl(&["is-active", "--quiet", "graphical-session.target"])? == 0)
}

pub fn restart(unit: &str, systemctl: &Systemctl) -> Result<(), String> {
    systemctl(&["reset-failed", unit])?;
    systemctl(&["start", "--no-block", unit])?;
    Ok(())
}

/// The `Notify(susssasa{sv}i)` parameters.
pub fn notify_parameters(unit: &str) -> glib::Variant {
    let name = component_name(unit);
    let hints = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::new("{sv}").unwrap(),
        [
            glib::variant::DictEntry::new("urgency".to_owned(), 2u8.to_variant()).to_variant(),
            glib::variant::DictEntry::new("desktop-entry".to_owned(), "global-settings".to_variant()).to_variant(),
        ],
    );
    let actions = vec!["restart".to_owned(), tr!("Restart")];
    glib::Variant::tuple_from_iter([
        tr!("Desktop suite").to_variant(),
        0u32.to_variant(),
        "dialog-warning".to_variant(),
        tr!("{name} stopped working", name = name.as_str()).to_variant(),
        tr!("{name} stopped repeatedly and was not restarted automatically.", name = name.as_str()).to_variant(),
        actions.to_variant(),
        hints,
        0i32.to_variant(),
    ])
}

/// Show the notification and wait (up to `wait`) for its action or closing.
pub fn notify(unit: &str, bus: &gio::DBusConnection, systemctl: Systemctl, wait: Duration) -> Result<(), glib::Error> {
    let main_loop = glib::MainLoop::new(None, false);
    let id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let mut subscriptions = Vec::new();
    for member in ["ActionInvoked", "NotificationClosed"] {
        let (id, main_loop, unit, systemctl) = (id.clone(), main_loop.clone(), unit.to_owned(), systemctl.clone());
        #[allow(deprecated)]
        let subscription = bus.signal_subscribe(
            Some(NAME),
            Some(NAME),
            Some(member),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_connection, _sender, _path, _interface, member, parameters| {
                let notification = parameters.try_child_value(0).and_then(|v| v.get::<u32>());
                if notification.is_none() || notification != id.get() {
                    return;
                }
                if member == "ActionInvoked" {
                    let action = parameters.try_child_value(1).and_then(|v| v.get::<String>());
                    if action.as_deref() == Some("restart") {
                        if let Err(error) = restart(&unit, &systemctl) {
                            // Python: the exception escapes the callback and the loop keeps waiting.
                            eprintln!("{error}");
                            return;
                        }
                    }
                }
                main_loop.quit();
            },
        );
        subscriptions.push(subscription);
    }
    let result = bus.call_sync(
        Some(NAME),
        PATH,
        NAME,
        "Notify",
        Some(&notify_parameters(unit)),
        Some(glib::VariantTy::new("(u)").unwrap()),
        gio::DBusCallFlags::NONE,
        10000,
        gio::Cancellable::NONE,
    );
    let outcome = result.map(|reply| {
        id.set(reply.try_child_value(0).and_then(|v| v.get::<u32>()));
        let quit = main_loop.clone();
        let timer = glib::timeout_add_local_once(wait, move || quit.quit());
        main_loop.run();
        if glib::MainContext::default().find_source_by_id(&timer).is_some() {
            timer.remove();
        }
    });
    for subscription in subscriptions {
        #[allow(deprecated)]
        bus.signal_unsubscribe(subscription);
    }
    outcome
}

/// `main(argv)`: exit 2 on a bad argument, else 0 (1 when systemctl itself cannot run,
/// where Python died with a traceback).
pub fn main(args: &[String]) -> i32 {
    if args.len() != 1 || !valid_unit(&args[0]) {
        eprintln!("{USAGE}");
        return 2;
    }
    let systemctl = real_systemctl();
    match session_running(&systemctl) {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    }
    let wait = std::env::var("DESKTOP_SUITE_FAILED_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(WAIT_MS));
    let result = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .and_then(|bus| notify(&args[0], &bus, systemctl, wait));
    if let Err(error) = result {
        // The notification service may be the unit that failed; the journal
        // still records the failure for diagnostics.
        eprintln!("{} failed; no notification could be shown: {}", args[0], error.message());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_follow_python_pattern() {
        for good in ["waydock.service", "a\\x2db.service", "user@1000.service".replace('@', ":").as_str(), "x.y-z_1.service"] {
            assert!(valid_unit(good), "{good}");
        }
        for bad in ["../../evil", ".service", "a b.service", "a.socket", "a/b.service", "é.service"] {
            assert!(!valid_unit(bad), "{bad}");
        }
    }

    #[test]
    fn component_names() {
        assert_eq!(component_name("netman.service"), "Network");
        assert_eq!(component_name("other.service"), "other");
    }

    #[test]
    fn notification_parameters_match_python() {
        let parameters = notify_parameters("netman.service");
        assert_eq!(parameters.type_().as_str(), "(susssasa{sv}i)");
        assert_eq!(
            parameters.print(true).as_str(),
            "('Desktop suite', uint32 0, 'dialog-warning', 'Network stopped working', \
             'Network stopped repeatedly and was not restarted automatically.', ['restart', 'Restart'], \
             {'urgency': <byte 0x02>, 'desktop-entry': <'global-settings'>}, 0)"
        );
    }
}
