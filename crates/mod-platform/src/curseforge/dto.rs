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
///
/// The listing endpoints add a `pagination` member beside it. It is optional
/// here because most endpoints do not send one, and it is *read* rather than
/// inferred: whether another page exists is a fact the API states, and
/// deriving it from "this page came back full" guesses wrong at exactly the
/// boundary where it matters.
#[derive(Debug, Deserialize)]
pub(super) struct ApiEnvelope<T> {
    pub(super) data: T,
    #[serde(default)]
    pub(super) pagination: Option<CurseForgePagination>,
}

/// How much of a listing there is, as the API reports it.
///
/// `index` and `result_count` are read together to say where the next page
/// begins: the request's own `index` is where a caller asked this page to
/// start, but how many raw entries the provider actually placed in it is not
/// assumed to equal the requested page size, so it is read from the response
/// rather than from `items.len()`. `page_size` is documented but not declared:
/// nothing here needs to know what was asked for, only what was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgePagination {
    /// Where this page starts, as the provider echoes it back.
    #[serde(default)]
    pub(super) index: u32,
    /// How many raw entries this page actually carries, before this
    /// launcher's own class and compatibility filtering narrows it.
    #[serde(default)]
    pub(super) result_count: u64,
    /// How many entries the whole listing has.
    #[serde(default)]
    pub(super) total_count: u64,
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
/// translation belongs with the catalog behavior that needs it - side support,
/// compatibility, and version lists - and inventing half of it here would mean
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
    /// Everyone CurseForge credits for the project.
    ///
    /// A list rather than a single name, because that is what the API reports.
    /// The neutral model carries one attribution string, and picking the first
    /// entry is a display decision made where that model is built.
    #[serde(default)]
    pub authors: Vec<CurseForgeAuthor>,
    /// The project's artwork, when it has any.
    #[serde(default)]
    pub logo: Option<CurseForgeLogo>,
    /// The project's most recently published files.
    ///
    /// CurseForge never states client/server support on the project itself: it
    /// states it per file, folded into that file's `gameVersions` array. Read
    /// only for that declaration, which is why each entry is nothing more than
    /// its `gameVersions`.
    #[serde(default)]
    pub latest_files: Vec<CurseForgeProjectFile>,
    /// How many times the project has been downloaded.
    ///
    /// Read leniently: a figure that is missing or is not a plain count leaves
    /// it unknown rather than refusing the whole project over a statistic.
    #[serde(default, deserialize_with = "crate::model::lenient_count")]
    pub download_count: Option<u64>,
}

/// One credited author of a project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CurseForgeAuthor {
    pub name: String,
}

/// One of a project's `latestFiles`, read only for its client/server
/// declaration.
///
/// The full file record has everything [`CurseForgeFileDocument`] declares and
/// more, but nothing here needs any of it: which exact file a project's
/// "latest" happens to be is not tracked, only what its `gameVersions` says
/// about which side(s) it runs on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CurseForgeProjectFile {
    #[serde(default)]
    pub game_versions: Vec<String>,
}

/// A project's artwork, as the API describes it.
///
/// An address, not an image. Only the full-size address is read: the thumbnail
/// is the same picture at a size this launcher does not depend on, and a
/// caller that caches artwork already bounds what it will fetch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CurseForgeLogo {
    #[serde(default)]
    pub url: Option<String>,
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
/// and what it hashes to. The response carries dozens more - dependencies,
/// sortable game versions, module fingerprints, download counts - and none of
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
    /// The label CurseForge shows for this release.
    ///
    /// CurseForge publishes no separate version *number*: this one string is
    /// the whole of what an author called the release, and nothing is parsed
    /// out of it. Absent on responses that omit it, which is a display problem
    /// rather than a reason to refuse a file.
    #[serde(default)]
    pub(super) display_name: Option<String>,
    /// How stable the author says this file is.
    ///
    /// See [`RELEASE_TYPE_RELEASE`] and the two beside it for what the numbers
    /// mean. Absent on a response that omits it.
    #[serde(default)]
    pub(super) release_type: Option<u32>,
    /// When the file was published.
    ///
    /// Read because version ordering is by publication date: a list ordered by
    /// anything else would make "the newest that fits" depend on the order the
    /// API happened to answer in.
    #[serde(default)]
    pub(super) file_date: Option<String>,
    /// Everything this file declares compatibility with, in one array.
    ///
    /// CurseForge mixes Minecraft releases and mod loader names here: a Forge
    /// file for 1.20.1 lists both `1.20.1` and `Forge`. Splitting them is a
    /// judgment made where this document becomes the neutral model, against
    /// the API's own documented loader names rather than against a guess.
    #[serde(default)]
    pub(super) game_versions: Vec<String>,
    /// What this file says about other projects.
    #[serde(default)]
    pub(super) dependencies: Vec<CurseForgeFileRelation>,
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

/// What `POST /v1/fingerprints` answers with.
///
/// Only the exact matches are read. The response also reports which
/// fingerprints matched and which did not, and carries each project's latest
/// files: none of that identifies the file that was asked about, which is the
/// only question this endpoint is used for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgeFingerprintMatches {
    #[serde(default)]
    pub(super) exact_matches: Vec<CurseForgeFingerprintMatch>,
}

/// One file CurseForge recognized, and the release it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgeFingerprintMatch {
    /// The project the matched file belongs to.
    pub(super) id: u32,
    pub(super) file: CurseForgeFileDocument,
}

/// One relation this file declares to another project.
///
/// CurseForge states relations project to project and never file to file, so
/// there is no version to pin here and none is invented. What that costs is
/// stated where dependencies are resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CurseForgeFileRelation {
    pub(super) mod_id: u32,
    pub(super) relation_type: u32,
}

/// The `releaseType` values this launcher names.
///
/// Alpha (3) has no constant because nothing branches on it: it is what an
/// unrecognized or absent tag already resolves to, and a second name for the
/// same outcome would only invite the two to drift apart.
pub(super) const RELEASE_TYPE_RELEASE: u32 = 1;
pub(super) const RELEASE_TYPE_BETA: u32 = 2;

/// The `relationType` values this launcher acts on, as the API documents them.
///
/// `Tool` (4) is deliberately absent: a tool is something a human might also
/// want installed, not something this mod needs, and turning one into a
/// dependency would install software nobody asked for.
pub(super) const RELATION_EMBEDDED_LIBRARY: u32 = 1;
pub(super) const RELATION_OPTIONAL_DEPENDENCY: u32 = 2;
pub(super) const RELATION_REQUIRED_DEPENDENCY: u32 = 3;
pub(super) const RELATION_INCOMPATIBLE: u32 = 5;
pub(super) const RELATION_INCLUDE: u32 = 6;

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

    fn downloads_of(count: &str) -> Option<u64> {
        let document =
            format!(r#"{{"id":1,"gameId":432,"name":"A","slug":"a","downloadCount":{count}}}"#);
        serde_json::from_str::<CurseForgeProject>(&document)
            .expect("the project parses whatever its count says")
            .download_count
    }

    #[test]
    fn a_download_count_is_read_as_a_number_and_anything_else_is_unknown() {
        assert_eq!(downloads_of("1234567"), Some(1_234_567));
        assert_eq!(downloads_of("2.5e6"), Some(2_500_000));
        assert_eq!(downloads_of("12.6"), Some(13));
        for odd in ["null", "-3", "\"many\"", "true"] {
            assert_eq!(downloads_of(odd), None, "{odd}");
        }
    }

    #[test]
    fn a_project_without_a_download_count_has_none() {
        let project: CurseForgeProject =
            serde_json::from_str(r#"{"id":1,"gameId":432,"name":"A","slug":"a"}"#).unwrap();
        assert_eq!(project.download_count, None);
    }

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

    #[test]
    fn pagination_is_read_from_the_providers_own_index_and_result_count() {
        let envelope: ApiEnvelope<Vec<CurseForgeProject>> = serde_json::from_str(
            r#"{"data":[],
                "pagination":{"index":10,"pageSize":10,"resultCount":8,"totalCount":100}}"#,
        )
        .expect("a documented search response");
        let pagination = envelope.pagination.expect("a pagination member");
        assert_eq!(pagination.index, 10);
        assert_eq!(pagination.result_count, 8);
        assert_eq!(pagination.total_count, 100);
    }
}
