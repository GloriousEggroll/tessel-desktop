//! `tessel-battery` (replaces `battery_manager.py`).

fn main() -> glib::ExitCode {
    tessel_battery::app::main()
}
