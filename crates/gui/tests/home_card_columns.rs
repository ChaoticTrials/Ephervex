//! How the cards on Home are laid out: two columns while each card can have
//! its minimum width, one below that.

#![allow(clippy::expect_used)]

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

slint::slint! {
    import { HomePage } from "../ui/home/home-page.slint";
    import { RunningRow } from "../ui/models.slint";
    import { GuiI18n } from "../ui/globals.slint";
    import { Theme } from "../ui/theme.slint";

    export { RunningRow, GuiI18n, Theme }

    export component GridProbe inherits Window {
        in property <[RunningRow]> running;
        background: Theme.background;
        HomePage {
            width: parent.width;
            height: parent.height;
            running: root.running;
        }
    }
}

/// The columns follow the page's own width, not the window's.
#[test]
fn cards_share_two_columns_on_a_wide_page_and_stack_on_a_narrow_one() {
    let _ = slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            threading: false,
        },
    )));
    let probe = GridProbe::new().expect("probe window");
    let on_card = |id: &str| RunningRow {
        id: id.into(),
        name: id.into(),
        kind: "client".into(),
        ..RunningRow::default()
    };
    probe.set_running(model(vec![on_card("a"), on_card("b")]));
    probe.show().expect("show");
    let columns = |width: u32| {
        probe.window().set_size(PhysicalSize::new(width, 900));
        settle_probe(&probe);
        let mut lefts: Vec<i32> = ElementHandle::find_by_element_type_name(&probe, "RunningCard")
            .filter(|element| element.size().width > 0.0)
            .map(|element| {
                #[allow(clippy::cast_possible_truncation)]
                let left = element.absolute_position().x.round() as i32;
                left
            })
            .collect();
        lefts.sort_unstable();
        lefts.dedup();
        lefts.len()
    };

    assert_eq!(
        columns(1100),
        2,
        "room for two cards at their minimum width"
    );
    assert_eq!(columns(700), 1, "too narrow for two");
}

fn settle_probe(probe: &GridProbe) {
    slint::platform::update_timers_and_animations();
    probe.window().take_snapshot().expect("render");
}
