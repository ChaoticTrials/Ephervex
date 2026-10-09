//! The single-line text form of an instance's JVM argument list.
//!
//! Arguments are separated by whitespace. An argument that contains
//! whitespace or a double quote, or that is empty, is written inside double
//! quotes, where `\"` stands for a quote and `\\` for a backslash. Every other
//! argument is written as is, so a stored list is shown the way it would be
//! typed and [`parse`] reads back exactly what [`render`] wrote.

/// Why a typed argument line cannot be read as an argument list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// A double quote was opened and never closed.
    UnclosedQuote,
    /// A backslash inside quotes was followed by something other than `"` or `\`.
    UnsupportedEscape,
}

impl ParseError {
    /// The localization message that explains this refusal.
    pub(crate) fn message_id(self) -> &'static str {
        match self {
            Self::UnclosedQuote => "gui-instance-jvm-args-unclosed-quote",
            Self::UnsupportedEscape => "gui-instance-jvm-args-unsupported-escape",
        }
    }
}

/// Writes `arguments` as one line that [`parse`] reads back unchanged.
pub(crate) fn render(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| render_one(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_one(argument: &str) -> String {
    let needs_quotes = argument.is_empty()
        || argument
            .chars()
            .any(|character| character.is_whitespace() || character == '"');
    if !needs_quotes {
        return argument.to_owned();
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    for character in argument.chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// Reads a typed line into arguments, refusing text it cannot read exactly.
///
/// A quoted run may start in the middle of a word, so `-Dx="a b"` is the one
/// argument `-Dx=a b`. Outside quotes a backslash is an ordinary character.
pub(crate) fn parse(line: &str) -> Result<Vec<String>, ParseError> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                in_word = true;
                loop {
                    match characters.next() {
                        None => return Err(ParseError::UnclosedQuote),
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(_) => return Err(ParseError::UnsupportedEscape),
                            None => return Err(ParseError::UnclosedQuote),
                        },
                        Some(inside) => current.push(inside),
                    }
                }
            }
            character if character.is_whitespace() => {
                if in_word {
                    arguments.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        arguments.push(current);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn arguments_without_whitespace_are_written_and_read_as_typed() {
        let stored = strings(&["-XX:+UseZGC", "-Dpath=C:\\games\\mc"]);
        assert_eq!(render(&stored), "-XX:+UseZGC -Dpath=C:\\games\\mc");
        assert_eq!(parse(&render(&stored)), Ok(stored));
    }

    #[test]
    fn an_argument_containing_whitespace_survives_a_render_and_parse() {
        let stored = strings(&["-Dx=a b", "-XX:+UseZGC", "-Dtab=a\tb"]);
        assert_eq!(render(&stored), "\"-Dx=a b\" -XX:+UseZGC \"-Dtab=a\tb\"");
        assert_eq!(parse(&render(&stored)), Ok(stored));
    }

    #[test]
    fn quotes_backslashes_and_empty_arguments_survive_a_render_and_parse() {
        let stored = strings(&["", "say \"hi\"", "a b\\", "\"", "\\"]);
        assert_eq!(parse(&render(&stored)), Ok(stored));
    }

    #[test]
    fn an_empty_or_blank_line_holds_no_arguments() {
        assert_eq!(parse(""), Ok(Vec::new()));
        assert_eq!(parse("  \t "), Ok(Vec::new()));
    }

    #[test]
    fn a_typed_quote_groups_whitespace_into_one_argument() {
        assert_eq!(
            parse("-Xss1m \"-Dx=a b\"  -Dy=\"c d\"e"),
            Ok(strings(&["-Xss1m", "-Dx=a b", "-Dy=c de"]))
        );
        assert_eq!(parse("\"\" x"), Ok(strings(&["", "x"])));
    }

    #[test]
    fn an_unclosed_quote_is_refused() {
        assert_eq!(parse("-Dx=\"a b"), Err(ParseError::UnclosedQuote));
        assert_eq!(parse("\"a\\"), Err(ParseError::UnclosedQuote));
    }

    #[test]
    fn an_unknown_escape_inside_quotes_is_refused() {
        assert_eq!(parse("\"a\\nb\""), Err(ParseError::UnsupportedEscape));
    }
}
