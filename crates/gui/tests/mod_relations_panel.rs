//! What a mod's own page says about its place among the others.
//!
//! The warning dialogs are derived from these edges, so a reader who wants to
//! check why disabling one library took three mods with it has to be able to
//! open each of them and see the relation that said so.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, RelationRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn row(name: &str, kind: &str, state: &str) -> RelationRow {
    RelationRow {
        name: name.into(),
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

/// The page of a mod that needs a library and is needed by another mod.
fn managing_a_mod(ui: &AppWindow) {
    ui.set_selected_instance_id("an-id".into());
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

#[test]
fn a_dependent_that_is_turned_off_says_so() {
    let ui = window();
    managing_a_mod(&ui);
    assert!(
        visible(&ui, &lookup("gui-mod-relation-disabled")),
        "a mod that is off is not stopped by this one, and the list must not imply it is"
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
