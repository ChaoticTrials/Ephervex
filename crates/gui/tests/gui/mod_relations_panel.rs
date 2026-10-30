//! What a mod's own page says about its place among the others.
//!
//! The warning dialogs are derived from these edges, so a reader who wants to
//! check why disabling one library took three mods with it has to be able to
//! open each of them and see the relation that said so.

use ephervex_gui::ui::{AppWindow, GuiI18n, RelationRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn row(name: &str, kind: &str, state: &str) -> RelationRow {
    RelationRow {
        name: name.into(),
        detail: String::new().into(),
        kind: lookup(kind).into(),
        state: if state.is_empty() {
            String::new().into()
        } else {
            lookup(state).into()
        },
        attention: !state.is_empty(),
    }
}

fn rows(entries: &[RelationRow]) -> ModelRc<RelationRow> {
    ModelRc::new(VecModel::from(entries.to_vec()))
}

/// A declaration the catalog has since named, with the id it resolves against.
fn named_row(title: &str, project_id: &str, state: &str) -> RelationRow {
    RelationRow {
        detail: project_id.into(),
        ..row(title, "gui-mod-relation-required", state)
    }
}

/// The page of a mod that needs a library and is needed by another mod.
fn managing_a_mod(ui: &AppWindow) {
    ui.set_mod_action_path("mods/SkyblockBuilder.jar".into());
    ui.set_mod_action_name("Skyblock Builder".into());
    ui.set_mod_action_requires(rows(&[row("LibX", "gui-mod-relation-required", "")]));
    ui.set_mod_action_required_by(rows(&[row(
        "Sky GUIs",
        "gui-mod-relation-required",
        "gui-mod-relation-disabled",
    )]));
    ui.set_modal("mod-manage".into());
}

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_element_id(ui, "RelationList::Text").any(|element| {
        element
            .accessible_label()
            .is_some_and(|label| label == text)
    }) || ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

#[test]
fn both_directions_are_named_with_what_the_relation_said() {
    let ui = window();
    managing_a_mod(&ui);

    assert!(visible(&ui, &lookup("gui-mod-requires")));
    assert!(visible(&ui, &lookup("gui-mod-required-by")));
    assert!(visible(&ui, "LibX"), "what this mod needs is not named");
    assert!(visible(&ui, "Sky GUIs"), "what needs this mod is not named");
    assert!(
        visible(&ui, &lookup("gui-mod-relation-required")),
        "a relation without its kind cannot be told from an optional one"
    );
}

/// A mod whose releases could not be listed, as on a host whose loader
/// cannot load mods, keeps its page together under its name rather than
/// spreading the few lines it has over the dialog's height.
#[test]
fn a_page_without_releases_keeps_its_lines_together() {
    let ui = window();
    managing_a_mod(&ui);
    let notice = lookup("error-mod-loader-cannot-host-mods");
    ui.set_mod_action_notice(notice.clone().into());
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");

    let shown = |text: &str| {
        ElementHandle::find_by_accessible_label(&ui, text)
            .find(|element| element.size().width > 0.0)
            .expect(text)
    };
    let notice = shown(&notice);
    let requires = shown(&lookup("gui-mod-requires"));
    let gap =
        requires.absolute_position().y - (notice.absolute_position().y + notice.size().height);
    assert!(gap < 40.0, "the relations sit {gap}px under the notice");
}

#[test]
fn a_dependent_that_is_turned_off_says_so() {
    let ui = window();
    managing_a_mod(&ui);
    assert!(
        visible(&ui, &lookup("gui-mod-relation-disabled")),
        "a mod that is off is not stopped by this one, and the list must not imply it is"
    );
}

/// The regression this list exists to prevent: a mod declares project ids its
/// catalog does not answer for, and wearing "not installed here" they read as
/// mods the user merely has to go and fetch.
#[test]
fn a_declaration_the_catalog_cannot_resolve_is_not_offered_as_something_to_install() {
    let ui = window();
    managing_a_mod(&ui);
    ui.set_mod_action_requires(rows(&[
        row(
            "200200",
            "gui-mod-relation-required",
            "gui-mod-relation-unknown-project",
        ),
        row(
            "200300",
            "gui-mod-relation-required",
            "gui-mod-relation-no-release",
        ),
    ]));

    assert!(visible(&ui, &lookup("gui-mod-relation-unknown-project")));
    assert!(visible(&ui, &lookup("gui-mod-relation-no-release")));
    assert!(
        !visible(&ui, &lookup("gui-mod-relation-absent")),
        "neither declaration is a mod that installing would supply"
    );
}

#[test]
fn a_resolved_declaration_leads_with_its_name_and_keeps_its_id_beside_it() {
    let ui = window();
    managing_a_mod(&ui);
    ui.set_mod_action_requires(rows(&[named_row(
        "Absent Library",
        "200200",
        "gui-mod-relation-no-release",
    )]));

    assert!(
        visible(&ui, "Absent Library"),
        "a reader scans this list by name, not by id"
    );
    assert!(
        visible(&ui, "200200"),
        "the exact id stays readable, because that is what a bug report quotes"
    );
}

#[test]
fn a_declaration_with_no_name_shows_its_id_once() {
    let ui = window();
    managing_a_mod(&ui);
    ui.set_mod_action_requires(rows(&[row(
        "200300",
        "gui-mod-relation-required",
        "gui-mod-relation-unchecked",
    )]));

    let printed = ElementHandle::find_by_accessible_label(&ui, "200300").count();
    assert_eq!(
        printed, 1,
        "with nothing resolved the id is the name, and a second copy of it says nothing"
    );
}

#[test]
fn an_unasked_mod_offers_the_refresh_rather_than_claiming_it_needs_nothing() {
    let ui = window();
    managing_a_mod(&ui);
    ui.set_mod_action_requires(rows(&[]));
    ui.set_mod_action_requires_note(lookup("gui-mod-requires-unrecorded").into());
    ui.set_mod_action_relations_refreshable(true);

    assert!(visible(&ui, &lookup("gui-mod-requires-unrecorded")));
    assert!(
        visible(&ui, &lookup("gui-mod-impact-refresh")),
        "the note has to carry the action that answers it"
    );

    // The same list, for a mod whose catalog did answer.
    ui.set_mod_action_requires_note(lookup("gui-mod-requires-nothing").into());
    ui.set_mod_action_relations_refreshable(false);
    assert!(
        !visible(&ui, &lookup("gui-mod-impact-refresh")),
        "there is nothing to refresh once the catalog has answered"
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
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
}
