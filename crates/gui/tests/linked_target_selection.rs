//! Choosing which linked servers a mod decision is carried to.
//!
//! Whether a server takes part is a decision the user makes per server, and the
//! state column beside it says what the operation *would* do rather than
//! whether it was chosen (ADR 0021). Those two are easy to confuse for one
//! another, so the control that carries the answer is asserted on directly:
//! what is submitted afterwards is read straight out of these rows.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, LinkedTargetRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel};

/// A mod whose server side is optional or undeclared arrives unselected, and
/// the row says "Will install" either way. Without a control of its own there
/// is nothing on screen that answers "is this server chosen".
#[test]
fn each_offered_server_carries_its_own_switch() {
    let ui = window();
    confirmation(&ui);

    assert!(
        switch(&ui, "Repro Server One").is_some(),
        "a server that can take part has to be answerable"
    );
    assert!(
        switch(&ui, "Repro Server Two").is_some(),
        "the answer is per server, not one for the set"
    );
}

#[test]
fn switching_a_server_on_adds_it_to_what_would_be_submitted() {
    let ui = window();
    confirmation(&ui);

    switch(&ui, "Repro Server One")
        .expect("the row is answerable")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        selected(&ui),
        vec!["server-one".to_owned()],
        "turning a row on is what puts its server in the operation"
    );
}

#[test]
fn switching_a_server_off_removes_it_again() {
    let ui = window();
    confirmation(&ui);
    ui.set_linked_targets(ModelRc::new(VecModel::from(vec![
        LinkedTargetRow {
            selected: true,
            ..offered("server-one", "Repro Server One")
        },
        offered("server-two", "Repro Server Two"),
    ])));
    settle(&ui);

    switch(&ui, "Repro Server One")
        .expect("the row is answerable")
        .invoke_accessible_default_action();
    settle(&ui);

    assert!(
        selected(&ui).is_empty(),
        "a row that was on has to be able to go off again"
    );
}

/// A server a release cannot be installed on is shown with its reason and is
/// not a choice. Offering a switch that does nothing would be worse than
/// offering none.
#[test]
fn a_server_that_cannot_take_part_is_not_answerable() {
    let ui = window();
    confirmation(&ui);

    let blocked = switch(&ui, "Repro Server Blocked").expect("the row is still shown");

    assert_eq!(
        blocked.accessible_enabled(),
        Some(false),
        "an ineligible target has to read as unavailable, not merely look it"
    );
    blocked.invoke_accessible_default_action();
    settle(&ui);
    assert!(
        selected(&ui).is_empty(),
        "a blocked server must not become part of the operation"
    );
}

/// A refusal is one word and every other badge in the launcher is drawn as
/// one, so this reads as a badge too. What a target would instead do is a
/// sentence, which does not fit one and stays plain text.
#[test]
fn a_refusal_is_a_badge_and_what_a_target_would_do_is_not() {
    let ui = window();
    confirmation(&ui);

    let badges: Vec<String> = ElementHandle::find_by_element_type_name(&ui, "Pill")
        .filter(|pill| pill.size().width > 0.0)
        .filter_map(|pill| pill.accessible_label().map(|label| label.to_string()))
        .collect();
    assert!(
        badges.contains(&lookup("gui-linked-unavailable")),
        "a blocked target's refusal is drawn as a badge: {badges:?}"
    );
    assert!(
        !badges.contains(&lookup("linked-target-will-install")),
        "what a target would do stays a sentence: {badges:?}"
    );
}

/// The list is the only part of the confirmation that stretches, so every pixel
/// the dialog's own arithmetic forgets comes out of it. A server the user has to
/// scroll to is one they will not answer.
#[test]
fn the_list_is_tall_enough_for_every_server_it_offers() {
    let ui = window();
    confirmation(&ui);

    let panel = ElementHandle::find_by_element_type_name(&ui, "LinkedTargetsPanel")
        .find(|element| element.size().height > 0.0)
        .expect("the confirmation lists its targets");

    // Three rows of 62px, the 6px between them, and the list's own 8px padding
    // and 1px border on each side.
    let needed = 3.0f32.mul_add(62.0, 2.0 * 6.0) + 18.0;
    assert!(
        panel.size().height >= needed,
        "the list is {}px for {needed}px of rows, so a server has to be scrolled to",
        panel.size().height
    );
}

/// The confirmation as the backend's plan opens it: two eligible servers, both
/// unselected because the project leaves its server side undeclared, and one
/// that cannot take part.
fn confirmation(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_mod_plan_title("Repro Mod".into());
    ui.set_mod_plan_version("1.2.3".into());
    ui.set_mod_plan_install_count(1);
    // A plan that also has something to warn about: the warning, the summary
    // and the support sentence are all fixed height, and the list is what is
    // left over.
    ui.set_mod_plan_warning_count(1);
    ui.set_linked_support(lookup("gui-linked-support-unknown").into());
    ui.set_linked_targets(ModelRc::new(VecModel::from(vec![
        offered("server-one", "Repro Server One"),
        offered("server-two", "Repro Server Two"),
        LinkedTargetRow {
            state: lookup("gui-linked-unavailable").into(),
            explanation: lookup("linked-target-incompatible-loader").into(),
            tone: "warn".into(),
            actionable: false,
            blocked: true,
            ..offered("server-blocked", "Repro Server Blocked")
        },
    ])));
    ui.set_modal("mod-plan".into());
    settle(ui);
}

fn offered(id: &str, name: &str) -> LinkedTargetRow {
    LinkedTargetRow {
        id: id.into(),
        name: name.into(),
        detail: "1.21.1 | NeoForge 21.1.249".into(),
        state: lookup("linked-target-will-install").into(),
        explanation: String::new().into(),
        tone: "ok".into(),
        actionable: true,
        blocked: false,
        selected: false,
    }
}

/// The server ids the confirmation would submit right now.
fn selected(ui: &AppWindow) -> Vec<String> {
    ui.get_linked_targets()
        .iter()
        .filter(|row| row.actionable && row.selected)
        .map(|row| row.id.to_string())
        .collect()
}

/// The switch belonging to one server, found by the name it is labeled with.
fn switch(ui: &AppWindow, name: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, name)
        .filter(|element| element.accessible_checked().is_some())
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
