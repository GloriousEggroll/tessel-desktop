fn main() {
    suite_core::i18n::initialize();
    let mut argv = std::env::args_os().map(|a| a.to_string_lossy().into_owned());
    let argv0 = argv.next().unwrap_or_else(|| "wayshell-prompt".into());
    let args: Vec<String> = argv.collect();
    std::process::exit(wayshell_prompt::run(&argv0, &args));
}
