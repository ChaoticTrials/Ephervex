//! What a release row in Manage Mod says about itself.
//!
//! "Installed" and "Newest" answer different questions, and one release often
//! answers both. They live at the end of the row where the eye can compare them
//! down a column, which only works if a badge that is not showing takes no
//! space from the one that is.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, ModVersionRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

/// Being up to date is not a reason to stop saying which release is the newest
/// one, and having the newest is not a reason to stop saying it is installed.
#[test]
fn a_release_that_is_both_installed_and_newest_says_both() {
    let ui = window();
    versions(
        &ui,
        &[release("v5", true, true), release("v4", false, false)],
    );

    assert!(
        badge(&ui, "gui-mod-installed-version").is_some(),
        "the row has to keep saying which release is in the instance"
    );
    assert!(
        badge(&ui, "gui-mod-newest-version").is_some(),
        "the row has to keep saying which release is the newest"
    );
}

#[test]
fn installed_sits_to_the_left_of_newest() {
    let ui = window();
    versions(
        &ui,
        &[release("v5", true, true), release("v4", false, false)],
    );

    let installed = badge(&ui, "gui-mod-installed-version").expect("the installed badge");
    let newest = badge(&ui, "gui-mod-newest-version").expect("the newest badge");

    assert!(
        installed.absolute_position().x < newest.absolute_position().x,
        "installed at {}px is not left of newest at {}px",
        installed.absolute_position().x,
        newest.absolute_position().x
    );
}

/// The badge that is showing has to end where the row ends, whichever of the
/// two it is, or the column they are read down is not a column.
#[test]
fn a_lone_badge_still_ends_where_the_row_does() {
    let ui = window();
    versions(
        &ui,
        &[release("v5", false, true), release("v4", true, false)],
    );

    let newest = badge(&ui, "gui-mod-newest-version").expect("the newest badge");
    let installed = badge(&ui, "gui-mod-installed-version").expect("the installed badge");

    assert!(
        (installed.absolute_position().x + installed.size().width
            - (newest.absolute_position().x + newest.size().width))
            .abs()
            < 1.0,
        "two rows' badges end at {}px and {}px",
        installed.absolute_position().x + installed.size().width,
        newest.absolute_position().x + newest.size().width
    );
}

fn versions(ui: &AppWindow, rows: &[ModVersionRow]) {
    ui.set_loading(false);
    ui.set_mod_action_name("Repro Mod".into());
    ui.set_mod_action_can_change(true);
    ui.set_mod_versions(ModelRc::new(VecModel::from(rows.to_vec())));
    ui.set_modal("mod-manage".into());
    settle(ui);
}

fn release(number: &str, installed: bool, newest: bool) -> ModVersionRow {
    ModVersionRow {
        version_id: number.into(),
        name: format!("Repro Mod {number}").into(),
        number: number.into(),
        channel: "release".into(),
        published: "2026-09-01".into(),
        installed,
        newest,
    }
}

/// The pill carrying one badge's text, which is the widest element bearing it.
fn badge(ui: &AppWindow, id: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}

fn install_i18n(ui: &AppWindow) {
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.to_string())),
                ("second", Arg::from(second.to_string())),
            ],
        )
        .into()
    });
}
