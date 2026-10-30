//! The dialog that says what turning a mod off takes with it.
//!
//! Four groups that answer four different questions, and the one thing they
//! must never do is read as one. A mod that provably stops loading and a mod
//! Ephervex has simply never asked about are different facts, and a dialog
//! that showed them the same way would turn "I do not know" into "it is fine".

use ephervex_gui::ui::{AppWindow, GuiI18n, ImpactRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn row(name: &str, distance: i32) -> ImpactRow {
    ImpactRow {
        name: name.into(),
        logical_path: format!("mods/{name}").into(),
        distance,
    }
}

fn rows(entries: &[ImpactRow]) -> ModelRc<ImpactRow> {
    ModelRc::new(VecModel::from(entries.to_vec()))
}

/// The dialog as it opens for a library three other mods need.
fn disabling_a_library(ui: &AppWindow) {
    ui.set_mod_impact_kind("client".into());
    ui.set_mod_impact_host("an-id".into());
    ui.set_mod_impact_path("mods/LibX.jar".into());
    ui.set_mod_impact_name("LibX".into());
    ui.set_mod_impact_breaks(rows(&[
        row("Skyblock Builder", 1),
        row("Sky GUIs", 1),
        row("UtilitiX", 1),
    ]));
    ui.set_mod_impact_cascade(true);
    ui.set_modal("mod-impact".into());
}

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_element_id(ui, "ImpactList::Text").any(|element| {
        element
            .accessible_label()
            .is_some_and(|label| label == text)
    }) || ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

#[test]
fn every_mod_that_stops_loading_is_named() {
    let ui = window();
    disabling_a_library(&ui);

    let title = lookup_args(
        "gui-mod-impact-disable-title",
        [("value", Arg::from("LibX"))],
    );
    assert!(
        visible(&ui, &title),
        "the dialog has to say which mod is being turned off"
    );
    for name in ["Skyblock Builder", "Sky GUIs", "UtilitiX"] {
        assert!(visible(&ui, name), "{name} is not named in the dialog");
    }
}

#[test]
fn the_cascade_offer_appears_only_when_something_is_proven() {
    let ui = window();
    disabling_a_library(&ui);
    let offer = lookup("gui-mod-impact-cascade-off");
    assert!(
        visible(&ui, &offer),
        "with dependents proven, turning them off too is the offer"
    );

    // The same dialog with nothing proven and only an unasked mod to report.
    ui.set_mod_impact_breaks(rows(&[]));
    ui.set_mod_impact_unknown(rows(&[row("Mystery", 1)]));
    assert!(
        !visible(&ui, &offer),
        "offering to turn off mods nothing is known about would be a guess"
    );
    assert!(
        visible(&ui, &lookup("gui-mod-impact-refresh")),
        "what it offers instead is asking the catalogs"
    );
}

#[test]
fn a_turn_off_with_nothing_proven_broken_is_confirmed_with_turn_off() {
    let ui = window();
    disabling_a_library(&ui);
    ui.set_mod_impact_breaks(rows(&[]));
    ui.set_mod_impact_unknown(rows(&[row("Mystery", 1)]));

    assert!(
        visible(&ui, &lookup("gui-mod-impact-turn-off")),
        "the button says what the user opened the dialog to do"
    );
    assert!(
        !visible(&ui, &lookup("gui-mod-impact-turn-on")),
        "a turn-off dialog never offers to turn the mod on"
    );
}

#[test]
fn a_turn_on_dialog_is_confirmed_with_turn_on() {
    let ui = window();
    disabling_a_library(&ui);
    ui.set_mod_impact_breaks(rows(&[]));
    ui.set_mod_impact_enabling(true);
    ui.set_mod_requires_absent(rows(&[row("Missing", 1)]));

    assert!(visible(&ui, &lookup("gui-mod-impact-turn-on")));
    assert!(!visible(&ui, &lookup("gui-mod-impact-turn-off")));
}

#[test]
fn the_dialog_always_offers_to_stop_asking() {
    let ui = window();
    disabling_a_library(&ui);
    assert!(
        visible(&ui, &lookup("gui-mod-impact-remember")),
        "the decision is recorded where it is made, not only in settings"
    );
}

#[test]
fn a_dependent_reached_through_a_chain_says_so() {
    let ui = window();
    disabling_a_library(&ui);
    ui.set_mod_impact_breaks(rows(&[row("Sky GUIs", 2)]));

    let through = lookup_args(
        "gui-mod-impact-through",
        [
            ("first", Arg::from("Sky GUIs")),
            ("second", Arg::from(i64::from(2))),
        ],
    );
    assert!(
        visible(&ui, &through),
        "a mod nothing obviously touches needs its chain explained"
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
