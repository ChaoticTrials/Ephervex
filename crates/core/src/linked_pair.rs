//! A client's paired linked server: the one of its linked servers the
//! development workflow shows beside it and can start with it (ADR 0040).

use std::collections::HashMap;

use ephervex_model::{InstanceId, ServerId};
use ephervex_storage::LinkedPairConfig;
use tracing::instrument;

use crate::{App, ConnectError, CoreResult, InstalledHost};

/// A client and the linked server chosen as its pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedPair {
    pub client: InstanceId,
    pub server: ServerId,
    /// Whether starting either side also starts the other.
    pub start_together: bool,
}

impl LinkedPair {
    #[must_use]
    pub fn includes(&self, host: InstalledHost) -> bool {
        self.counterpart(host).is_some()
    }

    /// The other side of the pair from `host`, or `None` when `host` is not in it.
    #[must_use]
    pub fn counterpart(&self, host: InstalledHost) -> Option<InstalledHost> {
        match host {
            InstalledHost::Instance(client) if client == self.client => {
                Some(InstalledHost::Server(self.server))
            }
            InstalledHost::Server(server) if server == self.server => {
                Some(InstalledHost::Instance(self.client))
            }
            _ => None,
        }
    }
}

impl App {
    /// Every recorded pair whose client and server both still exist and are
    /// still linked to each other. Anything else is stale and left out.
    pub fn linked_pairs(&self) -> CoreResult<Vec<LinkedPair>> {
        let links = self.server_links()?;
        Ok(links
            .iter()
            .filter_map(|(client, servers)| {
                resolve(
                    *client,
                    servers,
                    self.config().linked_pairs.get(&client.to_string()),
                )
            })
            .collect())
    }

    /// The pair `host` belongs to, if any.
    pub fn linked_pair_of(&self, host: InstalledHost) -> CoreResult<Option<LinkedPair>> {
        Ok(self
            .linked_pairs()?
            .into_iter()
            .find(|pair| pair.includes(host)))
    }

    /// Pairs `client` with one of its linked servers, replacing any pair it had.
    ///
    /// # Errors
    ///
    /// Fails when either host is unknown or the server is not linked to
    /// `client`. Nothing is written then.
    #[instrument(skip(self))]
    pub fn set_linked_pair(
        &mut self,
        client: InstanceId,
        server: ServerId,
        start_together: bool,
    ) -> CoreResult<LinkedPair> {
        self.client(client)?;
        match self.server(server)?.linked_client_id {
            Some(linked) if linked == client => {}
            Some(linked_client) => {
                return Err(ConnectError::RelationshipMismatch {
                    server,
                    requested_client: client,
                    linked_client,
                }
                .into());
            }
            None => return Err(ConnectError::ServerNotLinked { server }.into()),
        }
        let links = self.server_links()?;
        self.mutate_config(|config| {
            forget_stale(&mut config.linked_pairs, &links);
            config.linked_pairs.insert(
                client.to_string(),
                LinkedPairConfig {
                    server: server.to_string(),
                    start_together,
                    ..LinkedPairConfig::default()
                },
            );
        })?;
        Ok(LinkedPair {
            client,
            server,
            start_together,
        })
    }

    /// Unpairs `client`. Its servers stay linked to it, and its first linked
    /// server is not paired again for as long as it stays the first.
    #[instrument(skip(self))]
    pub fn clear_linked_pair(&mut self, client: InstanceId) -> CoreResult<()> {
        let links = self.server_links()?;
        let first = links
            .get(&client)
            .and_then(|servers| servers.first())
            .map(ToString::to_string);
        self.mutate_config(|config| {
            forget_stale(&mut config.linked_pairs, &links);
            match first {
                Some(first) => {
                    config.linked_pairs.insert(
                        client.to_string(),
                        LinkedPairConfig {
                            unpaired_from: first,
                            ..LinkedPairConfig::default()
                        },
                    );
                }
                None => {
                    config.linked_pairs.remove(&client.to_string());
                }
            }
        })
    }

    /// Each client's linked servers, oldest first, read once for resolving
    /// every pair.
    fn server_links(&self) -> CoreResult<HashMap<InstanceId, Vec<ServerId>>> {
        let clients = self
            .list_clients()?
            .into_iter()
            .map(|client| client.id)
            .collect::<Vec<_>>();
        let mut servers = self
            .list_servers()?
            .into_iter()
            .filter_map(|server| {
                let client = server.linked_client_id?;
                Some((client, server.created_at, server.id))
            })
            .filter(|(client, _, _)| clients.contains(client))
            .collect::<Vec<_>>();
        servers.sort_by_key(|&(_, created_at, id)| (created_at, id.to_string()));
        let mut links: HashMap<InstanceId, Vec<ServerId>> = HashMap::new();
        for (client, _, server) in servers {
            links.entry(client).or_default().push(server);
        }
        Ok(links)
    }
}

/// `client`'s pair: the linked server it recorded, else its first linked
/// server unless that is the one it was unpaired from.
fn resolve(
    client: InstanceId,
    servers: &[ServerId],
    entry: Option<&LinkedPairConfig>,
) -> Option<LinkedPair> {
    let recorded = entry
        .and_then(|entry| entry.server.parse::<ServerId>().ok())
        .filter(|server| servers.contains(server));
    if let Some(server) = recorded {
        return Some(LinkedPair {
            client,
            server,
            start_together: entry.is_some_and(|entry| entry.start_together),
        });
    }
    let first = *servers.first()?;
    let declined = entry.is_some_and(|entry| entry.unpaired_from == first.to_string());
    (!declined).then_some(LinkedPair {
        client,
        server: first,
        start_together: false,
    })
}

/// Drops the entries of clients that are gone or have no linked server left.
fn forget_stale(
    pairs: &mut std::collections::BTreeMap<String, LinkedPairConfig>,
    links: &HashMap<InstanceId, Vec<ServerId>>,
) {
    pairs.retain(|client, _| {
        client
            .parse::<InstanceId>()
            .is_ok_and(|client| links.contains_key(&client))
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use ephervex_model::LoaderSelection;
    use ephervex_storage::{FileDisposition, NewClient, NewServer, Paths};

    use super::*;

    struct World {
        _base: tempfile::TempDir,
        paths: Paths,
        app: App,
    }

    fn world() -> World {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let app = App::open_at(paths.clone()).expect("open");
        World {
            _base: base,
            paths,
            app,
        }
    }

    impl World {
        fn client(&self, name: &str) -> InstanceId {
            self.app
                .create_client_for_test(NewClient::new(name, "1.20.1", LoaderSelection::vanilla()))
                .expect("client")
                .id
        }

        fn linked_server(&self, name: &str, client: InstanceId) -> ServerId {
            self.app
                .registry_for_services()
                .create_server(NewServer::linked(name, client))
                .expect("server")
                .id
        }

        fn reopened(&self) -> App {
            App::open_at(self.paths.clone()).expect("reopen")
        }
    }

    #[test]
    fn a_pair_is_set_found_from_either_side_and_cleared() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);

        let pair = world
            .app
            .set_linked_pair(client, server, true)
            .expect("pair");
        assert_eq!(
            pair.counterpart(InstalledHost::Instance(client)),
            Some(InstalledHost::Server(server))
        );
        assert_eq!(
            pair.counterpart(InstalledHost::Server(server)),
            Some(InstalledHost::Instance(client))
        );
        for host in [
            InstalledHost::Instance(client),
            InstalledHost::Server(server),
        ] {
            assert_eq!(world.app.linked_pair_of(host).expect("pair"), Some(pair));
        }

        world.app.clear_linked_pair(client).expect("clear");
        assert_eq!(
            world
                .app
                .linked_pair_of(InstalledHost::Server(server))
                .expect("pair"),
            None
        );
        assert_eq!(
            world.app.linked_server_ids(client).expect("linked"),
            [server],
            "unpairing leaves the server linked"
        );
    }

    #[test]
    fn a_pair_survives_a_restart_of_the_application() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);
        world
            .app
            .set_linked_pair(client, server, false)
            .expect("pair");

        assert_eq!(
            world.reopened().linked_pairs().expect("pairs"),
            [LinkedPair {
                client,
                server,
                start_together: false
            }]
        );
    }

    #[test]
    fn choosing_another_linked_server_replaces_the_pair() {
        let mut world = world();
        let client = world.client("Client");
        let first = world.linked_server("First", client);
        let second = world.linked_server("Second", client);
        world
            .app
            .set_linked_pair(client, first, false)
            .expect("pair");
        world
            .app
            .set_linked_pair(client, second, true)
            .expect("pair");

        assert_eq!(
            world.app.linked_pairs().expect("pairs"),
            [LinkedPair {
                client,
                server: second,
                start_together: true
            }]
        );
        assert_eq!(
            world
                .app
                .linked_pair_of(InstalledHost::Server(first))
                .expect("pair"),
            None
        );
    }

    #[test]
    fn only_a_server_linked_to_the_client_can_be_its_pair() {
        let mut world = world();
        let client = world.client("Client");
        let other = world.client("Other");
        let others_server = world.linked_server("Other Server", other);
        let standalone = world
            .app
            .registry_for_services()
            .create_server(NewServer::new(
                "Standalone",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("server")
            .id;

        for server in [others_server, standalone] {
            assert!(world.app.set_linked_pair(client, server, true).is_err());
        }
        assert!(world.app.config().linked_pairs.is_empty());
    }

    #[test]
    fn a_deleted_server_leaves_no_pair_behind() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);
        world
            .app
            .set_linked_pair(client, server, true)
            .expect("pair");

        world
            .app
            .delete_server(server, FileDisposition::Remove)
            .expect("delete");

        assert!(world.app.linked_pairs().expect("pairs").is_empty());
        assert_eq!(
            world
                .app
                .linked_pair_of(InstalledHost::Instance(client))
                .expect("pair"),
            None
        );

        let replacement = world.linked_server("Replacement", client);
        world
            .app
            .set_linked_pair(client, replacement, false)
            .expect("pair");
        assert_eq!(
            world.app.config().linked_pairs.len(),
            1,
            "writing a pair drops the stale entry"
        );
    }

    #[test]
    fn a_deleted_client_leaves_no_pair_behind() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);
        world
            .app
            .set_linked_pair(client, server, true)
            .expect("pair");

        world
            .app
            .delete_client(client, FileDisposition::Remove)
            .expect("delete");

        assert!(world.reopened().linked_pairs().expect("pairs").is_empty());
    }

    #[test]
    fn a_server_that_is_no_longer_linked_is_no_longer_paired() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);
        world
            .app
            .set_linked_pair(client, server, true)
            .expect("pair");

        let mut record = world.app.server(server).expect("server");
        record.linked_client_id = None;
        world
            .app
            .registry_for_services()
            .update_server(&mut record)
            .expect("unlink");

        assert!(world.app.linked_pairs().expect("pairs").is_empty());
    }

    #[test]
    fn a_malformed_entry_is_ignored() {
        let mut world = world();
        let client = world.client("Client");
        world
            .app
            .mutate_config(|config| {
                config.linked_pairs.insert(
                    client.to_string(),
                    LinkedPairConfig {
                        server: "not an id".to_owned(),
                        ..LinkedPairConfig::default()
                    },
                );
                config
                    .linked_pairs
                    .insert("not an id".to_owned(), LinkedPairConfig::default());
            })
            .expect("write");

        assert!(world.app.linked_pairs().expect("pairs").is_empty());
    }

    #[test]
    fn a_clients_first_linked_server_is_its_pair_without_being_asked() {
        let world = world();
        let client = world.client("Client");
        let first = world.linked_server("First", client);
        world.linked_server("Second", client);

        assert_eq!(
            world.app.linked_pairs().expect("pairs"),
            [LinkedPair {
                client,
                server: first,
                start_together: false,
            }]
        );
        assert!(
            world.app.config().linked_pairs.is_empty(),
            "nothing is written"
        );
    }

    #[test]
    fn an_unpaired_first_server_stays_unpaired_while_it_is_the_first() {
        let mut world = world();
        let client = world.client("Client");
        let first = world.linked_server("First", client);
        let second = world.linked_server("Second", client);
        world.app.clear_linked_pair(client).expect("unpair");

        world.linked_server("Third", client);
        assert!(world.reopened().linked_pairs().expect("pairs").is_empty());

        world
            .app
            .delete_server(first, FileDisposition::Remove)
            .expect("delete");
        assert_eq!(
            world.app.linked_pairs().expect("pairs"),
            [LinkedPair {
                client,
                server: second,
                start_together: false,
            }],
            "the new first linked server is paired again"
        );
    }

    #[test]
    fn a_chosen_server_that_is_deleted_falls_back_to_the_first() {
        let mut world = world();
        let client = world.client("Client");
        let first = world.linked_server("First", client);
        let second = world.linked_server("Second", client);
        world
            .app
            .set_linked_pair(client, second, true)
            .expect("pair");

        world
            .app
            .delete_server(second, FileDisposition::Remove)
            .expect("delete");

        assert_eq!(
            world.app.linked_pairs().expect("pairs"),
            [LinkedPair {
                client,
                server: first,
                start_together: false,
            }]
        );
    }
}
