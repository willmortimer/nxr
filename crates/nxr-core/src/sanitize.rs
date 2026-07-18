//! Sanitize untrusted text before terminal rendering.

/// Replace terminal control sequences and disallowed control characters.
///
/// Preserves common whitespace (`\t`, `\n`, `\r`). ANSI/OSC escape sequences
/// and other C0/C1 controls are removed so flake-provided descriptions and
/// subprocess stderr cannot corrupt the user's terminal.
#[must_use]
pub fn sanitize_terminal_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            consume_escape_sequence(&mut chars);
            continue;
        }

        if is_allowed_char(ch) {
            output.push(ch);
        }
    }

    output
}

fn is_allowed_char(ch: char) -> bool {
    match ch {
        '\t' | '\n' | '\r' => true,
        ch if ch.is_control() => false,
        _ => true,
    }
}

fn consume_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(next) = chars.peek().copied() else {
        return;
    };

    match next {
        ']' => {
            chars.next();
            consume_osc_sequence(chars);
        }
        '[' | '(' | ')' | '*' | '#' => {
            chars.next();
            consume_until_final_letter(chars);
        }
        _ => {}
    }
}

fn consume_until_final_letter(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for ch in chars.by_ref() {
        if ch.is_ascii_alphabetic() {
            break;
        }
    }
}

fn consume_osc_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'\\') {
                chars.next();
                break;
            }
        } else if ch == '\u{7}' {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn preserves_plain_text() {
        assert_eq!(
            sanitize_terminal_text("Run the test suite"),
            "Run the test suite"
        );
    }

    #[test]
    fn preserves_tabs_and_newlines() {
        assert_eq!(sanitize_terminal_text("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn strips_ansi_color_sequences() {
        assert_eq!(sanitize_terminal_text("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn strips_bell_and_other_controls() {
        assert_eq!(sanitize_terminal_text("beep\u{7}end"), "beepend");
        assert_eq!(sanitize_terminal_text("a\u{0}b"), "ab");
    }

    #[test]
    fn strips_cursor_hide_show_sequences() {
        assert_eq!(
            sanitize_terminal_text("before\u{1b}[?25lhidden\u{1b}[?25hafter"),
            "beforehiddenafter"
        );
    }

    #[test]
    fn strips_osc_window_title_sequences() {
        assert_eq!(
            sanitize_terminal_text("safe\u{1b}]0;title\u{7}text"),
            "safetext"
        );
    }
}
