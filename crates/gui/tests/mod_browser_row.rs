//! What the mod browser's own action does.
//!
//! The browser row is the shortest path from "I need this mod" to having it, so
//! what its button says and what it invokes are both worth pinning down.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModSearchRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Image, ModelRc, PhysicalSize, VecModel};

#[test]
fn the_browser_row_installs_rather_than_offering_a_review() {
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

    let asked: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_install_from_browser(move |_instance, provider, project| {
        record
            .borrow_mut()
            .push((provider.to_string(), project.to_string()));
    });

    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_workspace_tab("Mods".into());
    ui.set_selected_instance_id("repro-client".into());
    ui.set_mod_provider("modrinth".into());
    ui.set_mod_results(ModelRc::new(VecModel::from(vec![ModSearchRow {
        project_id: "utilitix".into(),
        title: "UtilitiX".into(),
        description: "Nice to have items, blocks, and other features.".into(),
        author: "MelanX".into(),
        note: "Compatible".into(),
        icon: Image::default(),
    }])));

    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");

    let install = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-install"))
        .next()
        .expect("the row offers Install");
    install.mock_single_click(PointerEventButton::Left);

    assert_eq!(
        asked.borrow().as_slice(),
        [("modrinth".to_owned(), "utilitix".to_owned())]
    );
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
