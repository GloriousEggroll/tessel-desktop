fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(tessel_settings::app::main(argv));
}
