//! `audio-manager` — audio_manager.py (Audio Settings, popup, volume keys).
fn main() {
    std::process::exit(audio_manager::app::main(std::env::args().collect()));
}
