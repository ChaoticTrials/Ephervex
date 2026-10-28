//! The servers the host list shows: the combined search and the linked-servers
//! setting applied to every server's row, favorites first.
//!
//! Filtering happens here, in the window's own thread, over the rows the
//! newest snapshot described. It never asks the worker.

use super::{reconciled, server_is_globally_visible, stamp_open_hosts};
use crate::ui::{AppWindow, ServerRow};
use ephervex_core::SearchQuery;

/// What the window's controls currently ask of the server list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ServerFilter {
    /// The search text shared with the instance list.
    pub query: String,
    /// Whether servers linked to a client are listed on their own.
    pub show_linked_servers: bool,
}

#[derive(Default)]
pub(super) struct ServerListState {
    rows: Vec<ServerRow>,
}

/// The rows `filter` lets through, favorites first and otherwise in the order given.
pub(super) fn visible_servers(rows: &[ServerRow], filter: &ServerFilter) -> Vec<ServerRow> {
    let query = SearchQuery::new(&filter.query);
    let mut visible: Vec<ServerRow> = rows
        .iter()
        .filter(|row| server_is_globally_visible(row, filter.show_linked_servers))
        .filter(|row| {
            query.matches_any([
                row.name.as_str(),
                row.minecraft.as_str(),
                row.loader.as_str(),
                row.relationship.as_str(),
                row.address.as_str(),
            ])
        })
        .cloned()
        .collect();
    // Stable, so the servers keep the snapshot's order within each group.
    visible.sort_by_key(|row| !row.favorite);
    visible
}

impl ServerListState {
    /// Takes every server's row from the newest snapshot and shows the list.
    pub(super) fn replace(&mut self, ui: &AppWindow, rows: Vec<ServerRow>) {
        self.rows = rows;
        self.render(ui);
    }

    fn filter(ui: &AppWindow) -> ServerFilter {
        ServerFilter {
            query: ui.get_list_query().to_string(),
            show_linked_servers: ui.get_show_linked_servers(),
        }
    }

    /// Shows the servers the controls ask for.
    pub(super) fn render(&self, ui: &AppWindow) {
        let rows = visible_servers(&self.rows, &Self::filter(ui));
        ui.set_listed_servers(reconciled(&ui.get_listed_servers(), rows));
        stamp_open_hosts(ui);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn server(name: &str, linked_client: &str, favorite: bool) -> ServerRow {
        ServerRow {
            id: name.into(),
            name: name.into(),
            minecraft: "1.20.1".into(),
            loader: "Forge".into(),
            relationship: if linked_client.is_empty() {
                "Standalone".into()
            } else {
                "Linked client".into()
            },
            linked_client_id: linked_client.into(),
            address: "localhost:25565".into(),
            favorite,
            ..ServerRow::default()
        }
    }

    fn names(rows: &[ServerRow]) -> Vec<String> {
        rows.iter().map(|row| row.name.to_string()).collect()
    }

    #[test]
    fn favorites_lead_and_the_rest_keep_their_order() {
        let rows = [
            server("a", "", false),
            server("b", "", true),
            server("c", "", false),
            server("d", "", true),
        ];
        let filter = ServerFilter::default();
        assert_eq!(
            names(&visible_servers(&rows, &filter)),
            ["b", "d", "a", "c"]
        );
    }

    #[test]
    fn linked_servers_are_listed_only_when_the_setting_shows_them() {
        let rows = [
            server("solo", "", false),
            server("linked", "client-1", false),
        ];
        let hidden = ServerFilter::default();
        assert_eq!(names(&visible_servers(&rows, &hidden)), ["solo"]);
        let shown = ServerFilter {
            show_linked_servers: true,
            ..ServerFilter::default()
        };
        assert_eq!(names(&visible_servers(&rows, &shown)), ["solo", "linked"]);
    }

    #[test]
    fn the_search_reads_name_version_loader_relationship_and_address() {
        let mut other = server("Other", "", false);
        other.minecraft = "1.8.9".into();
        other.loader = "Vanilla".into();
        other.address = "192.168.0.4:25570".into();
        let rows = [server("Creative", "", false), other];
        let search = |query: &str| {
            names(&visible_servers(
                &rows,
                &ServerFilter {
                    query: query.to_owned(),
                    show_linked_servers: true,
                },
            ))
        };
        assert_eq!(search("CREATIVE"), ["Creative"]);
        assert_eq!(search("1.8"), ["Other"]);
        assert_eq!(search("vanilla"), ["Other"]);
        assert_eq!(search("standalone"), ["Creative", "Other"]);
        assert_eq!(search("192.168"), ["Other"]);
        assert!(search("nothing like this").is_empty());
        assert_eq!(search("   "), ["Creative", "Other"]);
    }
}
