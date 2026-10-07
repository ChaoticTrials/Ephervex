//! Line-preserving `server.properties` management.

use std::fmt::Write as _;
use std::net::IpAddr;

use crate::{ServerError, ServerResult};

pub const ONLINE_MODE_KEY: &str = "online-mode";
pub const SERVER_IP_KEY: &str = "server-ip";
pub const SERVER_PORT_KEY: &str = "server-port";
pub const EULA_KEY: &str = "eula";

const PROTECTED_KEYS: &[&str] = &[ONLINE_MODE_KEY, SERVER_IP_KEY, SERVER_PORT_KEY, EULA_KEY];

/// A validated server bind selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerBind {
    Localhost,
    AllInterfaces,
    Custom(IpAddr),
}

impl ServerBind {
    #[must_use]
    pub fn property_value(&self) -> String {
        match self {
            Self::Localhost => "127.0.0.1".to_owned(),
            Self::AllInterfaces => String::new(),
            Self::Custom(address) => address.to_string(),
        }
    }

    #[must_use]
    pub const fn exposes_beyond_localhost(&self) -> bool {
        !matches!(self, Self::Localhost)
    }
}

/// Canonical network values merged at creation, mutation, and startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkProperties {
    pub online_mode: bool,
    pub bind: ServerBind,
    pub port: u16,
}

impl Default for NetworkProperties {
    fn default() -> Self {
        Self {
            online_mode: false,
            bind: ServerBind::Localhost,
            port: 25565,
        }
    }
}

impl NetworkProperties {
    pub fn validate(&self) -> ServerResult<()> {
        if self.port == 0 {
            return Err(ServerError::InvalidProperty {
                key: SERVER_PORT_KEY.to_owned(),
                reason: "port zero is not valid",
            });
        }
        Ok(())
    }

    #[must_use]
    pub const fn warns_about_offline_exposure(&self) -> bool {
        !self.online_mode && self.bind.exposes_beyond_localhost()
    }
}

/// A Java-properties document that leaves every unchanged byte intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertiesDocument {
    lines: Vec<String>,
    preferred_ending: String,
}

impl PropertiesDocument {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let lines = split_lines_preserving(text);
        let preferred_ending = if text.contains("\r\n") {
            "\r\n"
        } else if text.contains('\r') && !text.contains('\n') {
            "\r"
        } else {
            "\n"
        }
        .to_owned();
        Self {
            lines,
            preferred_ending,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.lines.concat()
    }

    /// Returns the effective value using Java's last-value-wins behavior.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        self.logical_entries()
            .into_iter()
            .filter_map(|(start, end)| parse_entry(&self.lines[start..end]))
            .filter(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
            .next_back()
    }

    /// Changes a general property while reserving typed network and EULA keys.
    pub fn set(&mut self, key: &str, value: &str) -> ServerResult<()> {
        validate_key_value(key, value)?;
        if PROTECTED_KEYS.contains(&key) {
            return Err(ServerError::ProtectedProperty {
                key: key.to_owned(),
            });
        }
        self.set_unrestricted(key, value);
        Ok(())
    }

    /// Applies canonical typed network values.
    pub fn merge_network(&mut self, network: &NetworkProperties) -> ServerResult<()> {
        network.validate()?;
        self.set_unrestricted(
            ONLINE_MODE_KEY,
            if network.online_mode { "true" } else { "false" },
        );
        self.set_unrestricted(SERVER_IP_KEY, &network.bind.property_value());
        self.set_unrestricted(SERVER_PORT_KEY, &network.port.to_string());
        Ok(())
    }

    /// Writes EULA state after the caller has validated global consent.
    pub(crate) fn merge_eula_accepted(&mut self) {
        self.set_unrestricted(EULA_KEY, "true");
    }

    fn set_unrestricted(&mut self, key: &str, value: &str) {
        let replacement = format!("{}={}", escape_key(key), escape_value(value));
        let effective = self.logical_entries().into_iter().rfind(|(start, end)| {
            parse_entry(&self.lines[*start..*end]).is_some_and(|(candidate, _)| candidate == key)
        });

        if let Some((start, end)) = effective {
            let ending = line_ending(self.lines.get(end.saturating_sub(1)))
                .unwrap_or_default()
                .to_owned();
            self.lines
                .splice(start..end, [format!("{replacement}{ending}")]);
        } else {
            // A blank line ends a trailing continuation so the entry is not joined to it.
            let continued = self.ends_in_continuation();
            if self
                .lines
                .last()
                .is_some_and(|line| line_ending(Some(line)).is_none())
            {
                self.lines.push(self.preferred_ending.clone());
            }
            if continued {
                // A lone CR followed by LF would read as one CRLF and rejoin the entry.
                let spacer = match line_ending(self.lines.last()) {
                    Some("\r") => "\r".to_owned(),
                    _ => self.preferred_ending.clone(),
                };
                self.lines.push(spacer);
            }
            self.lines
                .push(format!("{replacement}{}", self.preferred_ending));
        }
    }

    fn logical_entries(&self) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        let mut start = 0;
        while start < self.lines.len() {
            let mut end = start + 1;
            if !is_comment(&self.lines[start]) {
                // A continuation that adds nothing starts a fresh logical line.
                let mut has_content = false;
                while end <= self.lines.len() {
                    let line = &self.lines[end - 1];
                    has_content |= !logical_part(line).is_empty();
                    if end < self.lines.len() && is_continued(line) && has_content {
                        end += 1;
                    } else {
                        break;
                    }
                }
            }
            result.push((start, end));
            start = end;
        }
        result
    }

    fn ends_in_continuation(&self) -> bool {
        self.logical_entries().last().is_some_and(|(start, end)| {
            !is_comment(&self.lines[*start]) && is_continued(&self.lines[end - 1])
        })
    }
}

impl Default for PropertiesDocument {
    fn default() -> Self {
        Self::parse("")
    }
}

fn validate_key_value(key: &str, value: &str) -> ServerResult<()> {
    if key.is_empty() || key.contains(['\0', '\n', '\r']) {
        return Err(ServerError::InvalidProperty {
            key: key.to_owned(),
            reason: "keys must be non-empty and single-line",
        });
    }
    if value.contains(['\0', '\n', '\r']) {
        return Err(ServerError::InvalidProperty {
            key: key.to_owned(),
            reason: "values must not contain NUL or line breaks",
        });
    }
    Ok(())
}

fn line_ending(line: Option<&String>) -> Option<&'static str> {
    let line = line?;
    if line.ends_with("\r\n") {
        Some("\r\n")
    } else if line.ends_with('\n') {
        Some("\n")
    } else if line.ends_with('\r') {
        Some("\r")
    } else {
        None
    }
}

fn split_lines_preserving(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let end = match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                index += 2;
                index
            }
            b'\r' | b'\n' => {
                index += 1;
                index
            }
            _ => {
                index += 1;
                continue;
            }
        };
        lines.push(text[start..end].to_owned());
        start = end;
    }
    if start < text.len() {
        lines.push(text[start..].to_owned());
    }
    lines
}

fn is_continued(line: &str) -> bool {
    let content = line.trim_end_matches(['\r', '\n']);
    content
        .chars()
        .rev()
        .take_while(|character| *character == '\\')
        .count()
        % 2
        == 1
}

fn is_comment(line: &str) -> bool {
    line.trim_start_matches(is_properties_whitespace)
        .starts_with(['#', '!'])
}

const fn is_properties_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\u{c}')
}

/// One physical line's contribution to its logical line: no leading whitespace, terminator, or
/// continuation backslash.
fn logical_part(line: &str) -> &str {
    let part = line
        .trim_end_matches(['\r', '\n'])
        .trim_start_matches(is_properties_whitespace);
    if is_continued(part) {
        &part[..part.len() - 1]
    } else {
        part
    }
}

/// Parses one logical entry as `java.util.Properties.load` does.
fn parse_entry(lines: &[String]) -> Option<(String, String)> {
    if is_comment(lines.first()?) {
        return None;
    }
    let logical: String = lines.iter().map(|line| logical_part(line)).collect();
    if logical.is_empty() {
        return None;
    }
    let logical = logical.as_str();

    let mut escaped = false;
    let mut key_end = logical.len();
    let mut value_start = logical.len();
    let mut has_separator = false;
    for (offset, character) in logical.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '=' | ':' => {
                key_end = offset;
                value_start = offset + 1;
                has_separator = true;
                break;
            }
            other if is_properties_whitespace(other) => {
                key_end = offset;
                value_start = offset + other.len_utf8();
                break;
            }
            _ => {}
        }
    }
    for (offset, character) in logical[value_start..].char_indices() {
        if is_properties_whitespace(character) {
            continue;
        }
        if !has_separator && matches!(character, '=' | ':') {
            has_separator = true;
            continue;
        }
        value_start += offset;
        return Some((
            unescape(&logical[..key_end])?,
            unescape(&logical[value_start..])?,
        ));
    }
    Some((unescape(&logical[..key_end])?, String::new()))
}

/// Decodes escapes; a malformed `\uXXXX` makes Java reject the file, so the entry yields nothing.
fn unescape(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => result.push('\t'),
            Some('n') => result.push('\n'),
            Some('r') => result.push('\r'),
            Some('f') => result.push('\u{c}'),
            Some('u') => {
                let unit = unicode_unit(&mut characters)?;
                result.push(match char::from_u32(u32::from(unit)) {
                    Some(decoded) => decoded,
                    None => decode_surrogate(unit, &mut characters),
                });
            }
            Some(other) => result.push(other),
            None => result.push('\\'),
        }
    }
    Some(result)
}

fn unicode_unit(characters: &mut std::str::Chars<'_>) -> Option<u16> {
    let mut unit = 0_u16;
    for _ in 0..4 {
        unit = unit * 16 + u16::try_from(characters.next()?.to_digit(16)?).ok()?;
    }
    Some(unit)
}

/// Pairs a high surrogate with a following `\uXXXX` low surrogate; unpaired halves become U+FFFD.
fn decode_surrogate(unit: u16, characters: &mut std::str::Chars<'_>) -> char {
    if !(0xD800..0xDC00).contains(&unit) {
        return char::REPLACEMENT_CHARACTER;
    }
    let mut lookahead = characters.clone();
    let low = (lookahead.next() == Some('\\') && lookahead.next() == Some('u'))
        .then(|| unicode_unit(&mut lookahead))
        .flatten()
        .filter(|low| (0xDC00..0xE000).contains(low));
    let Some(low) = low else {
        return char::REPLACEMENT_CHARACTER;
    };
    *characters = lookahead;
    let code = 0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
    char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// Escapes like `Properties.store`, so every Java loader reads the file the same whatever its charset.
fn escape_key(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        match character {
            '\\' | '=' | ':' | '#' | '!' | ' ' => {
                result.push('\\');
                result.push(character);
            }
            other => push_encoded(&mut result, other),
        }
    }
    result
}

/// Leading spaces are escaped because Java skips whitespace after the separator.
fn escape_value(value: &str) -> String {
    let mut leading = true;
    let mut result = String::new();
    for character in value.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            ' ' if leading => result.push_str("\\ "),
            other => push_encoded(&mut result, other),
        }
        leading = leading && character == ' ';
    }
    result
}

fn push_encoded(result: &mut String, character: char) {
    match character {
        '\t' => result.push_str("\\t"),
        '\u{c}' => result.push_str("\\f"),
        ' '..='~' => result.push(character),
        other => {
            for unit in other.encode_utf16(&mut [0; 2]) {
                // Writing to a String cannot fail.
                let _ = write!(result, "\\u{unit:04X}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn unchanged_properties_round_trip_byte_for_byte() {
        let source = "# heading\r\nweird\\=key : first\r\ncontinued=one\\\r\n  two\r\n";
        assert_eq!(PropertiesDocument::parse(source).render(), source);
    }

    #[test]
    fn carriage_return_line_endings_are_preserved_when_editing() {
        let mut document = PropertiesDocument::parse("first=one\rsecond=two\r");
        document.set("second", "changed").expect("set");
        assert_eq!(document.render(), "first=one\rsecond=changed\r");
    }

    #[test]
    fn updates_the_effective_duplicate_and_preserves_the_earlier_one() {
        let mut document =
            PropertiesDocument::parse("motd=first\n# keep\nmotd=second\nunknown=x\n");
        document.set("motd", "updated").expect("set");
        assert_eq!(
            document.render(),
            "motd=first\n# keep\nmotd=updated\nunknown=x\n"
        );
        assert_eq!(document.get("motd").as_deref(), Some("updated"));
    }

    #[test]
    fn escaped_separators_and_continuations_are_understood() {
        let document = PropertiesDocument::parse("a\\=b=one\\\n  two\n");
        assert_eq!(document.get("a=b").as_deref(), Some("onetwo"));
    }

    #[test]
    fn network_defaults_are_typed_and_general_edits_cannot_bypass_them() {
        let mut document = PropertiesDocument::default();
        document
            .merge_network(&NetworkProperties::default())
            .expect("merge");
        assert_eq!(document.get(ONLINE_MODE_KEY).as_deref(), Some("false"));
        assert_eq!(document.get(SERVER_IP_KEY).as_deref(), Some("127.0.0.1"));
        assert_eq!(document.get(SERVER_PORT_KEY).as_deref(), Some("25565"));
        assert!(matches!(
            document.set(SERVER_PORT_KEY, "1"),
            Err(ServerError::ProtectedProperty { .. })
        ));
    }

    fn merged(source: &str) -> PropertiesDocument {
        let mut document = PropertiesDocument::parse(source);
        document
            .merge_network(&NetworkProperties::default())
            .expect("merge");
        document
    }

    #[test]
    fn comment_lines_never_continue_onto_the_next_line() {
        let source = "server-ip=10.0.0.1\n# note \\\nserver-ip=\n";
        let document = PropertiesDocument::parse(source);
        assert_eq!(document.get(SERVER_IP_KEY).as_deref(), Some(""));

        let document = merged(source);
        assert_eq!(
            document.render(),
            "server-ip=10.0.0.1\n# note \\\nserver-ip=127.0.0.1\nonline-mode=false\nserver-port=25565\n"
        );
        assert_eq!(document.get(SERVER_IP_KEY).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn appended_entries_are_not_joined_to_an_unterminated_continuation() {
        let document = merged("online-mode=false\nmotd=hello\\");
        assert_eq!(
            document.render(),
            "online-mode=false\nmotd=hello\\\n\nserver-ip=127.0.0.1\nserver-port=25565\n"
        );
        assert_eq!(document.get("motd").as_deref(), Some("hello"));
        assert_eq!(document.get(SERVER_IP_KEY).as_deref(), Some("127.0.0.1"));
        assert_eq!(document.get(SERVER_PORT_KEY).as_deref(), Some("25565"));
    }

    #[test]
    fn appended_entries_are_not_joined_to_a_terminated_continuation() {
        let document = merged("motd=hello\\\n");
        assert_eq!(document.get("motd").as_deref(), Some("hello"));
        assert_eq!(document.get(ONLINE_MODE_KEY).as_deref(), Some("false"));
        assert_eq!(document.get(SERVER_IP_KEY).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn appended_entries_follow_a_comment_ending_in_a_backslash_without_a_spacer() {
        let document = merged("# note \\");
        assert_eq!(
            document.render(),
            "# note \\\nonline-mode=false\nserver-ip=127.0.0.1\nserver-port=25565\n"
        );
    }

    #[test]
    fn carriage_return_terminated_last_line_is_not_given_an_extra_line_break() {
        let mut document = PropertiesDocument::parse("first=one\r");
        document.set("second", "two").expect("set");
        assert_eq!(document.render(), "first=one\rsecond=two\r");
    }

    #[test]
    fn eula_is_not_joined_to_a_dangling_continuation() {
        let mut document = PropertiesDocument::parse("motd=hello\\");
        document.merge_eula_accepted();
        assert_eq!(document.get("motd").as_deref(), Some("hello"));
        assert_eq!(document.get(EULA_KEY).as_deref(), Some("true"));
    }

    #[test]
    fn unicode_escapes_are_decoded_and_surrogate_pairs_are_combined() {
        let document = PropertiesDocument::parse("motd=\\u00A7aHello\nface=\\uD83D\\uDE00\n");
        assert_eq!(document.get("motd").as_deref(), Some("\u{a7}aHello"));
        assert_eq!(document.get("face").as_deref(), Some("\u{1f600}"));
    }

    #[test]
    fn malformed_unicode_escapes_yield_no_entry() {
        let document = PropertiesDocument::parse("motd=\\u12G4\nshort=\\u12\n");
        assert_eq!(document.get("motd"), None);
        assert_eq!(document.get("short"), None);
    }

    #[test]
    fn separator_is_whitespace_then_at_most_one_delimiter_then_whitespace() {
        let document = PropertiesDocument::parse(
            "a==x\nb = = y\nc:d\ne\tf\ng \\\n  h\n  i=j\nk\\ l=m\nn\\\n\\ o\n",
        );
        assert_eq!(document.get("a").as_deref(), Some("=x"));
        assert_eq!(document.get("b").as_deref(), Some("= y"));
        assert_eq!(document.get("c").as_deref(), Some("d"));
        assert_eq!(document.get("e").as_deref(), Some("f"));
        assert_eq!(document.get("g").as_deref(), Some("h"));
        assert_eq!(document.get("i").as_deref(), Some("j"));
        assert_eq!(document.get("k l").as_deref(), Some("m"));
        assert_eq!(document.get("n o").as_deref(), Some(""));
    }

    #[test]
    fn dangling_backslash_at_end_of_input_is_dropped() {
        let document = PropertiesDocument::parse("a=b\\");
        assert_eq!(document.get("a").as_deref(), Some("b"));
        let document = PropertiesDocument::parse("a=b\\\\\\");
        assert_eq!(document.get("a").as_deref(), Some("b\\"));
    }

    #[test]
    fn written_values_read_back_unchanged() {
        let values = [
            "  centered",
            "\t\u{c}tabbed",
            "=x",
            "#not a comment",
            "back\\slash",
            "ends with backslash\\",
            "\u{a7}aHello",
            "trailing ",
            "",
        ];
        for value in values {
            let mut document = PropertiesDocument::parse("motd=old\n");
            document.set("motd", value).expect("set");
            assert_eq!(document.get("motd").as_deref(), Some(value), "{value:?}");
            let reparsed = PropertiesDocument::parse(&document.render());
            assert_eq!(reparsed.get("motd").as_deref(), Some(value), "{value:?}");
        }
    }

    #[test]
    fn written_keys_read_back_unchanged_and_never_become_comments() {
        for key in ["#hash", "!bang", "a=b", "a:b", "a b", "a\tb", "a\\b"] {
            let mut document = PropertiesDocument::default();
            document.set(key, "value").expect("set");
            let reparsed = PropertiesDocument::parse(&document.render());
            assert_eq!(reparsed.get(key).as_deref(), Some("value"), "{key:?}");
        }
    }

    #[test]
    fn saving_an_untouched_motd_does_not_change_what_java_reads() {
        for source in ["motd=\\u00A7aHello\n", "motd=\\ \\ centered\n", "motd==x\n"] {
            let before = PropertiesDocument::parse(source).get("motd").expect("motd");
            let mut document = PropertiesDocument::parse(source);
            document.set("motd", &before).expect("set");
            assert_eq!(document.get("motd").as_deref(), Some(before.as_str()));
        }
    }

    #[test]
    fn continuation_that_adds_nothing_lets_a_following_comment_end_at_its_line() {
        for source in [
            "server-ip=127.0.0.1\n\\\n#\\\nserver-ip=\n",
            "server-ip=127.0.0.1\n\\\n   #\\\nserver-ip=\n",
            "server-ip=127.0.0.1\n\\\n \\\n#\\\nserver-ip=\n",
        ] {
            assert_eq!(
                PropertiesDocument::parse(source)
                    .get(SERVER_IP_KEY)
                    .as_deref(),
                Some(""),
                "{source:?}"
            );
            let document = merged(source);
            assert!(
                document
                    .render()
                    .contains("\nserver-ip=127.0.0.1\nonline-mode"),
                "{source:?}"
            );
            let reparsed = PropertiesDocument::parse(&document.render());
            assert_eq!(
                reparsed.get(SERVER_IP_KEY).as_deref(),
                Some("127.0.0.1"),
                "{source:?}"
            );
        }
    }

    #[test]
    fn appended_entries_survive_a_continuation_ending_in_a_lone_carriage_return() {
        let document = merged("a=1\nmotd=hello\\\r");
        assert_eq!(
            document.render(),
            "a=1\nmotd=hello\\\r\ronline-mode=false\nserver-ip=127.0.0.1\nserver-port=25565\n"
        );
        let reparsed = PropertiesDocument::parse(&document.render());
        assert_eq!(reparsed.get("motd").as_deref(), Some("hello"));
        assert_eq!(reparsed.get(ONLINE_MODE_KEY).as_deref(), Some("false"));
        assert_eq!(reparsed.get(SERVER_IP_KEY).as_deref(), Some("127.0.0.1"));
        assert_eq!(reparsed.get(SERVER_PORT_KEY).as_deref(), Some("25565"));
    }

    #[test]
    fn non_ascii_characters_are_written_as_unicode_escapes() {
        let mut document = PropertiesDocument::default();
        document.set("motd", "\u{a7}aHello \u{1f600}").expect("set");
        document.set("k\u{e9}y", "v").expect("set");
        assert_eq!(
            document.render(),
            "motd=\\u00A7aHello \\uD83D\\uDE00\nk\\u00E9y=v\n"
        );
        assert!(document.render().is_ascii());
        assert_eq!(
            document.get("motd").as_deref(),
            Some("\u{a7}aHello \u{1f600}")
        );
        assert_eq!(document.get("k\u{e9}y").as_deref(), Some("v"));
    }
}
