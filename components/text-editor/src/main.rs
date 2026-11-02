use tessel_text_editor::cli::{self, Command};

fn main() -> std::process::ExitCode {
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_else(|| "tessel-text-editor".into());
    let prog = std::path::Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tessel-text-editor".into());
    let rest: Vec<String> = args.collect();
    match cli::parse(&rest) {
        Command::Help => {
            print!("{}", cli::help(&prog));
            std::process::ExitCode::SUCCESS
        }
        Command::Error(message) => {
            eprint!("{}{prog}: error: {message}\n", cli::usage(&prog));
            std::process::ExitCode::from(2)
        }
        Command::Check => {
            println!("{}", cli::check_json());
            std::process::ExitCode::SUCCESS
        }
        Command::Run(files) => {
            tessel_core::i18n::initialize();
            let app = tessel_text_editor::EditorApplication::new();
            let mut argv = vec![argv0];
            argv.extend(files);
            std::process::ExitCode::from(app.run(&argv).get())
        }
    }
}
