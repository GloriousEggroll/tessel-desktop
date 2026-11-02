fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(tessel_pictures::app::main(argv));
}
