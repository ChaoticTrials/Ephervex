//! Offline accounts: a chosen name, and the UUID Minecraft itself would give it.
//!
//! # The whole flow
//!
//! The user types a name. That is it - no network, no Microsoft, no keyring
//! entry, because there is no secret to store. Security is not weakened
//! here; it simply has nothing to apply to, which is a stronger position than a
//! laxer store would be.
//!
//! # The UUID is derived, not invented, and that is the important part
//!
//! A Minecraft server running `online-mode=false` does not ask Mojang who a
//! connecting player is. It computes their UUID itself, with
//! `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))` - an MD5
//! digest stamped as a version-3, RFC 4122 UUID.
//!
//! Ephervex must reproduce that byte for byte. Local reproduction servers
//! default to `online-mode=false`, and each server links to the client that
//! spawned it, so a launcher-invented UUID would make a client and *its own
//! linked test server* disagree about who the player is. The visible damage is
//! not an error message: it is the player's inventory, position, advancements,
//! ender chest, and op status silently belonging to a different identity than
//! the one the world already has - which, in a tool built for reproducing bugs,
//! would corrupt the reproduction rather than fail it.
//!
//! Verified 2026-08-08 against current community documentation of the server
//! behavior, and pinned below against published name/UUID pairs.
//!
//! # Consequences of it being a hash
//!
//! * **Case matters.** `Steve` and `steve` are different players.
//! * **Renaming is a new identity.** There is no rename; there is only a
//!   different account. [`OfflineAccount::new`] therefore recomputes rather
//!   than carrying an id forward, and the caller is expected to treat a renamed
//!   offline account as a new one.
//! * **It is not a secret and not a credential.** It is public, derivable by
//!   anyone who knows the name, and proves nothing. That is exactly why an
//!   offline account may never be presented as authenticated.

use md5::{Digest, Md5};
use uuid::Uuid;

use crate::error::{AuthError, AuthResult};

/// The prefix Minecraft hashes together with the player name.
///
/// A constant rather than a literal because getting it wrong produces a UUID
/// that is perfectly well-formed and wrong, which no type can catch.
pub const OFFLINE_PLAYER_PREFIX: &str = "OfflinePlayer:";

/// Longest name Minecraft accepts.
pub const MAX_NAME_LENGTH: usize = 16;

/// Shortest name Minecraft accepts.
pub const MIN_NAME_LENGTH: usize = 3;

/// A local player identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineAccount {
    pub id: Uuid,
    pub name: String,
}

impl OfflineAccount {
    /// Creates an offline account from a name, deriving the UUID.
    ///
    /// The name is validated against Minecraft's own rules rather than
    /// accepted verbatim: a name the game will refuse produces a launch that
    /// fails much later with a message about something else.
    pub fn new(name: &str) -> AuthResult<Self> {
        let name = validate_name(name)?;
        Ok(Self {
            id: offline_uuid(&name),
            name,
        })
    }
}

/// The UUID a Minecraft server computes for an offline player of this name.
///
/// Equivalent to Java's
/// `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))`.
///
/// Note that this is *not* `Uuid::new_v3`: that hashes a namespace UUID's 16
/// bytes followed by the name, and Java's `nameUUIDFromBytes` has no namespace
/// at all. Using the crate's helper would produce a plausible, consistent, and
/// entirely wrong answer.
#[must_use]
pub fn offline_uuid(name: &str) -> Uuid {
    let mut digest: [u8; 16] = Md5::digest(format!("{OFFLINE_PLAYER_PREFIX}{name}").as_bytes())
        .as_slice()
        .try_into()
        // An MD5 digest is 16 bytes by definition; the conversion cannot fail,
        // and a zero UUID would be visibly wrong rather than silently plausible
        // if it somehow did.
        .unwrap_or([0_u8; 16]);

    // Exactly the two bytes Java rewrites: version 3 in the high nibble of
    // byte 6, and the RFC 4122 variant in the top two bits of byte 8.
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    Uuid::from_bytes(digest)
}

/// Checks a name against Minecraft's rules and trims surrounding whitespace.
///
/// Interior whitespace is refused rather than stripped: silently turning
/// `My Name` into `MyName` would hand back an account whose UUID belongs to a
/// name the user did not type.
fn validate_name(name: &str) -> AuthResult<String> {
    let trimmed = name.trim();
    let invalid = || AuthError::OfflineNameInvalid {
        name: trimmed.to_owned(),
    };

    if !(MIN_NAME_LENGTH..=MAX_NAME_LENGTH).contains(&trimmed.chars().count()) {
        return Err(invalid());
    }
    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(invalid());
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_derived_uuid_matches_what_a_server_computes() {
        // The load-bearing test in this module. These pairs are what
        // `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))`
        // returns, and every offline-mode server - vanilla, CraftBukkit,
        // Spigot, Paper, and the forks - calls that method.
        //
        // If this test ever fails, a client and its own linked test server stop
        // agreeing about who the player is, and the symptom is a silently wrong
        // inventory rather than an error.
        for (name, expected) in [
            ("Notch", "b50ad385-829d-3141-a216-7e7d7539ba7f"),
            ("jeb_", "a762f560-4fce-3236-812a-b80efff0b62b"),
            ("Dinnerbone", "4d258a81-2358-3084-8166-05b9faccad80"),
            ("Player", "a01e3843-e521-3998-958a-f459800e4d11"),
        ] {
            assert_eq!(
                offline_uuid(name).to_string(),
                expected,
                "offline uuid for {name}"
            );
        }
    }

    #[test]
    fn the_result_is_a_version_3_rfc_4122_uuid() {
        // The two bytes Java rewrites. A digest used raw would be a valid-
        // looking UUID of no version at all, and servers do compare these.
        let id = offline_uuid("Notch");
        assert_eq!(id.get_version_num(), 3, "{id}");
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122, "{id}");
    }

    /// RFC 4122's *namespaced* version-3 UUID: MD5 over the namespace's 16
    /// bytes followed by the name. Spelled out rather than taken from the
    /// `uuid` crate's `v3` feature, so the comparison below does not depend on
    /// a feature flag that could be turned on later and change its meaning.
    fn namespaced_v3(namespace: Uuid, name: &str) -> Uuid {
        let mut hasher = Md5::new();
        hasher.update(namespace.as_bytes());
        hasher.update(name.as_bytes());
        let mut digest: [u8; 16] = hasher.finalize().as_slice().try_into().unwrap();
        digest[6] = (digest[6] & 0x0f) | 0x30;
        digest[8] = (digest[8] & 0x3f) | 0x80;
        Uuid::from_bytes(digest)
    }

    #[test]
    fn it_is_not_a_namespaced_version_3_uuid() {
        // The trap this test exists for: RFC 4122's version 3 prepends a
        // namespace, Java's `nameUUIDFromBytes` does not, and both produce a
        // well-formed version-3 UUID. Reaching for the standard helper is the
        // natural thing to do and is silently wrong - every server would
        // disagree with the result, and nothing about it looks incorrect.
        let ours = offline_uuid("Notch");
        for namespace in [
            Uuid::NAMESPACE_DNS,
            Uuid::NAMESPACE_URL,
            Uuid::NAMESPACE_OID,
            Uuid::NAMESPACE_X500,
            Uuid::nil(),
        ] {
            let namespaced = namespaced_v3(namespace, "OfflinePlayer:Notch");
            assert_eq!(namespaced.get_version_num(), 3, "still a v3 uuid");
            assert_ne!(ours, namespaced, "namespace {namespace}");
        }
    }

    #[test]
    fn the_derivation_is_deterministic_and_case_sensitive() {
        assert_eq!(offline_uuid("Steve"), offline_uuid("Steve"));
        // Minecraft treats these as different players, so Ephervex must too.
        assert_ne!(offline_uuid("Steve"), offline_uuid("steve"));
    }

    #[test]
    fn the_prefix_is_part_of_the_hash_and_not_decoration() {
        // Hashing the bare name is the obvious mistake and produces a
        // well-formed UUID that no server agrees with.
        let mut bare: [u8; 16] = Md5::digest(b"Notch").as_slice().try_into().unwrap();
        bare[6] = (bare[6] & 0x0f) | 0x30;
        bare[8] = (bare[8] & 0x3f) | 0x80;
        assert_ne!(offline_uuid("Notch"), Uuid::from_bytes(bare));
    }

    #[test]
    fn an_account_carries_the_name_it_hashed() {
        let account = OfflineAccount::new("  Dinnerbone  ").expect("a valid name");
        assert_eq!(account.name, "Dinnerbone");
        assert_eq!(
            account.id.to_string(),
            "4d258a81-2358-3084-8166-05b9faccad80",
            "the trimmed name is what gets hashed"
        );
    }

    #[test]
    fn names_minecraft_would_refuse_are_refused_here() {
        // Failing at creation beats failing at launch with a message about
        // something else entirely.
        for name in [
            "",
            "  ",
            "ab",                   // shorter than three
            "ThisNameIsFarTooLong", // longer than sixteen
            "has space",
            "hyphen-ated",
            "emoji🙂",
            "dots.here",
        ] {
            assert!(
                OfflineAccount::new(name).is_err(),
                "`{name}` should be refused"
            );
        }
    }

    #[test]
    fn names_minecraft_accepts_are_accepted() {
        for name in [
            "Notch",
            "jeb_",
            "_Under_",
            "abc",
            "SixteenCharsXXXX",
            "A1b2C3",
        ] {
            let account = OfflineAccount::new(name).expect(name);
            assert_eq!(account.name, name);
            assert_eq!(account.id, offline_uuid(name));
        }
    }

    #[test]
    fn interior_whitespace_is_refused_rather_than_stripped() {
        // Stripping would hand back an account whose UUID belongs to a name the
        // user never typed, which they would only discover when a server
        // disagreed with them.
        assert!(OfflineAccount::new("My Name").is_err());
    }
}
