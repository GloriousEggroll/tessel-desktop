fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(welcome::app::main(argv));
}
