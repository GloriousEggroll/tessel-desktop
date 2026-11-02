//! Tessel Dock indicator stream (port of notification_status.py): one JSON line
//! per change on stdout; exits on SIGTERM/SIGINT or a closed pipe.

use crate::client::NotificationClient;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use tessel_core::tr;

/// The indicator payload for a client snapshot.
pub fn status(ready: bool, error: &str, state: &Map<String, Value>) -> Value {
    let empty = Map::new();
    let settings = state.get("settings").and_then(Value::as_object).unwrap_or(&empty);
    let unread = state.get("unread").cloned().unwrap_or(json!(0));
    let paused = settings.get("do_not_disturb").map_or(false, tessel_core::pyjson::truthy);
    let fullscreen = settings.get("suppress_fullscreen").map_or(true, tessel_core::pyjson::truthy)
        && state.get("presentation").and_then(|p| p.get("fullscreen")).is_some_and(tessel_core::pyjson::truthy);
    let mut text = vec![tr!("Notifications")];
    if !ready {
        text.push(if error.is_empty() { tr!("Notification service unavailable") } else { error.to_owned() });
    } else {
        let count = match &unread {
            Value::Number(n) if !n.is_f64() => tessel_core::Arg::Int(n.as_i64().unwrap_or(0)),
            Value::Number(n) => tessel_core::Arg::Float(n.as_f64().unwrap_or(0.0)),
            other => tessel_core::Arg::Str(tessel_core::pyjson::py_str_or_empty(other)),
        };
        text.push(tessel_core::translate_with("{count} unread", &[("count", count)]));
        if paused {
            text.push(tr!("Do Not Disturb is on"));
        } else if fullscreen {
            text.push(tr!("Popups paused for fullscreen"));
        }
    }
    text.push(tr!("Click for history · Right-click for settings"));
    let unread_truthy = tessel_core::pyjson::truthy(&unread);
    json!({"text": if paused { "\u{f1f6}" } else { "\u{f0f3}" }, "tooltip": text.join("\n"),
           "class": if !ready { "unavailable" } else if paused || fullscreen { "paused" }
                    else if unread_truthy { "unread" } else { "notifications" }})
}

/// Run the stream until signalled or stdout closes.
pub fn run() -> i32 {
    tessel_core::i18n::initialize();
    let main_loop = glib::MainLoop::new(None, false);
    let previous: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let client_slot: Rc<RefCell<Option<NotificationClient>>> = Rc::new(RefCell::new(None));
    let changed = {
        let (previous, slot, main_loop) = (previous.clone(), client_slot.clone(), main_loop.clone());
        move || {
            let (ready, error, state) = match slot.borrow().as_ref() {
                Some(client) => (client.ready(), client.error(), client.state()),
                None => (false, String::new(), Map::new()),
            };
            // json.dumps default: ", "/": " separators, ASCII escapes.
            let encoded = tessel_core::pyfmt::json_dumps(&status(ready, &error, &state), &tessel_core::pyfmt::JsonStyle::DEFAULT);
            if previous.borrow().as_deref() != Some(encoded.as_str()) {
                let mut stdout = std::io::stdout().lock();
                if writeln!(stdout, "{encoded}").and_then(|()| stdout.flush()).is_err() {
                    main_loop.quit();
                } else {
                    previous.replace(Some(encoded));
                }
            }
        }
    };
    let changed = Rc::new(changed);
    let callback = changed.clone();
    let client = NotificationClient::new(move || callback());
    client_slot.replace(Some(client.clone()));
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        tessel_core::glib_compat::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    // Python ignores SIGPIPE-as-signal and sees EPIPE; Rust already ignores SIGPIPE.
    changed();
    client.start();
    main_loop.run();
    client.close();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_classes_and_tooltips() {
        let unavailable = status(false, "Notifications are not running in this session.", &Map::new());
        assert_eq!(unavailable["class"], "unavailable");
        assert_eq!(unavailable["text"], "\u{f0f3}");
        assert_eq!(unavailable["tooltip"], "Notifications\nNotifications are not running in this session.\nClick for history · Right-click for settings");
        let state = |value: Value| value.as_object().unwrap().clone();
        let unread = status(true, "", &state(json!({"unread": 3, "settings": {"do_not_disturb": false}})));
        assert_eq!(unread["class"], "unread");
        assert_eq!(unread["tooltip"], "Notifications\n3 unread\nClick for history · Right-click for settings");
        let dnd = status(true, "", &state(json!({"unread": 0, "settings": {"do_not_disturb": true}})));
        assert_eq!((dnd["class"].as_str(), dnd["text"].as_str()), (Some("paused"), Some("\u{f1f6}")));
        assert!(dnd["tooltip"].as_str().unwrap().contains("Do Not Disturb is on"));
        let game = status(true, "", &state(json!({"unread": 0, "settings": {}, "presentation": {"fullscreen": true}})));
        assert_eq!(game["class"], "paused");
        assert!(game["tooltip"].as_str().unwrap().contains("Popups paused for fullscreen"));
        let idle = status(true, "", &state(json!({"unread": 0, "settings": {"suppress_fullscreen": false},
                                                   "presentation": {"fullscreen": true}})));
        assert_eq!(idle["class"], "notifications");
        let fallback = status(false, "", &Map::new());
        assert!(fallback["tooltip"].as_str().unwrap().contains("Notification service unavailable"));
        // Python json.dumps escapes non-ASCII.
        let line = tessel_core::pyfmt::json_dumps(&unread, &tessel_core::pyfmt::JsonStyle::DEFAULT);
        assert!(line.starts_with("{\"text\": \"\\uf0f3\", \"tooltip\": "), "{line}");
    }
}
