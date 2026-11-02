//! `tessel-dock-launch <mode> <target>`: launches one application inside the
//! `app-tessel-dock-*.service` unit the dock started for it.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(tessel_dock::core::app_catalog::launch_helper_main(&args));
}
