fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(tessel_archive::app::main(argv));
}
