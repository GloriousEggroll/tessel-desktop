//! `audio-manager-loopbacks` — loopback_service.py.
fn main() {
    std::process::exit(audio_manager::service::main(std::env::args().collect()));
}
