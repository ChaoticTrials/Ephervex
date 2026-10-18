//! What a release row in Manage Mod says about itself.
//!
//! "Installed" and "Newest" answer different questions, and one release often
//! answers both. They live at the end of the row where the eye can compare them
//! down a column, which only works if a badge that is not showing takes no
//! space from the one that is.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModVersionRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
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

/// A mod the pack declares carries the most actions - remove, reset to the
/// pack's version, update, change version - and a fifth button for the
/// project page left them no room. The mod's own name leads there instead.
#[test]
fn the_project_page_is_reached_through_the_mods_name_rather_than_a_button() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorded = Rc::clone(&opened);
    ui.on_open_mod_project_page(move |url| recorded.borrow_mut().push(url.to_string()));

    ui.set_mod_action_project_url("https://modrinth.com/mod/repro".into());
    ui.set_mod_action_is_pack_owned(true);
    ui.set_mod_action_pack_original_version("v3".into());
    ui.set_mod_action_can_update(true);
    versions(
        &ui,
        &[release("v5", false, true), release("v4", true, false)],
    );

    let ways_in: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-open-project-page"))
            .filter(|element| element.size().width > 0.0)
            .collect();
    assert_eq!(
        ways_in.len(),
        1,
        "the project page has exactly one way in, not a button as well"
    );

    // Against an action only the footer carries: the dialog's chrome has a
    // Close of its own at the top, which would compare the header to itself.
    let change = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-change-version"))
        .find(|element| element.size().width > 0.0)
        .expect("the dialog offers Change Version");
    assert!(
        ways_in[0].absolute_position().y < change.absolute_position().y,
        "the way in belongs in the header, above the row of actions: {}px against {}px",
        ways_in[0].absolute_position().y,
        change.absolute_position().y
    );

    // The press lands on the name and its arrow rather than on the width of
    // the header, which would make the empty space beside the name open a
    // browser and would announce a button far larger than the one shown.
    assert!(
        ways_in[0].size().width < 400.0,
        "the way in spans {}px, which is the header rather than the name",
        ways_in[0].size().width
    );

    ways_in[0].mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        ["https://modrinth.com/mod/repro".to_owned()],
        "pressing the name has to open the project page it names"
    );
}

/// A mod with no catalog identity has no project page, so its name is a name
/// and nothing more.
#[test]
fn a_mod_without_a_project_page_offers_no_way_to_one() {
    let ui = window();
    ui.set_mod_action_project_url("".into());
    versions(&ui, &[release("v4", true, true)]);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-open-project-page"))
            .all(|element| element.size().width == 0.0),
        "nothing offers a project page that does not exist"
    );
}

/// Every action a pack mod can show at once has to sit inside the dialog's
/// padding without covering another, at the default text size and at larger
/// ones.
#[test]
fn the_full_set_of_mod_actions_fits_inside_the_dialogs_padding() {
    let ui = window();
    ui.set_mod_action_can_remove(true);
    ui.set_mod_action_is_pack_owned(true);
    ui.set_mod_action_pack_original_version("v3".into());
    ui.set_mod_action_can_update(true);
    for scale in [1.0_f32, 1.25, 1.5] {
        ui.global::<Theme>().set_text_scale(scale);
        versions(
            &ui,
            &[release("v5", false, true), release("v4", true, false)],
        );

        // The dialog's own Close sits at the end of the header row, so its
        // right edge is where the padding begins.
        let content_edge = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-close"))
            .filter(|element| element.size().width > 0.0)
            .min_by(|left, right| {
                left.absolute_position()
                    .y
                    .total_cmp(&right.absolute_position().y)
            })
            .map(|header| header.absolute_position().x + header.size().width)
            .expect("the dialog's header Close");

        let mut placed = Vec::new();
        for id in [
            "gui-remove",
            "gui-mod-reset-to-pack-version",
            "gui-mod-update",
            "gui-mod-change-version",
        ] {
            let button = ElementHandle::find_by_accessible_label(&ui, &lookup(id))
                .find(|element| element.size().width > 0.0)
                .expect("every action of a pack mod is offered");
            let (left, top) = (button.absolute_position().x, button.absolute_position().y);
            let (right, bottom) = (left + button.size().width, top + button.size().height);
            assert!(
                right <= content_edge + 0.5,
                "{id} ends at {right}px past the padding edge at {content_edge}px at text scale {scale}"
            );
            for (other, other_left, other_top, other_right, other_bottom) in &placed {
                assert!(
                    right <= *other_left
                        || *other_right <= left
                        || bottom <= *other_top
                        || *other_bottom <= top,
                    "{id} overlaps {other} at text scale {scale}"
                );
            }
            placed.push((id, left, top, right, bottom));
        }
    }
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
        restricted: false,
    }
}

/// The same release, with its author's third-party downloads turned off.
fn restricted(number: &str, installed: bool, newest: bool) -> ModVersionRow {
    ModVersionRow {
        restricted: true,
        ..release(number, installed, newest)
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

/// A download restriction is a fact about one release, not a reason to
/// replace the dialog: the list, the badges, the project link, and the action
/// that takes the chosen release all stay exactly where they are.
#[test]
fn a_restricted_release_keeps_the_ordinary_manage_mod_dialog() {
    let ui = window();
    ui.set_mod_action_project_url("https://www.curseforge.com/minecraft/mc-mods/repro".into());
    versions(
        &ui,
        &[
            restricted("7951520", false, true),
            restricted("7951519", true, false),
        ],
    );

    assert!(
        badge(&ui, "gui-mod-browser-download").is_some(),
        "the restriction is said on the release"
    );
    assert!(
        badge(&ui, "gui-mod-installed-version").is_some(),
        "which release the instance runs is still shown"
    );
    assert!(
        badge(&ui, "gui-mod-newest-version").is_some(),
        "which release is the newest is still shown"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-open-project-page"))
            .any(|element| element.size().width > 0.0),
        "the project browser link stays available"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-change-version"))
            .any(|element| element.size().width > 0.0),
        "another release can still be taken"
    );
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
