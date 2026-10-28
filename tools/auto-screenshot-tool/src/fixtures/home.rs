//! The Home page: what is running, what needs attention, the hosts to continue
//! with, recent activity, and the right column's counts and cache meter.

use ephervex_gui::ui::{AppWindow, AttentionRow, InstanceRow, ResumeRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_attribute_args};

use super::{in_progress, rows};

/// The greeting Home shows for an afternoon, named for the default account.
fn greeting(name: &str) -> String {
    lookup_attribute_args(
        "gui-home-greeting-afternoon",
        "named",
        [("name", Arg::from(name))],
    )
    .unwrap_or_else(|| lookup("gui-home-greeting-afternoon"))
}

fn resume_client(row: &InstanceRow) -> ResumeRow {
    ResumeRow {
        kind: "client".into(),
        id: row.id.clone(),
        host_name: row.name.clone(),
        last_run: row.last_run.clone(),
        state_line: row.state_line.clone(),
        minecraft: row.minecraft.clone(),
        loader: row.loader.clone(),
        loader_kind: row.loader_kind.clone(),
        state_kind: row.state_kind.clone(),
        needs_attention: row.needs_attention,
        can_launch: row.can_launch,
    }
}

fn resume_server(row: &ServerRow) -> ResumeRow {
    ResumeRow {
        kind: "server".into(),
        id: row.id.clone(),
        host_name: row.name.clone(),
        last_run: row.last_run.clone(),
        state_line: row.state_line.clone(),
        minecraft: row.minecraft.clone(),
        loader: row.loader.clone(),
        loader_kind: row.loader_kind.clone(),
        state_kind: row.state_kind.clone(),
        needs_attention: row.needs_attention,
        can_launch: row.can_start,
    }
}

/// The Home data every capture carries: the greeting, the counts beside the
/// list, the hosts to continue with, and the shared cache meter. What is
/// running, what needs attention, and activity are left to the states that
/// show them.
pub(crate) fn install_home_base(ui: &AppWindow, instances: &[InstanceRow], servers: &[ServerRow]) {
    ui.set_home_greeting(greeting("MelanX").into());
    let count = |kind: &str| {
        i32::try_from(
            instances
                .iter()
                .filter(|row| row.storage_kind == kind)
                .count(),
        )
        .unwrap_or(i32::MAX)
    };
    ui.set_home_temporary_count(count("temporary"));
    ui.set_home_persistent_count(count("persistent"));
    ui.set_home_server_count(i32::try_from(servers.len()).unwrap_or(i32::MAX));
    let mut resume: Vec<ResumeRow> = instances
        .iter()
        .filter(|row| row.state_kind == "ready" && !row.needs_attention)
        .take(3)
        .map(resume_client)
        .collect();
    resume.extend(
        servers
            .iter()
            .filter(|row| row.state_kind == "stopped")
            .take(1)
            .map(resume_server),
    );
    ui.set_home_resume(rows(resume));
    ui.set_artifact_cache_usage_known(true);
    ui.set_artifact_cache_used("4.2 GiB".into());
    ui.set_artifact_cache_total("20 GiB".into());
    ui.set_artifact_cache_fill(0.21);
}

/// Home with running hosts, a host that needs a fix, and an import under way.
pub(crate) fn home_page(ui: &AppWindow) {
    in_progress::running_page(ui);
    in_progress::activity_import_in_progress(ui);
    ui.set_home_summary(
        format!(
            "{} | {}",
            ephervex_i18n::lookup_args("gui-home-summary-running", [("count", Arg::Count(2))]),
            ephervex_i18n::lookup_args("gui-home-summary-problems", [("count", Arg::Count(1))]),
        )
        .into(),
    );
    ui.set_home_attention(rows(vec![AttentionRow {
        kind: "client".into(),
        id: "fog".into(),
        host_name: "the horrors in the fog".into(),
        title: lookup("gui-attention-missing-root").into(),
        surface: "Diagnostics".into(),
        ..AttentionRow::default()
    }]));
}
