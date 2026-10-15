//! Each shown host's mod list, and the catalog browser beside it.
//!
//! Kept per host rather than per pane: two panes on one client's Mods show one
//! list, one filter, and one search. The lists themselves are the backend's; a
//! host that leaves the workspace forgets them and asks again when it returns.

use std::collections::HashMap;

use ephervex_core::InstalledHost;
use ephervex_model::ModProvider;
use slint::SharedString;

use super::workspace::reuse;
use super::{
    BrowserInstallQueue, Workspace, matching_mods, next_mod_search_request_id, request_limit, send,
    to_i32,
};
use crate::backend::{self, Command};
use crate::presentation;
use crate::ui::{HostView, ModRow, ModSearchRow};

/// One catalog search and where it has got to.
#[derive(Debug, Default)]
struct Browser {
    /// The catalog the chips show.
    provider: String,
    /// The text the search on screen was submitted with, which is not what the
    /// field says: the user may keep typing while it answers.
    submitted: String,
    /// The catalog the search on screen was sent to.
    search_provider: String,
    loading: bool,
    /// Whether a search was submitted, so an empty list can say "no results".
    ran: bool,
    notice: String,
    results: Vec<ModSearchRow>,
    /// Where the provider says its next page begins, or -1 once there is none.
    next_offset: i32,
    /// The offset a page is awaited at; a page for any other is stale.
    requested_offset: i32,
    /// The id minted for the request in flight. Instance, catalog, text, and
    /// offset can all repeat between two requests; this cannot.
    expected_request_id: String,
    /// How many rows the fill under way is aiming for.
    target: i32,
    /// Whether results the host cannot take are listed too, marked as such.
    include_incompatible: bool,
}

impl Browser {
    fn on(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            next_offset: -1,
            ..Self::default()
        }
    }

    /// Points the browser at one catalog and retires everything the previous
    /// one could still be answered by, in one step.
    fn switch(&mut self, provider: &str) {
        let reset = presentation::CatalogSwitchReset::for_provider(provider);
        provider.clone_into(&mut self.provider);
        self.search_provider = reset.provider;
        self.submitted = reset.submitted;
        self.expected_request_id = reset.request_id;
        self.loading = reset.loading;
        self.requested_offset = reset.requested_offset;
        self.results.clear();
        self.ran = false;
        self.notice.clear();
        self.next_offset = -1;
    }

    /// Retires the search on screen, whose results were chosen under the other
    /// setting.
    fn reset_search(&mut self) {
        self.search_provider.clear();
        self.expected_request_id.clear();
        self.loading = false;
        self.requested_offset = 0;
        self.results.clear();
        self.next_offset = -1;
    }
}

/// What one host's Mods surface shows.
#[derive(Debug, Default)]
pub(super) struct HostMods {
    /// The whole list, before the filter.
    rows: Vec<ModRow>,
    query: String,
    notice: String,
    mods_are_the_users: bool,
    browser: Browser,
}

/// Every shown host's mods.
#[derive(Debug, Default)]
pub(super) struct ModViews {
    hosts: HashMap<InstalledHost, HostMods>,
}

impl ModViews {
    /// Forgets every host the workspace no longer shows, and starts a client
    /// that just entered on `catalog`.
    pub(super) fn follow(&mut self, shown: &[InstalledHost], catalog: &str) {
        self.hosts.retain(|host, _| shown.contains(host));
        for &host in shown {
            self.hosts.entry(host).or_insert_with(|| HostMods {
                browser: Browser::on(catalog),
                ..HostMods::default()
            });
        }
    }

    pub(super) fn host(&self, host: InstalledHost) -> Option<&HostMods> {
        self.hosts.get(&host)
    }

    /// The title a host's list gives a mod, empty when it lists no such mod.
    pub(super) fn title(&self, host: InstalledHost, path: &str) -> SharedString {
        self.hosts
            .get(&host)
            .and_then(|mods| mods.rows.iter().find(|row| row.logical_path == path))
            .map_or_else(SharedString::default, |row| row.title.clone())
    }

    /// Takes a host's list. The filter stays: it is about the host, and the
    /// list it filters is still that host's.
    pub(super) fn apply_list(
        &mut self,
        host: InstalledHost,
        rows: Vec<ModRow>,
        notice: String,
        mods_are_the_users: bool,
    ) -> bool {
        let Some(mods) = self.hosts.get_mut(&host) else {
            return false;
        };
        mods.rows = rows;
        mods.notice = notice;
        mods.mods_are_the_users = mods_are_the_users;
        mark_installed(mods);
        true
    }

    pub(super) fn filter(&mut self, host: InstalledHost, query: &str) -> bool {
        let Some(mods) = self.hosts.get_mut(&host) else {
            return false;
        };
        query.clone_into(&mut mods.query);
        true
    }

    /// Widens or narrows what the browser lists, and answers whether the change
    /// retired a search that has to be asked again.
    pub(super) fn set_include_incompatible(&mut self, host: InstalledHost, on: bool) -> bool {
        let Some(mods) = self.hosts.get_mut(&host) else {
            return false;
        };
        if mods.browser.include_incompatible == on {
            return false;
        }
        mods.browser.include_incompatible = on;
        let asked = mods.browser.ran;
        if asked {
            mods.browser.reset_search();
        }
        asked
    }

    /// The search that was on screen, asked again after a setting changed.
    pub(super) fn repeat_search(
        &mut self,
        host: InstalledHost,
        counter: &std::cell::Cell<u64>,
        page_size: i32,
    ) -> Option<Command> {
        let browser = &self.hosts.get(&host)?.browser;
        let (provider, query) = (browser.provider.clone(), browser.submitted.clone());
        self.search(host, &provider, &query, counter, page_size)
    }

    pub(super) fn switch_catalog(&mut self, host: InstalledHost, provider: &str) -> bool {
        let Some(mods) = self.hosts.get_mut(&host) else {
            return false;
        };
        mods.browser.switch(provider);
        true
    }

    /// Starts a search, answering the command to send, or nothing when the
    /// host is not shown or no request id can be minted.
    pub(super) fn search(
        &mut self,
        host: InstalledHost,
        provider: &str,
        query: &str,
        counter: &std::cell::Cell<u64>,
        page_size: i32,
    ) -> Option<Command> {
        let catalog = ModProvider::parse(provider)?;
        let mods = self.hosts.get_mut(&host)?;
        // Minted before anything about the browser changes, so an id that
        // could not be allocated leaves it exactly as it was.
        let request_id = next_mod_search_request_id(counter)?;
        let browser = &mut mods.browser;
        query.clone_into(&mut browser.submitted);
        provider.clone_into(&mut browser.search_provider);
        browser.ran = true;
        browser.loading = true;
        browser.notice.clear();
        // A new search replaces the list, so the rows on screen always belong
        // to one question, and it always starts at zero.
        browser.results.clear();
        browser.next_offset = -1;
        browser.requested_offset = 0;
        browser.target = page_size;
        browser.expected_request_id = request_id.to_string();
        Some(Command::SearchMods {
            host,
            provider: catalog,
            query: query.to_owned(),
            offset: 0,
            limit: request_limit(),
            include_incompatible: browser.include_incompatible,
            request_id,
        })
    }

    /// Asks for the page the provider named as the continuation.
    pub(super) fn load_more(
        &mut self,
        host: InstalledHost,
        provider: &str,
        query: &str,
        offset: i32,
        counter: &std::cell::Cell<u64>,
        page_size: i32,
    ) -> Option<Command> {
        let catalog = ModProvider::parse(provider)?;
        let mods = self.hosts.get_mut(&host)?;
        let request_id = next_mod_search_request_id(counter)?;
        let browser = &mut mods.browser;
        browser.loading = true;
        browser.requested_offset = offset;
        // One more page on top of what is shown, not a target long passed.
        browser.target = to_i32(browser.results.len()).saturating_add(page_size);
        browser.expected_request_id = request_id.to_string();
        Some(Command::SearchMods {
            host,
            provider: catalog,
            query: query.to_owned(),
            offset: offset.try_into().unwrap_or(0),
            limit: request_limit(),
            include_incompatible: browser.include_incompatible,
            request_id,
        })
    }

    /// Takes a page of results if it answers the search its host is waiting
    /// on, and answers the request that continues the fill, if one is needed.
    ///
    /// A page is discarded when it is for another catalog, a superseded query,
    /// an offset not awaited, or a request id other than the one minted for
    /// it; see [`presentation::search_page_belongs`].
    pub(super) fn apply_results(
        &mut self,
        results: backend::ModResultsData,
        queue: &BrowserInstallQueue,
        counter: &std::cell::Cell<u64>,
    ) -> Option<Command> {
        let host = results.host;
        let mods = self.hosts.get_mut(&host)?;
        let host_id = host.id();
        let browser = &mut mods.browser;
        let expected_offset = u32::try_from(browser.requested_offset).unwrap_or(0);
        if !presentation::search_page_belongs(
            presentation::BrowserQuestion {
                instance: &host_id,
                provider: &browser.search_provider,
                reference: &browser.submitted,
            },
            presentation::BrowserQuestion {
                instance: &host_id,
                provider: &results.provider,
                reference: &results.query,
            },
            results.offset,
            expected_offset,
            &results.request_id.to_string(),
            &browser.expected_request_id,
        ) {
            return None;
        }
        results.notice.clone_into(&mut browser.notice);
        if !results.notice.is_empty() {
            browser.loading = false;
            return None;
        }
        browser.results.extend(super::mod_search_rows(results.rows));
        // The provider's own continuation, never how many rows are shown: a
        // page can consume results this launcher declined to show.
        browser.next_offset = results.next_offset.map_or(-1, |offset| {
            to_i32(usize::try_from(offset).unwrap_or(usize::MAX))
        });
        mark_installed(mods);
        mark_queued(host, mods, queue);
        let browser = &mut mods.browser;
        let target = usize::try_from(browser.target)
            .unwrap_or(usize::from(ephervex_core::DEFAULT_SEARCH_LIMIT))
            .clamp(1, presentation::MAX_RESULT_PAGE_SIZE.saturating_mul(100));
        // A page size larger than one provider request is filled by asking
        // again, each continuation an ordinary request with its own id.
        if browser.results.len() < target
            && browser.next_offset >= 0
            && let Some(catalog) = ModProvider::parse(&results.provider)
            && let Some(request_id) = next_mod_search_request_id(counter)
        {
            browser.requested_offset = browser.next_offset;
            browser.expected_request_id = request_id.to_string();
            return Some(Command::SearchMods {
                host,
                provider: catalog,
                query: results.query,
                offset: u32::try_from(browser.next_offset).unwrap_or(0),
                limit: request_limit(),
                include_incompatible: browser.include_incompatible,
                request_id,
            });
        }
        browser.loading = false;
        None
    }

    /// Marks every host's results whose install is queued or under way.
    pub(super) fn mark_queued(&mut self, queue: &BrowserInstallQueue) -> Vec<InstalledHost> {
        let mut marked = Vec::new();
        for (&host, mods) in &mut self.hosts {
            mark_queued(host, mods, queue);
            marked.push(host);
        }
        marked
    }
}

/// Marks the results a host already has.
fn mark_installed(mods: &mut HostMods) {
    let managed = mods
        .rows
        .iter()
        .map(|row| row.project_id.clone())
        .filter(|project| !project.is_empty())
        .collect::<std::collections::HashSet<_>>();
    for row in &mut mods.browser.results {
        row.installed = managed.contains(&row.project_id);
    }
}

fn mark_queued(host: InstalledHost, mods: &mut HostMods, queue: &BrowserInstallQueue) {
    let instance = host.id();
    let outstanding = queue
        .outstanding(&instance, &mods.browser.provider)
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    for row in &mut mods.browser.results {
        row.queued = outstanding.contains(row.project_id.as_str());
    }
}

/// Writes one host's mods into its view, in every pane showing it.
pub(super) fn publish(workspace: &Workspace, views: &ModViews, host: InstalledHost) {
    let Some(mods) = views.host(host) else {
        return;
    };
    workspace.update(host, |view| write_view(view, mods));
}

fn write_view(view: &mut HostView, mods: &HostMods) {
    view.mods = reuse(&view.mods, matching_mods(&mods.rows, &mods.query));
    view.mods_query = mods.query.as_str().into();
    view.mods_notice = mods.notice.as_str().into();
    view.mods_are_the_users = mods.mods_are_the_users;
    let browser = &mods.browser;
    view.browser_provider = browser.provider.as_str().into();
    view.browser_submitted = browser.submitted.as_str().into();
    view.browser_search_provider = browser.search_provider.as_str().into();
    view.browser_loading = browser.loading;
    view.browser_ran = browser.ran;
    view.browser_notice = browser.notice.as_str().into();
    view.browser_results = reuse(&view.browser_results, browser.results.clone());
    view.browser_next_offset = browser.next_offset;
    view.browser_include_incompatible = browser.include_incompatible;
}

/// Sends a command when there is one.
pub(super) fn send_if(commands: &std::sync::mpsc::Sender<Command>, command: Option<Command>) {
    if let Some(command) = command {
        send(commands, command);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use std::cell::Cell;

    use ephervex_model::InstanceId;

    use super::*;
    use crate::backend::ModSearchData;

    fn request_id(command: Option<Command>) -> u64 {
        match command.expect("a search is sent") {
            Command::SearchMods { request_id, .. } => request_id,
            _ => panic!("not a search"),
        }
    }

    fn page(
        instance: InstanceId,
        query: &str,
        request_id: u64,
        title: &str,
    ) -> backend::ModResultsData {
        backend::ModResultsData {
            host: InstalledHost::Instance(instance),
            provider: "modrinth".to_owned(),
            query: query.to_owned(),
            offset: 0,
            rows: vec![ModSearchData {
                project_id: title.to_lowercase(),
                title: title.to_owned(),
                description: String::new(),
                author: String::new(),
                icon: String::new(),
                note: String::new(),
                server_support: String::new(),
                blocked_reason: String::new(),
            }],
            total: 1,
            next_offset: None,
            request_id,
            notice: String::new(),
        }
    }

    fn titles(views: &ModViews, host: InstalledHost) -> Vec<String> {
        views
            .host(host)
            .expect("shown")
            .browser
            .results
            .iter()
            .map(|row| row.title.to_string())
            .collect()
    }

    #[test]
    fn a_page_lands_only_in_the_browser_that_asked_for_it() {
        let (first, second) = (InstanceId::new(), InstanceId::new());
        let hosts = [
            InstalledHost::Instance(first),
            InstalledHost::Instance(second),
        ];
        let mut views = ModViews::default();
        views.follow(&hosts, "modrinth");
        let counter = Cell::new(0);
        let queue = BrowserInstallQueue::default();

        let asked = request_id(views.search(hosts[0], "modrinth", "jei", &counter, 10));
        views.search(hosts[1], "modrinth", "jei", &counter, 10);

        // The same question and id, but answered for the other client.
        views.apply_results(page(second, "jei", asked, "Wrong"), &queue, &counter);
        views.apply_results(page(first, "jei", asked, "Jei"), &queue, &counter);

        assert_eq!(titles(&views, hosts[0]), ["Jei"]);
        assert!(titles(&views, hosts[1]).is_empty());
        assert!(
            views.host(hosts[1]).expect("shown").browser.loading,
            "still waiting"
        );
    }

    #[test]
    fn a_newer_search_retires_the_answer_to_an_older_one() {
        let instance = InstanceId::new();
        let host = InstalledHost::Instance(instance);
        let mut views = ModViews::default();
        views.follow(&[host], "modrinth");
        let counter = Cell::new(0);
        let queue = BrowserInstallQueue::default();

        let older = request_id(views.search(host, "modrinth", "jei", &counter, 10));
        let newer = request_id(views.search(host, "modrinth", "jei", &counter, 10));
        views.apply_results(page(instance, "jei", older, "Old"), &queue, &counter);
        assert!(
            titles(&views, host).is_empty(),
            "the same words, an older request"
        );
        views.apply_results(page(instance, "jei", newer, "New"), &queue, &counter);
        assert_eq!(titles(&views, host), ["New"]);
    }

    #[test]
    fn a_host_leaving_the_workspace_forgets_its_list_and_search() {
        let host = InstalledHost::Instance(InstanceId::new());
        let mut views = ModViews::default();
        views.follow(&[host], "curseforge");
        assert_eq!(
            views.host(host).expect("shown").browser.provider,
            "curseforge"
        );
        views.filter(host, "sodium");

        views.follow(&[], "curseforge");
        assert!(views.host(host).is_none());
        assert!(
            !views.filter(host, "late"),
            "a host no pane shows keeps nothing"
        );
    }
}
