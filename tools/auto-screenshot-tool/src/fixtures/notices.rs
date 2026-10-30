//! Transient notices: toasts and the sign-in code prompt.

use ephervex_gui::ui::{AppWindow, ToastRow};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute};

use super::rows;

fn toast(id: i32, message: String, error: bool, repeats: i32) -> ToastRow {
    ToastRow {
        id,
        message: message.into(),
        error,
        repeats,
    }
}

pub(crate) fn toasts_success_and_error(ui: &AppWindow) {
    let failure = {
        let message = lookup("error-launch-download-failed");
        match lookup_attribute("error-launch-download-failed", "action") {
            Some(action) => format!("{message}\n{action}"),
            None => message,
        }
    };
    ui.set_toasts(rows(vec![
        toast(
            1,
            lookup_args(
                "gui-created-entity",
                [("value", Arg::from("Skyblock Tools"))],
            ),
            false,
            1,
        ),
        toast(2, failure, true, 3),
    ]));
    ui.set_workspace_open(false);
    ui.set_settings_open(false);
}

pub(crate) fn accounts_device_code(ui: &AppWindow) {
    ui.set_toasts(rows(vec![
        toast(
            1,
            lookup_args(
                "gui-device-code-prompt",
                [
                    ("first", Arg::from("https://www.microsoft.com/link")),
                    ("second", Arg::from("ABCD1234")),
                ],
            ),
            false,
            1,
        ),
        toast(
            2,
            lookup_args("gui-signin-stage", [("value", Arg::from("microsoft"))]),
            false,
            1,
        ),
    ]));
    ui.set_workspace_open(false);
    ui.set_settings_open(true);
    ui.set_settings_tab("Accounts".into());
}
