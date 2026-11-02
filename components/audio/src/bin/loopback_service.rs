//! `tessel-audio-loopbacks` — loopback_service.py.
fn main() {
    std::process::exit(tessel_audio::service::main(std::env::args().collect()));
}
