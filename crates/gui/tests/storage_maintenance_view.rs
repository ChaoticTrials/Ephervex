//! The maintenance section of the disk usage view and the cleanup review: what
//! each shows, when each action is available, and what asks for confirmation.
#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, StorageGroupRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

fn button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no button labeled {label}"))
}

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect::<Vec<_>>(),
    ))
}

fn row(label: &str, size: &str, lines: &[&str], examples: &[&str]) -> StorageGroupRow {
    StorageGroupRow {
        id: label.into(),
        label: label.into(),
        detail: format!("{label} detail").into(),
        size: size.into(),
        lines: strings(lines),
        examples: strings(examples),
    }
}

fn settings(ui: &AppWindow, tab: &str) {
    ui.set_loading(false);
    ui.set_settings_open(true);
    ui.set_workspace_open(false);
    ui.set_settings_tab(tab.into());
    settle(ui);
}

#[test]
fn every_maintenance_action_is_offered_and_raises_its_own_request() {
    let ui = window();
    let asked = Rc::new(RefCell::new(Vec::new()));
    for (name, register) in [
        (
            "inspect",
            AppWindow::on_inspect_storage as fn(&AppWindow, _),
        ),
        ("verify", AppWindow::on_verify_storage),
        ("review", AppWindow::on_review_storage_cleanup),
    ] {
        let sink = Rc::clone(&asked);
        register(&ui, move || sink.borrow_mut().push(name));
    }
    settings(&ui, "Storage");

    button(&ui, &lookup("gui-storage-inspect")).mock_single_click(PointerEventButton::Left);
    button(&ui, &lookup("gui-storage-verify")).mock_single_click(PointerEventButton::Left);
    button(&ui, &lookup("gui-storage-preview-cleanup")).mock_single_click(PointerEventButton::Left);

    assert_eq!(asked.borrow().as_slice(), ["inspect", "verify", "review"]);
    assert!(
        visible(&ui, &lookup("gui-storage-idle")),
        "nothing is claimed before something has been inspected"
    );
}

#[test]
fn a_running_step_disables_the_others_with_a_reason_and_can_be_stopped() {
    let ui = window();
    let stopped = Rc::new(RefCell::new(false));
    let sink = Rc::clone(&stopped);
    ui.on_stop_storage_maintenance(move || *sink.borrow_mut() = true);
    settings(&ui, "Storage");
    ui.set_storage_maintenance_busy("verify".into());
    ui.set_storage_maintenance_progress("a-library.jar".into());
    settle(&ui);

    for message in [
        "gui-storage-inspect",
        "gui-storage-verify",
        "gui-storage-preview-cleanup",
    ] {
        let disabled = button(&ui, &lookup(message));
        assert_eq!(disabled.accessible_enabled(), Some(false), "{message}");
        assert_eq!(
            disabled.accessible_description().as_deref(),
            Some(lookup("gui-storage-busy-hint").as_str()),
            "{message} says why"
        );
    }
    assert!(visible(&ui, "a-library.jar"));
    button(&ui, &lookup("gui-storage-stop")).mock_single_click(PointerEventButton::Left);
    assert!(*stopped.borrow());
}

#[test]
fn recovery_waits_for_damage_an_instance_needs_and_says_so() {
    let ui = window();
    let recovered = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&recovered);
    ui.on_recover_storage(move || *sink.borrow_mut() += 1);
    settings(&ui, "Storage");

    let recover = button(&ui, &lookup("gui-storage-recover"));
    assert_eq!(recover.accessible_enabled(), Some(false));
    assert_eq!(
        recover.accessible_description().as_deref(),
        Some(lookup("gui-storage-recover-hint").as_str())
    );

    ui.set_storage_can_recover(true);
    settle(&ui);
    button(&ui, &lookup("gui-storage-recover")).mock_single_click(PointerEventButton::Left);
    assert_eq!(*recovered.borrow(), 1);
}

#[test]
fn an_inspection_shows_each_kind_what_is_reclaimable_and_why_the_rest_is_kept() {
    let ui = window();
    settings(&ui, "Storage");
    ui.set_storage_inspected(true);
    ui.set_storage_headline("2.0 KiB can be reclaimed".into());
    ui.set_storage_hosts("Based on 2 instances and 1 server".into());
    ui.set_storage_incomplete(true);
    ui.set_storage_groups(ModelRc::new(VecModel::from(vec![
        row(
            "Downloaded files",
            "2.0 KiB",
            &["Not needed by any instance or server: 2.0 KiB"],
            &[],
        ),
        row(
            "Libraries",
            "0 B",
            &["Kept: 3.0 MiB, because a Forge instance needs them"],
            &[],
        ),
    ])));
    settle(&ui);

    for text in [
        "2.0 KiB can be reclaimed",
        "Downloaded files",
        "Not needed by any instance or server: 2.0 KiB",
        "Libraries",
        "Kept: 3.0 MiB, because a Forge instance needs them",
        "Based on 2 instances and 1 server",
    ] {
        assert!(visible(&ui, text), "{text}");
    }
    assert!(visible(&ui, &lookup("gui-storage-incomplete")));
}

#[test]
fn an_integrity_result_lists_damage_and_what_could_not_be_checked() {
    let ui = window();
    settings(&ui, "Storage");
    ui.set_storage_checked(true);
    ui.set_storage_integrity("1 file is damaged or missing.".into());
    ui.set_storage_faults(strings(&[
        "libraries/a.jar (damaged), needed by an installed game version",
    ]));
    ui.set_storage_unchecked(strings(&["Unpacked natives were not checked"]));
    ui.set_storage_result(strings(&["1 file was fetched again and verified."]));
    settle(&ui);

    for text in [
        "1 file is damaged or missing.",
        "libraries/a.jar (damaged), needed by an installed game version",
        "Unpacked natives were not checked",
        "1 file was fetched again and verified.",
    ] {
        assert!(visible(&ui, text), "{text}");
    }
}

#[test]
fn totals_that_maintenance_made_stale_are_replaced_by_a_notice() {
    let ui = window();
    settings(&ui, "Storage");
    ui.set_disk_usage_stale(true);
    settle(&ui);

    assert!(visible(&ui, &lookup("gui-disk-usage-stale")));
    assert!(!visible(&ui, &lookup("gui-disk-usage-idle")));
}

fn review(ui: &AppWindow, empty: bool) {
    ui.set_storage_plan_id(4);
    ui.set_storage_plan_empty(empty);
    ui.set_storage_plan_summary(if empty {
        "Nothing can be reclaimed right now".into()
    } else {
        "2 items would be removed, freeing up to 4.0 KiB.".into()
    });
    ui.set_storage_plan_confirm("Remove 4.0 KiB".into());
    ui.set_storage_plan_items(ModelRc::new(VecModel::from(if empty {
        Vec::new()
    } else {
        vec![row(
            "Libraries",
            "4.0 KiB",
            &["2 items, 4.0 KiB"],
            &["Largest: libraries/org/old/old.jar"],
        )]
    })));
    ui.set_storage_plan_kept(strings(&["Game assets: 9.0 KiB in use by an instance"]));
    ui.set_storage_plan_incomplete(false);
    ui.set_modal("storage-cleanup".into());
    settle(ui);
}

#[test]
fn the_review_lists_what_goes_what_stays_and_nothing_is_removed_until_confirmed() {
    let ui = window();
    let confirmed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&confirmed);
    ui.on_confirm_storage_cleanup(move |plan| sink.borrow_mut().push(plan));
    review(&ui, false);

    for text in [
        &lookup("gui-storage-cleanup-title"),
        "2 items would be removed, freeing up to 4.0 KiB.",
        "Libraries",
        "Largest: libraries/org/old/old.jar",
        "Game assets: 9.0 KiB in use by an instance",
        &lookup("gui-storage-cleanup-protected"),
        &lookup("gui-storage-cleanup-kept"),
    ] {
        assert!(visible(&ui, text), "{text}");
    }
    assert!(confirmed.borrow().is_empty(), "showing it removes nothing");

    button(&ui, "Remove 4.0 KiB").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        confirmed.borrow().as_slice(),
        [4],
        "that plan, and no other"
    );
    assert_eq!(ui.get_modal(), "");
}

#[test]
fn canceling_the_review_confirms_nothing() {
    let ui = window();
    let confirmed = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&confirmed);
    ui.on_confirm_storage_cleanup(move |_| *sink.borrow_mut() += 1);
    review(&ui, false);

    button(&ui, &lookup("gui-cancel")).mock_single_click(PointerEventButton::Left);

    assert_eq!(*confirmed.borrow(), 0);
    assert_eq!(ui.get_modal(), "");
}

#[test]
fn an_empty_review_cannot_be_confirmed_and_says_why() {
    let ui = window();
    review(&ui, true);

    let confirm = button(&ui, "Remove 4.0 KiB");
    assert_eq!(confirm.accessible_enabled(), Some(false));
    assert_eq!(
        confirm.accessible_description().as_deref(),
        Some(lookup("gui-storage-cleanup-empty").as_str())
    );
}

#[test]
fn the_storage_page_offers_a_review_and_no_unconditional_clear() {
    let ui = window();
    let reviews = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&reviews);
    ui.on_review_storage_cleanup(move || *sink.borrow_mut() += 1);
    settings(&ui, "Storage");

    button(&ui, &lookup("gui-review-storage-cleanup")).mock_single_click(PointerEventButton::Left);

    assert_eq!(*reviews.borrow(), 1);
    assert_eq!(ui.get_modal(), "", "reviewing is the controller's to open");
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
    ui.window().set_size(PhysicalSize::new(1480, 2600));
    ui.show().expect("show");
    ui
}
