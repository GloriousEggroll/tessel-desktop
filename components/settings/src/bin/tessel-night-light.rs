//! `tessel-night-light`: run wlsunset with the Night Light settings (tessel-night-light.service).

fn main() {
    std::process::exit(gs_system::night_light::run_main());
}
