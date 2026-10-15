//! What identifies one cached Modrinth answer.
//!
//! The metadata cache addresses an entry by the SHA-256 digest of a canonical
//! key ([`ephervex_download::MetadataKey::addressed`]). This module is the one
//! place that decides what goes into that key for Modrinth, and it answers a
//! single question: which facts change the response?
//!
//! # Why not the URL
//!
//! A URL carries things that are not identity - the API root, which differs
//! between production and a loopback test server; parameter order; percent
//! encoding. Two requests for the same thing would address two entries, and a
//! future change to how a request is assembled would silently orphan every
//! entry on disk. So the key is written from the request's *meaning*, and the
//! URL is left to be a transport detail.
//!
//! # Shape
//!
//! ```text
//! modrinth:v1:project-versions
//! project=AABBCCDD
//! ```
//!
//! One header line naming the provider, the key schema, and the operation, then
//! one `name=value` line per parameter in a fixed order. A parameter that does
//! not apply to a request is omitted rather than written empty, so "no loader
//! filter" and "an empty loader filter" cannot address the same entry. Values
//! are escaped, so nothing a user types can forge a line and make one request
//! look like another.

use std::fmt::Write as _;

use ephervex_download::{CacheKeyError, MetadataKey};

/// The directory Modrinth's cached documents live in, beneath the metadata
/// store root. Part of the on-disk layout a user can inspect, so it is stable.
pub(super) const CACHE_NAMESPACE: &str = "modrinth";

/// The version of the canonical-key format, carried in every key.
///
/// This versions the mapping from a request to a digest, and nothing else. It
/// is not the envelope schema the cache writes, not Modrinth's API version, and
/// not the pack recipe's version. Bumping it makes every previously addressed
/// entry a clean miss, which is what a redesign of the rules below needs so
/// that a new key cannot land on an old answer.
const KEY_SCHEMA: &str = "v1";

/// One logical question this launcher asks Modrinth.
///
/// Modeled as an enum rather than assembled at each call site so that a new
/// endpoint cannot quietly reuse another one's identity, and so the parameters
/// that matter are the ones the type demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModrinthRequest<'a> {
    /// One project, by id or slug.
    ///
    /// The reference is part of the identity rather than resolved to an id
    /// first: resolving it is the request.
    Project { reference: &'a str },
    /// Every version of one project, unfiltered.
    ProjectVersions { project_id: &'a str },
    /// One page of search results.
    Search {
        text: &'a str,
        /// The page size actually sent, after this launcher's own ceiling has
        /// been applied. Two callers asking for more than the ceiling are
        /// asking for the same page.
        limit: u8,
        offset: u32,
        /// The compatibility narrowing, absent when the caller asked to see
        /// incompatible projects too - that search sends no such filter, and a
        /// key that pretended otherwise would split one answer across every
        /// instance that asked for it.
        filter: Option<CompatibilityFilter<'a>>,
        /// Whether projects declaring no server support were asked to be left
        /// out. That changes which projects the page holds.
        excluding_client_only: bool,
    },
    /// Several projects in one request.
    Projects { ids: &'a [String] },
    /// Several versions in one request.
    Versions { ids: &'a [String] },
}

/// The instance a compatible-only search is narrowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompatibilityFilter<'a> {
    pub(super) minecraft_version: &'a str,
    /// Modrinth's own name for the loader, which is what the facet carries.
    pub(super) loader: &'a str,
}

impl ModrinthRequest<'_> {
    /// Where this request's answer is cached.
    pub(super) fn cache_key(&self) -> Result<MetadataKey, CacheKeyError> {
        MetadataKey::addressed(CACHE_NAMESPACE, &self.canonical_key())
    }

    /// The stable rendering of this request's identity.
    ///
    /// Written by hand rather than derived from `Debug` or from a serializer:
    /// this string is a persistent contract, and neither of those promises to
    /// keep producing the same bytes.
    fn canonical_key(&self) -> String {
        match self {
            Self::Project { reference } => {
                let mut key = header("project");
                field(&mut key, "reference", reference);
                key
            }
            Self::ProjectVersions { project_id } => {
                let mut key = header("project-versions");
                field(&mut key, "project", project_id);
                key
            }
            Self::Search {
                text,
                limit,
                offset,
                filter,
                excluding_client_only,
            } => {
                let mut key = header("search");
                field(&mut key, "query", text);
                field(&mut key, "limit", &limit.to_string());
                field(&mut key, "offset", &offset.to_string());
                if let Some(filter) = filter {
                    field(&mut key, "game_version", filter.minecraft_version);
                    field(&mut key, "loader", filter.loader);
                }
                if *excluding_client_only {
                    field(&mut key, "server_side", "!=unsupported");
                }
                key
            }
            Self::Projects { ids } => {
                let mut key = header("projects");
                id_list(&mut key, ids);
                key
            }
            Self::Versions { ids } => {
                let mut key = header("versions");
                id_list(&mut key, ids);
                key
            }
        }
    }
}

/// The line naming the provider, the key schema, and the operation.
fn header(operation: &str) -> String {
    format!("modrinth:{KEY_SCHEMA}:{operation}\n")
}

/// Appends one parameter.
fn field(key: &mut String, name: &str, value: &str) {
    // Writing into a `String` cannot fail; the result is discarded rather than
    // unwrapped so this stays free of a panic that nothing can reach.
    let _ = writeln!(key, "{name}={}", escaped(value));
}

/// Appends a batch endpoint's id list.
///
/// Sorted, because the endpoint answers with a set: asking for `[b, a]` and
/// asking for `[a, b]` is asking the same question, and only the caller's
/// iteration order differs. Repeats are kept rather than collapsed - a list
/// that names a project twice is a different list, and guessing otherwise would
/// be inventing an equivalence the API does not document.
fn id_list(key: &mut String, ids: &[String]) {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    for id in sorted {
        field(key, "id", id);
    }
}

/// Escapes a value so it cannot be mistaken for the structure around it.
///
/// A search query is whatever a user typed, including newlines and backslashes.
/// Without this, typing `\nloader=forge` would produce the same key as a
/// genuinely loader-filtered search for the empty string, and one answer would
/// be served for the other.
fn escaped(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn search(text: &str) -> ModrinthRequest<'_> {
        ModrinthRequest::Search {
            text,
            limit: 10,
            offset: 0,
            excluding_client_only: false,
            filter: Some(CompatibilityFilter {
                minecraft_version: "1.20.1",
                loader: "neoforge",
            }),
        }
    }

    #[test]
    fn a_canonical_key_names_the_provider_the_schema_and_the_operation() {
        // The header is the part a future redesign bumps to get clean misses
        // instead of aliased entries, so it is worth pinning exactly.
        assert_eq!(
            ModrinthRequest::ProjectVersions {
                project_id: "AABBCCDD"
            }
            .canonical_key(),
            "modrinth:v1:project-versions\nproject=AABBCCDD\n"
        );
    }

    #[test]
    fn every_operation_has_an_identity_of_its_own() {
        let ids = ["AABBCCDD".to_owned()];
        let requests = [
            ModrinthRequest::Project {
                reference: "AABBCCDD",
            },
            ModrinthRequest::ProjectVersions {
                project_id: "AABBCCDD",
            },
            search("AABBCCDD"),
            ModrinthRequest::Projects { ids: &ids },
            ModrinthRequest::Versions { ids: &ids },
        ];

        for (index, request) in requests.iter().enumerate() {
            for other in &requests[index + 1..] {
                assert_ne!(
                    request.cache_key().expect("addressable"),
                    other.cache_key().expect("addressable"),
                    "{request:?} and {other:?} share an address"
                );
            }
        }
    }

    #[test]
    fn requests_that_differ_in_a_meaningful_parameter_are_addressed_apart() {
        let distinct = [
            ModrinthRequest::Project { reference: "jei" },
            ModrinthRequest::Project { reference: "rei" },
            ModrinthRequest::ProjectVersions { project_id: "jei" },
            ModrinthRequest::ProjectVersions { project_id: "rei" },
            search("jei"),
            search("rei"),
            ModrinthRequest::Search {
                text: "jei",
                limit: 25,
                offset: 0,
                excluding_client_only: false,
                filter: Some(CompatibilityFilter {
                    minecraft_version: "1.20.1",
                    loader: "neoforge",
                }),
            },
            ModrinthRequest::Search {
                text: "jei",
                limit: 10,
                offset: 10,
                excluding_client_only: false,
                filter: Some(CompatibilityFilter {
                    minecraft_version: "1.20.1",
                    loader: "neoforge",
                }),
            },
            ModrinthRequest::Search {
                text: "jei",
                limit: 10,
                offset: 0,
                excluding_client_only: false,
                filter: Some(CompatibilityFilter {
                    minecraft_version: "1.21.1",
                    loader: "neoforge",
                }),
            },
            ModrinthRequest::Search {
                text: "jei",
                limit: 10,
                offset: 0,
                excluding_client_only: false,
                filter: Some(CompatibilityFilter {
                    minecraft_version: "1.20.1",
                    loader: "forge",
                }),
            },
            ModrinthRequest::Search {
                text: "jei",
                limit: 10,
                offset: 0,
                excluding_client_only: false,
                filter: None,
            },
        ];

        for (index, request) in distinct.iter().enumerate() {
            for other in &distinct[index + 1..] {
                assert_ne!(
                    request.cache_key().expect("addressable"),
                    other.cache_key().expect("addressable"),
                    "{request:?} and {other:?} share an address"
                );
            }
        }
    }

    #[test]
    fn the_same_request_is_addressed_the_same_way_every_time() {
        let first = search("jei").cache_key().expect("addressable");
        let second = search("jei").cache_key().expect("addressable");
        assert_eq!(first, second);

        // Built from separately allocated strings, not from the same borrow.
        let text = String::from("jei");
        assert_eq!(
            ModrinthRequest::Search {
                text: &text,
                limit: 10,
                offset: 0,
                excluding_client_only: false,
                filter: Some(CompatibilityFilter {
                    minecraft_version: &String::from("1.20.1"),
                    loader: &String::from("neoforge"),
                }),
            }
            .cache_key()
            .expect("addressable"),
            first
        );
    }

    #[test]
    fn a_batch_lookup_is_addressed_by_which_ids_it_asks_for_not_by_their_order() {
        let forward = ["AAAAAAAA".to_owned(), "BBBBBBBB".to_owned()];
        let reversed = ["BBBBBBBB".to_owned(), "AAAAAAAA".to_owned()];
        assert_eq!(
            ModrinthRequest::Projects { ids: &forward }
                .cache_key()
                .expect("addressable"),
            ModrinthRequest::Projects { ids: &reversed }
                .cache_key()
                .expect("addressable")
        );

        // A different set is still a different question.
        let other = ["AAAAAAAA".to_owned(), "CCCCCCCC".to_owned()];
        assert_ne!(
            ModrinthRequest::Projects { ids: &forward }
                .cache_key()
                .expect("addressable"),
            ModrinthRequest::Projects { ids: &other }
                .cache_key()
                .expect("addressable")
        );
    }

    #[test]
    fn an_absent_parameter_is_not_the_same_as_an_empty_one() {
        let absent = ModrinthRequest::Search {
            text: "jei",
            limit: 10,
            offset: 0,
            excluding_client_only: false,
            filter: None,
        };
        let empty = ModrinthRequest::Search {
            text: "jei",
            limit: 10,
            offset: 0,
            excluding_client_only: false,
            filter: Some(CompatibilityFilter {
                minecraft_version: "",
                loader: "",
            }),
        };
        assert_ne!(
            absent.cache_key().expect("addressable"),
            empty.cache_key().expect("addressable")
        );
    }

    #[test]
    fn a_query_cannot_forge_the_parameters_around_it() {
        // Both would render as the same lines without escaping, and one search
        // would then be served the other's answer.
        let forged = ModrinthRequest::Search {
            text: "\nloader=forge",
            limit: 10,
            offset: 0,
            excluding_client_only: false,
            filter: None,
        };
        let genuine = ModrinthRequest::Search {
            text: "",
            limit: 10,
            offset: 0,
            excluding_client_only: false,
            filter: None,
        };
        assert_ne!(
            forged.cache_key().expect("addressable"),
            genuine.cache_key().expect("addressable")
        );
        assert!(!forged.canonical_key().contains("\nloader=forge\n"));
    }

    #[test]
    fn asking_to_leave_out_client_only_mods_is_a_different_page() {
        let narrowed = ModrinthRequest::Search {
            text: "jei",
            limit: 10,
            offset: 0,
            excluding_client_only: true,
            filter: Some(CompatibilityFilter {
                minecraft_version: "1.20.1",
                loader: "neoforge",
            }),
        };

        assert_ne!(
            narrowed.cache_key().expect("addressable"),
            search("jei").cache_key().expect("addressable")
        );
    }
}
