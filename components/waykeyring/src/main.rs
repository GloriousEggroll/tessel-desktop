//! waykeyring: the desktop suite's Secret Service.
//!
//!   waykeyring                     serve org.freedesktop.secrets (D-Bus activated / waykeyring.service)
//!   waykeyring --login             PAM helper: password on stdin, unlocks for this login
//!   waykeyring --change-password   PAM helper: "old NUL new" on stdin
//!   waykeyring --status            print the running daemon's status as JSON
//!
//! Test/dev options: --dir DIR, --gnome-dir DIR, --kwallet-dir DIR, --prompt PROGRAM, --no-kernel, --no-chkpwd.
use std::path::PathBuf;
use waykeyring::login::{self, Outcome};
use waykeyring::service::{self, Config};

const USAGE: &str = "Usage: waykeyring [--login | --change-password | --status] [--dir DIR] [--gnome-dir DIR] [--kwallet-dir DIR] \
[--prompt PROGRAM] [--no-kernel] [--no-chkpwd]";

fn status() -> i32 {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        eprintln!("waykeyring: no session bus");
        return 1;
    };
    match bus.call_sync(Some(waykeyring::BUS_NAME), waykeyring::CONTROL_PATH, waykeyring::CONTROL_INTERFACE, "Status",
                        None, None, gio::DBusCallFlags::NO_AUTO_START, 3000, None::<&gio::Cancellable>) {
        Ok(reply) => {
            let map = reply.child_value(0);
            let mut out = serde_json::Map::new();
            for entry in map.iter() {
                let key = entry.child_value(0).get::<String>().unwrap_or_default();
                let value = entry.child_value(1).as_variant().unwrap();
                let json = if let Some(b) = value.get::<bool>() { serde_json::json!(b) }
                    else if let Some(n) = value.get::<u32>() { serde_json::json!(n) }
                    else if let Some(list) = value.get::<Vec<String>>() { serde_json::json!(list) }
                    else { serde_json::json!(value.to_string()) };
                out.insert(key, json);
            }
            println!("{}", serde_json::Value::Object(out));
            0
        }
        Err(e) => {
            eprintln!("waykeyring: not running ({e})");
            1
        }
    }
}

fn main() {
    suite_core::i18n::initialize();
    let exe = std::env::current_exe().unwrap_or_else(|_| "waykeyring".into());
    let mut config = Config::default_for(&exe);
    let mut mode = "serve";
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().map(PathBuf::from).unwrap_or_else(|| {
            eprintln!("waykeyring: {name} needs a value\n{USAGE}");
            std::process::exit(2)
        });
        match arg.as_str() {
            "--login" => mode = "login",
            "--change-password" => mode = "change",
            "--status" => mode = "status",
            "--dir" => config.dir = value("--dir"),
            "--gnome-dir" => config.gnome_dir = value("--gnome-dir"),
            "--kwallet-dir" => config.kwallet_dir = value("--kwallet-dir"),
            "--prompt" => config.prompt = value("--prompt"),
            "--no-kernel" => config.kernel = false,
            "--no-chkpwd" => config.chkpwd = None,
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("waykeyring: unknown argument {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let code = match mode {
        "login" => match login::login(config.dir, &config.kwallet_dir, &login::read_stdin(), config.kernel) {
            Ok(Outcome::WrongPassword) => {
                eprintln!("waykeyring: the login password does not open the keyring (changed elsewhere?); \
                           it will ask once when a secret is needed");
                1
            }
            Ok(_) => 0,
            Err(e) => {
                eprintln!("waykeyring: {e}");
                1
            }
        },
        "change" => match login::change_password(config.dir, &login::read_stdin()) {
            Ok(Outcome::Changed) => 0,
            Ok(_) => {
                eprintln!("waykeyring: the old password does not open the keyring; not changed");
                1
            }
            Err(e) => {
                eprintln!("waykeyring: {e}");
                1
            }
        },
        "status" => status(),
        _ => match service::run(config) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("waykeyring: {e}");
                1
            }
        },
    };
    std::process::exit(code);
}
