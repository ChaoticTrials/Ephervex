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

/// How a client relates to linked servers, for filtering the instance list.
///
/// Advisory and derived: every client is in exactly one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkingState {
    /// It has a pair: one of its linked servers is shown beside it.
    Paired,
    /// A server links to it, but it is unpaired.
    LinkedServers,
    /// No server links to it.
    None,
}

impl LinkingState {
    /// A stable identifier for `--json` output and command-line options.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paired => "paired",
            Self::LinkedServers => "servers",
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Paired, Self::LinkedServers, Self::None]
            .into_iter()
            .find(|state| state.as_str() == name)
    }
}

/// What pressing Launch or Start on one side of a pair does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedStart {
    /// Start only the side asked for.
    Alone,
    /// One Launch & Connect run: start or reuse the server, then launch the
    /// client connected to it.
    Connect {
        client: InstanceId,
        server: ServerId,
    },
}

/// Decides [`LinkedStart`] for a fresh start of `initiator`.
///
/// A start that goes both ways is one operation, so nothing it starts is ever
/// a new trigger. A running client is never launched again: the server then
/// starts alone. Restart never comes here.
#[must_use]
fn linked_start(
    initiator: InstalledHost,
    pair: Option<LinkedPair>,
    client_running: bool,
) -> LinkedStart {
    match pair {
        Some(pair) if pair.start_together && pair.includes(initiator) && !client_running => {
            LinkedStart::Connect {
                client: pair.client,
                server: pair.server,
            }
        }
        _ => LinkedStart::Alone,
    }
}

impl App {
    /// What a fresh start of `initiator` should do, from its current pair and
    /// whether the pair's client is running.
    pub fn plan_linked_start(&self, initiator: InstalledHost) -> CoreResult<LinkedStart> {
        let pair = self.linked_pair_of(initiator)?;
        let client_running = match pair {
            Some(pair) => self.launch_state(pair.client)?.is_some(),
            None => false,
        };
        Ok(linked_start(initiator, pair, client_running))
    }

    /// Every client's pair. A client's first linked server is its pair unless
    /// the user paired another one or unpaired it; a recorded server that is
    /// gone or no longer linked falls back to that first one.
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

    /// The [`LinkingState`] of every client.
    ///
    /// A pair that names a server no longer linked to its client counts as
    /// not paired, because pairs are resolved against the live links.
    pub fn linking_states(&self) -> CoreResult<HashMap<InstanceId, LinkingState>> {
        let links = self.server_links()?;
        let mut states: HashMap<InstanceId, LinkingState> = self
            .list_clients()?
            .into_iter()
            .map(|client| (client.id, LinkingState::None))
            .collect();
        for (client, servers) in &links {
            let paired = resolve(
                *client,
                servers,
                self.config().linked_pairs.get(&client.to_string()),
            )
            .is_some();
            states.insert(
                *client,
                if paired {
                    LinkingState::Paired
                } else {
                    LinkingState::LinkedServers
                },
            );
        }
        Ok(states)
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

    fn pair(start_together: bool) -> LinkedPair {
        LinkedPair {
            client: InstanceId::new(),
            server: ServerId::new(),
            start_together,
        }
    }

    #[test]
    fn either_side_of_a_pair_that_starts_together_starts_both_as_one_operation() {
        let pair = pair(true);
        let both = LinkedStart::Connect {
            client: pair.client,
            server: pair.server,
        };
        assert_eq!(
            linked_start(InstalledHost::Instance(pair.client), Some(pair), false),
            both
        );
        assert_eq!(
            linked_start(InstalledHost::Server(pair.server), Some(pair), false),
            both
        );
    }

    #[test]
    fn a_running_client_is_never_launched_again() {
        let pair = pair(true);
        for initiator in [
            InstalledHost::Server(pair.server),
            InstalledHost::Instance(pair.client),
        ] {
            assert_eq!(
                linked_start(initiator, Some(pair), true),
                LinkedStart::Alone
            );
        }
    }

    #[test]
    fn only_a_paired_host_that_starts_together_starts_its_counterpart() {
        let separate = pair(false);
        let together = pair(true);
        let cases = [
            (InstalledHost::Instance(separate.client), Some(separate)),
            (InstalledHost::Server(separate.server), Some(separate)),
            (InstalledHost::Instance(InstanceId::new()), Some(together)),
            (InstalledHost::Server(ServerId::new()), Some(together)),
            (InstalledHost::Instance(together.client), None),
        ];
        for (initiator, pair) in cases {
            assert_eq!(linked_start(initiator, pair, false), LinkedStart::Alone);
        }
    }

    #[test]
    fn the_planned_start_reads_the_current_pair() {
        let mut world = world();
        let client = world.client("Client");
        let server = world.linked_server("Server", client);
        let host = InstalledHost::Server(server);
        assert_eq!(
            world.app.plan_linked_start(host).expect("plan"),
            LinkedStart::Alone
        );

        world
            .app
            .set_linked_pair(client, server, true)
            .expect("pair");
        assert_eq!(
            world.app.plan_linked_start(host).expect("plan"),
            LinkedStart::Connect { client, server }
        );

        world
            .app
            .set_linked_pair(client, server, false)
            .expect("pair");
        assert_eq!(
            world.app.plan_linked_start(host).expect("plan"),
            LinkedStart::Alone
        );
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
