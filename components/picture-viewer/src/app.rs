//! picture_viewer.py: CLI (argparse), `--check` and the unique GApplication.

use crate::ui::PicturesWindow;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use suite_core::tr;

pub const USAGE: &str = "usage: picture_viewer.py [-h] [-f] [-s] [--check] [files ...]\n";
pub const HELP: &str = "usage: picture_viewer.py [-h] [-f] [-s] [--check] [files ...]

Standalone GTK4 picture viewer and simple image editor.

positional arguments:
  files

options:
  -h, --help        show this help message and exit
  -f, --fullscreen
  -s, --slideshow
  --check
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub files: Vec<String>,
    pub fullscreen: bool,
    pub slideshow: bool,
    pub check: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn error(message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{USAGE}picture_viewer.py: error: {message}\n") }
}

const LONG: [(&str, &str); 4] = [("--help", "-h/--help"), ("--fullscreen", "-f/--fullscreen"), ("--slideshow", "-s/--slideshow"), ("--check", "--check")];

fn negative_number(text: &str) -> bool {
    let body = &text[1..];
    match body.split_once('.') {
        None => !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()),
        Some((i, f)) => i.bytes().all(|b| b.is_ascii_digit()) && !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// `arguments(argv)`: argparse semantics (prefixes, `-fs`, `--`, negative numbers are files).
pub fn arguments(argv: &[String]) -> Result<Options, Exit> {
    let mut options = Options::default();
    let mut extras = Vec::new();
    let mut only_positional = false;
    let set = |options: &mut Options, name: &str| -> Option<Exit> {
        match name {
            "--help" => return Some(Exit { code: 0, stdout: HELP.into(), stderr: String::new() }),
            "--fullscreen" => options.fullscreen = true,
            "--slideshow" => options.slideshow = true,
            _ => options.check = true,
        }
        None
    };
    for argument in argv {
        if only_positional || !argument.starts_with('-') || argument == "-" {
            options.files.push(argument.clone());
            continue;
        }
        if argument == "--" {
            only_positional = true;
            continue;
        }
        if let Some(long) = argument.strip_prefix("--") {
            let (name, explicit) = match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (argument.clone(), None),
            };
            let matches: Vec<&(&str, &str)> = LONG.iter().filter(|(o, _)| *o == name || o.starts_with(name.as_str())).collect();
            let exact = LONG.iter().find(|(o, _)| *o == name);
            let found = exact.or(if matches.len() == 1 { Some(matches[0]) } else { None });
            match found {
                Some((option, shown)) => {
                    if let Some(value) = explicit {
                        return Err(error(&format!("argument {shown}: ignored explicit argument '{value}'")));
                    }
                    if let Some(exit) = set(&mut options, option) {
                        return Err(exit);
                    }
                }
                None if matches.len() > 1 => {
                    let names: Vec<&str> = matches.iter().map(|(o, _)| *o).collect();
                    return Err(error(&format!("ambiguous option: {name} could match {}", names.join(", "))));
                }
                None if negative_number(argument) || argument.contains(' ') => options.files.push(argument.clone()),
                None => extras.push(argument.clone()),
            }
            continue;
        }
        if negative_number(argument) || argument.contains(' ') {
            options.files.push(argument.clone());
            continue;
        }
        // Short flags, possibly combined (-fs); -f=1 is an explicit value.
        let flags: Vec<char> = argument[1..].chars().collect();
        let mut index = 0;
        let mut unknown = false;
        while index < flags.len() {
            let option = match flags[index] {
                'h' => "--help",
                'f' => "--fullscreen",
                's' => "--slideshow",
                _ => { unknown = true; break; }
            };
            if flags.get(index + 1) == Some(&'=') || (index + 1 < flags.len() && !"hfs".contains(flags[index + 1])) {
                let value: String = flags[index + 1..].iter().collect();
                let value = value.strip_prefix('=').unwrap_or(&value).to_string();
                let shown = LONG.iter().find(|(o, _)| *o == option).unwrap().1;
                return Err(error(&format!("argument {shown}: ignored explicit argument '{value}'")));
            }
            if let Some(exit) = set(&mut options, option) {
                return Err(exit);
            }
            index += 1;
        }
        if unknown && index == 0 {
            extras.push(argument.clone());
        }
    }
    if !extras.is_empty() {
        return Err(error(&format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(options)
}

/// `--check`.
pub fn check() -> (String, i32) {
    let mut formats: Vec<&str> = crate::model::SAVE_FORMATS.iter().map(|(_, f)| *f).collect();
    formats.sort();
    formats.dedup();
    let list = formats.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ");
    (format!("{{\"ready\": true, \"gtk\": {}, \"application_id\": \"{}\", \"save_formats\": [{list}]}}", gtk::major_version(),
             crate::APPLICATION_ID), 0)
}

/// `PicturesApplication`.
#[derive(Clone)]
pub struct PicturesApp {
    pub app: gtk::Application,
    pub window: Rc<RefCell<Option<PicturesWindow>>>,
}

impl PicturesApp {
    pub fn window(&self) -> Option<PicturesWindow> { self.window.borrow().clone() }

    /// `do_activate`.
    pub fn activate(&self) -> PicturesWindow {
        let window = match self.window().filter(|w| !w.closed.get()) {
            Some(window) => window,
            None => {
                let window = PicturesWindow::new(&self.app);
                self.window.replace(Some(window.clone()));
                window
            }
        };
        window.window.unminimize();
        window.window.present();
        window
    }
}

pub fn create_application() -> PicturesApp {
    suite_core::i18n::initialize();
    let app = gtk::Application::new(Some(crate::APPLICATION_ID), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    glib::set_prgname(Some(crate::APPLICATION_ID));
    let this = PicturesApp { app: app.clone(), window: Rc::default() };
    let handle = this.clone();
    app.connect_activate(move |_| { handle.activate(); });
    let handle = this.clone();
    app.connect_command_line(move |_, command| {
        let argv: Vec<String> = command.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let options = match arguments(&argv) {
            Ok(options) => options,
            Err(exit) => {
                print!("{}", exit.stdout);
                eprint!("{}", exit.stderr);
                return glib::ExitCode::from(exit.code as u8);
            }
        };
        let window = handle.activate();
        if !options.files.is_empty() {
            let paths: Vec<Option<PathBuf>> = options.files.iter().map(|arg| command.create_file_for_arg(arg).path()).collect();
            if paths.iter().any(Option::is_none) {
                window.message(&tr!("Open a local file or a mounted network folder."));
                return glib::ExitCode::from(1);
            }
            window.open_paths(&paths.into_iter().flatten().collect::<Vec<_>>(), options.slideshow);
        } else if options.slideshow {
            window.toggle_slideshow();
        }
        if options.fullscreen {
            window.window.fullscreen();
        }
        glib::ExitCode::SUCCESS
    });
    this
}

pub fn main(argv: Vec<String>) -> i32 {
    let options = match arguments(&argv[1.min(argv.len())..]) {
        Ok(options) => options,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    if options.check {
        let (status, code) = check();
        println!("{status}");
        return code;
    }
    let app = create_application();
    i32::from(app.app.run_with_args(&argv).get())
}
