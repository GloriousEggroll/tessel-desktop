//! `tessel-capslock-status`: the dock's Caps Lock status stream (JSON lines).

fn main() {
    std::process::exit(tessel_dock::status::capslock::main());
}
