//! The issue a modpack link named, in the import dialog.
//!
//! The link is shown because it is stored: what a web page offered is on
//! screen before the instance that carries it exists.

use crate::support;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

const LINK: &str = "https://github.com/example/cavestone/issues/42";

fn import_dialog(issue_link: &str) -> AppWindow {
    let ui = support::test_window();
    // Over the settings with one instance listed, as the other import dialog
    // tests do: Home and the empty list each have an "Import Modpack" button.
    support::open_settings(&ui, "General");
    ui.set_instances(ModelRc::new(VecModel::from(vec![
        ephervex_gui::ui::InstanceRow {
            id: "listed".into(),
            name: "Listed".into(),
            ..ephervex_gui::ui::InstanceRow::default()
        },
    ])));
    ui.set_import_issue_link(SharedString::from(issue_link));
    ui.set_modal("import".into());
    settle(&ui);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn visible(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|element| element.size().width > 0.0)
}

#[test]
fn the_dialog_shows_the_issue_the_link_named() {
    let ui = import_dialog(LINK);

    assert!(
        visible(&ui, &lookup("gui-issue-link")).is_some(),
        "the row says what it is"
    );
    assert!(
        visible(&ui, LINK).is_some(),
        "the address that will be stored is on screen"
    );
}

#[test]
fn a_link_that_named_no_issue_leaves_the_dialog_as_it_was() {
    let ui = import_dialog("");

    assert!(visible(&ui, &lookup("gui-issue-link")).is_none());
    assert!(visible(&ui, LINK).is_none());
}

/// An address has no spaces to break at, so a long one has to be broken inside
/// or it runs out of the dialog, or is cut off and no longer says what will be
/// stored.
#[test]
fn a_very_long_issue_link_wraps_instead_of_running_out_of_the_dialog() {
    let long = format!("https://example.com/issues/{}", "a".repeat(1500));
    let short_row = visible(&import_dialog(LINK), LINK).expect("the short address is shown");
    let ui = import_dialog(&long);
    let long_row = visible(&ui, &long).expect("the long address is shown");

    assert!(
        long_row.size().height > short_row.size().height * 2.0,
        "the long address takes {} high against {} for a short one",
        long_row.size().height,
        short_row.size().height
    );
    assert!(
        f64::from(long_row.size().width) <= f64::from(ui.window().size().width),
        "the row is {} wide in a window {} wide",
        long_row.size().width,
        ui.window().size().width
    );
}
