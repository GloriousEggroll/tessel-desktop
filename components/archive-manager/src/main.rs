fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(archive_manager::app::main(argv));
}
