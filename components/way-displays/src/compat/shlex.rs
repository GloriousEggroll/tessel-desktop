//! `shlex.split(text, comments=True)` (POSIX mode) including its errors.

/// Python `shlex.split(text, comments=True)`; `Err` is the ValueError text.
pub fn split_comments(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut token = String::new();
    let mut quoted = false; // a quote or escape was seen in this token
    let mut in_token = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '#' => {
                // Comment to end of line; it ends the current token.
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                if in_token && (!token.is_empty() || quoted) {
                    words.push(std::mem::take(&mut token));
                }
                in_token = false;
                quoted = false;
            }
            '\'' => {
                in_token = true;
                quoted = true;
                loop {
                    match chars.next() {
                        None => return Err("No closing quotation".into()),
                        Some('\'') => break,
                        Some(q) => token.push(q),
                    }
                }
            }
            '"' => {
                in_token = true;
                quoted = true;
                loop {
                    match chars.next() {
                        None => return Err("No closing quotation".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            None => return Err("No escaped character".into()),
                            Some(e @ ('"' | '\\')) => token.push(e),
                            Some(e) => {
                                token.push('\\');
                                token.push(e);
                            }
                        },
                        Some(q) => token.push(q),
                    }
                }
            }
            '\\' => {
                in_token = true;
                quoted = true;
                match chars.next() {
                    None => return Err("No escaped character".into()),
                    Some(e) => token.push(e),
                }
            }
            ' ' | '\t' | '\r' | '\n' => {
                if in_token && (!token.is_empty() || quoted) {
                    words.push(std::mem::take(&mut token));
                }
                in_token = false;
                quoted = false;
            }
            other => {
                in_token = true;
                token.push(other);
            }
        }
    }
    if in_token && (!token.is_empty() || quoted) {
        words.push(token);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_shlex() {
        assert_eq!(split_comments("xrandr --output \"old\" --primary\n").unwrap(), ["xrandr", "--output", "old", "--primary"]);
        assert_eq!(split_comments("a 'b c' # tail").unwrap(), ["a", "b c"]);
        assert_eq!(split_comments("a\"\\x\\\"\"").unwrap(), ["a\\x\""]);
        assert_eq!(split_comments("''").unwrap(), [""]);
        assert!(split_comments("'open").is_err());
        assert!(split_comments("x \\").is_err());
        assert_eq!(split_comments("a#b c").unwrap(), ["a"]);
    }
}
