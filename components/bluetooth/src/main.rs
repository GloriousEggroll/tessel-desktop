//! `tessel-bluetooth.py` main(): `--selftest`, `--gen-pot [file]`, `--gen-mo [dir]`,
//! else validate the launch arguments in this process and run the unique
//! GtkApplication (secondary launches forward their argv to the primary).

use tessel_bluetooth::app::{BluetoothApplication, Hooks, PopupOptions, APPLICATION_ID};
use tessel_bluetooth::{args, i18n, selftest};

fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tessel-bluetooth".into())
}

fn argument_after(args: &[String], flag: &str, default: &str) -> String {
    let index = args.iter().position(|a| a == flag).unwrap();
    args.get(index + 1).cloned().unwrap_or_else(|| default.into())
}

fn write_or_exit(path: &std::path::Path, data: &[u8]) {
    if let Err(error) = std::fs::write(path, data) {
        eprintln!("Traceback (most recent call last):\n{}", tessel_core::pyerr::os_error_path(&error, path));
        std::process::exit(1);
    }
}

fn main() -> std::process::ExitCode {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = prog(argv.first().map(String::as_str).unwrap_or("tessel-bluetooth"));
    let words = &argv[1.min(argv.len())..];
    if words.iter().any(|a| a == "--selftest") {
        return std::process::ExitCode::from(selftest::run() as u8);
    }
    if words.iter().any(|a| a == "--gen-pot") {
        let dest = argument_after(words, "--gen-pot", "messages.po");
        write_or_exit(std::path::Path::new(&dest), i18n::generate_po(&i18n::pot_catalog(), "").as_bytes());
        println!("wrote {dest} ({} msgids)", i18n::messids().len());
        return std::process::ExitCode::SUCCESS;
    }
    if words.iter().any(|a| a == "--gen-mo") {
        let root = argument_after(words, "--gen-mo", "po");
        let languages = i18n::available_languages();
        for lang in &languages {
            let catalog = i18n::mo_catalog(lang);
            let directory = std::path::Path::new(&root).join(lang);
            if let Err(error) = std::fs::create_dir_all(&directory) {
                eprintln!("{}", tessel_core::pyerr::os_error_path(&error, &directory));
                return std::process::ExitCode::from(1);
            }
            write_or_exit(&directory.join("messages.po"), i18n::generate_po(&catalog, lang).as_bytes());
            write_or_exit(&directory.join("messages.mo"), &i18n::generate_mo(&catalog));
        }
        println!("wrote {} catalogs under {root}/", languages.len());
        return std::process::ExitCode::SUCCESS;
    }
    // Validate before registering; the primary re-parses the same arguments.
    // No secondary process constructs a Bluetooth backend.
    if let Err((code, text, stdout)) = args::parse(&prog, words) {
        if stdout { print!("{text}") } else { eprint!("{text}") }
        return std::process::ExitCode::from(code as u8);
    }
    let application = BluetoothApplication::new(APPLICATION_ID, None,
        PopupOptions { popup: false, edge: "bottom".into(), offset: 76 }, Hooks::default(), &prog);
    let code = application.run(&argv);
    std::process::ExitCode::from(i32::from(code) as u8)
}
