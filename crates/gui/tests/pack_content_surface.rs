//! The Packs surface: what a pack placed, whose world owns each data pack, and
//! that a long name stays inside its card.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, PackContentRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

const LONG_NAME: &str =
    "Complementary Reimagined Unbound Extra Long Shader Pack Name For A Narrow Card Edition.zip";

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
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui
}

fn instance() -> InstanceRow {
    InstanceRow {
        id: "client".into(),
        name: "Repro Client".into(),
        ..InstanceRow::default()
    }
}

fn row(kind: &str, name: &str, path: &str, owner: &str, missing: bool) -> PackContentRow {
    PackContentRow {
        kind: kind.into(),
        name: name.into(),
        path: path.into(),
        owner: owner.into(),
        origin: lookup("pack-content-origin-shipped").into(),
        status: lookup(if missing {
            "pack-content-presence-absent"
        } else {
            "pack-content-presence-present"
        })
        .into(),
        missing,
    }
}

fn show(ui: &AppWindow, rows: Vec<PackContentRow>) {
    let mut view = support::client_view(instance());
    view.pack_content = ModelRc::new(VecModel::from(rows));
    support::show(ui, 0, vec![view], vec![support::pane(1, 0, "Packs")]);
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn labeled(ui: &AppWindow, text: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, text).collect()
}

#[test]
fn each_pack_is_listed_under_its_kind_with_the_world_that_owns_it() {
    let ui = window();
    let world = lookup_args(
        "pack-content-owner-saved-world",
        [("world", Arg::from("Repro"))],
    );
    show(
        &ui,
        vec![
            row(
                &lookup("pack-content-kind-resource-pack"),
                "Faithful.zip",
                "resourcepacks/Faithful.zip",
                "",
                false,
            ),
            row(
                &lookup("pack-content-kind-data-pack"),
                "tweaks.zip",
                "saves/Repro/datapacks/tweaks.zip",
                &world,
                false,
            ),
        ],
    );

    assert!(!labeled(&ui, "Faithful.zip").is_empty());
    let title = &labeled(&ui, "tweaks.zip")[0];
    let path = &labeled(&ui, "saves/Repro/datapacks/tweaks.zip")[0];
    let owner = &labeled(&ui, &world)[0];
    let origin = &labeled(&ui, &lookup("pack-content-origin-shipped"))[1];
    let bottom = |element: &ElementHandle| element.absolute_position().y + element.size().height;
    assert!(
        bottom(title) <= path.absolute_position().y + 0.5
            && bottom(path) <= owner.absolute_position().y + 0.5
            && bottom(owner) <= origin.absolute_position().y + 0.5,
        "the lines of a card do not overlap: a data pack names the world that owns it"
    );
}

#[test]
fn a_surface_with_nothing_placed_says_so() {
    let ui = window();
    show(&ui, Vec::new());
    assert!(!labeled(&ui, &lookup("gui-packs-none")).is_empty());
}

#[test]
fn a_pack_missing_from_the_game_folder_says_so() {
    let ui = window();
    show(
        &ui,
        vec![row(
            &lookup("pack-content-kind-shader-pack"),
            "Complementary.zip",
            "shaderpacks/Complementary.zip",
            "",
            true,
        )],
    );
    assert!(!labeled(&ui, &lookup("pack-content-presence-absent")).is_empty());
}

#[test]
fn a_long_pack_name_stays_inside_its_card() {
    let ui = window();
    show(
        &ui,
        vec![row(
            &lookup("pack-content-kind-shader-pack"),
            LONG_NAME,
            &format!("shaderpacks/{LONG_NAME}"),
            "",
            false,
        )],
    );
    let title = labeled(&ui, LONG_NAME)
        .into_iter()
        .next()
        .expect("the title is drawn");
    let status = labeled(&ui, &lookup("pack-content-presence-present"))
        .into_iter()
        .next()
        .expect("the status is drawn");
    let title_right = title.absolute_position().x + title.size().width;
    assert!(
        title_right <= status.absolute_position().x,
        "the title reaches {title_right}px and runs into the status at {}px",
        status.absolute_position().x
    );
}
