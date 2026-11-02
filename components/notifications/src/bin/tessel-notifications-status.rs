//! `tessel-notifications-status`: Tessel Dock indicator stream (notification_status.py).

fn main() {
    std::process::exit(tessel_notifications::status::run());
}
