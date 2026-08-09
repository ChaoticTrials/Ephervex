//! Line-preserving `server.properties` management.

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
            if self.lines.last().is_some_and(|line| !line.ends_with('\n')) {
                self.lines.push(self.preferred_ending.clone());
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
            while end < self.lines.len() && is_continued(&self.lines[end - 1]) {
                end += 1;
            }
            result.push((start, end));
            start = end;
        }
        result
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

fn parse_entry(lines: &[String]) -> Option<(String, String)> {
    let mut logical = String::new();
    for (index, line) in lines.iter().enumerate() {
        let mut part = line.trim_end_matches(['\r', '\n']);
        if index > 0 {
            part = part.trim_start_matches([' ', '\t', '\u{c}']);
        }
        if index + 1 < lines.len() && part.ends_with('\\') {
            part = &part[..part.len() - 1];
        }
        logical.push_str(part);
    }

    let logical = logical.trim_start_matches([' ', '\t', '\u{c}']);
    if logical.is_empty() || logical.starts_with(['#', '!']) {
        return None;
    }

    let mut escaped = false;
    let mut separator = logical.len();
    let mut value_start = logical.len();
    for (offset, character) in logical.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '=' || character == ':' || character.is_ascii_whitespace() {
            separator = offset;
            value_start = offset + character.len_utf8();
            break;
        }
    }
    while value_start < logical.len() {
        let Some(character) = logical[value_start..].chars().next() else {
            break;
        };
        if character == '=' || character == ':' || character.is_ascii_whitespace() {
            value_start += character.len_utf8();
        } else {
            break;
        }
    }
    Some((
        unescape(&logical[..separator]),
        unescape(&logical[value_start..]),
    ))
}

fn unescape(value: &str) -> String {
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
            Some(other) => result.push(other),
            None => result.push('\\'),
        }
    }
    result
}

fn escape_key(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' | '=' | ':' | ' ' | '\t' => vec!['\\', character],
            other => vec![other],
        })
        .collect()
}

fn escape_value(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' => vec!['\\', '\\'],
            other => vec![other],
        })
        .collect()
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
}
