//! Dropdowns show the value the application holds and submits.
//!
//! A stock combo box shows whichever entry sits at its own index, whatever the
//! application says the value is. These pin down that the entry on screen is the
//! one the dialogs submit, however the application changes the value or
//! reorders the entries, and that a pick by the user does not stop a dropdown
//! from following later changes.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, MinecraftPreferenceRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn the_create_dialog_shows_the_release_it_will_submit_when_the_entries_are_reordered() {
    let ui = window();
    ui.set_loading(false);
    ui.set_minecraft_releases(strings(&["1.21.1", "1.16.3"]));
    ui.set_create_minecraft("1.16.3".into());
    ui.set_modal("create-instance".into());
    settle(&ui);
    assert_eq!(shown(&ui, &lookup("gui-minecraft-release")), "1.16.3");

    // Starring a release moves it first, so the entry holding the value now
    // sits at a different position.
    ui.set_minecraft_releases(strings(&["1.16.3", "1.21.1", "1.20.1"]));
    settle(&ui);
    assert_eq!(shown(&ui, &lookup("gui-minecraft-release")), "1.16.3");

    ui.set_minecraft_releases(strings(&["1.21.1", "1.20.1", "1.16.3"]));
    settle(&ui);
    assert_eq!(shown(&ui, &lookup("gui-minecraft-release")), "1.16.3");

    ui.set_create_minecraft("1.20.1".into());
    settle(&ui);
    assert_eq!(shown(&ui, &lookup("gui-minecraft-release")), "1.20.1");
}

#[test]
fn dropdowns_in_a_fixed_height_dialog_stay_dropdown_sized_and_fill_their_row() {
    let ui = window();
    ui.set_loading(false);
    ui.set_minecraft_releases(strings(&["1.21.1"]));
    ui.set_create_minecraft("1.21.1".into());
    ui.set_create_loaders(strings(&["Vanilla", "NeoForge"]));
    ui.set_create_loader("NeoForge".into());
    let builds: Vec<String> = (0..200).map(|n| format!("21.1.{n}")).collect();
    let builds: Vec<&str> = builds.iter().map(String::as_str).collect();
    ui.set_create_loader_versions(strings(&builds));
    ui.set_create_loader_version("21.1.8".into());
    ui.set_modal("create-instance".into());
    settle(&ui);

    // Entries must not make a dropdown claim the dialog's spare height.
    for label in [
        lookup("gui-minecraft-release"),
        lookup("gui-loader"),
        lookup("gui-loader-version-placeholder"),
    ] {
        let height = dropdown(&ui, &label).size().height;
        assert!(height < 60.0, "{label} is {height}px tall");
    }

    // The release dropdown takes the row beside its star.
    let loader = dropdown(&ui, &lookup("gui-loader")).size().width;
    let release = dropdown(&ui, &lookup("gui-minecraft-release")).size().width;
    assert!(
        release > loader / 2.0,
        "the release dropdown is {release}px wide in a {loader}px dialog"
    );
}

#[test]
fn the_create_dialog_shows_the_loader_and_build_the_application_chose() {
    let ui = window();
    ui.set_loading(false);
    ui.set_minecraft_releases(strings(&["1.21.1"]));
    ui.set_create_minecraft("1.21.1".into());
    ui.set_create_loaders(strings(&["Vanilla", "Fabric", "NeoForge"]));
    ui.set_create_loader("NeoForge".into());
    ui.set_modal("create-instance".into());
    settle(&ui);
    assert_eq!(shown(&ui, &lookup("gui-loader")), "NeoForge");

    ui.set_create_loader_versions(strings(&["21.1.9", "21.1.8", "21.1.7"]));
    ui.set_create_loader_version("21.1.8".into());
    settle(&ui);
    assert_eq!(
        shown(&ui, &lookup("gui-loader-version-placeholder")),
        "21.1.8"
    );

    // A newer catalog arrives with the chosen build somewhere else in it.
    ui.set_create_loader_versions(strings(&["21.1.10", "21.1.9", "21.1.8"]));
    settle(&ui);
    assert_eq!(
        shown(&ui, &lookup("gui-loader-version-placeholder")),
        "21.1.8"
    );
}

#[test]
fn the_server_dialog_shows_the_release_loader_and_build_it_will_submit() {
    let ui = window();
    ui.set_loading(false);
    ui.set_minecraft_releases(strings(&["1.21.1", "1.20.4"]));
    ui.set_create_server_minecraft("1.20.4".into());
    ui.set_create_server_loaders(strings(&["Vanilla", "Fabric"]));
    ui.set_create_server_loader("Fabric".into());
    ui.set_create_server_loader_versions(strings(&["0.16.0", "0.15.11"]));
    ui.set_create_server_loader_version("0.15.11".into());
    ui.set_modal("create-server".into());
    settle(&ui);

    assert_eq!(shown(&ui, &lookup("gui-minecraft-release")), "1.20.4");
    assert_eq!(shown(&ui, &lookup("gui-loader")), "Fabric");
    assert_eq!(
        shown(&ui, &lookup("gui-loader-version-placeholder")),
        "0.15.11"
    );
}

#[test]
fn the_loader_change_dialog_shows_the_build_it_will_install() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11", "0.15.10"]));
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");
    assert_eq!(shown(&ui, &target), "0.15.11");

    // A pick by the user is the value the dialog installs.
    pick_down(&ui, &target, 1);
    assert_eq!(ui.get_loader_change_selected(), "0.15.10");
    assert_eq!(shown(&ui, &target), "0.15.10");

    ui.set_loader_change_selected("0.16.0".into());
    settle(&ui);
    assert_eq!(shown(&ui, &target), "0.16.0");
}

#[test]
fn a_dropdown_shows_nothing_for_a_value_that_is_in_none_of_its_entries() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11"]));
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");
    assert_eq!(shown(&ui, &target), "0.15.11");

    ui.set_loader_change_selected("".into());
    settle(&ui);
    assert_eq!(shown(&ui, &target), "");

    // New entries that hold the value bring it back, wherever they put it.
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_loader_change_versions(strings(&["0.15.11", "0.15.10"]));
    settle(&ui);
    assert_eq!(shown(&ui, &target), "0.15.11");

    // Entries that do not hold it show nothing, rather than their first.
    ui.set_loader_change_versions(strings(&["0.17.0", "0.16.0"]));
    settle(&ui);
    assert_eq!(shown(&ui, &target), "");
}

#[test]
fn the_results_per_page_and_cache_unit_dropdowns_show_their_stored_values() {
    let ui = window();
    ui.set_loading(false);
    ui.invoke_open_settings("Storage".into());
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    ui.set_artifact_cache_units(strings(&["MiB", "GiB"]));
    ui.set_artifact_cache_custom_unit("GiB".into());
    settle(&ui);
    let label = lookup("gui-artifact-cache-custom-value");
    // The unit dropdown only exists while the limit is custom.
    ui.set_artifact_cache_custom_index(ui.get_artifact_cache_limit_index());
    settle(&ui);
    assert_eq!(shown(&ui, &label), "GiB");

    ui.set_artifact_cache_custom_unit("MiB".into());
    settle(&ui);
    assert_eq!(shown(&ui, &label), "MiB");
}

#[test]
fn a_general_settings_dropdown_follows_the_stored_value_after_the_user_picked() {
    let ui = window();
    ui.set_loading(false);
    ui.invoke_open_settings("General".into());
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    ui.set_operator_permission_level(1);
    settle(&ui);
    let label = lookup("gui-default-operator-level");
    assert_eq!(shown(&ui, &label), lookup("gui-operator-level-1"));

    pick_down(&ui, &label, 2);
    assert_eq!(shown(&ui, &label), lookup("gui-operator-level-3"));
    assert_eq!(ui.get_operator_permission_level(), 3);

    // A refused save, or a change made elsewhere, puts the stored level back.
    ui.set_operator_permission_level(2);
    settle(&ui);
    assert_eq!(shown(&ui, &label), lookup("gui-operator-level-2"));
}

#[test]
fn a_minecraft_default_dropdown_follows_the_stored_choice_after_the_user_picked() {
    let ui = window();
    ui.set_loading(false);
    ui.invoke_open_settings("Minecraft".into());
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    let row = MinecraftPreferenceRow {
        id: "difficulty".into(),
        label: "Difficulty".into(),
        editor: "enum".into(),
        enum_labels: strings(&["Peaceful", "Easy", "Normal", "Hard"]),
        enum_index: 2,
        ..MinecraftPreferenceRow::default()
    };
    let rows = Rc::new(VecModel::from(vec![row.clone()]));
    ui.set_minecraft_preference_rows(ModelRc::from(Rc::clone(&rows)));
    settle(&ui);
    assert_eq!(shown(&ui, "Difficulty"), "Normal");

    pick_down(&ui, "Difficulty", 1);
    assert_eq!(shown(&ui, "Difficulty"), "Hard");

    // The stored choice moves back, as when the change was not accepted.
    let mut stored = row;
    stored.enum_index = 1;
    rows.set_row_data(0, stored);
    settle(&ui);
    assert_eq!(shown(&ui, "Difficulty"), "Easy");
}

#[test]
fn escape_closes_an_open_list_without_picking_what_was_highlighted() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11", "0.15.10"]));
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");

    dropdown(&ui, &target).invoke_accessible_expand_action();
    settle(&ui);
    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(true));
    press(&ui, Key::DownArrow);
    press(&ui, Key::Escape);
    settle(&ui);

    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(false));
    assert_eq!(ui.get_loader_change_selected(), "0.15.11");
    assert_eq!(shown(&ui, &target), "0.15.11");
}

#[test]
fn pressing_an_entry_of_the_open_list_picks_it() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11", "0.15.10"]));
    ui.set_loader_change_selected("0.16.0".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");

    let closed = dropdown(&ui, &target);
    closed.invoke_accessible_expand_action();
    settle(&ui);
    let entry = ElementHandle::find_by_accessible_label(&ui, "0.15.10")
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the open list has an entry for 0.15.10");
    // An entry reports its place in the list, which opens just below the
    // dropdown.
    let (list, at, size) = (
        closed.absolute_position(),
        entry.absolute_position(),
        entry.size(),
    );
    let position = slint::LogicalPosition::new(
        list.x + at.x + size.width / 2.0,
        list.y + closed.size().height + 4.0 + at.y + size.height / 2.0,
    );
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    settle(&ui);

    assert_eq!(ui.get_loader_change_selected(), "0.15.10");
    assert_eq!(shown(&ui, &target), "0.15.10");
    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(false));
}

#[test]
fn an_open_list_closes_when_the_window_is_resized_under_it() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11", "0.15.10"]));
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");

    dropdown(&ui, &target).invoke_accessible_expand_action();
    settle(&ui);
    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(true));

    // The dialog stays centered, so the dropdown moves with the window while
    // the list would stay where it was drawn.
    let before = dropdown(&ui, &target).absolute_position();
    ui.window().set_size(PhysicalSize::new(1300, 900));
    settle(&ui);
    assert_ne!(dropdown(&ui, &target).absolute_position(), before);

    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(false));
    assert_eq!(ui.get_loader_change_selected(), "0.15.11");
    assert_eq!(shown(&ui, &target), "0.15.11");
}

#[test]
fn an_open_list_stays_open_while_nothing_moves() {
    let ui = window();
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(strings(&["0.16.0", "0.15.11", "0.15.10"]));
    ui.set_loader_change_selected("0.15.11".into());
    ui.set_modal("loader-change".into());
    settle(&ui);
    let target = lookup("gui-loader-change-target");

    dropdown(&ui, &target).invoke_accessible_expand_action();
    settle(&ui);
    settle(&ui);

    assert_eq!(dropdown(&ui, &target).accessible_expanded(), Some(true));
}

#[test]
fn clicking_a_dropdown_cut_off_by_the_page_edge_opens_its_list_and_keeps_it_open() {
    let ui = window();
    ui.set_loading(false);
    ui.invoke_open_settings("General".into());

    // A dropdown lower on the page is not on screen until the window is tall
    // enough to show part of it, so grow the window from its smallest height
    // until the page's last dropdown first appears across the bottom edge.
    let theme = ui.global::<Theme>();
    let width = theme.get_window_min_width() + 60.0;
    let lowest = |ui: &AppWindow| {
        ElementHandle::find_by_element_type_name(ui, "ChoiceBox")
            .filter(|element| element.size().width > 0.0)
            .max_by(|left, right| {
                left.absolute_position()
                    .y
                    .total_cmp(&right.absolute_position().y)
            })
    };
    let mut cut_off = None;
    let mut height = theme.get_window_min_height();
    while height < theme.get_window_min_height() + 600.0 && cut_off.is_none() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        ui.window()
            .set_size(PhysicalSize::new(width as u32, height as u32));
        settle(&ui);
        let page_bottom = height - theme.get_shell_padding() - theme.get_strong_border_width();
        cut_off = lowest(&ui)
            .filter(|found| {
                let top = found.absolute_position().y;
                // Enough of it shows to be pressed, and not all of it.
                top + 12.0 < page_bottom && top + found.size().height > page_bottom
            })
            .map(|found| (found.absolute_position(), found.size()));
        height += 4.0;
    }
    let (at, size) = cut_off.expect("the last dropdown is cut off by the page edge at some height");
    let position = slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + 4.0);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    settle(&ui);
    settle(&ui);

    // Focusing the dropdown scrolls it into view, which moves it.
    let moved = lowest(&ui).expect("the dropdown is still on the page");
    assert_eq!(moved.accessible_expanded(), Some(true));
    assert_ne!(moved.absolute_position(), at);
}

/// The dropdown carrying `label`, not the text beside it that reads the same.
fn dropdown(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("a dropdown with that label")
}

fn shown(ui: &AppWindow, label: &str) -> String {
    dropdown(ui, label)
        .accessible_value()
        .expect("a dropdown shows a value")
        .to_string()
}

/// Opens the dropdown and moves its selection down by `steps` entries, the way
/// the arrow keys do, then confirms.
fn pick_down(ui: &AppWindow, label: &str, steps: usize) {
    dropdown(ui, label).invoke_accessible_expand_action();
    settle(ui);
    for key in std::iter::repeat_n(Key::DownArrow, steps).chain([Key::Return]) {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    settle(ui);
}

fn press(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

fn strings(entries: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        entries
            .iter()
            .map(|e| SharedString::from(*e))
            .collect::<Vec<_>>(),
    ))
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

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
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
