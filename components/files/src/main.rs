fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(tessel_files::app::main(argv));
}
