//! Localization resources and message lookup.
//!
//! English is the only bundled locale. The localization boundary keeps
//! user-facing strings out of business logic.
//! Provider names, technical ids, paths, versions, and Minecraft log text stay
//! untranslated - they travel through this layer as *arguments*, never as part
//! of a translatable message.
//!
//! # How the rest of the workspace uses this
//!
//! Business logic never produces English. A typed error carries structured data
//! and a stable message id ([`ephervex_storage`-style `message_id()` methods]);
//! the frontend calls [`lookup`] or [`lookup_args`] to turn that into text the
//! user reads. That is what keeps the CLI, the GUI, and a future locale from
//! having to agree on prose.
//!
//! [`ephervex_storage`-style `message_id()` methods]: https://github.com/ChaoticTrials/Ephervex
//!
//! # Two lookup paths, deliberately
//!
//! * [`fl!`](i18n_embed_fl::fl) - used *inside this crate*. The macro reads the
//!   fallback resource at compile time, so a typo in a message id or a missing
//!   argument is a build failure. It can only be used by a crate that owns an
//!   `i18n.toml`, which is why the ids storage and registry support needs get wrapped in the
//!   [`messages`] module rather than being called from `ephervex-storage`.
//! * [`lookup`] / [`lookup_args`] - the runtime path, used by every other
//!   crate. Ids come from error types and are checked by the tests in this
//!   crate rather than by the compiler.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use i18n_embed::fluent::{FluentLanguageLoader, fluent_language_loader};
use i18n_embed::{DesktopLanguageRequester, LanguageLoader};
use rust_embed::RustEmbed;
use unic_langid::LanguageIdentifier;

pub use unic_langid;

pub mod messages;

/// The bundled `.ftl` resources, compiled into the binary so a stripped-down
/// installation cannot end up without its own user interface text.
#[derive(RustEmbed, Debug)]
#[folder = "locales/"]
struct Localizations;

/// The language every message is guaranteed to exist in.
pub const FALLBACK_LANGUAGE: &str = "en-US";

/// The process-wide message loader.
///
/// Public because `ephervex-i18n`'s own `fl!` call sites need it and because a
/// frontend may want the raw Fluent API for something this module does not
/// wrap. Prefer [`lookup`] and [`lookup_args`].
pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();

    // Load English eagerly. Without this, any lookup made before the frontend
    // gets around to selecting a language would return the loader's
    // `No localization for id` placeholder - which is exactly what happens on
    // the error paths that run before configuration has been read.
    if let Err(error) = loader.load_fallback_language(&Localizations) {
        // Tracing may not be initialized this early, and there is no
        // user-facing text to show when the text itself failed to load.
        tracing::error!(%error, "failed to load bundled English localization");
    }

    disable_bidi_isolation(&loader);
    loader
});

/// Turns off Fluent's Unicode bidi isolate marks around interpolated values.
///
/// They are invisible in a browser and pure noise in a terminal, a log line, or
/// a test assertion, and everything this application interpolates is a path, a
/// version, or a name that reads left-to-right in the only locale that ships.
///
/// It has to be re-applied after every load: the setting lives on the Fluent
/// bundles, and loading a language builds new ones.
fn disable_bidi_isolation(loader: &FluentLanguageLoader) {
    loader.set_use_isolating(false);
}

/// Errors from selecting a language. Message lookup itself never fails: an
/// unknown id degrades to a visible placeholder plus a logged error, because
/// a missing translation must not be able to take down a launch.
#[derive(Debug, thiserror::Error)]
pub enum I18nError {
    /// The tag is not a valid Unicode language identifier.
    #[error("`{tag}` is not a well-formed language identifier")]
    MalformedLanguageTag {
        tag: String,
        #[source]
        source: unic_langid::LanguageIdentifierError,
    },
    /// The tag is well-formed but no resource is bundled for it.
    #[error("no localization is bundled for `{tag}`")]
    UnavailableLanguage { tag: String },
    /// The bundled resources could not be read or parsed.
    #[error("localization assets could not be loaded")]
    Assets(#[from] i18n_embed::I18nEmbedError),
}

/// A value substituted into a parameterized message.
///
/// The split matters: [`Arg::Count`] reaches Fluent as a number, which is what
/// its plural rules select on. Passing "3" as text would silently fall through
/// to the `*[other]` branch in every language, and the bug would only be
/// visible to speakers of languages with more than two plural forms.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg<'a> {
    /// Untranslated text: a path, a version, a provider name, or a
    /// user-authored instance name.
    Text(Cow<'a, str>),
    /// A quantity, used for plural selection.
    Count(i64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(value: &'a str) -> Self {
        Self::Text(Cow::Borrowed(value))
    }
}

impl From<String> for Arg<'static> {
    fn from(value: String) -> Self {
        Self::Text(Cow::Owned(value))
    }
}

impl From<&Path> for Arg<'static> {
    fn from(value: &Path) -> Self {
        // Paths are shown verbatim and lossily: a path that is not UTF-8 is
        // still more useful to a user than no path at all.
        Self::Text(Cow::Owned(value.display().to_string()))
    }
}

macro_rules! arg_from_integer {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<$ty> for Arg<'static> {
                fn from(value: $ty) -> Self {
                    Self::Count(i64::from(value))
                }
            }
        )+
    };
}

arg_from_integer!(i8, i16, i32, i64, u8, u16, u32);

impl From<usize> for Arg<'static> {
    fn from(value: usize) -> Self {
        // Saturating rather than wrapping: a count that large is already a bug,
        // and a negative-looking plural form would be a confusing way to learn
        // about it.
        Self::Count(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

/// Converts [`Arg`]s into the argument map Fluent wants.
///
/// The value type is left generic on purpose. `i18n-embed` does not re-export
/// `fluent::FluentValue`, so it cannot be named here; bounding on the three
/// `From` conversions it provides lets the compiler infer it at the call site
/// and keeps `fluent` out of this crate's dependency list.
fn fluent_args<'a, I, V>(args: I) -> HashMap<&'a str, V>
where
    I: IntoIterator<Item = (&'a str, Arg<'a>)>,
    V: From<&'a str> + From<String> + From<i64>,
{
    args.into_iter()
        .map(|(name, value)| {
            let value = match value {
                Arg::Text(Cow::Borrowed(text)) => V::from(text),
                Arg::Text(Cow::Owned(text)) => V::from(text),
                Arg::Count(count) => V::from(count),
            };
            (name, value)
        })
        .collect()
}

/// Looks up a message with no arguments.
///
/// An unknown id yields a visible placeholder and a logged error rather than a
/// panic - a missing string is a translation bug, not a reason to abort a
/// launch the user is in the middle of.
#[must_use]
pub fn lookup(message_id: &str) -> String {
    LANGUAGE_LOADER.get(message_id)
}

/// Looks up a message and substitutes its arguments.
///
/// ```
/// use ephervex_i18n::{Arg, lookup_args};
///
/// let text = lookup_args("instances-list-count", [("count", Arg::from(1_usize))]);
/// assert_eq!(text, "1 instance");
/// ```
#[must_use]
pub fn lookup_args<'a, I>(message_id: &str, args: I) -> String
where
    I: IntoIterator<Item = (&'a str, Arg<'a>)>,
{
    LANGUAGE_LOADER.get_args_concrete(message_id, fluent_args(args))
}

/// Looks up an attribute of a message, e.g. the actionable next step attached
/// to an error summary.
///
/// Returns `None` when the message has no such attribute, so a caller can omit
/// the "what to do about it" line rather than printing a placeholder.
#[must_use]
pub fn lookup_attribute(message_id: &str, attribute_id: &str) -> Option<String> {
    has_attribute(message_id, attribute_id)
        .then(|| LANGUAGE_LOADER.get_attr(message_id, attribute_id))
}

/// Looks up an attribute and substitutes its arguments.
#[must_use]
pub fn lookup_attribute_args<'a, I>(message_id: &str, attribute_id: &str, args: I) -> Option<String>
where
    I: IntoIterator<Item = (&'a str, Arg<'a>)>,
{
    has_attribute(message_id, attribute_id).then(|| {
        LANGUAGE_LOADER.get_attr_args_concrete(message_id, attribute_id, fluent_args(args))
    })
}

/// Whether a message id exists in the currently selected language chain.
#[must_use]
pub fn has(message_id: &str) -> bool {
    LANGUAGE_LOADER.has(message_id)
}

/// Whether a message has the given attribute.
#[must_use]
pub fn has_attribute(message_id: &str, attribute_id: &str) -> bool {
    LANGUAGE_LOADER.has_attr(message_id, attribute_id)
}

/// The languages this build actually ships resources for.
///
/// This is what a settings dropdown is populated from; it is derived from the
/// embedded assets, so shipping a new locale is dropping a directory into
/// `locales/` with no code change.
pub fn available_languages() -> Result<Vec<LanguageIdentifier>, I18nError> {
    Ok(LANGUAGE_LOADER.available_languages(&Localizations)?)
}

/// The language currently used for lookups.
#[must_use]
pub fn current_language() -> LanguageIdentifier {
    LANGUAGE_LOADER.current_language()
}

/// Selects the language named by a tag such as `en-US`.
///
/// A language setting exists even though only English ships, so the plumbing
/// that a second locale needs is exercised from day one rather than being
/// discovered to be missing when someone offers a translation.
pub fn select_language(tag: &str) -> Result<LanguageIdentifier, I18nError> {
    let requested: LanguageIdentifier =
        tag.parse()
            .map_err(|source| I18nError::MalformedLanguageTag {
                tag: tag.to_owned(),
                source,
            })?;

    let selected = i18n_embed::select(
        &*LANGUAGE_LOADER,
        &Localizations,
        std::slice::from_ref(&requested),
    )?;
    disable_bidi_isolation(&LANGUAGE_LOADER);

    // `select` negotiates, and negotiation happily falls back. Reporting that
    // explicitly is the difference between "your language is not available" and
    // an interface that silently stays English for no stated reason.
    if selected.iter().any(|language| language == &requested) {
        Ok(requested)
    } else {
        Err(I18nError::UnavailableLanguage {
            tag: tag.to_owned(),
        })
    }
}

/// Selects the language the desktop environment asks for, falling back to
/// English.
///
/// Used when configuration does not pin a language.
pub fn select_system_language() -> Vec<LanguageIdentifier> {
    let requested = DesktopLanguageRequester::requested_languages();
    match i18n_embed::select(&*LANGUAGE_LOADER, &Localizations, &requested) {
        Ok(selected) => {
            disable_bidi_isolation(&LANGUAGE_LOADER);
            selected
        }
        Err(error) => {
            tracing::warn!(%error, "falling back to English; system language could not be selected");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn english_is_available_and_is_the_fallback() {
        let available = available_languages().expect("bundled resources should enumerate");
        assert!(
            available
                .iter()
                .any(|language| *language == FALLBACK_LANGUAGE),
            "en-US must ship: {available:?}"
        );
        assert_eq!(
            LANGUAGE_LOADER.fallback_language().to_string(),
            FALLBACK_LANGUAGE
        );
    }

    #[test]
    fn messages_are_resolvable_before_any_language_is_selected() {
        // The eager fallback load in `LANGUAGE_LOADER` is what makes this true,
        // and errors raised before configuration is read depend on it.
        assert_eq!(lookup("app-name"), "Ephervex");
    }

    #[test]
    fn parameterized_messages_substitute_untranslated_values() {
        let text = lookup_args(
            "instance-summary",
            [
                ("name", Arg::from("Crash repro")),
                ("version", Arg::from("1.20.1")),
                ("loader", Arg::from("NeoForge 47.4.0")),
            ],
        );
        assert_eq!(text, "Crash repro - Minecraft 1.20.1, NeoForge 47.4.0");
    }

    #[test]
    fn counts_select_the_matching_plural_form() {
        assert_eq!(
            lookup_args("instances-list-count", [("count", Arg::from(1_usize))]),
            "1 instance"
        );
        assert_eq!(
            lookup_args("instances-list-count", [("count", Arg::from(0_usize))]),
            "0 instances"
        );
        assert_eq!(
            lookup_args("instances-list-count", [("count", Arg::from(7_usize))]),
            "7 instances"
        );
    }

    #[test]
    fn counts_are_passed_to_fluent_as_numbers_not_text() {
        // A count sent as text falls through to `*[other]` in every language,
        // which looks correct in English and is wrong everywhere else.
        assert_eq!(Arg::from(3_usize), Arg::Count(3));
        assert_ne!(Arg::from(3_usize), Arg::from("3"));
    }

    #[test]
    fn errors_expose_an_optional_actionable_attribute() {
        let summary = lookup("error-config-invalid");
        assert!(!summary.is_empty());

        let action = lookup_attribute_args(
            "error-config-invalid",
            "action",
            [(
                "path",
                Arg::from(Path::new("/home/dev/.config/ephervex/config.toml")),
            )],
        )
        .expect("this error carries a next step");
        assert!(action.contains("/home/dev/.config/ephervex/config.toml"));

        // Not every message has one, and callers must be able to tell.
        assert!(lookup_attribute("app-name", "action").is_none());
    }

    #[test]
    fn paths_are_interpolated_verbatim_rather_than_translated() {
        let text = lookup_args(
            "reconcile-persistent-root-missing-detail",
            [
                ("name", Arg::from("Modpack repro")),
                (
                    "path",
                    Arg::from(Path::new("/home/dev/.local/share/ephervex/instances/x")),
                ),
            ],
        );
        assert!(text.contains("/home/dev/.local/share/ephervex/instances/x"));
    }

    #[test]
    fn selecting_english_succeeds_and_selecting_an_unshipped_language_reports_why() {
        assert_eq!(select_language("en-US").unwrap().to_string(), "en-US");

        let error = select_language("de-DE").expect_err("only English ships");
        assert!(matches!(error, I18nError::UnavailableLanguage { .. }));

        let error = select_language("not a language").expect_err("malformed tag");
        assert!(matches!(error, I18nError::MalformedLanguageTag { .. }));

        // Leave the loader on English for whichever test runs next.
        select_language("en-US").unwrap();
    }

    #[test]
    fn no_message_id_is_defined_twice() {
        // Fluent keeps the first definition and writes "Overriding" to stderr
        // for the second. That is invisible in the GUI and corrupts `--json`,
        // whose failures share stderr with it, so a duplicate is caught here
        // rather than by a command that mysteriously stops being parseable.
        let source = include_str!("../locales/en-US/main.ftl");
        let mut seen = std::collections::BTreeSet::new();
        let mut repeated = Vec::new();
        for line in source.lines() {
            // A message definition starts in column zero; an attribute line
            // starts with `.` and a continuation is indented.
            let Some((id, _)) = line.split_once(" = ") else {
                continue;
            };
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                continue;
            }
            if !seen.insert(id) {
                repeated.push(id);
            }
        }
        assert!(repeated.is_empty(), "defined more than once: {repeated:?}");
    }

    #[test]
    fn no_message_uses_the_unicode_ellipsis() {
        // User-facing text trails off with three ASCII periods.
        let source = include_str!("../locales/en-US/main.ftl");
        let offenders: Vec<&str> = source
            .lines()
            .filter(|line| !line.starts_with('#') && line.contains('\u{2026}'))
            .collect();
        assert!(offenders.is_empty(), "Unicode ellipsis in: {offenders:?}");
    }

    #[test]
    fn no_message_defines_the_same_attribute_twice() {
        // Fluent keeps only the first attribute of a given name, so a second
        // `.action` is unreachable. It usually means a message was inserted
        // between another message and its attribute line.
        let source = include_str!("../locales/en-US/main.ftl");
        let mut current = "";
        let mut seen = std::collections::BTreeSet::new();
        let mut repeated = Vec::new();
        for line in source.lines() {
            let Some((head, _)) = line.split_once(" = ") else {
                continue;
            };
            if !head.is_empty()
                && head
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                current = head;
            } else if let Some(name) = head.trim_start().strip_prefix('.')
                && !seen.insert((current, name))
            {
                repeated.push(format!("{current}.{name}"));
            }
        }
        assert!(
            repeated.is_empty(),
            "attribute defined more than once: {repeated:?}"
        );
    }

    #[test]
    fn mod_error_recovery_advice_stays_on_the_error_it_describes() {
        select_language("en-US").unwrap();

        let identity = lookup_attribute("error-mod-identity-incomplete", "action")
            .expect("a mod with no identity says how to proceed");
        assert!(identity.contains("managed by hand"));

        // A rename failure can hit a fully identified mod, so it must not
        // carry advice about mods that have no identity.
        assert_eq!(
            lookup_attribute("error-mod-file-not-toggled", "action"),
            None
        );

        let path = "/home/dev/mods/example.jar";
        let unrecorded = lookup_attribute_args(
            "error-mod-managed-identity-missing",
            "action",
            [("path", Arg::from(path))],
        )
        .expect("the refusal to record a file says why");
        assert!(unrecorded.contains(path));

        let hand_placed = lookup_attribute("error-mod-hand-placed-not-mutable", "action")
            .expect("a hand-placed jar says how to get rid of it");
        assert!(hand_placed.contains("Delete it from the mods list"));
        assert!(!hand_placed.contains("look up again"));
    }

    #[test]
    fn an_unknown_message_id_degrades_visibly_instead_of_panicking() {
        assert!(!has("no-such-message-id"));
        assert!(lookup("no-such-message-id").contains("no-such-message-id"));
    }

    #[test]
    fn every_message_id_referenced_by_storage_errors_exists() {
        // `ephervex-storage` cannot use `fl!` (it owns no `i18n.toml`), so the
        // ids it hands out are checked here instead. Keep this list in step
        // with the `message_id()` implementations in that crate.
        for id in [
            "error-config-unreadable",
            "error-config-invalid",
            "error-config-unwritable",
            "error-config-busy",
            "error-config-unsupported-schema",
            "error-config-secret-rejected",
            "error-paths-no-home-directory",
            "error-paths-directory-creation",
            "error-index-unopenable",
            "error-index-query-failed",
            "error-index-migration-failed",
            "error-index-schema-from-future",
            "error-registry-instance-unknown",
            "error-registry-server-unknown",
            "error-registry-instance-metadata-foreign",
            "error-registry-server-metadata-foreign",
            "error-registry-metadata-unwritable",
            "error-registry-metadata-unreadable",
            "error-registry-metadata-invalid",
            "error-registry-metadata-unsupported-version",
            "error-registry-root-occupied",
            "error-registry-host-busy",
            "error-registry-host-not-claimed",
            "error-registry-recipe-busy",
            "error-registry-restore-unfinished",
            "error-registry-host-guard-unusable",
            "storage-class-temporary",
            "storage-class-persistent",
            "attention-missing-root",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn every_message_id_referenced_by_instance_management_exists() {
        // `ephervex-options` and `ephervex-storage` own no `i18n.toml`, so
        // `OptionsError::message_id()`, `InvalidPreferenceValue::message_id()`,
        // and the instance and preference management CLI ids are checked here.
        for id in [
            "error-options-unreadable",
            "error-options-unwritable",
            "error-options-preference-value-invalid",
            "error-options-preference-value-unusable",
            "error-options-preference-unrecognized",
            "error-options-preference-catalog-invalid",
            "error-options-compatibility-invalid",
            "error-options-feed-unavailable",
            "error-registry-root-missing",
            "instance-duplicated",
            "instance-duplicate-skipped-links",
            "instance-folder-unavailable",
            "preferences-none-managed",
            "preferences-managed-count",
            "preferences-applied",
            "preferences-unsupported-for-version",
            "preferences-unverified-for-version",
            "preferences-value-not-applied",
            "preferences-language-unverified",
            "preferences-language-absent",
            "preference-set",
            "preference-unmanaged",
        ] {
            assert!(has(id), "missing message id: {id}");
        }

        // The two refusals a user can act on carry a next step, and both of
        // them interpolate: an action that lost its argument would render a
        // literal `{$…}`.
        let syntax = lookup_attribute_args(
            "error-options-preference-value-invalid",
            "action",
            [("syntax", Arg::from("auto, or 1 to 4"))],
        )
        .expect("a refused value says what would have worked");
        assert!(syntax.contains("auto, or 1 to 4"), "{syntax}");

        let folder = lookup_attribute_args(
            "instance-folder-unavailable",
            "action",
            [("folder", Arg::from("crash-reports"))],
        )
        .expect("an unavailable folder explains itself");
        assert!(folder.contains("crash-reports"), "{folder}");
    }

    #[test]
    fn a_vanished_instance_directory_does_not_read_as_a_missing_record() {
        // Keep the record of a persistent instance whose files are gone,
        // so the two failures must not share a sentence.
        assert_ne!(
            lookup("error-registry-root-missing"),
            lookup("error-registry-instance-unknown")
        );
        assert!(
            lookup_attribute("error-registry-root-missing", "action")
                .expect("a next step")
                .contains("kept the record")
        );
    }

    #[test]
    fn foreign_host_metadata_names_the_directory_and_the_way_to_forget_the_record() {
        for id in [
            "error-registry-instance-metadata-foreign",
            "error-registry-server-metadata-foreign",
        ] {
            let path = [("path", Arg::from("/data/copy"))];
            let summary = lookup_args(id, path.clone());
            let action = lookup_attribute_args(id, "action", path).expect("a next step");
            assert!(summary.contains("/data/copy"), "{summary}");
            assert!(action.contains("keep its files"), "{action}");
            assert_ne!(summary, lookup("error-registry-instance-unknown"));
            assert_ne!(summary, lookup("error-registry-server-unknown"));
        }
    }

    #[test]
    fn every_message_id_referenced_by_minecraft_metadata_errors_exists() {
        // Same arrangement as above: `ephervex-minecraft` owns no `i18n.toml`,
        // so its `MinecraftError::message_id()` arms are checked here.
        for id in [
            "error-minecraft-manifest-unavailable",
            "error-minecraft-manifest-invalid",
            "error-minecraft-version-unknown",
            "error-minecraft-version-unsupported",
            "error-minecraft-version-not-a-release",
            "error-minecraft-version-metadata-unavailable",
            "error-minecraft-version-metadata-corrupt",
            "error-minecraft-cached-version-metadata-damaged",
            "error-minecraft-version-metadata-invalid",
            "error-minecraft-version-metadata-unreadable",
            "error-minecraft-language-catalog-unavailable",
            "error-minecraft-asset-index-invalid",
            "error-minecraft-metadata-incomplete",
            "error-minecraft-metadata-url-invalid",
            "error-minecraft-cache-key-invalid",
            "minecraft-versions-list-empty",
            "minecraft-versions-list-count",
            "minecraft-versions-stale",
            "minecraft-version-id-unrecognized",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn every_message_id_referenced_by_java_runtime_errors_exists() {
        // `ephervex-java` owns no `i18n.toml` either, so its
        // `JavaError::message_id()`, `ProbeError::message_id()`, and
        // `ArchiveError::message_id()` arms are checked here. An id with no
        // message renders as a placeholder in the user's face.
        for id in [
            "error-java-no-compatible-runtime",
            "error-java-pinned-incompatible",
            "error-java-runtime-unusable",
            "error-java-auto-detect-disabled",
            "error-java-managed-download-disabled",
            "error-java-managed-arch-unsupported",
            "error-java-managed-api-unusable",
            "error-java-managed-index-unavailable",
            "error-java-managed-index-invalid",
            "error-java-managed-release-missing",
            "error-java-managed-release-malformed",
            "error-java-managed-download-failed",
            "error-java-managed-extraction-failed",
            "error-java-managed-canceled",
            "error-java-managed-runtime-unusable",
            "error-java-managed-install-blocked",
            "error-java-managed-cache-key-invalid",
            "error-java-io",
            "error-java-archive-unsafe",
            "error-java-archive-too-large",
            "error-java-archive-unreadable",
            "error-java-archive-unwritable",
            "error-java-archive-empty",
            "error-java-probe-not-executable",
            "error-java-probe-unintelligible",
            "error-java-probe-timed-out",
            "java-runtimes-list-empty",
            "java-runtimes-list-count",
            "java-runtime-probe-failed",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn every_message_id_referenced_by_authentication_errors_exists() {
        // `ephervex-auth` owns no `i18n.toml` either, so its
        // `AuthError::message_id()`, `XstsDenial::message_id()`,
        // `AuthStage::message_id()`, and `CredentialStorage::message_id()` arms
        // are checked here. An id with no message renders as a placeholder at
        // the exact moment a user is being told why they cannot log in.
        for id in [
            "error-auth-client-id-missing",
            "error-auth-client-id-invalid",
            "error-auth-endpoint-unusable",
            "error-auth-unreachable",
            "error-auth-unexpected-response",
            "error-auth-service-unavailable",
            "error-auth-microsoft-rejected",
            "error-auth-declined",
            "error-auth-device-code-expired",
            "error-auth-timed-out",
            "error-auth-state-mismatch",
            "error-auth-redirect-listener",
            "error-auth-canceled",
            "error-auth-xbox-authentication-failed",
            "error-auth-xbox-no-account",
            "error-auth-xbox-account-banned",
            "error-auth-xbox-country-unavailable",
            "error-auth-xbox-adult-verification",
            "error-auth-xbox-child-account",
            "error-auth-xbox-denied",
            "error-auth-xbox-user-hash-missing",
            "error-auth-minecraft-login-rejected",
            "error-auth-minecraft-unavailable",
            "error-auth-minecraft-api-not-approved",
            "error-auth-game-not-owned",
            "error-auth-game-pass-setup-required",
            "error-auth-game-ownership-unconfirmed",
            "error-auth-profile-unavailable",
            "error-auth-profile-malformed",
            "error-auth-secret-storage-unavailable",
            "error-auth-credential-change-busy",
            "error-auth-credential-change-unavailable",
            "error-auth-credential-store-failed",
            "error-auth-credential-blank",
            "error-auth-not-signed-in",
            "error-auth-account-mismatch",
            "error-auth-reauthentication-required",
            "auth-stage-microsoft",
            "auth-stage-xbox-live",
            "auth-stage-xsts",
            "auth-stage-minecraft-login",
            "auth-stage-entitlements",
            "auth-stage-profile",
            "auth-storage-keyring",
            "auth-storage-session-only",
            "auth-signed-out",
            "auth-signed-in",
            "auth-session-only-warning",
            "auth-open-browser",
            "auth-device-code-prompt",
            "auth-logged-out-credential-deleted",
            "auth-logged-out-credential-pending",
            "auth-logged-out-no-credential",
            "auth-nothing-to-log-out-of",
            "auth-storage-none-needed",
            "auth-storage-not-saved",
            "auth-storage-unknown",
            "account-kind-microsoft",
            "account-kind-offline",
            "accounts-list-empty",
            "accounts-list-count",
            "account-added-offline",
            "account-default-set",
            "account-active-offline",
            "account-removed",
            "account-removed-with-credential",
            "account-removed-credential-pending",
            "error-auth-offline-name-invalid",
            "error-auth-offline-account-exists",
            "error-auth-account-unknown",
            "error-auth-account-ambiguous",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn every_message_id_referenced_by_the_launch_pipeline_exists() {
        // `ephervex-minecraft` and `ephervex-storage`'s lock own no `i18n.toml`
        // either, so `LaunchError::message_id()`, `NativesError::message_id()`,
        // and `LockError::message_id()` are checked here. An id with no message
        // renders as a placeholder at the exact moment a launch is failing.
        for id in [
            "error-launch-already-running",
            "error-launch-lock-unusable",
            "error-launch-lock-busy",
            "error-launch-lock-lost",
            "error-launch-id-invalid",
            "error-launch-invalid-version-metadata",
            "error-launch-loader-not-supported",
            "error-launch-incompatible-native-library",
            "error-launch-missing-native-artifact",
            "error-launch-download-failed",
            "error-launch-artifact-mismatched",
            "error-loader-metadata-unavailable",
            "error-loader-metadata-invalid",
            "error-loader-version-unavailable",
            "error-loader-identity-mismatch",
            "error-loader-installer-invalid",
            "error-loader-installer-format-unsupported",
            "error-loader-installer-library-conflict",
            "error-loader-download-failed",
            "error-loader-version-metadata-invalid",
            "error-loader-io",
            "error-loader-processor-failed",
            "error-loader-server-target-occupied",
            "error-launch-asset-index-invalid",
            "error-launch-natives-unreadable",
            "error-launch-natives-unsafe-path",
            "error-launch-natives-too-large",
            "error-launch-natives-write",
            "error-launch-unresolved-placeholder",
            "error-launch-path-not-utf8",
            "error-launch-game-directory",
            "error-launch-process-start",
            "error-launch-process-lost",
            "error-launch-canceled",
            "launch-preparing",
            "launch-starting",
            "launch-stopping",
            "launch-output-dropped",
            "server-output-dropped",
            "launch-account-offline",
            "launch-account-microsoft",
            "launch-exited-cleanly",
            "launch-exited-with-code",
            "launch-exited-with-signal",
            "launch-dry-run",
            "launch-command-redacted",
            "operation-launching-minecraft",
            "operation-identifier",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }

    #[test]
    fn every_message_id_the_diagnostics_browser_needs_exists() {
        // `ephervex-storage` owns no `i18n.toml`, so `DiagnosticError` is
        // checked here. A log browser that renders a placeholder where the
        // reason should be is a log browser nobody can act on.
        for id in [
            "error-diagnostics-directory-unreadable",
            "error-diagnostics-file-missing",
            "error-diagnostics-file-unreadable",
            "error-diagnostics-file-refused",
            "diagnostics-nothing-recorded",
            "diagnostics-window-truncated",
            "diagnostics-window-lossy",
            "diagnostics-search-no-matches",
            "diagnostics-search-truncated",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
        for id in [
            "error-diagnostics-directory-unreadable",
            "error-diagnostics-file-missing",
            "error-diagnostics-file-unreadable",
            "error-diagnostics-file-refused",
        ] {
            assert!(
                has_attribute(id, "action"),
                "a diagnostics failure needs a next step: {id}"
            );
        }
    }

    #[test]
    fn launching_as_an_offline_account_says_it_cannot_join_online_servers() {
        // The one thing a user has to understand about an offline account, at
        // the one moment it matters.
        let message = lookup_args("launch-account-offline", [("name", "Notch".into())]);
        assert!(message.contains("Notch"));
        assert!(
            message.to_lowercase().contains("online-mode"),
            "an offline identity proves nothing, and the launch must say so: {message}"
        );
    }

    #[test]
    fn the_no_secure_storage_message_names_the_alternative_and_the_guarantee() {
        // This message must say that secure storage is
        // unavailable, offer session-only login, and make clear nothing is
        // being written to a file. All three have to survive a reword.
        let action = lookup_attribute("error-auth-secret-storage-unavailable", "action")
            .expect("this refusal has a next step");
        assert!(action.contains("session only"), "{action}");
        assert!(action.contains("never write"), "{action}");
    }

    #[test]
    fn an_offline_account_is_never_described_as_a_missing_credential() {
        // Offline accounts have no credential material to
        // store, not by a laxer store. The wording has to reflect that, or a
        // user will read it as a broken keyring.
        let none_needed = lookup("auth-storage-none-needed");
        let not_saved = lookup("auth-storage-not-saved");
        assert_ne!(none_needed, not_saved);
        assert!(
            !none_needed.to_lowercase().contains("not saved"),
            "{none_needed}"
        );

        // The two kinds must be distinguishable wherever they are listed.
        assert_ne!(
            lookup("account-kind-microsoft"),
            lookup("account-kind-offline")
        );
    }

    #[test]
    fn a_permanent_authentication_failure_never_advises_waiting() {
        // The regression this pins: a 403 from Minecraft Services means the
        // Azure application is not approved, which is permanent until someone
        // fills in a form. It was reported as "Try again in a few minutes",
        // which is advice that can never come true.
        let action = lookup_attribute_args(
            "error-auth-minecraft-api-not-approved",
            "action",
            [
                ("url", Arg::from("https://aka.ms/mce-reviewappid")),
                (
                    "client_id",
                    Arg::from("00001111-aaaa-2222-bbbb-3333cccc4444"),
                ),
            ],
        )
        .expect("this refusal has a next step");
        assert!(
            action.contains("https://aka.ms/mce-reviewappid"),
            "{action}"
        );
        assert!(
            action.contains("00001111-aaaa-2222-bbbb-3333cccc4444"),
            "{action}"
        );
        assert!(!action.contains("few minutes"), "{action}");

        // The genuinely transient neighbors keep that advice, and must not
        // share a message with the permanent one.
        for id in [
            "error-auth-minecraft-unavailable",
            "error-auth-service-unavailable",
        ] {
            let action = lookup_attribute(id, "action").expect("a next step");
            assert!(action.contains("few minutes"), "{id}: {action}");
        }
        assert_ne!(
            lookup("error-auth-minecraft-api-not-approved"),
            lookup("error-auth-minecraft-unavailable")
        );
        assert_ne!(
            lookup("error-auth-minecraft-login-rejected"),
            lookup("error-auth-minecraft-unavailable")
        );
    }

    #[test]
    fn not_owning_the_game_and_owning_it_through_game_pass_read_differently() {
        // A Game Pass subscriber with no Java profile does own the game. The
        // two messages must not be interchangeable.
        let not_owned = lookup("error-auth-game-not-owned");
        let game_pass = lookup("error-auth-game-pass-setup-required");
        assert_ne!(not_owned, game_pass);
        assert!(game_pass.contains("Game Pass"), "{game_pass}");
        assert!(
            lookup_attribute("error-auth-game-pass-setup-required", "action")
                .expect("a next step")
                .contains("official Minecraft Launcher")
        );
    }

    #[test]
    fn unconfirmed_ownership_does_not_advise_buying_the_game() {
        let unconfirmed = lookup("error-auth-game-ownership-unconfirmed");
        assert_ne!(unconfirmed, lookup("error-auth-game-not-owned"));
        let action = lookup_attribute("error-auth-game-ownership-unconfirmed", "action")
            .expect("a next step");
        assert!(action.contains("Try again"), "{action}");
        assert!(!action.contains("buy"), "{action}");
    }

    #[test]
    fn an_offline_version_list_offers_a_next_step_without_needing_a_path() {
        // Most error actions interpolate `$path`. The network ones cannot, and
        // a frontend must still be able to show them.
        let action = lookup_attribute("error-minecraft-manifest-unavailable", "action")
            .expect("being offline has an obvious next step");
        assert!(action.contains("network"));
    }
}
