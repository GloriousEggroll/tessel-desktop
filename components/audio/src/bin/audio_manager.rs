//! `tessel-audio` — audio_manager.py (Audio Settings, popup, volume keys).
fn main() {
    std::process::exit(tessel_audio::app::main(std::env::args().collect()));
}
