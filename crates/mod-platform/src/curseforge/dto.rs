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
}

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
                "summary":"View items and recipes","downloadCount":1}}"#,
        )
        .expect("a documented project response");
        assert_eq!(envelope.data.game_id, 432);
        assert_eq!(envelope.data.slug, "jei");
        assert_eq!(envelope.data.summary, "View items and recipes");
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
    fn a_response_missing_a_required_member_is_refused_rather_than_defaulted() {
        // The alternative is a project with an id of zero, which resolves to
        // nothing and looks like a launcher bug rather than a bad response.
        let parsed: Result<ApiEnvelope<CurseForgeProject>, _> =
            serde_json::from_str(r#"{"data":{"id":238222,"name":"Just Enough Items"}}"#);
        assert!(parsed.is_err());
    }
}
