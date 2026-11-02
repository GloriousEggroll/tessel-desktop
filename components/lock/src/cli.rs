//! argparse-compatible CLI (`--preview`, `--check`, `-h/--help`, unambiguous
//! `--` prefixes as with argparse's allow_abbrev) and process hardening.
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub preview: bool,
    pub check: bool,
    pub help: bool,
}

const LONG: [&str; 3] = ["--help", "--preview", "--check"];

pub fn usage(prog: &str) -> String { format!("usage: {prog} [-h] [--preview] [--check]") }

pub fn help(prog: &str) -> String {
    format!("{}\n\nKasumi-inspired GTK4 session lock; only PAM success releases the compositor.\n\noptions:\n  -h, --help  show this help message and exit\n  --preview   show an explicitly unlocked appearance preview\n  --check     check secure locking and authentication support\n", usage(prog))
}

/// Err(message) = argparse error (exit 2).
pub fn parse(arguments: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut unknown = Vec::new();
    let mut positional = false;
    for argument in arguments {
        if positional || !argument.starts_with('-') || argument == "-" {
            unknown.push(argument.clone());
            continue;
        }
        if argument == "--" {
            positional = true;
            continue;
        }
        if argument == "-h" {
            args.help = true;
            continue;
        }
        if argument.starts_with("--") {
            let matches: Vec<&str> = LONG.iter().copied().filter(|name| name.starts_with(argument.as_str())).collect();
            let exact = LONG.contains(&argument.as_str());
            match (exact, matches.as_slice()) {
                (true, _) | (false, [_]) => {
                    let name = if exact { argument.as_str() } else { matches[0] };
                    match name { "--help" => args.help = true, "--preview" => args.preview = true, _ => args.check = true }
                }
                (false, []) => unknown.push(argument.clone()),
                (false, several) => {
                    return Err(format!("ambiguous option: {argument} could match {}", several.join(", ")));
                }
            }
            continue;
        }
        unknown.push(argument.clone());
    }
    if !unknown.is_empty() {
        return Err(format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    Ok(args)
}

/// RLIMIT_CORE 0 and PR_SET_DUMPABLE 0 (also blocks same-uid ptrace).
pub fn protect_process() -> bool {
    // SAFETY: plain syscalls.
    unsafe {
        let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &core);
        libc::prctl(libc::PR_SET_DUMPABLE, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong) == 0
    }
}

/// `--check` JSON (Python json.dumps key order and separators).
pub fn check_json(session_lock: bool, pam_helper: bool, pam_policy: bool) -> String {
    format!("{{\"session_lock\": {session_lock}, \"pam_helper\": {pam_helper}, \"pam_policy\": {pam_policy}}}")
}

pub fn executable(path: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return false };
    // SAFETY: valid C string.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, String> { parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()) }

    #[test]
    fn argparse_semantics() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        assert!(p(&["--preview"]).unwrap().preview);
        assert!(p(&["--pre"]).unwrap().preview);
        assert!(p(&["--c"]).unwrap().check);
        assert!(p(&["-h"]).unwrap().help);
        assert!(p(&["--he"]).unwrap().help);
        assert_eq!(p(&["--bogus"]).unwrap_err(), "unrecognized arguments: --bogus");
        assert_eq!(p(&["x"]).unwrap_err(), "unrecognized arguments: x");
        let both = p(&["--check", "--preview"]).unwrap();
        assert!(both.check && both.preview);
    }

    #[test]
    fn check_json_matches_python() {
        assert_eq!(check_json(true, true, false), "{\"session_lock\": true, \"pam_helper\": true, \"pam_policy\": false}");
    }
}
