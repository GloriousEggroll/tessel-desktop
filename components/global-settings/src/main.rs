fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(global_settings::app::main(argv));
}
