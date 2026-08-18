//! CurseForge's wire shapes.
//!
//! Declared exactly as much as this launcher reads and no more. Every field the
//! API documents but nothing here needs is left out on purpose: an unknown
//! field is ignored by `serde` by default, so a shape that grows upstream
//! cannot break a build, while a field declared "for later" is one more thing a
//! response has to satisfy for no current benefit.
//!
//! # The envelope
//!
//! Every documented endpoint of the third-party REST API wraps its payload in
//! an object with a `data` member. [`ApiEnvelope`] is that wrapper, generic
//! over what is inside it, so no endpoint's own type has to carry it.

use serde::Deserialize;

/// The `{ "data": ... }` wrapper every documented endpoint answers with.
#[derive(Debug, Deserialize)]
pub(super) struct ApiEnvelope<T> {
    pub(super) data: T,
}

/// One game, as `GET /v1/games/{gameId}` describes it.
///
/// Read for one purpose: proving that a configured API key is accepted. The
/// identifiers are exact upstream values and stay untranslated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CurseForgeGame {
    /// The numeric game id. Minecraft's is [`MINECRAFT_GAME_ID`].
    ///
    /// [`MINECRAFT_GAME_ID`]: super::MINECRAFT_GAME_ID
    pub id: u32,
    /// The game's display name, as CurseForge spells it.
    pub name: String,
    /// The game's URL slug.
    pub slug: String,
}

/// One project, as `GET /v1/mods/{modId}` describes it.
///
/// Deliberately *not* mapped into [`crate::model::ModProject`] yet. That
/// translation belongs with the catalog behavior that needs it — side support,
/// compatibility, and version lists — and inventing half of it here would mean
/// a neutral model carrying values nothing had determined.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CurseForgeProject {
    /// The numeric project id, which is what every other endpoint addresses it
    /// by.
    pub id: u32,
    /// The game this project belongs to. Checked rather than assumed: a project
    /// id is global, so an id from another game resolves perfectly well and
    /// means nothing to a Minecraft instance.
    pub game_id: u32,
    /// The project's display name.
    pub name: String,
    /// The project's URL slug.
    pub slug: String,
    /// The one-line description CurseForge shows in a listing.
    #[serde(default)]
    pub summary: String,
    /// The numeric id of the top-level project class this project belongs to
    /// (Mods, Resource Packs, Worlds, and so on).
    ///
    /// The upstream schema documents this member as nullable, and it is kept
    /// that way rather than defaulted to some assumed class: a project with no
    /// reported class is a real, schema-valid state, and treating it as though
    /// it were the Mods class (or any other) would accept something Ephervex
    /// never confirmed. Which numeric value *means* Mods is never assumed
    /// either: it is discovered at runtime from [`CurseForgeCategoryClass`].
    #[serde(default)]
    pub class_id: Option<u32>,
}

/// One top-level project class for a game, as
/// `GET /v1/categories?classesOnly=true` describes it.
///
/// A "class" is CurseForge's own term for a project's top-level kind (Mods,
/// Resource Packs, Worlds, Bukkit Plugins, and so on), and `classesOnly=true`
/// is what limits the response to exactly those, rather than every
/// subcategory beneath them. `slug` is the identifier this launcher matches
/// the Mods class by; `id` is only meaningful once matched, and is not
/// assumed to be any particular number. `name` is kept so a project belonging
/// to some other class can be described by the provider's own name for that
/// class, rather than by the unrelated project's own display name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgeCategoryClass {
    pub(super) id: u32,
    pub(super) name: String,
    pub(super) slug: String,
}

/// One file of one project, as `GET /v1/mods/{modId}/files/{fileId}` describes
/// it.
///
/// Exactly the members a pack import needs to fetch and verify one artifact:
/// which file this is, whether it can be downloaded, where from, how big it is,
/// and what it hashes to. The response carries dozens more — dependencies,
/// sortable game versions, module fingerprints, download counts — and none of
/// them is read, because a field declared "for later" is one more thing every
/// response has to satisfy for no current benefit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgeFileDocument {
    /// The file's own numeric id. Checked against the requested one: a response
    /// describing another file is refused rather than substituted.
    pub(super) id: u32,
    /// The project this file belongs to. Checked for the same reason.
    pub(super) mod_id: u32,
    /// Whether CurseForge currently serves this file at all.
    pub(super) is_available: bool,
    /// The provider's own file name. Untrusted input: validated as a safe
    /// relative path before it is allowed to name a destination.
    pub(super) file_name: String,
    /// The declared size, verified against the bytes actually received.
    #[serde(default)]
    pub(super) file_length: Option<u64>,
    /// Where the file can be fetched.
    ///
    /// Null when the project's owner has turned off third-party distribution,
    /// which is a supported and deliberate state rather than a fault. There is
    /// no other address: a CDN path assembled from the ids would be routing
    /// around a decision the author made.
    #[serde(default)]
    pub(super) download_url: Option<String>,
    #[serde(default)]
    pub(super) hashes: Vec<CurseForgeFileHash>,
}

/// One published digest, tagged with the algorithm that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct CurseForgeFileHash {
    pub(super) value: String,
    pub(super) algo: u32,
}

/// The `algo` value that tags a SHA-1 digest.
///
/// The only algorithm in this response Ephervex can verify with. The other
/// value CurseForge publishes is MD5, which is not in the launcher's hash model
/// and is not a substitute for a SHA-1 that is present.
///
/// Read defensively: the digest is still parsed by
/// [`Hash::new`](ephervex_model::Hash::new), which enforces the hex length
/// SHA-1 has. A digest whose tag and shape disagree is discarded rather than
/// trusted on the strength of its tag.
pub(super) const HASH_ALGO_SHA1: u32 = 1;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_game_document_is_read_out_of_the_data_envelope() {
        let envelope: ApiEnvelope<CurseForgeGame> = serde_json::from_str(
            r#"{"data":{"id":432,"name":"Minecraft","slug":"minecraft","status":1}}"#,
        )
        .expect("a documented game response");
        assert_eq!(envelope.data.id, 432);
        assert_eq!(envelope.data.name, "Minecraft");
        assert_eq!(envelope.data.slug, "minecraft");
    }

    #[test]
    fn a_project_document_keeps_the_game_it_belongs_to() {
        let envelope: ApiEnvelope<CurseForgeProject> = serde_json::from_str(
            r#"{"data":{"id":238222,"gameId":432,"name":"Just Enough Items","slug":"jei",
                "summary":"View items and recipes","downloadCount":1,"classId":6}}"#,
        )
        .expect("a documented project response");
        assert_eq!(envelope.data.game_id, 432);
        assert_eq!(envelope.data.slug, "jei");
        assert_eq!(envelope.data.summary, "View items and recipes");
        assert_eq!(envelope.data.class_id, Some(6));
    }

    #[test]
    fn a_field_the_api_adds_later_does_not_break_a_response() {
        let envelope: ApiEnvelope<CurseForgeGame> = serde_json::from_str(
            r#"{"data":{"id":432,"name":"Minecraft","slug":"minecraft","somethingNew":[1,2]},
                "pagination":{"index":0}}"#,
        )
        .expect("unknown members are ignored");
        assert_eq!(envelope.data.id, 432);
    }

    #[test]
    fn a_file_document_carries_the_identity_address_size_and_digests_an_import_needs() {
        let envelope: ApiEnvelope<CurseForgeFileDocument> = serde_json::from_str(
            r#"{"data":{"id":4593548,"gameId":432,"modId":238222,"isAvailable":true,
                "displayName":"JEI 15.2.0.27","fileName":"jei-1.20.1-15.2.0.27.jar",
                "releaseType":1,"fileStatus":4,"fileLength":1234567,
                "downloadUrl":"https://mediafilez.forgecdn.net/files/4593/548/jei.jar",
                "hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1},
                          {"value":"0123456789abcdef0123456789abcdef","algo":2}],
                "dependencies":[{"modId":1,"relationType":3}],
                "sortableGameVersions":[{"gameVersionName":"1.20.1"}]}}"#,
        )
        .expect("a documented file response");

        let file = envelope.data;
        assert_eq!(file.id, 4_593_548);
        assert_eq!(file.mod_id, 238_222);
        assert!(file.is_available);
        assert_eq!(file.file_name, "jei-1.20.1-15.2.0.27.jar");
        assert_eq!(file.file_length, Some(1_234_567));
        assert_eq!(
            file.download_url.as_deref(),
            Some("https://mediafilez.forgecdn.net/files/4593/548/jei.jar")
        );
        assert_eq!(file.hashes.len(), 2);
        assert_eq!(file.hashes[0].algo, HASH_ALGO_SHA1);
    }

    #[test]
    fn a_file_whose_owner_disabled_third_party_distribution_reads_as_having_no_address() {
        // A supported state rather than a fault: the response is well formed
        // and simply declines to say where the file is.
        let envelope: ApiEnvelope<CurseForgeFileDocument> = serde_json::from_str(
            r#"{"data":{"id":1,"gameId":432,"modId":2,"isAvailable":true,
                "fileName":"restricted.jar","fileLength":10,"downloadUrl":null,"hashes":[]}}"#,
        )
        .expect("a documented file response");
        assert_eq!(envelope.data.download_url, None);
    }

    #[test]
    fn a_response_missing_a_required_member_is_refused_rather_than_defaulted() {
        // The alternative is a project with an id of zero, which resolves to
        // nothing and looks like a launcher bug rather than a bad response.
        let parsed: Result<ApiEnvelope<CurseForgeProject>, _> =
            serde_json::from_str(r#"{"data":{"id":238222,"name":"Just Enough Items"}}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn a_project_reporting_no_class_parses_as_none_rather_than_failing() {
        // The upstream schema documents `classId` as nullable. A schema-valid
        // `null` is not this DTO's business to reject: only the caller that
        // needs proof of the Mods class can decide what a missing class means.
        let envelope: ApiEnvelope<CurseForgeProject> = serde_json::from_str(
            r#"{"data":{"id":238222,"gameId":432,"name":"Just Enough Items","slug":"jei",
                "classId":null}}"#,
        )
        .expect("a schema-valid null must parse, not fail");
        assert_eq!(envelope.data.class_id, None);
    }

    #[test]
    fn a_project_omitting_the_class_member_entirely_also_parses_as_none() {
        let envelope: ApiEnvelope<CurseForgeProject> = serde_json::from_str(
            r#"{"data":{"id":238222,"gameId":432,"name":"Just Enough Items","slug":"jei"}}"#,
        )
        .expect("an omitted optional member must parse, not fail");
        assert_eq!(envelope.data.class_id, None);
    }

    #[test]
    fn a_top_level_class_document_carries_the_id_and_name_its_slug_names() {
        let envelope: ApiEnvelope<Vec<CurseForgeCategoryClass>> = serde_json::from_str(
            r#"{"data":[{"id":6,"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true},
                {"id":12,"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}]}"#,
        )
        .expect("a documented categories response");
        assert_eq!(envelope.data.len(), 2);
        assert_eq!(envelope.data[0].id, 6);
        assert_eq!(envelope.data[0].name, "Mods");
        assert_eq!(envelope.data[0].slug, "mc-mods");
        assert_eq!(envelope.data[1].name, "Resource Packs");
        assert_eq!(envelope.data[1].slug, "texture-packs");
    }
}
