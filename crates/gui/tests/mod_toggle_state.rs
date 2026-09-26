//! What a mod row's switch shows after the list behind it changes.
//!
//! A cascade turns on mods the user never touched, so the list has to be able
//! to tell them so. A switch that kept whatever it was last clicked to would
//! report the wrong thing about a different mod entirely, and reopening the tab
//! would silently correct it - which is worse than being wrong, because it
//! makes the wrong answer look like a refresh problem.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ModRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use std::rc::Rc;

use slint::{ComponentHandle, Image, Model, ModelRc, PhysicalSize, SharedString, VecModel};

fn mod_row(title: &str, enabled: bool) -> ModRow {
    ModRow {
        project_id: title.into(),
        logical_path: format!("mods/{title}.jar").into(),
        title: title.into(),
        file_name: format!("{title}.jar").into(),
        version: "1.0.0".into(),
        version_known: true,
        provider: "Modrinth".into(),
        provider_kind: "modrinth".into(),
        origin: "Added".into(),
        origin_kind: "user_selected".into(),
        compatibility: SharedString::default(),
        author: SharedString::default(),
        icon: Image::default(),
        enabled,
        deleted: false,
        pack_owned: false,
        can_manage: true,
        can_change_version: false,
        can_update: false,
        can_remove: true,
        can_delete_file: false,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

/// Opens the mods tab with a list, the way the controller keeps it.
///
/// The rows are written into the model already there rather than replacing it,
/// because that is what the controller does: handing a repeater a new model
/// tears down every row component, and the list has to survive a snapshot
/// arriving for unrelated reasons.
fn show(ui: &AppWindow, rows: &[ModRow]) -> Rc<VecModel<ModRow>> {
    support::show_client(
        ui,
        InstanceRow {
            id: "an-id".into(),
            ..InstanceRow::default()
        },
        "Mods",
    );
    let model = Rc::new(VecModel::from(rows.to_vec()));
    support::update_host(ui, 0, |view| view.mods = ModelRc::from(model.clone()));
    model
}

/// A later read of the same list, written into the model in place.
fn update(model: &Rc<VecModel<ModRow>>, rows: &[ModRow]) {
    for (index, row) in rows.iter().enumerate() {
        if model.row_data(index).as_ref() != Some(row) {
            model.set_row_data(index, row.clone());
        }
    }
}

/// The switch belonging to one mod, by the label it carries.
fn switch(ui: &AppWindow, title: &str) -> ElementHandle {
    let label = lookup_args("gui-mod-loads", [("value", Arg::from(title))]);
    ElementHandle::find_by_accessible_label(ui, &label)
        .next()
        .unwrap_or_else(|| panic!("no switch for {title}"))
}

fn is_on(ui: &AppWindow, title: &str) -> bool {
    switch(ui, title)
        .accessible_checked()
        .expect("a switch says whether it is on")
}

#[test]
fn a_mod_turned_on_by_a_cascade_shows_as_on() {
    // Turn the library off, which takes everything with it, then turn one mod
    // back on, which brings the library back. The library's switch has been
    // clicked once and must still follow what the list says afterwards.
    let ui = window();
    let model = show(
        &ui,
        &[
            mod_row("LibX", true),
            mod_row("Skyblock Builder", true),
            mod_row("Sky GUIs", true),
            mod_row("UtilitiX", true),
        ],
    );

    switch(&ui, "LibX").mock_single_click(PointerEventButton::Left);
    update(
        &model,
        &[
            mod_row("LibX", false),
            mod_row("Skyblock Builder", false),
            mod_row("Sky GUIs", false),
            mod_row("UtilitiX", false),
        ],
    );
    assert!(!is_on(&ui, "LibX"), "the cascade turned everything off");

    switch(&ui, "Skyblock Builder").mock_single_click(PointerEventButton::Left);
    update(
        &model,
        &[
            mod_row("LibX", true),
            mod_row("Skyblock Builder", true),
            mod_row("Sky GUIs", false),
            mod_row("UtilitiX", false),
        ],
    );

    assert!(
        is_on(&ui, "LibX"),
        "the cascade turned LibX back on, so its row has to say so without \
         needing the tab reopened"
    );
    assert!(is_on(&ui, "Skyblock Builder"));
    assert!(!is_on(&ui, "Sky GUIs"));
    assert!(!is_on(&ui, "UtilitiX"));
}

#[test]
fn a_switch_follows_the_list_after_any_click() {
    // The same in reverse, and with the clicked row itself: a refusal leaves
    // the mod on, and the switch has to go back to on rather than keep the
    // position the click put it in.
    let ui = window();
    let model = show(&ui, &[mod_row("LibX", true), mod_row("UtilitiX", true)]);

    switch(&ui, "LibX").mock_single_click(PointerEventButton::Left);
    // Nothing changed on disk, and the backend says so.
    update(&model, &[mod_row("LibX", true), mod_row("UtilitiX", true)]);

    assert!(
        is_on(&ui, "LibX"),
        "a click the launcher did not act on leaves the switch where the list says"
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
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}
