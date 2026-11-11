//! Splitting a Windows command line into its arguments.
//!
//! The service manager keeps a service's program and arguments as one line. The
//! `windows-service` crate quotes each argument by the rules of the Microsoft C
//! runtime, and this undoes that with the same rules that Rust's own
//! `std::env::args` follows on Windows.

/// The program, then each argument.
pub fn split(line: &str) -> Vec<String> {
    let mut chars = line.chars().peekable();
    let mut arguments = Vec::new();

    // The program name has no escapes: a quote only switches quoting on or off.
    let mut program = String::new();
    let mut in_quotes = false;
    for c in chars.by_ref() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => break,
            _ => program.push(c),
        }
    }
    arguments.push(program);
    while chars.next_if(|&c| c == ' ' || c == '\t').is_some() {}

    let mut current = String::new();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if !in_quotes => {
                arguments.push(std::mem::take(&mut current));
                while chars.next_if(|&c| c == ' ' || c == '\t').is_some() {}
            }
            '\\' => {
                let mut count = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    count += 1;
                }
                if chars.peek() == Some(&'"') {
                    current.extend(std::iter::repeat_n('\\', count / 2));
                    if count % 2 == 1 {
                        chars.next();
                        current.push('"');
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', count));
                }
            }
            '"' if in_quotes => match chars.peek() {
                Some('"') => {
                    current.push('"');
                    chars.next();
                }
                Some(_) => in_quotes = false,
                // A quoted argument at the very end, which may be empty.
                None => break,
            },
            '"' => in_quotes = true,
            _ => current.push(c),
        }
    }
    if !current.is_empty() || in_quotes {
        arguments.push(current);
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quoting of the `windows-service` crate, which wrote the old service's
    /// command line: an argument with a space, tab, newline or quote, or an empty
    /// one, is quoted, and backslashes before a quote or the closing quote doubled.
    fn quote(argument: &str) -> String {
        if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\u{b}', '"']) {
            return argument.to_string();
        }
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for c in argument.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    quoted.push(c);
                    backslashes = 0;
                }
            }
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }

    #[test]
    fn splits_a_plain_line() {
        assert_eq!(
            split(r"C:\speech\speech.exe service --threads 4"),
            [r"C:\speech\speech.exe", "service", "--threads", "4"]
        );
        assert_eq!(split(r"C:\speech\speech.exe"), [r"C:\speech\speech.exe"]);
        assert_eq!(split("a  \t b   c "), ["a", "b", "c"]);
    }

    #[test]
    fn a_quoted_program_path_keeps_its_spaces_and_backslashes() {
        let line = r#""C:\Program Files\Example Speech\speech.exe" service --token-file "C:\Program Data\a b\token""#;
        assert_eq!(
            split(line),
            [r"C:\Program Files\Example Speech\speech.exe", "service", "--token-file", r"C:\Program Data\a b\token"]
        );
    }

    #[test]
    fn follows_the_backslash_and_quote_rules() {
        assert_eq!(split(r#"p a\\b "a\\b""#), ["p", r"a\\b", r"a\\b"]);
        assert_eq!(split(r#"p \"x\" "\\""#), ["p", r#""x""#, r"\"]);
        // Inside quotes, two quotes are one literal quote.
        assert_eq!(split(r#"p "say ""hi""" x"#), ["p", r#"say "hi""#, "x"]);
        assert_eq!(split(r#"p """"#), ["p", "\""]);
        // An empty quoted argument at the end still counts.
        assert_eq!(split(r#"p a """#), ["p", "a", ""]);
        assert_eq!(split(r#"p "" x"#), ["p", "", "x"]);
        assert_eq!(split(r#"p a"b c"d"#), ["p", "ab cd"]);
    }

    #[test]
    fn undoes_the_quoting_the_old_service_was_registered_with() {
        let arguments = [
            r"C:\Program Files\Example Speech\speech.exe",
            "service",
            "--token-file",
            r"C:\a b\token",
            "",
            r"C:\trailing\",
            r"C:\trailing space\",
            r#"a "quoted" word"#,
            r#"back\"slash"#,
            "tab\there",
            "ünïcödé path",
        ];
        let line = arguments.iter().map(|argument| quote(argument)).collect::<Vec<_>>().join(" ");
        assert_eq!(split(&line), arguments);
    }
}
