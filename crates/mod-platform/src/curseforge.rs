//! The CurseForge transport: the official third-party API, authenticated with
//! the user's own key.
//!
//! ```text
//! curseforge.rs   the API root, and what a caller has to supply to use it
//! ├── credential  where the key comes from, without knowing where it is kept
//! ├── api         authenticated requests, and what a failure means
//! ├── dto         CurseForge's wire shapes, declared and nothing more
//! ├── mapping     what those shapes mean as Ephervex's own model
//! └── browser     the provider the mod browser and the resolver call
//! ```
//!
//! # The credential is the user's, and Ephervex only borrows it
//!
//! CurseForge's third-party API is reached with a key that the user applies for
//! themselves, through CurseForge/Overwolf, who decide whether to grant it.
//! Ephervex ships no key of its own. The key is a secret: it lives in the
//! desktop keyring and nowhere else, and this module never learns which keyring
//! or which entry - it asks a [`CurseForgeCredentialSource`] for an opaque
//! [`SecretString`](ephervex_model::SecretString) and puts it in one header.
//!
//! That boundary is what makes the eventual replacement of the credential
//! *source* - an approved application-owned credential, say - a different
//! implementation of one trait rather than a change to the transport.
//!
//! # Nothing here is written down
//!
//! Every other provider in this crate reads through
//! [`MetadataCache`](ephervex_download::MetadataCache), which stages a document
//! on disk and keeps it. This one does not, and must not: the published
//! CurseForge third-party API terms prohibit saving or caching data obtained
//! through the API. So responses are parsed in memory, used by whatever asked
//! for them, and dropped. There is no cache key type in this module, no TTL,
//! and no namespace under the metadata store, and that absence is the feature.
//!
//! # The key never leaves the origin it was issued for
//!
//! The client is built with
//! [`build_single_origin_client`](ephervex_download::build_single_origin_client),
//! so a redirect pointing off the API origin is refused rather than followed.
//! `x-api-key` is attached per request rather than installed as a default
//! header, so no other code path can inherit it by sharing the client.

mod api;
mod browser;
mod credential;
mod dto;
mod mapping;

use ephervex_model::CurseForgeFileId;

pub use api::{
    CURSEFORGE_API_BASE, CURSEFORGE_API_KEY_HEADER, CurseForgeApi, CurseForgeFileRelease,
    CurseForgeOperation, CurseForgeProjectClass, CurseForgeProjectClasses, MINECRAFT_GAME_ID,
    credential_refusal,
};
pub use browser::CurseForgeProvider;
pub use credential::{ConfiguredApiKey, CurseForgeCredentialSource};
pub use dto::{CurseForgeAuthor, CurseForgeGame, CurseForgeLogo, CurseForgeProject};

/// Where a user applies for a CurseForge third-party API key.
///
/// The support article rather than the application form it currently links to:
/// the article is the stable address, it explains the approval process, and it
/// keeps working when CurseForge moves the form. A named constant rather than a
/// literal in a sentence, for the same reason every other external address in
/// this workspace is one - it is untranslated technical text that a frontend
/// turns into a link.
///
/// This is the key for the third-party *API*. The CurseForge Upload API's
/// token is a different credential for a different service, and it is not what
/// Ephervex asks for.
pub const CURSEFORGE_API_KEY_HELP: &str = "https://support.curseforge.com/support/solutions/articles/9000208346-about-the-curseforge-api-and-how-to-apply-for-a-key";

/// The page on CurseForge's own site that starts one exact file's download.
///
/// `class_segment` is the project class's own path segment (see
/// [`CurseForgeProjectClass::browser_path_segment`]) and `slug` the project's
/// own handle, both as CurseForge published them. This is the address a user
/// is sent to when the author forbids third-party downloads: it is the site's
/// own page rather than a delivery address assembled out of the ids, so it
/// respects the author's decision instead of routing around it.
///
/// One definition, because a pack import and a version change both send the
/// user to the same place and must not disagree about where that is.
#[must_use]
pub fn file_browser_url(class_segment: &str, slug: &str, file_id: CurseForgeFileId) -> String {
    format!("https://www.curseforge.com/minecraft/{class_segment}/{slug}/download/{file_id}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_key_application_address_is_the_stable_support_article() {
        // Not the application form itself: that address has moved before, and a
        // launcher that hard-codes it sends users to a dead page.
        assert!(CURSEFORGE_API_KEY_HELP.starts_with("https://support.curseforge.com/"));
        assert!(CURSEFORGE_API_KEY_HELP.contains("how-to-apply-for-a-key"));
    }
}
