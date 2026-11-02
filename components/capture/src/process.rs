//! `subprocess.run(args, input=…, capture_output=True, timeout=…)` and friends.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// `subprocess.TimeoutExpired` (the child was killed).
    Timeout(String),
    /// `OSError` text (e.g. "[Errno 2] No such file or directory: 'ffmpeg'").
    Os(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Timeout(text) | RunError::Os(text) => f.write_str(text),
        }
    }
}

/// `str(TimeoutExpired)`.
pub fn timeout_text(args: &[String], timeout: Duration) -> String {
    let seconds = timeout.as_secs_f64();
    let seconds = if seconds.fract() == 0.0 { format!("{}", seconds as u64) } else { tessel_core::pyfmt::float_repr(seconds) };
    format!("Command '{}' timed out after {seconds} seconds", crate::compat::py_list_repr(args))
}

/// OSError text for a failed spawn: `[Errno 2] No such file or directory: 'ffmpeg'`.
pub fn spawn_error(error: &std::io::Error, program: &str) -> String {
    format!("{}: {}", tessel_core::pyerr::os_error_text(error), crate::compat::py_repr(program))
}

/// Return code like Python: negative signal numbers.
pub fn code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(1))
}

/// Run with stdin `input`, capture stdout/stderr, kill on timeout.
pub fn run(args: &[String], input: &[u8], timeout: Duration) -> Result<Output, RunError> {
    let mut child = Command::new(&args[0]).args(&args[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| RunError::Os(spawn_error(&e, &args[0])))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let err = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = out.join();
                let _ = err.join();
                return Err(RunError::Timeout(timeout_text(args, timeout)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(RunError::Os(tessel_core::pyerr::os_error_text(&error))),
        }
    };
    let _ = writer.join();
    Ok(Output { status: code(status), stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

/// `subprocess.check_output(args, timeout=…, stderr=DEVNULL)`.
pub fn check_output(args: &[&str], timeout: Duration) -> Result<Vec<u8>, RunError> {
    let mut child = Command::new(args[0]).args(&args[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| RunError::Os(spawn_error(&e, args[0])))?;
    let mut stdout = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let data = out.join().unwrap_or_default();
                let code = code(status);
                if code != 0 {
                    return Err(RunError::Os(called_process_error(&owned, code)));
                }
                return Ok(data);
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout(timeout_text(&owned, timeout)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(RunError::Os(tessel_core::pyerr::os_error_text(&error))),
        }
    }
}

/// `str(CalledProcessError)`.
pub fn called_process_error(args: &[String], code: i32) -> String {
    if code < 0 {
        let name = match -code { 2 => "SIGINT", 9 => "SIGKILL", 15 => "SIGTERM", 6 => "SIGABRT", 11 => "SIGSEGV", _ => "" };
        if !name.is_empty() {
            return format!("Command '{}' died with <Signals.{name}: {}>.", crate::compat::py_list_repr(args), -code);
        }
        return format!("Command '{}' died with unknown signal {}.", crate::compat::py_list_repr(args), -code);
    }
    format!("Command '{}' returned non-zero exit status {code}.", crate::compat::py_list_repr(args))
}
