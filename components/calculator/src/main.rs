fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(calculator::app::main(argv));
}
