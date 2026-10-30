//! Removing a managed Java runtime from Settings > Java while settings pin it.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, JavaRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

const PATH: &str = "/data/ephervex/java/temurin-17/bin/java";
const PINNING: &str = "instance Survival, the launcher preference";

#[test]
fn a_runtime_row_says_which_settings_pin_it() {
    let ui = window();
    java_page(&ui, PINNING);

    let pinned = lookup_args("gui-java-pinned-by", [("hosts", Arg::from(PINNING))]);
    assert!(widest(&ui, &pinned).is_some(), "the row names its pins");
    assert!(
        widest(&ui, "Java 17.0.12, Eclipse Adoptium, amd64").is_some(),
        "the row names the architecture"
    );
}

/// The confirmation names every setting the removal leaves pointing at a
/// runtime that is gone, and the removal stays the user's to confirm.
#[test]
fn removing_a_pinned_runtime_names_its_pins_before_it_is_confirmed() {
    let ui = window();
    let removals = record_removals(&ui);
    java_page(&ui, PINNING);

    open_removal(&ui);
    let warning = lookup_args(
        "gui-java-remove-runtime-pinned",
        [("value", Arg::from(PINNING))],
    );
    assert!(widest(&ui, &warning).is_some(), "the dialog names the pins");
    widest(&ui, &lookup("gui-remove"))
        .expect("the removal is offered")
        .invoke_accessible_default_action();

    assert_eq!(removals.borrow().as_slice(), [PATH.to_owned()]);
}

#[test]
fn removing_a_runtime_nothing_pins_shows_no_pin_warning() {
    let ui = window();
    java_page(&ui, "");

    open_removal(&ui);
    let warning = lookup_args("gui-java-remove-runtime-pinned", [("value", Arg::from(""))]);
    assert!(widest(&ui, &warning).is_none());
    assert!(widest(&ui, &lookup("gui-java-remove-runtime-pins-unknown")).is_none());
    assert!(widest(&ui, &lookup("gui-java-remove-runtime-note")).is_some());
}

/// Pins that could not be read are never presented as none.
#[test]
fn removing_a_runtime_whose_pins_are_unknown_says_so() {
    let ui = window();
    let removals = record_removals(&ui);
    runtime_row(&ui, lookup("gui-java-pins-unknown"), "", false);

    assert!(widest(&ui, &lookup("gui-java-pins-unknown")).is_some());
    open_removal(&ui);
    assert!(widest(&ui, &lookup("gui-java-remove-runtime-pins-unknown")).is_some());
    widest(&ui, &lookup("gui-remove"))
        .expect("the removal is still offered")
        .invoke_accessible_default_action();

    assert_eq!(removals.borrow().as_slice(), [PATH.to_owned()]);
}

fn java_page(ui: &AppWindow, pinning: &str) {
    let pinned_by = if pinning.is_empty() {
        String::new()
    } else {
        lookup_args("gui-java-pinned-by", [("hosts", Arg::from(pinning))])
    };
    runtime_row(ui, pinned_by, pinning, true);
}

fn runtime_row(ui: &AppWindow, pinned_by: String, pinning: &str, pins_known: bool) {
    ui.set_java_runtimes(ModelRc::new(VecModel::from(vec![JavaRow {
        path: PATH.into(),
        version: "17.0.12".into(),
        vendor: "Eclipse Adoptium".into(),
        title: "Java 17.0.12, Eclipse Adoptium, amd64".into(),
        origin: "managed".into(),
        compatible: "Java 17".into(),
        pinned_by: pinned_by.into(),
        pins_known,
        pinning: pinning.into(),
    }])));
    ui.invoke_open_settings("Java".into());
    settle(ui);
}

fn open_removal(ui: &AppWindow) {
    let remove = lookup_args("gui-java-remove-runtime", [("value", Arg::from(PATH))]);
    widest(ui, &remove)
        .expect("a managed runtime offers removal")
        .invoke_accessible_default_action();
    settle(ui);
}

fn record_removals(ui: &AppWindow) -> Rc<RefCell<Vec<String>>> {
    let removals: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&removals);
    ui.on_remove_java_runtime(move |path| sink.borrow_mut().push(path.to_string()));
    removals
}

fn widest(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
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
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
}
