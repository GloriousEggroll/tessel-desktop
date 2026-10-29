fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(picture_viewer::app::main(argv));
}
