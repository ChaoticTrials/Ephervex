//! A segmented choice is exactly as wide as its cells and their outline.

use crate::support;

use ephervex_gui::ui::{AppWindow, Theme};
use ephervex_i18n::lookup;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, PhysicalSize};

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

#[test]
fn the_last_cell_ends_at_the_outline_with_no_gap_beside_it() {
    let ui = support::test_window();
    support::open_settings(&ui, "General");
    ui.window().set_size(PhysicalSize::new(1500, 1200));
    settle(&ui);

    let group = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-application-text-size"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
        .expect("the text size choice");
    let last = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-text-scale-125"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("the last cell");

    let outline = ui.global::<Theme>().get_strong_border_width();
    let group_right = group.absolute_position().x + group.size().width;
    let last_right = last.absolute_position().x + last.size().width;
    assert!(
        (group_right - last_right - outline).abs() < 0.5,
        "the cell ends {} px before the choice does, where only the outline ({outline} px) belongs",
        group_right - last_right
    );
}

fn group(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
        .expect("the choice")
}

#[test]
fn a_choice_that_has_the_room_stays_in_one_row() {
    let ui = support::test_window();
    support::open_settings(&ui, "General");
    ui.window().set_size(PhysicalSize::new(2000, 1200));
    settle(&ui);

    let height = ui.global::<Theme>().get_control_height();
    for label in ["gui-application-text-size", "gui-technical-text-size"] {
        let size = group(&ui, &lookup(label)).size();
        assert!(
            (size.height - height).abs() < 0.5,
            "{label} is {} px tall where one row is {height} px",
            size.height
        );
    }
}

#[test]
fn a_choice_without_the_room_becomes_a_dropdown_that_stays_inside_the_window() {
    let ui = support::test_window();
    support::open_settings(&ui, "General");
    ui.window().set_size(PhysicalSize::new(1100, 1200));
    settle(&ui);

    let label = lookup("gui-technical-text-size");
    let menu = ElementHandle::find_by_accessible_label(&ui, &label)
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Combobox)
                && element.size().width > 0.0
        })
        .expect("the choice is offered as a dropdown");
    let right = menu.absolute_position().x + menu.size().width;
    assert!(
        right <= 1100.0,
        "the dropdown ends at {right}, past the window's edge"
    );
}
