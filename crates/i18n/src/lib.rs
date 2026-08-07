//! Localization resources and message lookup.
//!
//! Only English ships initially, but the architecture is in place from the
//! start so no user-facing string ends up embedded deep inside business logic.
//! Provider names, technical ids, paths, versions, and Minecraft log text stay
//! untranslated — they travel through this layer as *arguments*, never as part
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
//! [`ephervex_storage`-style `message_id()` methods]: https://github.com/ChaoticTrials/ephervex
//!
//! # Two lookup paths, deliberately
//!
//! * [`fl!`](i18n_embed_fl::fl) — used *inside this crate*. The macro reads the
//!   fallback resource at compile time, so a typo in a message id or a missing
//!   argument is a build failure. It can only be used by a crate that owns an
//!   `i18n.toml`, which is why the ids storage and registry support needs get wrapped in the
//!   [`messages`] module rather than being called from `ephervex-storage`.
//! * [`lookup`] / [`lookup_args`] — the runtime path, used by every other
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
    // `No localization for id` placeholder — which is exactly what happens on
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
/// panic — a missing string is a translation bug, not a reason to abort a
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
        assert_eq!(text, "Crash repro — Minecraft 1.20.1, NeoForge 47.4.0");
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
            "error-registry-metadata-unwritable",
            "error-registry-metadata-unreadable",
            "error-registry-metadata-invalid",
            "error-registry-metadata-unsupported-version",
            "error-registry-root-occupied",
            "storage-class-temporary",
            "storage-class-persistent",
            "attention-missing-root",
        ] {
            assert!(has(id), "missing message id: {id}");
        }
    }
}
