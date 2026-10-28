//! The launcher's log files on the Diagnostics page.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, BundlePartRow};
use ephervex_i18n::{Arg, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn part(name: &str, path: &str, modified: &str) -> BundlePartRow {
    BundlePartRow {
        name: name.into(),
        kind: "Ephervex log".into(),
        size: "1.9 KiB".into(),
        path: path.into(),
        modified: modified.into(),
        selected: true,
    }
}

fn diagnostics() -> AppWindow {
    let ui = support::test_window();
    ui.set_launcher_bundle_parts(ModelRc::new(VecModel::from(vec![
        part(
            "launcher/launcher-20261025.log",
            "/logs/launcher-20261025.log",
            "3 h ago",
        ),
        part(
            "launcher/launcher-20261024.log",
            "/logs/launcher-20261024.log",
            "2026-10-24 09:12",
        ),
    ])));
    support::open_settings(&ui, "Diagnostics");
    ui.window().set_size(PhysicalSize::new(1500, 1200));
    settle(&ui);
    ui
}

#[test]
fn each_log_file_has_a_button_that_opens_that_file() {
    let ui = diagnostics();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&opened);
    ui.on_open_file(move |path| sink.borrow_mut().push(path.to_string()));

    let label = lookup_args(
        "gui-show-file-named",
        [("value", Arg::from("launcher/launcher-20261024.log"))],
    );
    ElementHandle::find_by_accessible_label(&ui, &label)
        .next()
        .expect("the button of the second file")
        .invoke_accessible_default_action();

    assert_eq!(opened.borrow().as_slice(), ["/logs/launcher-20261024.log"]);
}

#[test]
fn a_log_file_says_when_it_was_written() {
    let ui = diagnostics();
    assert!(
        ElementHandle::find_by_element_type_name(&ui, "Text")
            .filter_map(|element| element.accessible_label())
            .any(|label| label == "Ephervex log \u{b7} 1.9 KiB \u{b7} 3 h ago"),
        "the line under the name carries the age of the file"
    );
}
