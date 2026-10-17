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
async fn a_server_lists_an_installed_mod_offline_by_its_catalog_title_and_release() {
    let world = world().await;
    let server = plain_server(&world);

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");

    // The offline listing, with nothing fetched for it.
    let snapshot = world.app.server_mod_snapshot(server).expect("snapshot");
    let root = snapshot
        .mods
        .iter()
        .find(|entry| entry.identity.project_id() == Some("SECONDPROJ"))
        .expect("the installed mod is listed");
    assert_eq!(root.title(), Some("Second Root"));
    assert_eq!(root.installed_version(), Some("SECONDV1"));
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

/// Confirming a mod's place on a server is asked once: a mod already on the
/// server is not asked about again, alone or as another mod's dependency.
#[tokio::test]
async fn a_mod_already_on_the_server_needs_no_second_confirmation() {
    let world = world().await;
    let server = plain_server(&world);
    install(
        &world,
        server,
        &ModInstallRequest::new("UNKPROJ").confirming_server_support(["UNKPROJ"]),
    )
    .await
    .expect("installs once confirmed");

    for project in ["UNKPROJ", "NEEDSUNK"] {
        let request = ModInstallRequest::new(project);
        let planned = plan(&world, server, &request).await.expect("plans");
        assert!(
            planned.undeclared_server_support().is_empty(),
            "{project}: nothing new needs confirming"
        );
        install(&world, server, &request)
            .await
            .unwrap_or_else(|error| panic!("{project}: {error:?}"));
    }
    assert!(installed_on_server(
        &world.server_recipe(server),
        "NEEDSUNK"
    ));
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

#[tokio::test]
async fn a_server_from_a_pack_says_its_mods_are_the_users_only_once_allowed() {
    let world = world().await;
    let made_here = plain_server(&world);
    let mut record = world
        .app
        .registry_for_services()
        .prepare_server(
            NewServer::new("From A Pack", MINECRAFT, neoforge()).with_source(
                ephervex_storage::ImportSource::imported(
                    ephervex_model::ImportSourceKind::ModrinthPack,
                    "report.mrpack",
                ),
            ),
        )
        .expect("prepare");
    record.installation = ServerInstallation::Installed {
        launch: ServerLaunch::Jar {
            path: ServerRelativePath::new("server.jar").expect("launch path"),
        },
    };
    let staging = record.root.with_extension("staging");
    std::fs::create_dir_all(ephervex_storage::metadata::server_game_dir(&staging).join("mods"))
        .expect("mods dir");
    world
        .app
        .registry_for_services()
        .publish_staged_server(&record, &staging)
        .expect("publish");

    let mods_are_the_users = |server| {
        world
            .app
            .host_mod_snapshot(host(server))
            .expect("snapshot")
            .mods_are_the_users
    };
    assert!(mods_are_the_users(made_here));
    assert!(!mods_are_the_users(record.id));

    // The user can let the pack's mods be theirs, as on a client, and the
    // choice is what the server's metadata records from then on.
    world
        .app
        .set_server_allow_pack_mod_version_changes(record.id, true)
        .expect("allow");
    assert!(mods_are_the_users(record.id));
    assert!(
        world
            .app
            .current_server(record.id)
            .expect("record")
            .allow_pack_mod_version_changes
    );
    world
        .app
        .set_server_allow_pack_mod_version_changes(record.id, false)
        .expect("disallow");
    assert!(!mods_are_the_users(record.id));
}

#[tokio::test]
async fn removing_a_servers_mod_takes_the_dependency_nothing_else_needs_with_it() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");
    let target = PathBuf::from("mods/secondproj-SECONDV1.jar");

    let plan = world
        .app
        .plan_mod_removal(host(server), &target, &silent(), &CancellationToken::new())
        .await
        .expect("plans");
    assert_eq!(plan.host, host(server));
    assert_eq!(
        plan.removes
            .iter()
            .map(|file| file.project_id.as_deref().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["SECONDPROJ", "LIBPROJ"]
    );

    world
        .app
        .remove_managed_mod(host(server), &target, &silent(), &CancellationToken::new())
        .await
        .expect("removes");

    assert!(world.server_jars(server).is_empty());
    let recipe = world.server_recipe(server);
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "SECONDPROJ")
            .is_none()
    );
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "LIBPROJ")
            .is_none()
    );
    assert!(world.client_jars().is_empty());
}

#[tokio::test]
async fn a_servers_mod_moves_to_an_exact_release_resolved_for_that_server() {
    let world = world().await;
    let server = plain_server(&world);
    install(
        &world,
        server,
        &ModInstallRequest::new("MOVEPROJ").at_version("MOVEV1"),
    )
    .await
    .expect("installs");

    world
        .app
        .change_managed_mod_version(
            host(server),
            std::path::Path::new("mods/moveproj-MOVEV1.jar"),
            &crate::ModVersionChoice::Exact("MOVEV2".to_owned()),
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("changes");

    assert_eq!(
        world.server_jars(server),
        ["moveproj-MOVEV2.jar", "newlib-NEWV2.jar"]
    );
    let recipe = world.server_recipe(server);
    assert!(installed_on_server(&recipe, "MOVEPROJ"));
    assert!(installed_on_server(&recipe, "NEWLIB"));
    assert_eq!(
        recipe
            .managed_project(ModProvider::Modrinth, "NEWLIB")
            .expect("dependency")
            .origin,
        ModOrigin::RequiredDependency
    );
    assert!(world.client_jars().is_empty());
}

#[tokio::test]
async fn a_server_mod_whose_jar_was_put_aside_can_still_be_removed() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");
    let target = PathBuf::from("mods/secondproj-SECONDV1.jar");
    world
        .app
        .delete_mod_files(host(server), std::slice::from_ref(&target))
        .expect("puts the jar aside");

    world
        .app
        .remove_managed_mod(host(server), &target, &silent(), &CancellationToken::new())
        .await
        .expect("removes");

    let recipe = world.server_recipe(server);
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "SECONDPROJ")
            .is_none()
    );
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "LIBPROJ")
            .is_none()
    );
    assert!(world.server_jars(server).is_empty());
}

#[tokio::test]
async fn a_server_version_change_that_adds_an_undeclared_mod_is_refused_until_it_is_installed() {
    let world = world().await;
    let server = plain_server(&world);
    install(
        &world,
        server,
        &ModInstallRequest::new("LATERUNK").at_version("LATERUNKV1"),
    )
    .await
    .expect("installs");
    let target = std::path::Path::new("mods/laterunk-LATERUNKV1.jar");
    let to_v2 = crate::ModVersionChoice::Exact("LATERUNKV2".to_owned());

    let refused = world
        .app
        .change_managed_mod_version(
            host(server),
            target,
            &to_v2,
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("refused");
    assert!(
        matches!(&refused, CoreError::ModChangeServerSupportUnconfirmed { projects } if projects == &["Undeclared Mod"]),
        "{refused:?}"
    );
    assert_eq!(world.server_jars(server), ["laterunk-LATERUNKV1.jar"]);

    install(
        &world,
        server,
        &ModInstallRequest::new("UNKPROJ").confirming_server_support(vec!["UNKPROJ".to_owned()]),
    )
    .await
    .expect("installs once confirmed");
    world
        .app
        .change_managed_mod_version(
            host(server),
            target,
            &to_v2,
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("changes once the mod is on the server");
    assert_eq!(
        world.server_jars(server),
        ["laterunk-LATERUNKV2.jar", "unkproj-UNKV1.jar"]
    );
}

/// Installs a mod on the linked client.
async fn install_on_client(world: &World, request: &ModInstallRequest) {
    world
        .app
        .install_mods(
            InstalledHost::Instance(world.client),
            request,
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("installs on the client");
}

#[tokio::test]
async fn a_dependency_a_linked_server_records_only_for_its_client_is_installed_on_the_server() {
    let world = world().await;
    install_on_client(&world, &ModInstallRequest::new("CLIENTLIB")).await;
    let server = world.link_server("Linked Server", MINECRAFT);
    let recipe = world.server_recipe(server);
    assert!(
        !recipe
            .managed_project(ModProvider::Modrinth, "CLIENTLIB")
            .expect("copied from the client")
            .is_installed_on(PackSide::Server, &OptionalSelection::none())
    );
    let request = ModInstallRequest::new("ROOTPROJ").at_version("ROOTV1");

    let planned = plan(&world, server, &request).await.expect("plans");
    assert!(
        planned.already_installed.is_empty(),
        "a record the server does not take puts no jar on it: {:?}",
        planned.already_installed
    );
    assert!(
        planned
            .to_install()
            .iter()
            .any(|planned| planned.version.project_id == "CLIENTLIB")
    );

    install(&world, server, &request).await.expect("installs");
    assert_eq!(
        world.server_jars(server),
        ["clientlib-CLIENTLIBV1.jar", "rootproj-ROOTV1.jar"]
    );
    assert!(installed_on_server(
        &world.server_recipe(server),
        "CLIENTLIB"
    ));
}

#[tokio::test]
async fn a_linked_server_version_change_needing_a_mod_only_its_client_has_is_refused_until_installed()
 {
    let world = world().await;
    install_on_client(&world, &ModInstallRequest::new("UNKPROJ")).await;
    let server = world.link_server("Linked Server", MINECRAFT);
    install(
        &world,
        server,
        &ModInstallRequest::new("LATERUNK").at_version("LATERUNKV1"),
    )
    .await
    .expect("installs");
    let target = std::path::Path::new("mods/laterunk-LATERUNKV1.jar");
    let to_v2 = crate::ModVersionChoice::Exact("LATERUNKV2".to_owned());

    let refused = world
        .app
        .change_managed_mod_version(
            host(server),
            target,
            &to_v2,
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the server does not have the client's copy");
    assert!(
        matches!(&refused, CoreError::ModChangeServerSupportUnconfirmed { projects } if projects == &["Undeclared Mod"]),
        "{refused:?}"
    );
    assert_eq!(world.server_jars(server), ["laterunk-LATERUNKV1.jar"]);

    install(
        &world,
        server,
        &ModInstallRequest::new("UNKPROJ").confirming_server_support(vec!["UNKPROJ".to_owned()]),
    )
    .await
    .expect("installs once confirmed");
    world
        .app
        .change_managed_mod_version(
            host(server),
            target,
            &to_v2,
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("changes once the mod is on the server");
    assert_eq!(
        world.server_jars(server),
        ["laterunk-LATERUNKV2.jar", "unkproj-UNKV1.jar"]
    );
}

/// The CurseForge ids a rewritten entry is given.
const OTHER_PROJECT: &str = "238222";
const OTHER_FILE: &str = "5000001";

/// Rewrites a host's Modrinth entry for `project` as one installed from
/// CurseForge, as though the same jar had come from there.
///
/// `file_name` moves the jar and its record to another name, and `bytes`
/// replaces the recorded digests, so the record no longer describes the
/// Modrinth release's file.
fn as_installed_from_curseforge(
    game_dir: &std::path::Path,
    store: &SourceStore,
    project: &str,
    file_name: Option<&str>,
    bytes: Option<&[u8]>,
) {
    let mut recipe = store.read_recipe().expect("recipe");
    let entry = recipe
        .files
        .iter_mut()
        .find(|file| {
            file.identity
                .as_ref()
                .and_then(|identity| identity.project_id.as_deref())
                == Some(project)
        })
        .expect("recorded");
    entry.identity = Some(ephervex_pack::ProviderFileIdentity {
        provider: Some(ModProvider::CurseForge),
        project_id: Some(OTHER_PROJECT.to_owned()),
        version_id: Some(OTHER_FILE.to_owned()),
        file_id: Some(OTHER_FILE.to_owned()),
    });
    entry.downloads.clear();
    entry.relations = Some(Vec::new());
    if let Some(name) = file_name {
        let renamed = PathBuf::from("mods").join(name);
        std::fs::rename(game_dir.join(&entry.logical_path), game_dir.join(&renamed))
            .expect("rename");
        entry.logical_path = renamed;
    }
    if let Some(bytes) = bytes {
        use sha1::Digest as _;
        let digest = hex::encode(sha1::Sha1::digest(bytes));
        entry.hashes = vec![
            ephervex_model::Hash::new(ephervex_model::HashAlgorithm::Sha1, &digest)
                .expect("a SHA-1"),
        ];
    }
    store.write_recipe(&recipe).expect("write");
}

fn server_store(world: &World, server: ServerId) -> SourceStore {
    SourceStore::new(ephervex_storage::metadata::source_dir(
        &world.server_root(server),
    ))
}

fn server_game_dir(world: &World, server: ServerId) -> PathBuf {
    ephervex_storage::metadata::server_game_dir(&world.server_root(server))
}

#[tokio::test]
async fn a_dependency_installed_from_the_other_catalog_as_the_same_file_is_reused() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("LIBPROJ"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "LIBPROJ",
        None,
        None,
    );

    let installed = install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("the library that is there is the one required");

    assert!(
        installed
            .plan
            .already_installed
            .iter()
            .any(|present| present.project_id == "LIBPROJ")
    );
    assert_eq!(
        installed.installation.already_present,
        [PathBuf::from("mods/libproj-LIBV1.jar")]
    );
    assert_eq!(
        world.server_jars(server),
        ["libproj-LIBV1.jar", "secondproj-SECONDV1.jar"]
    );
    let recipe = world.server_recipe(server);
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "LIBPROJ")
            .is_none(),
        "the library stays recorded as the other catalog's"
    );
    assert!(
        recipe
            .managed_project(ModProvider::CurseForge, OTHER_PROJECT)
            .is_some()
    );
}

#[tokio::test]
async fn the_same_file_from_the_other_catalog_under_another_name_is_not_installed_twice() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("LIBPROJ"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "LIBPROJ",
        Some("LibProj-curseforge.jar"),
        None,
    );

    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");

    assert_eq!(
        world.server_jars(server),
        ["LibProj-curseforge.jar", "secondproj-SECONDV1.jar"]
    );
}

#[tokio::test]
async fn a_client_reuses_the_same_file_installed_from_the_other_catalog() {
    let world = world().await;
    install_on_client(&world, &ModInstallRequest::new("LIBPROJ")).await;
    let root = world.app.client(world.client).expect("client").root;
    as_installed_from_curseforge(
        &ephervex_storage::metadata::client_game_dir(&root),
        &SourceStore::new(ephervex_storage::metadata::source_dir(&root)),
        "LIBPROJ",
        None,
        None,
    );

    install_on_client(&world, &ModInstallRequest::new("SECONDPROJ")).await;

    assert_eq!(
        world.client_jars(),
        ["libproj-LIBV1.jar", "secondproj-SECONDV1.jar"]
    );
}

#[tokio::test]
async fn a_different_file_the_other_catalog_installed_in_the_way_is_named_as_its_mod() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("LIBPROJ"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "LIBPROJ",
        None,
        Some(b"another build"),
    );

    let refused = install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect_err("a different file is in the way");

    assert!(
        matches!(
            &refused,
            CoreError::Pack(ephervex_pack::PackError::DestinationRecorded {
                provider: Some(ModProvider::CurseForge),
                ..
            })
        ),
        "{refused:?}"
    );
    assert_eq!(world.server_jars(server), ["libproj-LIBV1.jar"]);
}

#[tokio::test]
async fn a_dependency_another_catalogs_mod_may_need_is_kept_when_its_dependent_goes() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("SECONDPROJ"))
        .await
        .expect("installs");
    install(&world, server, &ModInstallRequest::new("MOVEPROJ"))
        .await
        .expect("installs");
    // A mod from the other catalog that needs a project its catalog has no
    // entry for here: the library above may be that project's file.
    let store = server_store(&world, server);
    let mut recipe = store.read_recipe().expect("recipe");
    let dependent = recipe
        .files
        .iter_mut()
        .find(|file| {
            file.identity
                .as_ref()
                .and_then(|identity| identity.project_id.as_deref())
                == Some("MOVEPROJ")
        })
        .expect("recorded");
    dependent.identity = Some(ephervex_pack::ProviderFileIdentity {
        provider: Some(ModProvider::CurseForge),
        project_id: Some("410811".to_owned()),
        version_id: Some("5000002".to_owned()),
        file_id: Some("5000002".to_owned()),
    });
    dependent.relations = Some(vec![ephervex_pack::RecipeRelation {
        kind: ephervex_model::ModRelationKind::Required,
        target: ephervex_pack::RelationTarget::Project {
            project_id: OTHER_PROJECT.to_owned(),
            version_id: None,
        },
    }]);
    store.write_recipe(&recipe).expect("write");
    let target = PathBuf::from("mods/secondproj-SECONDV1.jar");

    let plan = world
        .app
        .plan_mod_removal(host(server), &target, &silent(), &CancellationToken::new())
        .await
        .expect("plans");

    assert_eq!(
        plan.removes
            .iter()
            .map(|file| file.project_id.as_deref().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["SECONDPROJ"]
    );
    assert!(plan.orphans_unprovable);
}

#[tokio::test]
async fn a_dependency_that_is_another_release_of_the_other_catalogs_mod_is_refused() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("DUPLIB"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "DUPLIB",
        Some("DupLib-curseforge.jar"),
        Some(b"the other catalog's release"),
    );

    let refused = install(&world, server, &ModInstallRequest::new("DUPROOT"))
        .await
        .expect_err("a second copy of the library is refused");

    assert!(
        matches!(
            &refused,
            CoreError::ModDependencyAlreadyProvided {
                project_id,
                mod_id,
                path,
                ..
            } if project_id == "DUPLIB"
                && mod_id == "duplib"
                && path.ends_with("mods/DupLib-curseforge.jar")
        ),
        "{refused:?}"
    );
    assert_eq!(world.server_jars(server), ["DupLib-curseforge.jar"]);
    assert!(
        world
            .server_recipe(server)
            .managed_project(ModProvider::Modrinth, "DUPROOT")
            .is_none()
    );
}

#[tokio::test]
async fn a_client_refuses_a_dependency_that_is_another_release_of_the_other_catalogs_mod() {
    let world = world().await;
    install_on_client(&world, &ModInstallRequest::new("DUPLIB")).await;
    let root = world.app.client(world.client).expect("client").root;
    as_installed_from_curseforge(
        &ephervex_storage::metadata::client_game_dir(&root),
        &SourceStore::new(ephervex_storage::metadata::source_dir(&root)),
        "DUPLIB",
        Some("DupLib-curseforge.jar"),
        Some(b"the other catalog's release"),
    );

    let refused = world
        .app
        .install_mods(
            InstalledHost::Instance(world.client),
            &ModInstallRequest::new("DUPROOT"),
            &silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("a second copy of the library is refused");

    assert!(
        matches!(
            &refused,
            CoreError::ModDependencyAlreadyProvided { project_id, .. } if project_id == "DUPLIB"
        ),
        "{refused:?}"
    );
    assert_eq!(world.client_jars(), ["DupLib-curseforge.jar"]);
}

#[tokio::test]
async fn the_same_jar_from_the_other_catalog_is_reused_rather_than_refused_as_a_copy() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("DUPLIB"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "DUPLIB",
        Some("DupLib-curseforge.jar"),
        None,
    );

    install(&world, server, &ModInstallRequest::new("DUPROOT"))
        .await
        .expect("the library that is there is the one required");

    assert_eq!(
        world.server_jars(server),
        ["DupLib-curseforge.jar", "duproot-DUPROOTV1.jar"]
    );
}

#[tokio::test]
async fn the_other_catalogs_release_stands_in_for_a_dependency_once_the_request_allows_it() {
    let world = world().await;
    let server = plain_server(&world);
    install(&world, server, &ModInstallRequest::new("DUPLIB"))
        .await
        .expect("installs the library");
    as_installed_from_curseforge(
        &server_game_dir(&world, server),
        &server_store(&world, server),
        "DUPLIB",
        Some("DupLib-curseforge.jar"),
        Some(b"the other catalog's release"),
    );

    let installed = install(
        &world,
        server,
        &ModInstallRequest::new("DUPROOT").letting_installed_stand_in_for(["DUPLIB"]),
    )
    .await
    .expect("the installed release stands in for the library");

    assert_eq!(
        installed.installation.stood_in,
        [ephervex_pack::InstalledStandIn {
            planned: PathBuf::from("mods/duplib-DUPLIBV1.jar"),
            installed: PathBuf::from("mods/DupLib-curseforge.jar"),
            mod_ids: vec!["duplib".to_owned()],
        }]
    );
    assert_eq!(
        world.server_jars(server),
        ["DupLib-curseforge.jar", "duproot-DUPROOTV1.jar"]
    );
    let recipe = world.server_recipe(server);
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "DUPROOT")
            .is_some()
    );
    assert!(
        recipe
            .managed_project(ModProvider::Modrinth, "DUPLIB")
            .is_none(),
        "the release that was not written is not recorded"
    );
}

/// Writes a developer's own build of the duplicated library into `mods/`.
fn place_library_build(game_dir: &std::path::Path) {
    use std::io::Write as _;
    let mut bytes = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    zip.start_file(
        "META-INF/neoforge.mods.toml",
        zip::write::SimpleFileOptions::default(),
    )
    .expect("entry");
    zip.write_all(b"[[mods]]\nmodId=\"duplib\"\nversion=\"dev\"\n")
        .expect("write");
    zip.finish().expect("finish");
    std::fs::create_dir_all(game_dir.join("mods")).expect("mods dir");
    std::fs::write(game_dir.join("mods/duplib-dev.jar"), bytes).expect("write");
}

#[tokio::test]
async fn a_hand_placed_build_stands_in_for_a_dependency_only_after_it_is_allowed_to() {
    let world = world().await;
    let root = world.app.client(world.client).expect("client").root;
    place_library_build(&ephervex_storage::metadata::client_game_dir(&root));
    let install_root = |request: ModInstallRequest| {
        let world = &world;
        async move {
            world
                .app
                .install_mods(
                    InstalledHost::Instance(world.client),
                    &request,
                    &silent(),
                    &CancellationToken::new(),
                )
                .await
        }
    };

    let refused = install_root(ModInstallRequest::new("DUPROOT"))
        .await
        .expect_err("the build is not used unasked");
    assert!(
        matches!(
            &refused,
            CoreError::ModDependencyAlreadyProvided { project_id, path, .. }
                if project_id == "DUPLIB" && path.ends_with("mods/duplib-dev.jar")
        ),
        "{refused:?}"
    );
    assert_eq!(world.client_jars(), ["duplib-dev.jar"]);

    let installed =
        install_root(ModInstallRequest::new("DUPROOT").letting_installed_stand_in_for(["DUPLIB"]))
            .await
            .expect("the build stands in for the library");

    assert_eq!(
        installed
            .installation
            .stood_in
            .iter()
            .map(|stand_in| stand_in.installed.clone())
            .collect::<Vec<_>>(),
        [PathBuf::from("mods/duplib-dev.jar")]
    );
    assert_eq!(
        world.client_jars(),
        ["duplib-dev.jar", "duproot-DUPROOTV1.jar"]
    );
}

#[tokio::test]
async fn nothing_stands_in_for_the_mod_asked_for() {
    let world = world().await;
    let server = plain_server(&world);
    place_library_build(&server_game_dir(&world, server));

    let refused = install(
        &world,
        server,
        &ModInstallRequest::new("DUPLIB").letting_installed_stand_in_for(["DUPLIB"]),
    )
    .await
    .expect_err("the library asked for is refused as a second copy");

    assert!(
        matches!(
            &refused,
            CoreError::Pack(ephervex_pack::PackError::DuplicateModId { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(world.server_jars(server), ["duplib-dev.jar"]);
}
