//! The issue or bug report an instance reproduces.
//!
//! The link is host metadata the user typed, so it is validated where it is
//! stored and again where it is opened: a hand-edited `instance.json` is not
//! trusted to hold an address that is safe to hand to the desktop.

use ephervex_download::Url;
use ephervex_model::InstanceId;
use ephervex_storage::{ClientCommit, ClientRecord};
use tracing::instrument;

use crate::app::App;
use crate::error::{CoreError, CoreResult};

/// The longest issue link Ephervex stores, in bytes.
pub const MAX_ISSUE_LINK_BYTES: usize = 2048;

/// Why an issue link the user typed was refused.
///
/// Typed so the frontend localizes it. Each variant is something the user can
/// fix by typing a different address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IssueLinkRefusal {
    /// Longer than [`MAX_ISSUE_LINK_BYTES`].
    TooLong,
    /// Contains whitespace, control characters, or invisible direction marks.
    InvalidCharacters,
    /// Not an absolute `http://` or `https://` address.
    NotAnAddress,
    /// Uses a scheme other than `http` or `https`.
    UnsupportedScheme,
    /// Names no host.
    MissingHost,
    /// Carries a user name or password in the address.
    Credentials,
}

impl IssueLinkRefusal {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::TooLong => "error-issue-link-too-long",
            Self::InvalidCharacters => "error-issue-link-invalid-characters",
            Self::NotAnAddress => "error-issue-link-not-an-address",
            Self::UnsupportedScheme => "error-issue-link-unsupported-scheme",
            Self::MissingHost => "error-issue-link-missing-host",
            Self::Credentials => "error-issue-link-credentials",
        }
    }
}

impl std::fmt::Display for IssueLinkRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLong => "it is too long",
            Self::InvalidCharacters => "it contains whitespace or control characters",
            Self::NotAnAddress => "it is not an http or https address",
            Self::UnsupportedScheme => "only http and https addresses are allowed",
            Self::MissingHost => "it names no host",
            Self::Credentials => "it contains a user name or password",
        })
    }
}

/// Characters that are not whitespace or controls but still hide what an
/// address says: zero-width and direction-changing marks.
const fn is_invisible_mark(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

fn refuse(reason: IssueLinkRefusal) -> CoreError {
    CoreError::IssueLinkInvalid { reason }
}

/// The issue link to store, or why this one cannot be stored.
///
/// Surrounding whitespace is dropped and an empty input means "no link", so
/// `Ok(None)` clears. A value that passes is returned exactly as typed:
/// nothing here normalizes or rewrites the address.
pub fn validated_issue_link(input: &str) -> CoreResult<Option<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > MAX_ISSUE_LINK_BYTES {
        return Err(refuse(IssueLinkRefusal::TooLong));
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || is_invisible_mark(c))
    {
        return Err(refuse(IssueLinkRefusal::InvalidCharacters));
    }
    let parsed = Url::parse(trimmed).map_err(|_| refuse(IssueLinkRefusal::NotAnAddress))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(refuse(IssueLinkRefusal::UnsupportedScheme));
    }
    // The parser also accepts `http:host`, `http:///host`, and backslashes
    // for slashes, which no one reads as the address they would open.
    let after_scheme = trimmed
        .get(scheme.len()..)
        .and_then(|rest| rest.strip_prefix("://"));
    if after_scheme.is_none_or(|rest| rest.starts_with('/')) || trimmed.contains('\\') {
        return Err(refuse(IssueLinkRefusal::NotAnAddress));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(refuse(IssueLinkRefusal::MissingHost));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refuse(IssueLinkRefusal::Credentials));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Hands an issue link to the desktop's browser.
///
/// Refuses anything [`validated_issue_link`] would, so a link edited into the
/// instance's metadata by hand never reaches the desktop as a file path or a
/// custom scheme. Returns whether the desktop accepted it.
pub fn open_issue_link(link: &str) -> CoreResult<bool> {
    match validated_issue_link(link)? {
        Some(valid) => Ok(crate::open_in_browser(&valid)),
        None => Err(refuse(IssueLinkRefusal::NotAnAddress)),
    }
}

impl App {
    /// Sets or clears the issue link of a client instance.
    ///
    /// `None` or an empty string clears it. Available while the instance is
    /// running: the link is only shown, never read by the game. Refused while
    /// the instance's files are missing, since the link lives only there.
    // The raw text is skipped: a mistyped address may carry credentials.
    #[instrument(skip(self, link))]
    pub fn set_client_issue_link(
        &self,
        id: InstanceId,
        link: Option<&str>,
    ) -> CoreResult<ClientCommit<ClientRecord>> {
        let link = link.map(validated_issue_link).transpose()?.flatten();
        Ok(self
            .registry()
            .modify_client_metadata_committed(id, |record| record.issue_link = link)?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn refusal(input: &str) -> IssueLinkRefusal {
        match validated_issue_link(input) {
            Err(CoreError::IssueLinkInvalid { reason }) => reason,
            other => panic!("`{input}` should be refused, got {other:?}"),
        }
    }

    #[test]
    fn http_and_https_addresses_are_stored_exactly_as_typed() {
        for input in [
            "https://github.com/ChaoticTrials/Ephervex/issues/42",
            "http://tracker.example/bug?id=7&view=full#comment-3",
            "HTTPS://Example.COM/Path",
            "https://[::1]:8080/issue",
            "https://bücher.example/ü",
        ] {
            assert_eq!(
                validated_issue_link(input).expect("valid").as_deref(),
                Some(input),
                "{input}"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_dropped_and_empty_input_clears() {
        assert_eq!(
            validated_issue_link("  \thttps://example.com/1 \n")
                .expect("valid")
                .as_deref(),
            Some("https://example.com/1")
        );
        assert_eq!(validated_issue_link("").expect("clears"), None);
        assert_eq!(validated_issue_link("  \n").expect("clears"), None);
    }

    #[test]
    fn addresses_that_are_not_http_or_https_are_refused() {
        for (input, reason) in [
            ("example.com/issues/1", IssueLinkRefusal::NotAnAddress),
            ("/issues/1", IssueLinkRefusal::NotAnAddress),
            ("http:example.com", IssueLinkRefusal::NotAnAddress),
            ("http:\\\\example.com", IssueLinkRefusal::NotAnAddress),
            (
                "ftp://example.com/file",
                IssueLinkRefusal::UnsupportedScheme,
            ),
            ("file:///etc/passwd", IssueLinkRefusal::UnsupportedScheme),
            ("javascript:alert(1)", IssueLinkRefusal::UnsupportedScheme),
            ("mailto:a@example.com", IssueLinkRefusal::UnsupportedScheme),
            (
                "ephervex://import?url=x",
                IssueLinkRefusal::UnsupportedScheme,
            ),
            ("https://", IssueLinkRefusal::NotAnAddress),
            ("https:///path", IssueLinkRefusal::NotAnAddress),
            ("https://\\example.com", IssueLinkRefusal::NotAnAddress),
        ] {
            assert_eq!(refusal(input), reason, "{input}");
        }
    }

    #[test]
    fn credentials_in_the_address_are_refused() {
        for input in [
            "https://user:secret@example.com/1",
            "https://token@example.com/1",
            "http://:secret@example.com/1",
        ] {
            assert_eq!(refusal(input), IssueLinkRefusal::Credentials, "{input}");
        }
    }

    #[test]
    fn whitespace_and_control_characters_inside_the_address_are_refused() {
        for input in [
            "https://example.com/a b",
            "https://example.com/a\tb",
            "https://example.com/a\nb",
            "https://exam\u{0}ple.com/",
            "https://example.com/\u{1b}[31m",
            "https://example.com/\u{a0}x",
            "https://example.com/\u{202e}x",
            "https://example.com/\u{200b}x",
        ] {
            assert_eq!(
                refusal(input),
                IssueLinkRefusal::InvalidCharacters,
                "{input:?}"
            );
        }
    }

    #[test]
    fn an_address_over_the_byte_limit_is_refused_and_one_at_the_limit_is_kept() {
        let prefix = "https://example.com/";
        let at_limit = format!(
            "{prefix}{}",
            "a".repeat(MAX_ISSUE_LINK_BYTES - prefix.len())
        );
        assert_eq!(at_limit.len(), MAX_ISSUE_LINK_BYTES);
        assert_eq!(
            validated_issue_link(&at_limit).expect("valid").as_deref(),
            Some(at_limit.as_str())
        );
        assert_eq!(refusal(&format!("{at_limit}a")), IssueLinkRefusal::TooLong);
    }

    #[test]
    fn opening_a_link_that_is_not_http_or_https_is_refused_before_the_desktop_is_asked() {
        for link in ["file:///etc/passwd", "javascript:alert(1)", "", "  "] {
            assert!(open_issue_link(link).is_err(), "{link}");
        }
    }

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(ephervex_storage::Paths::rooted_at(base)).expect("app opens")
    }

    fn client_in(app: &App) -> ClientRecord {
        app.create_client_for_test(ephervex_storage::NewClient::new(
            "Bug repro",
            "1.20.1",
            ephervex_model::LoaderSelection::vanilla(),
        ))
        .expect("create")
    }

    fn stored_json(client: &ClientRecord) -> serde_json::Value {
        let bytes = std::fs::read(ephervex_storage::metadata::client_metadata_path(
            &client.root,
        ))
        .expect("read instance.json");
        serde_json::from_slice(&bytes).expect("instance.json is JSON")
    }

    #[test]
    fn a_set_link_survives_reopening_and_reconciliation_without_repair_churn() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = client_in(&app);
        assert_eq!(created.issue_link, None);
        assert!(stored_json(&created).get("issue_link").is_none());

        let link = "https://example.invalid/issues/42";
        let committed = app
            .set_client_issue_link(created.id, Some("  https://example.invalid/issues/42 \n"))
            .expect("set");
        assert_eq!(committed.record.issue_link.as_deref(), Some(link));
        assert_eq!(stored_json(&created)["issue_link"], link);

        let reopened = app_rooted_at(base.path());
        let report = reopened.reconcile().expect("reconcile");
        assert!(
            report.index_repaired.is_empty(),
            "reconciliation must not rewrite the row for a metadata-only field"
        );
        assert_eq!(
            reopened
                .current_client(created.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some(link)
        );
        assert_eq!(stored_json(&created)["issue_link"], link);
    }

    #[test]
    fn clearing_a_link_removes_it_from_the_metadata_and_empty_input_clears() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = client_in(&app);
        app.set_client_issue_link(created.id, Some("https://example.invalid/1"))
            .expect("set");

        app.set_client_issue_link(created.id, None).expect("clear");
        assert_eq!(
            app.current_client(created.id).expect("client").issue_link,
            None
        );
        assert!(stored_json(&created).get("issue_link").is_none());

        app.set_client_issue_link(created.id, Some("https://example.invalid/2"))
            .expect("set again");
        app.set_client_issue_link(created.id, Some("   "))
            .expect("blank clears");
        assert_eq!(
            app.current_client(created.id).expect("client").issue_link,
            None
        );
    }

    #[test]
    fn a_refused_link_leaves_the_stored_link_untouched() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = client_in(&app);
        app.set_client_issue_link(created.id, Some("https://example.invalid/1"))
            .expect("set");

        let error = app
            .set_client_issue_link(created.id, Some("file:///etc/passwd"))
            .expect_err("refused");
        assert!(matches!(
            error,
            CoreError::IssueLinkInvalid {
                reason: IssueLinkRefusal::UnsupportedScheme
            }
        ));
        assert_eq!(
            app.current_client(created.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/1")
        );
    }

    #[test]
    fn a_link_cannot_be_changed_while_the_instance_root_is_missing() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(
                ephervex_storage::NewClient::new(
                    "Unmounted",
                    "1.20.1",
                    ephervex_model::LoaderSelection::vanilla(),
                )
                .with_storage(ephervex_model::StorageClass::Persistent)
                .with_issue_link("https://example.invalid/1"),
            )
            .expect("create");
        let backup = base.path().join("backup");
        std::fs::rename(&created.root, &backup).expect("unmount");

        for change in [Some("https://example.invalid/2"), None] {
            let error = app
                .set_client_issue_link(created.id, change)
                .expect_err("the metadata cannot take the edit");
            assert!(matches!(
                error,
                CoreError::Storage(ephervex_storage::StorageError::Registry(
                    ephervex_storage::RegistryError::RootMissing { .. }
                ))
            ));
        }

        std::fs::rename(&backup, &created.root).expect("remount");
        app.reconcile().expect("reconcile");
        assert_eq!(
            app.current_client(created.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/1")
        );
    }

    #[test]
    fn a_link_can_be_changed_while_the_instance_is_running() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = client_in(&app);
        let _lock = ephervex_storage::LaunchLock::acquire(&created.root).expect("acquire lock");

        let committed = app
            .set_client_issue_link(created.id, Some("https://example.invalid/1"))
            .expect("a running copy does not block the link");
        assert_eq!(
            committed.record.issue_link.as_deref(),
            Some("https://example.invalid/1")
        );
    }

    #[test]
    fn a_duplicate_inherits_the_issue_link_of_its_original() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = client_in(&app);
        app.set_client_issue_link(created.id, Some("https://example.invalid/1"))
            .expect("set");

        let copy = app
            .duplicate_client(
                created.id,
                &ephervex_storage::DuplicateClient::default(),
                ephervex_model::AbsentModPolicy::default(),
            )
            .expect("duplicate");

        assert_eq!(
            copy.record.issue_link.as_deref(),
            Some("https://example.invalid/1")
        );
        assert_eq!(
            app.current_client(copy.record.id)
                .expect("copy")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/1")
        );
        assert_eq!(
            stored_json(&copy.record)["issue_link"],
            "https://example.invalid/1"
        );
    }

    #[test]
    fn a_new_instance_records_the_link_it_was_created_with() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let created = app
            .create_client_for_test(
                ephervex_storage::NewClient::new(
                    "With link",
                    "1.20.1",
                    ephervex_model::LoaderSelection::vanilla(),
                )
                .with_issue_link("https://example.invalid/issues/7"),
            )
            .expect("create");

        assert_eq!(
            created.issue_link.as_deref(),
            Some("https://example.invalid/issues/7")
        );
        assert_eq!(
            stored_json(&created)["issue_link"],
            "https://example.invalid/issues/7"
        );
        assert_eq!(
            app_rooted_at(base.path())
                .current_client(created.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/issues/7")
        );
    }

    #[tokio::test]
    async fn a_link_that_cannot_be_stored_is_refused_before_an_instance_is_created() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        // An unknown release would fail later, so reaching this error proves
        // the link was checked before the catalog was asked.
        let error = app
            .create_client_reporting(
                ephervex_storage::NewClient::new(
                    "Bad link",
                    "1.21.f",
                    ephervex_model::LoaderSelection::vanilla(),
                )
                .with_issue_link("https://user:secret@example.invalid/1"),
            )
            .await
            .expect_err("refused");

        assert!(matches!(
            error,
            CoreError::IssueLinkInvalid {
                reason: IssueLinkRefusal::Credentials
            }
        ));
        assert!(!error.to_string().contains("secret"));
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[test]
    fn every_refusal_has_a_message_in_the_english_catalog() {
        for reason in [
            IssueLinkRefusal::TooLong,
            IssueLinkRefusal::InvalidCharacters,
            IssueLinkRefusal::NotAnAddress,
            IssueLinkRefusal::UnsupportedScheme,
            IssueLinkRefusal::MissingHost,
            IssueLinkRefusal::Credentials,
        ] {
            let message = ephervex_i18n::lookup(reason.message_id());
            assert!(
                !message.contains(reason.message_id()),
                "{} has no message",
                reason.message_id()
            );
        }
    }
}
