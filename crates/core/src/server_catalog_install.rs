//! Installing catalog mods directly onto a server.
//!
//! Every test runs against the loopback catalog the linked synchronization
//! tests use, so none of them contacts a live service.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ephervex_mod_platform::{ModInstallPlan, PlanWarning};
use ephervex_model::{
    LoaderKind, LoaderSelection, ModOrigin, ModProvider, ServerId, ServerInstallation,
    ServerLaunch, ServerRelativePath,
};
use ephervex_pack::{OptionalSelection, PackRecipe, PackSide, SideRequirement, SourceStore};
use ephervex_storage::NewServer;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use crate::error::CoreError;
use crate::linked_mods::tests::{
    LOADER_BUILD, MINECRAFT, OTHER_MINECRAFT, World, jars_in, silent, world,
};
use crate::mods::{InstalledHost, InstalledMods, ModInstallRequest};
use crate::verify::RepairRefusal;

fn neoforge() -> LoaderSelection {
    LoaderSelection::exact(LoaderKind::NeoForge, LOADER_BUILD)
}

/// A server created on its own: installed, stopped, linked to nothing.
fn standalone_server(
    world: &World,
    name: &str,
    loader: LoaderSelection,
    installation: Option<ServerInstallation>,
) -> ServerId {
    let mut record = world
        .app
        .registry_for_services()
        .prepare_server(NewServer::new(name, MINECRAFT, loader))
        .expect("prepare");
    record.installation = installation.unwrap_or(ServerInstallation::Installed {
        launch: ServerLaunch::Jar {
            path: ServerRelativePath::new("server.jar").expect("launch path"),
        },
    });
    let staging = record.root.with_extension("staging");
    std::fs::create_dir_all(ephervex_storage::metadata::server_game_dir(&staging).join("mods"))
        .expect("mods dir");
    world
        .app
        .registry_for_services()
        .publish_staged_server(&record, &staging)
        .expect("publish");
    record.id
}

fn plain_server(world: &World) -> ServerId {
    standalone_server(world, "Plain Server", neoforge(), None)
}

fn host(server: ServerId) -> InstalledHost {
    InstalledHost::Server(server)
}

async fn plan(
    world: &World,
    server: ServerId,
    request: &ModInstallRequest,
) -> Result<ModInstallPlan, CoreError> {
    world.app.plan_mod_installation(host(server), request).await
}

async fn install(
    world: &World,
    server: ServerId,
    request: &ModInstallRequest,
) -> Result<InstalledMods, CoreError> {
    world
        .app
        .install_mods(host(server), request, &silent(), &CancellationToken::new())
        .await
}

fn client_recipe_bytes(world: &World) -> Vec<u8> {
    let root = world.app.client(world.client).expect("client").root;
    std::fs::read(SourceStore::new(ephervex_storage::metadata::source_dir(&root)).recipe_path())
        .expect("the client's recipe")
}

fn has_recipe(world: &World, server: ServerId) -> bool {
    SourceStore::new(ephervex_storage::metadata::source_dir(
        &world.server_root(server),
    ))
    .has_recipe()
}

fn installed_on_server(recipe: &PackRecipe, project: &str) -> bool {
    recipe
        .managed_project(ModProvider::Modrinth, project)
        .expect("recorded")
        .is_installed_on(PackSide::Server, &OptionalSelection::none())
}

#[tokio::test]
async fn a_server_with_no_linked_client_and_no_recipe_installs_a_mod_and_its_dependency() {
    let world = world().await;
    let server = plain_server(&world);
    assert!(!has_recipe(&world, server));

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");

    assert_eq!(
        world.server_jars(server),
        ["libproj-LIBV1.jar", "secondproj-SECONDV1.jar"]
    );
    let recipe = world.server_recipe(server);
    let root = recipe
        .managed_project(ModProvider::Modrinth, "SECONDPROJ")
        .expect("root");
    assert_eq!(root.origin, ModOrigin::UserSelected);
    let library = recipe
        .managed_project(ModProvider::Modrinth, "LIBPROJ")
        .expect("dependency");
    assert_eq!(library.origin, ModOrigin::RequiredDependency);
    assert!(installed_on_server(&recipe, "SECONDPROJ"));
    assert!(installed_on_server(&recipe, "LIBPROJ"));
    assert!(world.client_jars().is_empty());
}

#[tokio::test]
async fn installing_on_a_linked_server_leaves_its_client_untouched() {
    let world = world().await;
    let server = world.link_server("Linked Server", MINECRAFT);
    let recipe_before = client_recipe_bytes(&world);
    let jars_before = world.client_jars();

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");

    assert!(!world.server_jars(server).is_empty());
    assert_eq!(client_recipe_bytes(&world), recipe_before);
    assert_eq!(world.client_jars(), jars_before);
    assert!(
        world
            .app
            .managed_mods(world.client)
            .expect("client mods")
            .iter()
            .all(|managed| managed.project_id != "SECONDPROJ")
    );
}

#[tokio::test]
async fn the_default_release_is_the_newest_that_fits_the_servers_own_minecraft_version() {
    let world = world().await;
    let server = world.link_server("Newer Server", OTHER_MINECRAFT);

    let for_server = plan(&world, server, &ModInstallRequest::new("ROOTPROJ"))
        .await
        .expect("plans");
    let for_client = world
        .app
        .plan_mod_installation(world.client, &ModInstallRequest::new("ROOTPROJ"))
        .await
        .expect("plans");

    assert_eq!(for_server.root.version.id, "ROOTV3");
    assert_eq!(for_client.root.version.id, "ROOTV2");
}

#[tokio::test]
async fn a_dependency_the_server_already_records_is_resolved_against_the_servers_own_set() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("LIBPROJ"))
        .await
        .expect("installs the library");

    let planned = plan(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("plans");

    assert!(
        planned
            .already_installed
            .iter()
            .any(|installed| installed.project_id == "LIBPROJ"),
        "the library the server holds is not planned again"
    );
}

#[tokio::test]
async fn a_mod_with_no_stated_server_side_needs_confirmation_and_is_recorded_as_chosen() {
    let world = world().await;
    let server = plain_server(&world);
    let request = ModInstallRequest::new("UNKPROJ");

    let planned = plan(&world, server, &request).await.expect("plans");
    let unconfirmed: Vec<&str> = planned
        .undeclared_server_support()
        .iter()
        .map(|planned| planned.project.id.as_str())
        .collect();
    assert_eq!(unconfirmed, ["UNKPROJ"]);

    let refused = install(&world, server, &request)
        .await
        .expect_err("refused");
    assert!(
        matches!(&refused, CoreError::ModServerSupportUnconfirmed { projects } if projects == &["Undeclared Mod"]),
        "{refused:?}"
    );
    assert!(world.server_jars(server).is_empty());
    assert!(!has_recipe(&world, server));

    install(
        &world,
        server,
        &request.clone().confirming_server_support(["UNKPROJ"]),
    )
    .await
    .expect("installs once confirmed");
    let recipe = world.server_recipe(server);
    let entry = recipe
        .managed_project(ModProvider::Modrinth, "UNKPROJ")
        .expect("recorded");
    assert_eq!(entry.server, SideRequirement::Unknown);
    assert_eq!(entry.selected, Some(true));
    assert!(installed_on_server(&recipe, "UNKPROJ"));
}

#[tokio::test]
async fn a_dependency_with_no_stated_server_side_is_reported_and_needs_confirmation() {
    let world = world().await;
    let server = plain_server(&world);
    let request = ModInstallRequest::new("NEEDSUNK");

    let planned = plan(&world, server, &request).await.expect("plans");
    let unconfirmed: Vec<&str> = planned
        .undeclared_server_support()
        .iter()
        .map(|planned| planned.project.id.as_str())
        .collect();
    assert_eq!(unconfirmed, ["UNKPROJ"]);

    let refused = install(&world, server, &request)
        .await
        .expect_err("refused");
    assert!(matches!(
        refused,
        CoreError::ModServerSupportUnconfirmed { .. }
    ));
    assert!(world.server_jars(server).is_empty());
}

#[tokio::test]
async fn a_client_only_required_dependency_is_installed_with_a_warning_and_counts_as_installed() {
    let world = world().await;
    let server = plain_server(&world);
    let request = ModInstallRequest::new("ROOTPROJ");

    let planned = plan(&world, server, &request).await.expect("plans");
    assert!(planned.warnings.iter().any(|warning| matches!(
        warning,
        PlanWarning::ServerSideUnsupported { project_id, required_by }
            if project_id == "CLIENTLIB" && required_by == &["ROOTPROJ"]
    )));

    install(&world, server, &request).await.expect("installs");

    assert!(
        world
            .server_jars(server)
            .contains(&"clientlib-CLIENTLIBV1.jar".to_owned())
    );
    let recipe = world.server_recipe(server);
    let library = recipe
        .managed_project(ModProvider::Modrinth, "CLIENTLIB")
        .expect("recorded");
    assert_eq!(
        library.server,
        SideRequirement::Unsupported,
        "the catalog's declaration is kept"
    );
    assert_eq!(
        library.installed_despite_unsupported,
        Some(PackSide::Server)
    );
    assert!(installed_on_server(&recipe, "CLIENTLIB"));

    let snapshot = world.app.server_mod_snapshot(server).expect("server mods");
    assert!(
        snapshot
            .mods
            .iter()
            .any(|entry| entry.identity.project_id() == Some("CLIENTLIB")
                && entry.presence == crate::ModPresence::Installed),
        "the server's own mod list shows it"
    );
    let report = world
        .app
        .verify_host(host(server), &silent(), &CancellationToken::new())
        .await
        .expect("verifies");
    assert!(
        report.files.len() >= 3 && report.is_sound(),
        "verification covers the jar: {report:?}"
    );
    let problems = world.app.requirement_problems(host(server)).expect("graph");
    assert!(
        problems.unmet.is_empty(),
        "the root's requirement is met by the jar that is there"
    );
}

/// A mod that runs only on a server is exactly what a server install is for,
/// so nothing warns that it does not run on a client.
#[tokio::test]
async fn a_server_only_mod_installs_on_a_server_without_a_client_side_warning() {
    let world = world().await;
    let server = plain_server(&world);

    let installed = install(&world, server, &ModInstallRequest::new("SERVERONLY"))
        .await
        .expect("installs");

    assert!(
        !installed
            .plan
            .warnings
            .iter()
            .any(|warning| matches!(warning, PlanWarning::ClientSideUnsupported { .. })),
        "{:?}",
        installed.plan.warnings
    );
    assert!(
        world
            .server_jars(server)
            .contains(&"serveronly-SERVERONLYV1.jar".to_owned())
    );
}

#[tokio::test]
async fn an_exact_release_that_does_not_fit_the_server_is_refused_not_substituted() {
    let world = world().await;
    let server = plain_server(&world);

    let refused = install(
        &world,
        server,
        &ModInstallRequest::new("ROOTPROJ").at_version("ROOTV3"),
    )
    .await
    .expect_err("ROOTV3 only supports another Minecraft release");

    assert!(matches!(refused, CoreError::ModPlatform(_)), "{refused:?}");
    assert!(world.server_jars(server).is_empty());
    assert!(!has_recipe(&world, server));
}

#[tokio::test]
async fn a_mod_declared_client_only_is_refused_as_a_server_root() {
    let world = world().await;
    let server = plain_server(&world);

    for project in ["CLIENTMOD", "RELDECLPROJ"] {
        let refused = plan(&world, server, &ModInstallRequest::new(project))
            .await
            .expect_err("client-only");
        assert!(
            matches!(refused, CoreError::ModClientOnly { .. }),
            "{project}: {refused:?}"
        );
        let refused = install(&world, server, &ModInstallRequest::new(project))
            .await
            .expect_err("client-only");
        assert!(matches!(refused, CoreError::ModClientOnly { .. }));
    }
    assert!(world.server_jars(server).is_empty());
}

#[tokio::test]
async fn a_vanilla_server_has_no_catalog_to_install_from() {
    let world = world().await;
    let server = standalone_server(&world, "Vanilla Server", LoaderSelection::vanilla(), None);

    let refused = plan(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect_err("no loader");

    assert!(
        matches!(
            refused,
            CoreError::ModPlatform(
                ephervex_mod_platform::ModPlatformError::LoaderCannotHostMods {
                    loader: LoaderKind::Vanilla
                }
            )
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_server_without_its_base_installation_is_refused() {
    let world = world().await;
    let server = standalone_server(
        &world,
        "Unfinished Server",
        neoforge(),
        Some(ServerInstallation::NotInstalled),
    );

    let refused = install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect_err("not installed");

    assert!(matches!(
        refused,
        CoreError::Server(ephervex_server::ServerError::NotInstalled)
    ));
}

#[tokio::test]
async fn a_running_server_receives_the_install_for_its_next_start() {
    let world = world().await;
    let server = world.link_server("Running Server", MINECRAFT);
    world.mark_running(server);

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("a running server is not refused");

    assert!(
        world
            .server_jars(server)
            .contains(&"secondproj-SECONDV1.jar".to_owned())
    );
}

#[tokio::test]
async fn a_server_in_an_isolation_session_refuses_an_install() {
    let world = world().await;
    let server = plain_server(&world);
    world
        .app
        .begin_isolation(
            host(server),
            crate::IsolationRequest {
                pinned: Vec::new(),
                accept_unrecorded: true,
                accept_missing: true,
            },
            &CancellationToken::new(),
        )
        .expect("the server starts a session");

    let refused = install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect_err("refused");

    assert!(matches!(
        refused,
        CoreError::ChangeRefused {
            reason: RepairRefusal::IsolationActive
        }
    ));
    assert!(world.server_jars(server).is_empty());
}

#[tokio::test]
async fn a_failed_install_leaves_the_server_exactly_as_it_was() {
    let world = world().await;
    let server = plain_server(&world);
    let mods: PathBuf =
        ephervex_storage::metadata::server_game_dir(&world.server_root(server)).join("mods");
    std::fs::write(mods.join("libproj-LIBV1.jar"), b"the developer's own build")
        .expect("a hand-placed jar where the dependency would go");

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect_err("the destination is occupied");

    assert_eq!(jars_in(mods.parent().unwrap()), ["libproj-LIBV1.jar"]);
    assert_eq!(
        std::fs::read(mods.join("libproj-LIBV1.jar")).expect("read"),
        b"the developer's own build"
    );
    assert!(!has_recipe(&world, server));
}

#[tokio::test]
async fn a_server_search_leaves_client_only_mods_out_and_a_client_search_keeps_them() {
    let world = world().await;
    let hit = |id: &str, server_side: &str| {
        json!({
            "project_id": id, "slug": id.to_ascii_lowercase(), "title": id,
            "description": "", "author": "a", "client_side": "required",
            "server_side": server_side, "project_type": "mod",
            "categories": ["neoforge"], "versions": [MINECRAFT],
        })
    };
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hits": [hit("ROOTPROJ", "required"), hit("CLIENTMOD", "unsupported")],
            "offset": 0, "limit": 10, "total_hits": 2,
        })))
        .mount(&world.catalog)
        .await;
    let server = plain_server(&world);
    let query = ephervex_mod_platform::ModSearchQuery::new("");
    let ids = |results: ephervex_mod_platform::ModSearchResults| -> Vec<String> {
        results.hits.into_iter().map(|hit| hit.project.id).collect()
    };

    let for_server = world
        .app
        .search_mods(host(server), ModProvider::Modrinth, &query)
        .await
        .expect("searches");
    let for_client = world
        .app
        .search_mods(world.client, ModProvider::Modrinth, &query)
        .await
        .expect("searches");

    assert_eq!(ids(for_server), ["ROOTPROJ"]);
    assert_eq!(ids(for_client), ["ROOTPROJ", "CLIENTMOD"]);
}
