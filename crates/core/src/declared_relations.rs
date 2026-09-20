//! What a declared relation names, when no jar of the host matches it.
//!
//! A recipe entry records the relations its catalog declared (ADR 0034), and
//! most of them name a project id. [`App::relations_of`] matches those against
//! the host's own files and leaves the rest as the bare id the catalog stated,
//! because it is offline and a project it has never seen is not something it
//! can describe.
//!
//! A bare id describes nothing. "Not installed here" beside one reads as an
//! invitation to install it, which is wrong for a project the catalog does not
//! know and wrong again for one with no release for this host's Minecraft
//! version and loader. Telling those apart needs the catalog.
//!
//! This is that half of the question, and it is deliberately its own call: it
//! reaches the network, so a frontend asks for it after the offline answer is
//! already on screen. Nothing it learns is written down (ADR 0037).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_mod_platform::{
    InstanceTarget, ModPlatformError, ModProviderApi, loader_name_for, newest_compatible,
};
use ephervex_model::ModProvider;
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::app::App;
use crate::error::CoreResult;
use crate::mod_display_cache;
use crate::mods::InstalledHost;

/// Whether a declared project can be installed into this host at all.
///
/// Four answers rather than a boolean, because "no" has three different
/// reasons and only one of them is the user's to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAvailability {
    /// The catalog knows the project and has a release for this host's exact
    /// Minecraft version and loader. The one case where installing it is the
    /// answer.
    Installable { version_id: String },
    /// The catalog knows the project and has no release that fits this host.
    NoCompatibleRelease,
    /// The catalog does not know this project id.
    UnknownToCatalog,
    /// Nothing was asked, or the catalog could not be reached. Not a negative
    /// answer, and never to be shown as one.
    NotChecked,
}

impl ProjectAvailability {
    /// Whether offering to install this project would be honest.
    #[must_use]
    pub const fn is_installable(&self) -> bool {
        matches!(self, Self::Installable { .. })
    }
}

/// One declared project the host has no jar for, as its catalog describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredProject {
    /// The catalog's own title, where it answered with one. A title is what a
    /// reader reads; the project id stays the key, because that is what
    /// anything resolves against.
    pub title: Option<String>,
    pub availability: ProjectAvailability,
}

impl DeclaredProject {
    /// The answer for a project nothing could be asked about.
    const fn unchecked() -> Self {
        Self {
            title: None,
            availability: ProjectAvailability::NotChecked,
        }
    }
}

impl App {
    /// What the catalog says about the projects one mod declares and this host
    /// does not have.
    ///
    /// Keyed by the exact project id the catalog stated, so a caller matches it
    /// against [`crate::RelatedMod::project_id`] rather than by position.
    /// Relations that already match a jar here are left out: the host itself
    /// answers those, and it answers them offline.
    ///
    /// The catalog asked is the declaring mod's own (ADR 0034): a dependency
    /// graph never crosses catalogs, so a relation's project id means nothing
    /// anywhere else. A catalog that cannot be reached, or one this build has
    /// no client for, costs only the verdict: every project it would have
    /// answered for stays [`ProjectAvailability::NotChecked`] rather than
    /// becoming a refusal the caller would have to explain.
    ///
    /// Nothing here is recorded. A resolved title is read from the display
    /// cache where one is already there and is never added to it: that cache's
    /// departure from ADR 0022 covers mods the user has installed, and a
    /// project merely named by a declaration is not one.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn declared_projects_of(
        &self,
        host: InstalledHost,
        logical_path: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<BTreeMap<String, DeclaredProject>> {
        let relations = self.relations_of(host, logical_path)?;
        let wanted: BTreeSet<String> = relations
            .requires
            .iter()
            .filter(|related| related.logical_path.is_none())
            .filter_map(|related| related.project_id.clone())
            .collect();
        if wanted.is_empty() {
            return Ok(BTreeMap::new());
        }

        // The declaring mod's own catalog. Read from the snapshot rather than
        // carried on the relation, because a relation deliberately has no
        // provider of its own.
        let Some(catalog) = self.declaring_catalog(host, logical_path)? else {
            return Ok(unchecked(wanted));
        };
        let provider = match self.mod_provider(catalog) {
            Ok(provider) => provider,
            Err(error) => {
                warn!(%error, provider = %catalog, "no client for this catalog");
                return Ok(unchecked(wanted));
            }
        };

        let mut answers = self.remembered_titles(catalog, &wanted);
        self.ask_for_missing_titles(provider, &wanted, &mut answers, reporter, cancel)
            .await;

        let target = self.host_target(host)?;
        match loader_name_for(provider, &target) {
            Ok(loader_name) => {
                self.check_availability(
                    provider,
                    &target,
                    loader_name,
                    &mut answers,
                    reporter,
                    cancel,
                )
                .await;
            }
            // A host no catalog can supply mods for. Every title already found
            // still stands; no release of anything fits, and saying so as a
            // property of each project would blame the project for it.
            Err(error) => warn!(%error, provider = %catalog, "this host cannot be offered mods"),
        }
        Ok(answers)
    }

    /// The catalog the mod at `logical_path` resolves against.
    fn declaring_catalog(
        &self,
        host: InstalledHost,
        logical_path: &Path,
    ) -> CoreResult<Option<ModProvider>> {
        Ok(self
            .host_mod_snapshot(host)?
            .mods
            .iter()
            .find(|entry| entry.logical_path == logical_path)
            .and_then(|entry| entry.identity.provider()))
    }

    /// The host's exact Minecraft version and loader, whichever kind it is.
    fn host_target(&self, host: InstalledHost) -> CoreResult<InstanceTarget> {
        Ok(match host {
            InstalledHost::Instance(id) => {
                let record = self.client(id)?;
                InstanceTarget::new(record.minecraft_version, record.loader.kind)
            }
            InstalledHost::Server(id) => {
                let record = self.server(id)?;
                InstanceTarget::new(record.minecraft_version, record.loader.kind)
            }
        })
    }

    /// Titles already in the display cache, so a project the user has seen
    /// before needs no request at all.
    fn remembered_titles(
        &self,
        catalog: ModProvider,
        wanted: &BTreeSet<String>,
    ) -> BTreeMap<String, DeclaredProject> {
        let cache = CacheLayout::new(self.paths().cache_dir());
        let remembered = mod_display_cache::read(&cache, catalog.as_str());
        wanted
            .iter()
            .map(|project_id| {
                let title = remembered
                    .compose(project_id, None)
                    .map(|display| display.title);
                (
                    project_id.clone(),
                    DeclaredProject {
                        title,
                        availability: ProjectAvailability::NotChecked,
                    },
                )
            })
            .collect()
    }

    /// Asks the catalog about every project no title was remembered for.
    ///
    /// Batched first, because that is one request for a whole list. A batch
    /// that fails is retried one project at a time rather than abandoned:
    /// CurseForge answers a batch containing one unknown id by failing all of
    /// it, and an unknown id is exactly the case this call exists to name.
    async fn ask_for_missing_titles(
        &self,
        provider: &dyn ModProviderApi,
        wanted: &BTreeSet<String>,
        answers: &mut BTreeMap<String, DeclaredProject>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) {
        let missing: Vec<String> = wanted
            .iter()
            .filter(|project_id| {
                answers
                    .get(*project_id)
                    .is_none_or(|answer| answer.title.is_none())
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }

        match provider.projects_by_id(&missing, reporter, cancel).await {
            Ok(projects) => {
                for project in projects {
                    if let Some(answer) = answers.get_mut(&project.id) {
                        answer.title = Some(project.title);
                    }
                }
                // A catalog answers a batch by omitting the ids it does not
                // know, so what is still untitled is what it has never heard
                // of.
                for project_id in &missing {
                    mark_unknown(answers, project_id);
                }
            }
            Err(error) => {
                warn!(%error, "a batch of declared projects could not be looked up");
                for project_id in &missing {
                    match provider.project(project_id, reporter, cancel).await {
                        Ok(project) => {
                            if let Some(answer) = answers.get_mut(project_id) {
                                answer.title = Some(project.title);
                            }
                        }
                        Err(ModPlatformError::ProjectUnknown { .. }) => {
                            mark_unknown(answers, project_id);
                        }
                        Err(error) => {
                            warn!(%error, project_id, "a declared project could not be looked up");
                        }
                    }
                }
            }
        }
    }

    /// Whether each known project has a release this host could install.
    ///
    /// Only projects the catalog answered for are asked about: there is no
    /// release list to fetch for an id it does not know, and asking would turn
    /// one honest answer into two requests and a second way to fail.
    async fn check_availability(
        &self,
        provider: &dyn ModProviderApi,
        target: &InstanceTarget,
        loader_name: &str,
        answers: &mut BTreeMap<String, DeclaredProject>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) {
        for (project_id, answer) in answers.iter_mut() {
            if answer.title.is_none() {
                continue;
            }
            answer.availability = match provider
                .versions(project_id, target, reporter, cancel)
                .await
            {
                Ok(versions) => newest_compatible(&versions, target, loader_name).map_or(
                    ProjectAvailability::NoCompatibleRelease,
                    |version| ProjectAvailability::Installable {
                        version_id: version.id,
                    },
                ),
                Err(ModPlatformError::ProjectUnknown { .. }) => {
                    ProjectAvailability::UnknownToCatalog
                }
                Err(error) => {
                    warn!(%error, project_id, "a declared project's releases could not be listed");
                    ProjectAvailability::NotChecked
                }
            };
        }
    }
}

/// Every wanted project, with no verdict at all.
fn unchecked(wanted: BTreeSet<String>) -> BTreeMap<String, DeclaredProject> {
    wanted
        .into_iter()
        .map(|project_id| (project_id, DeclaredProject::unchecked()))
        .collect()
}

/// Records that the catalog has no such project, keeping whatever title an
/// earlier answer left behind out of it.
fn mark_unknown(answers: &mut BTreeMap<String, DeclaredProject>, project_id: &str) {
    if let Some(answer) = answers.get_mut(project_id)
        && answer.title.is_none()
    {
        answer.availability = ProjectAvailability::UnknownToCatalog;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::PathBuf;
    use std::sync::Arc;

    use ephervex_auth::{CredentialStore, MemoryStore};
    use ephervex_model::{
        Hash, HashAlgorithm, LoaderKind, LoaderSelection, ModOrigin, ModRelationKind, SecretString,
    };
    use ephervex_pack::{
        PackFileRecipe, PackRecipe, ProviderFileIdentity, RecipeRelation, RelationTarget,
        SideRequirement, SourceStore,
    };
    use ephervex_storage::{NewClient, Paths};
    use wiremock::matchers::{method, path as route};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Obviously not a credential.
    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";
    /// CurseForge's own class id for Minecraft mods.
    const MODS_CLASS_ID: u32 = 6;

    /// The shape this module exists for: one installed mod whose file declares
    /// two projects, and an instance with a jar for neither, so both reach the
    /// catalog and nothing else can answer for them.
    ///
    /// Invented ids. A real project's declared relations are its author's to
    /// change at any time, so a test written around them would describe the
    /// catalog on the day it was written rather than the behavior under test.
    const DECLARING_PROJECT: &str = "100100";
    const DECLARING_FILE: &str = "100101";
    const FIRST_DECLARED: &str = "200200";
    const SECOND_DECLARED: &str = "200300";

    const MINECRAFT: &str = "1.21.1";

    fn store_with_key() -> Arc<dyn CredentialStore> {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect("store the key");
        Arc::new(store)
    }

    /// An instance holding one CurseForge mod whose recorded relations name two
    /// projects the instance does not have.
    fn instance_declaring_two_absent_projects(
        base: &std::path::Path,
        server: &MockServer,
    ) -> (App, InstalledHost, PathBuf) {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Declared Relation Example",
                MINECRAFT,
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create the instance");

        let mut recipe =
            PackRecipe::for_manual_instance("Declared Relation Example", MINECRAFT, record.loader);
        recipe.files = vec![PackFileRecipe {
            logical_path: PathBuf::from("mods").join("DeclaringMod.jar"),
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
                provider: Some(ModProvider::CurseForge),
                project_id: Some(DECLARING_PROJECT.to_owned()),
                version_id: Some(DECLARING_FILE.to_owned()),
                file_id: Some(DECLARING_FILE.to_owned()),
            }),
            original_identity: None,
            relations: Some(
                [FIRST_DECLARED, SECOND_DECLARED]
                    .into_iter()
                    .map(|project_id| RecipeRelation {
                        kind: ModRelationKind::Required,
                        target: RelationTarget::Project {
                            project_id: project_id.to_owned(),
                            version_id: None,
                        },
                    })
                    .collect(),
            ),
        }];
        SourceStore::new(ephervex_storage::metadata::source_dir(&record.root))
            .write_recipe(&recipe)
            .expect("write the recipe");

        let mods = ephervex_storage::metadata::client_game_dir(&record.root).join("mods");
        std::fs::create_dir_all(&mods).expect("mods dir");
        std::fs::write(mods.join("DeclaringMod.jar"), b"jar").expect("write jar");

        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), store_with_key())
            .expect("a loopback client");
        (
            app,
            InstalledHost::Instance(record.id),
            PathBuf::from("mods").join("DeclaringMod.jar"),
        )
    }

    /// The class listing every project lookup reads before it trusts a
    /// document's type.
    async fn serve_project_classes(server: &MockServer) {
        Mock::given(method("GET"))
            .and(route("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[{{"id":{MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}}]}}"#
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// `POST /v1/mods`, answering for exactly the projects named.
    async fn serve_batch(server: &MockServer, projects: &[(&str, &str)]) {
        let data = projects
            .iter()
            .map(|(id, name)| {
                format!(
                    r#"{{"id":{id},"gameId":432,"name":"{name}","slug":"{name}","classId":{MODS_CLASS_ID}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        Mock::given(method("POST"))
            .and(route("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(format!(r#"{{"data":[{data}]}}"#), "application/json"),
            )
            .mount(server)
            .await;
    }

    /// `GET /v1/mods/{id}/files`, with whatever releases the project has for
    /// the instance in hand.
    async fn serve_files(server: &MockServer, project: &str, files: &str) {
        Mock::given(method("GET"))
            .and(route(format!("/v1/mods/{project}/files")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[{files}],"pagination":{{"index":0,"pageSize":50,"resultCount":0,"totalCount":0}}}}"#
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    fn file_document(server: &MockServer, project: &str, file: u32) -> String {
        format!(
            r#"{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"1.0.0","fileName":"library-1.0.0.jar","releaseType":1,
                "fileDate":"2026-01-01T00:00:00Z",
                "gameVersions":["{MINECRAFT}","NeoForge"],
                "fileLength":4096,
                "downloadUrl":"{base}/files/{project}/{file}/library-1.0.0.jar",
                "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}"#,
            base = server.uri(),
        )
    }

    async fn declared(
        app: &App,
        host: InstalledHost,
        path: &Path,
    ) -> BTreeMap<String, DeclaredProject> {
        app.declared_projects_of(
            host,
            path,
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect("the host's own state is readable")
    }

    #[tokio::test]
    async fn a_declared_project_the_catalog_does_not_know_is_not_presented_as_installable() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        // The catalog answers the batch by omitting both ids, which is how it
        // says it has never heard of them.
        serve_batch(&server, &[]).await;
        let (app, host, path) = instance_declaring_two_absent_projects(base.path(), &server);

        let answers = declared(&app, host, &path).await;

        for project_id in [FIRST_DECLARED, SECOND_DECLARED] {
            let answer = answers.get(project_id).expect("every declaration answered");
            assert_eq!(
                answer.availability,
                ProjectAvailability::UnknownToCatalog,
                "{project_id} is not a project this catalog has"
            );
            assert!(
                answer.title.is_none(),
                "nothing may be named for a project that was never answered for"
            );
            assert!(!answer.availability.is_installable());
        }
    }

    #[tokio::test]
    async fn a_declared_project_with_no_release_for_this_instance_is_not_presented_as_installable()
    {
        let base = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_batch(
            &server,
            &[
                (FIRST_DECLARED, "Absent Library"),
                (SECOND_DECLARED, "Absent Extra"),
            ],
        )
        .await;
        serve_files(&server, FIRST_DECLARED, "").await;
        serve_files(&server, SECOND_DECLARED, "").await;
        let (app, host, path) = instance_declaring_two_absent_projects(base.path(), &server);

        let answers = declared(&app, host, &path).await;

        for project_id in [FIRST_DECLARED, SECOND_DECLARED] {
            let answer = answers.get(project_id).expect("every declaration answered");
            assert_eq!(
                answer.availability,
                ProjectAvailability::NoCompatibleRelease,
                "{project_id} has nothing this instance could install"
            );
            assert!(
                answer.title.is_some(),
                "the catalog named it, so a reader gets the name"
            );
            assert!(!answer.availability.is_installable());
        }
    }

    #[tokio::test]
    async fn a_declared_project_with_a_release_that_fits_is_the_one_case_worth_offering() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_batch(
            &server,
            &[
                (FIRST_DECLARED, "Absent Library"),
                (SECOND_DECLARED, "Absent Extra"),
            ],
        )
        .await;
        serve_files(
            &server,
            FIRST_DECLARED,
            &file_document(&server, FIRST_DECLARED, 5001),
        )
        .await;
        serve_files(&server, SECOND_DECLARED, "").await;
        let (app, host, path) = instance_declaring_two_absent_projects(base.path(), &server);

        let answers = declared(&app, host, &path).await;

        assert_eq!(
            answers[FIRST_DECLARED].availability,
            ProjectAvailability::Installable {
                version_id: "5001".to_owned()
            }
        );
        assert!(!answers[SECOND_DECLARED].availability.is_installable());
    }

    #[tokio::test]
    async fn a_catalog_that_cannot_be_reached_leaves_the_declaration_unjudged() {
        let base = tempfile::tempdir().expect("temp dir");
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        // Both the batch and the per-project retry behind it fail, which is an
        // outage rather than an answer.
        Mock::given(method("POST"))
            .and(route("/v1/mods"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        for project_id in [FIRST_DECLARED, SECOND_DECLARED] {
            Mock::given(method("GET"))
                .and(route(format!("/v1/mods/{project_id}")))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
        }
        let (app, host, path) = instance_declaring_two_absent_projects(base.path(), &server);

        let answers = declared(&app, host, &path).await;

        for project_id in [FIRST_DECLARED, SECOND_DECLARED] {
            assert_eq!(
                answers[project_id].availability,
                ProjectAvailability::NotChecked,
                "an unreachable catalog must not be reported as a missing project"
            );
        }
    }
}
