//! The room under a dialog's last row of buttons.
//!
//! A button's hard shadow overflows its bottom edge, so the dialog's body
//! padding is all that keeps the shadow off the dialog's border. A dialog that
//! sizes itself from a formula, or from a measure that misses wrapped text,
//! lets the content crowd that padding out.

use crate::support;

use ephervex_gui::ui::{AppWindow, Theme};
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

/// The padding around a dialog's body.
const BODY_PADDING: f32 = 22.0;

const LONG_ERROR: &str = "A long failure message that wraps onto a second line because the dialog is only so wide, and then keeps going so that it needs a third line as well.";

#[test]
fn the_deletion_dialogs_keep_the_body_padding_under_cancel_whatever_notes_they_carry() {
    let ui = support::test_window();
    for modal in ["delete-instance", "delete-server"] {
        for scale in [1.0_f32, 1.25] {
            ui.global::<Theme>().set_text_scale(scale);
            for (linked, stoppable, unstoppable, error) in [
                (0, 0, 0, ""),
                (1, 0, 0, ""),
                (1, 1, 0, ""),
                (1, 0, 1, ""),
                (1, 1, 1, "The deletion failed."),
                (0, 0, 0, LONG_ERROR),
            ] {
                ui.set_pending_action_id("repro-client".into());
                ui.set_pending_action_name("Repro Client".into());
                ui.set_pending_snapshot_deletion_note(
                    "Deleting files also deletes 2 retained snapshots.".into(),
                );
                ui.set_pending_action_linked_servers(linked);
                ui.set_pending_action_stoppable_linked_servers(stoppable);
                ui.set_pending_action_unstoppable_linked_servers(unstoppable);
                ui.set_modal_error(error.into());
                ui.set_modal(modal.into());
                settle(&ui);

                let gap = space_under_cancel(&ui);
                assert!(
                    gap >= BODY_PADDING,
                    "{modal} at text scale {scale} with {linked}/{stoppable}/{unstoppable} linked servers and error {error:?}: {gap}px under Cancel"
                );
            }
        }
    }
}

#[test]
fn the_rename_dialog_keeps_the_body_padding_under_cancel() {
    let ui = support::test_window();
    for scale in [1.0_f32, 1.25] {
        ui.global::<Theme>().set_text_scale(scale);
        for error in ["", LONG_ERROR] {
            ui.set_pending_action_id("repro-client".into());
            ui.set_modal_error(error.into());
            ui.set_modal("rename".into());
            settle(&ui);

            let gap = space_under_cancel(&ui);
            assert!(
                gap >= BODY_PADDING,
                "rename at text scale {scale} with error {error:?}: {gap}px under Cancel"
            );
        }
    }
}

/// Pixels between the bottom of the Cancel button and the dialog's bottom edge.
fn space_under_cancel(ui: &AppWindow) -> f32 {
    let cancel = ElementHandle::find_by_accessible_label(ui, &lookup("gui-cancel"))
        .filter(|element| element.size().width > 0.0)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .expect("Cancel");
    let position = cancel.absolute_position();
    let size = cancel.size();
    let bottom = position.y + size.height;
    // The button is a plate too, so only a plate well wider than it can be the dialog.
    let dialog = ElementHandle::find_by_element_type_name(ui, "Plate")
        .filter(|plate| {
            let origin = plate.absolute_position();
            let extent = plate.size();
            extent.width > size.width * 3.0
                && origin.x <= position.x
                && origin.y <= position.y
                && origin.x + extent.width >= position.x + size.width
                && origin.y + extent.height >= bottom
        })
        .min_by(|left, right| {
            (left.size().width * left.size().height)
                .total_cmp(&(right.size().width * right.size().height))
        })
        .expect("the dialog around Cancel");
    dialog.absolute_position().y + dialog.size().height - bottom
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}
