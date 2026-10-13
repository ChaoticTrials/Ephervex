//! Mod row keys: a logical mod path carried through UI strings and back.
//!
//! A Linux file name is an arbitrary byte string, while the UI layer only holds
//! UTF-8 text. A lossy conversion would turn an invalid byte into U+FFFD, and
//! the path rebuilt from that text would name a different file. The key is a
//! lossless text form instead: valid UTF-8 is kept as is, every byte that is
//! not valid UTF-8 becomes `%XX`, and a literal `%` becomes `%25` so decoding
//! is unambiguous. Paths without `%` or invalid bytes are their own key.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The key that names `path`. Decoding it gives back exactly `path`.
pub(crate) fn encode(path: &Path) -> String {
    let mut key = String::new();
    let mut rest = &*path_bytes(path);
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                push_escaped(&mut key, valid);
                return key;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                push_escaped(&mut key, &String::from_utf8_lossy(valid));
                let invalid = error.error_len().unwrap_or(after.len());
                for byte in &after[..invalid] {
                    push_byte(&mut key, *byte);
                }
                rest = &after[invalid..];
            }
        }
    }
}

/// The path a key names. A `%` that does not start a two-digit hex escape is
/// taken literally, so any text decodes to some path.
pub(crate) fn decode(key: &str) -> PathBuf {
    let bytes = key.as_bytes();
    let mut raw = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|pair| Some((hex_value(pair[0])?, hex_value(pair[1])?)));
        if let Some((high, low)) = escaped {
            raw.push(high << 4 | low);
            index += 3;
        } else {
            raw.push(bytes[index]);
            index += 1;
        }
    }
    path_from_bytes(raw)
}

fn push_escaped(key: &mut String, valid: &str) {
    for character in valid.chars() {
        if character == '%' {
            key.push_str("%25");
        } else {
            key.push(character);
        }
    }
}

fn push_byte(key: &mut String, byte: u8) {
    key.push('%');
    key.push(hex_digit(byte >> 4));
    key.push(hex_digit(byte & 0x0f));
}

fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).map_or('0', |digit| digit.to_ascii_uppercase())
}

fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt as _;
    Cow::Borrowed(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
}

#[cfg(unix)]
fn path_from_bytes(raw: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(raw))
}

#[cfg(not(unix))]
fn path_from_bytes(raw: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&raw).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_is_its_own_key() {
        let path = Path::new("mods/Create-1.21.1.jar");
        assert_eq!(encode(path), "mods/Create-1.21.1.jar");
        assert_eq!(decode("mods/Create-1.21.1.jar"), path);
    }

    #[test]
    fn a_literal_percent_sign_survives_a_round_trip() {
        let path = Path::new("mods/100%25-pure%.jar");
        let key = encode(path);
        assert_eq!(key, "mods/100%2525-pure%25.jar");
        assert_eq!(decode(&key), path);
    }

    #[test]
    fn a_replacement_character_in_a_name_is_kept_as_that_character() {
        let path = Path::new("mods/a\u{fffd}b.jar");
        assert_eq!(encode(path), "mods/a\u{fffd}b.jar");
        assert_eq!(decode(&encode(path)), path);
    }

    #[test]
    fn text_with_a_malformed_escape_decodes_literally() {
        assert_eq!(decode("mods/50%.jar"), Path::new("mods/50%.jar"));
        assert_eq!(decode("mods/%zz.jar"), Path::new("mods/%zz.jar"));
        assert_eq!(decode("mods/end%4"), Path::new("mods/end%4"));
    }

    #[cfg(unix)]
    mod unix {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        use super::*;

        fn path_of(bytes: &[u8]) -> PathBuf {
            PathBuf::from(OsString::from_vec(bytes.to_vec()))
        }

        #[test]
        fn a_name_with_an_invalid_byte_round_trips_to_the_same_file() {
            let path = path_of(b"mods/bad\xffname.jar");
            let key = encode(&path);
            assert_eq!(key, "mods/bad%FFname.jar");
            assert_eq!(decode(&key), path);
        }

        #[test]
        fn a_name_with_an_invalid_byte_never_collides_with_a_replacement_character() {
            let invalid = path_of(b"mods/a\xffb.jar");
            let replacement = Path::new("mods/a\u{fffd}b.jar");
            assert_ne!(encode(&invalid), encode(replacement));
            assert_ne!(decode(&encode(&invalid)), replacement);
        }

        #[test]
        fn truncated_and_stray_sequences_each_become_escapes() {
            let path = path_of(b"mods/\xe2\x82-\x80\xc0%.jar");
            let key = encode(&path);
            assert_eq!(key, "mods/%E2%82-%80%C0%25.jar");
            assert_eq!(decode(&key), path);
        }

        #[test]
        fn a_trailing_truncated_sequence_is_escaped() {
            let path = path_of(b"mods/x\xe2\x82");
            let key = encode(&path);
            assert_eq!(key, "mods/x%E2%82");
            assert_eq!(decode(&key), path);
        }
    }
}
