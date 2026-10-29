//! `voice_controls.py KIND ACTION` → one JSON line on stdout (exit 0).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", kasumi_screen::voice::main_with(&args));
}
