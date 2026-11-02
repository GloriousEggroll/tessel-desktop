//! session_transition.py entry point (labwc `--startup` command after Tessel Login).
use gtk::prelude::*;

fn main() {
    let fade = tessel_login::desktop_fade::DesktopFade::new();
    std::process::exit(i32::from(fade.application().run()));
}
