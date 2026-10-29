//! session_transition.py entry point (labwc `--startup` command after Waylogin).
use gtk::prelude::*;

fn main() {
    let fade = waylogin::desktop_fade::DesktopFade::new();
    std::process::exit(i32::from(fade.application().run()));
}
