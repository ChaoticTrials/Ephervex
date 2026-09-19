//! What an instance's mods need from each other.
//!
//! A recipe entry records the relations its catalog declared about that file
//! (ADR 0034). Installing a mod and importing a CurseForge pack record them as
//! they go, because both already hold the catalog's answer. Everything else
//! does not: a Modrinth pack index declares no relations at all, and an
//! instance built before relations were recorded has none.
//!
//! This module is how those instances catch up. It is an explicit, user-invoked
//! refresh and never part of reading a mod list, which has to stay a directory
//! read that reaches no network.

use ephervex_download::OperationReporter;
use ephervex_model::ModProvider;
use ephervex_pack::{ModIdentity, PackRecipe, SourceStore};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::CoreResult;
use crate::mods::{ModHost, recipe_relations};

/// What a dependency refresh learned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyRefreshReport {
    /// Mods whose recorded relations changed.
    pub recorded: usize,
    /// Mods whose catalog answer matched what was already recorded.
    pub unchanged: usize,
    /// Mods with no exact catalog release to ask about, so nothing could be
    /// asked and nothing is claimed.
    pub unidentified: usize,
    /// Catalogs that could not be reached. Their mods keep whatever they had,
    /// which for most of them is nothing recorded at all.
    pub providers_unavailable: Vec<ModProvider>,
}

impl DependencyRefreshReport {
    /// Whether anything about the host's recorded state changed.
    #[must_use]
    pub const fn changed_anything(&self) -> bool {
        self.recorded > 0
    }

    /// Whether every mod that could be asked about was answered.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.providers_unavailable.is_empty()
    }
}

impl App {
    /// Asks each catalog what this host's mods need, and records the answers.
    ///
    /// Explicit on purpose. It reaches the network, so it is something a user
    /// asks for rather than something a mod list does on its own.
    ///
    /// A catalog that cannot be reached is reported and costs only its own
    /// mods' answers: the rest are still recorded, and the ones that were not
    /// stay unasked rather than being written as needing nothing. A mod with no
    /// exact catalog release is counted and skipped - there is no version to
    /// ask about, and asking about the project would answer for a release the
    /// instance does not have.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn refresh_mod_dependencies(
        &self,
        host: ModHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<DependencyRefreshReport> {
        let layout = self.mod_host_layout(host)?;
        let mut report = DependencyRefreshReport::default();
        // A host with no recipe records nothing and has nothing to refresh.
        let Some(mut recipe) = layout.recipe.clone() else {
            return Ok(report);
        };

        report.unidentified = recipe
            .files
            .iter()
            .filter(|file| file.is_mod_file())
            .filter(|file| {
                !matches!(
                    file.identity.as_ref().map_or(
                        ModIdentity::Unresolved,
                        ephervex_pack::ProviderFileIdentity::resolution
                    ),
                    ModIdentity::Version { .. }
                )
            })
            .count();

        for catalog in [ModProvider::Modrinth, ModProvider::CurseForge] {
            let versions = exact_versions(&recipe, catalog);
            if versions.is_empty() {
                continue;
            }
            let provider = match self.mod_provider(catalog) {
                Ok(provider) => provider,
                Err(error) => {
                    warn!(%error, provider = %catalog, "no client for this catalog");
                    report.providers_unavailable.push(catalog);
                    continue;
                }
            };
            let answers = match provider.versions_by_id(&versions, reporter, cancel).await {
                Ok(answers) => answers,
                Err(error) => {
                    warn!(%error, provider = %catalog, "dependencies could not be fetched");
                    report.providers_unavailable.push(catalog);
                    continue;
                }
            };

            for answer in answers {
                let relations = recipe_relations(&answer.dependencies);
                for file in &mut recipe.files {
                    if file
                        .identity
                        .as_ref()
                        .and_then(|identity| identity.version_id.as_deref())
                        != Some(answer.id.as_str())
                    {
                        continue;
                    }
                    if file.relations.as_ref() == Some(&relations) {
                        report.unchanged += 1;
                    } else {
                        file.relations = Some(relations.clone());
                        report.recorded += 1;
                    }
                }
            }
        }

        if report.changed_anything() {
            SourceStore::new(ephervex_storage::metadata::source_dir(&layout.root))
                .write_recipe(&recipe)?;
        }
        info!(
            recorded = report.recorded,
            unchanged = report.unchanged,
            unidentified = report.unidentified,
            "refreshed what this host's mods require"
        );
        Ok(report)
    }
}

/// The exact catalog releases one catalog can be asked about.
///
/// Mods only, and only those whose recorded identity names a release. A project
/// without one has nothing to look up: asking about the project would answer
/// for whichever release the catalog considers current, which is not the one
/// installed here.
fn exact_versions(recipe: &PackRecipe, catalog: ModProvider) -> Vec<String> {
    let mut versions: Vec<String> = recipe
        .files
        .iter()
        .filter(|file| file.is_mod_file())
        .filter_map(|file| match file.identity.as_ref()?.resolution() {
            ModIdentity::Version {
                provider,
                version_id,
                ..
            } if provider == catalog => Some(version_id),
            _ => None,
        })
        .collect();
    versions.sort_unstable();
    versions.dedup();
    versions
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::{Path, PathBuf};

    use ephervex_model::{Hash, HashAlgorithm, InstanceId, LoaderKind, LoaderSelection, ModOrigin};
    use ephervex_pack::{
        PackFileRecipe, PackRecipe, ProviderFileIdentity, SideRequirement, SourceStore,
    };
    use ephervex_storage::{NewClient, Paths};
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path as route, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const LIBRARY: &str = "qEH6GYul";
    const LIBRARY_RELEASE: &str = "f5GBMQmv";
    const DEPENDENT: &str = "por2AZc5";
    const DEPENDENT_RELEASE: &str = "Kxvkni7I";

    /// An instance whose recipe records exact releases and no relations, which
    /// is every instance built before relations were recorded.
    fn instance_with_unasked_mods(base: &std::path::Path) -> (App, InstanceId, PathBuf) {
        let app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Dependency Aware Example",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create the instance");

        let mut recipe = PackRecipe::for_manual_instance(
            "Dependency Aware Example",
            "1.21.1",
            record.loader.clone(),
        );
        recipe.files = vec![
            recorded("LibX.jar", LIBRARY, LIBRARY_RELEASE),
            recorded("SkyblockBuilder.jar", DEPENDENT, DEPENDENT_RELEASE),
        ];
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        store.write_recipe(&recipe).expect("write the recipe");
        (app, record.id, record.root)
    }

    fn recorded(name: &str, project: &str, version: &str) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from("mods").join(name),
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            downloads: Vec::new(),
            size: Some(4096),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some(project.to_owned()),
                version_id: Some(version.to_owned()),
                file_id: None,
            }),
            original_identity: None,
            relations: None,
        }
    }

    fn version_json(id: &str, project: &str, requires: &[&str]) -> Value {
        json!({
            "id": id,
            "project_id": project,
            "name": format!("{project} {id}"),
            "version_number": "1.0.0",
            "version_type": "release",
            "game_versions": ["1.21.1"],
            "loaders": ["neoforge"],
            "date_published": "2026-01-01T00:00:00Z",
            "files": [{
                "hashes": {"sha1": "da39a3ee5e6b4b0d3255bfef95601890afd80709"},
                "url": "https://cdn.modrinth.com/data/x/a.jar",
                "filename": "a.jar",
                "primary": true,
                "size": 4096,
            }],
            "dependencies": requires
                .iter()
                .map(|id| json!({"project_id": id, "dependency_type": "required"}))
                .collect::<Vec<_>>(),
        })
    }

    /// One batched answer, which is how the refresh asks: every release the
    /// instance names, in one request per catalog.
    async fn catalog_answering() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(route("/versions"))
            // Sorted and deduplicated by the refresh, so the request is stable
            // whatever order the recipe happens to list its mods in.
            .and(query_param(
                "ids",
                format!("[\"{DEPENDENT_RELEASE}\",\"{LIBRARY_RELEASE}\"]"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![
                version_json(LIBRARY_RELEASE, LIBRARY, &[]),
                version_json(DEPENDENT_RELEASE, DEPENDENT, &[LIBRARY]),
            ]))
            .mount(&server)
            .await;
        server
    }

    fn recipe_at(root: &std::path::Path) -> PackRecipe {
        SourceStore::new(ephervex_storage::metadata::source_dir(root))
            .read_recipe()
            .expect("the retained recipe")
    }

    #[tokio::test]
    async fn a_refresh_records_what_the_catalog_says_each_mod_needs() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        assert!(
            recipe_at(&root)
                .files
                .iter()
                .all(|file| file.recorded_relations().is_none()),
            "the instance starts with nothing asked"
        );

        let report = app
            .refresh_mod_dependencies(
                ModHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert_eq!(report.recorded, 2);
        assert_eq!(report.unidentified, 0);
        assert!(report.is_complete());

        let recipe = recipe_at(&root);
        let dependents = recipe.required_by(LIBRARY);
        assert_eq!(dependents.len(), 1);
        assert_eq!(
            dependents[0].logical_path,
            PathBuf::from("mods/SkyblockBuilder.jar"),
            "the reverse index is what a disable warning reads"
        );
        let library = recipe
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("mods/LibX.jar"))
            .expect("the library");
        assert_eq!(
            library.recorded_relations(),
            Some(&[][..]),
            "the catalog answered that it needs nothing, which is not the same as unasked"
        );
    }

    #[tokio::test]
    async fn refreshing_twice_records_nothing_the_second_time() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, _root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        let reporter = OperationReporter::silent("test-refresh");
        let cancel = CancellationToken::new();
        let first = app
            .refresh_mod_dependencies(ModHost::Instance(instance), &reporter, &cancel)
            .await
            .expect("first");
        assert_eq!(first.recorded, 2);
        let second = app
            .refresh_mod_dependencies(ModHost::Instance(instance), &reporter, &cancel)
            .await
            .expect("second");
        assert_eq!(second.recorded, 0);
        assert_eq!(second.unchanged, 2);
    }

    #[tokio::test]
    async fn a_catalog_that_cannot_answer_leaves_every_mod_unasked() {
        // Recording "needs nothing" because a request failed is the one
        // outcome that turns a missing answer into a confident wrong one.
        let base = tempfile::tempdir().expect("temp dir");
        let unreachable = MockServer::start().await;
        Mock::given(method("GET"))
            .and(route("/versions"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&unreachable)
            .await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&unreachable.uri())
            .expect("loopback catalog");

        let report = app
            .refresh_mod_dependencies(
                ModHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("a refresh reports rather than fails");

        assert_eq!(report.recorded, 0);
        assert!(!report.is_complete());
        assert_eq!(report.providers_unavailable, vec![ModProvider::Modrinth]);
        assert!(
            recipe_at(&root)
                .files
                .iter()
                .all(|file| file.recorded_relations().is_none()),
            "an unreachable catalog leaves every mod unasked rather than empty"
        );
    }

    #[tokio::test]
    async fn a_mod_with_no_exact_release_is_counted_and_not_asked_about() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        let mut project_only = recorded("UtilitiX.jar", "MMMWYsjm", "unused");
        project_only.identity.as_mut().expect("identity").version_id = None;
        recipe.files.push(project_only);
        store.write_recipe(&recipe).expect("write");

        let report = app
            .refresh_mod_dependencies(
                ModHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert_eq!(report.unidentified, 1);
        assert_eq!(
            report.recorded, 2,
            "the two that name a release are recorded"
        );
    }
}
