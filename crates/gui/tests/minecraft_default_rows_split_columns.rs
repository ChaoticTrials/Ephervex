//! The Minecraft defaults rows on a wide page and on a narrow one.
//!
//! Past a width the page can hold both columns, a row's text sits on the left
//! and its editor with the Reset action in a fixed column on the right. Below
//! it the rows stack. The window size is the widest editor, so it is the one
//! that must still fit its column at every offered text size.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, MinecraftPreferenceRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

/// The text sizes the application offers.
const TEXT_SCALES: [f32; 4] = [0.9, 1.0, 1.1, 1.25];

const DESCRIPTION: &str = "Controls how large the game window is when it opens, in pixels, and whether the aspect ratio is held while either side is edited.";
const UNAVAILABLE: &str = "Unavailable: Minecraft 1.12.2 does not have this setting";

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Rect {
    fn right(self) -> f32 {
        self.x + self.width
    }

    fn bottom(self) -> f32 {
        self.y + self.height
    }
}

#[test]
fn wide_rows_keep_their_editors_in_one_column_beside_their_text() {
    let ui = window(2400, 1800);
    for scale in TEXT_SCALES {
        ui.global::<Theme>().set_text_scale(scale);
        settle(&ui);
        let column = ui.global::<Theme>().get_preference_control_column_width();

        let resets = rects(&ui, &lookup("gui-minecraft-forget"));
        assert_eq!(
            resets.len(),
            3,
            "scale {scale}: every row can be reset: {resets:?}"
        );
        for reset in &resets {
            assert!(
                (reset.right() - resets[0].right()).abs() < 1.0,
                "scale {scale}: Reset buttons end at {} and {}",
                resets[0].right(),
                reset.right()
            );
        }
        let column_left = resets[0].right() - column;

        for label in ["Window size", "Field of view", "Cloud status"] {
            let name = one(&ui, label);
            assert!(
                name.right() <= column_left,
                "scale {scale}: {label} ends at {} inside the editor column from {column_left}",
                name.right()
            );
        }

        let width = one(&ui, &lookup("gui-minecraft-width"));
        let open = one(&ui, &lookup("gui-minecraft-window-open"));
        assert!(
            width.x >= column_left - 1.0,
            "scale {scale}: the width field starts at {} before the column at {column_left}",
            width.x
        );
        assert!(
            open.right() <= resets[0].x,
            "scale {scale}: the window size editor ends at {} over Reset at {}",
            open.right(),
            resets[0].x
        );
        assert!(
            resets[0].y >= width.y && resets[0].bottom() <= open.bottom(),
            "scale {scale}: Reset is not beside the window size editor"
        );

        assert_rows_do_not_overlap(&ui, scale);
    }
}

#[test]
fn a_narrow_page_stacks_each_row_as_before() {
    let ui = window(1000, 1800);
    for scale in TEXT_SCALES {
        ui.global::<Theme>().set_text_scale(scale);
        settle(&ui);

        let resets = rects(&ui, &lookup("gui-minecraft-forget"));
        assert_eq!(resets.len(), 3, "scale {scale}: every row can be reset");
        for (reset, label) in resets
            .iter()
            .zip(["Window size", "Field of view", "Cloud status"])
        {
            let name = one(&ui, label);
            assert!(
                reset.y >= name.bottom(),
                "scale {scale}: Reset at {} is not under {label} ending at {}",
                reset.y,
                name.bottom()
            );
        }

        assert_rows_do_not_overlap(&ui, scale);
    }
}

#[test]
fn rows_split_exactly_from_the_named_row_width() {
    let ui = window(1000, 1800);
    for scale in [1.0, 1.25] {
        ui.global::<Theme>().set_text_scale(scale);
        let threshold = ui.global::<Theme>().get_preference_row_split_width();
        let mut seen = (false, false);
        for width in (1000..=2400).step_by(50) {
            ui.window().set_size(PhysicalSize::new(width, 1800));
            settle(&ui);

            let name = one(&ui, "Window size");
            let reset = rects(&ui, &lookup("gui-minecraft-forget"))[0];
            // The card spans its label's inset on the left to Reset's on the
            // right, in either arrangement.
            let card_width = reset.right() - name.x + 24.0;
            // Split rows keep the editor away from the text's own edge.
            let split = one(&ui, &lookup("gui-minecraft-width")).x > name.x + 100.0;
            seen = (seen.0 || split, seen.1 || !split);
            assert_eq!(
                split,
                card_width >= threshold,
                "scale {scale}: a {card_width}px row against a split width of {threshold}px"
            );
        }
        assert!(
            seen.0 && seen.1,
            "scale {scale}: the sweep never crossed the split"
        );
    }
}

/// Every badge of the busiest row stays inside the text column at the narrowest
/// width that is split, where that column is at its narrowest.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn badges_stay_in_the_text_column_at_the_narrowest_split() {
    let ui = window(2400, 1800);
    for scale in TEXT_SCALES {
        ui.global::<Theme>().set_text_scale(scale);
        settle(&ui);
        // Below the width the page stops growing, so one measurement says
        // how wide the window must be for a row of the threshold's width.
        ui.window().set_size(PhysicalSize::new(1400, 1800));
        settle(&ui);
        let theme = ui.global::<Theme>();
        let (threshold, column) = (
            theme.get_preference_row_split_width(),
            theme.get_preference_control_column_width(),
        );
        let reset = rects(&ui, &lookup("gui-minecraft-forget"))[0];
        let card_width = reset.right() - one(&ui, "Window size").x + 24.0;
        let window_width = ui.window().size().width as f32 - card_width + threshold;
        ui.window()
            .set_size(PhysicalSize::new(window_width.ceil() as u32 + 1, 1800));
        settle(&ui);

        let reset = rects(&ui, &lookup("gui-minecraft-forget"))[1];
        let column_left = reset.right() - column;
        for badge in ["Advanced", "Not representable"] {
            let shown = one(&ui, badge);
            assert!(
                shown.right() <= column_left,
                "scale {scale}: the {badge} badge ends at {} inside the editor column from {column_left}",
                shown.right()
            );
        }
        let width = one(&ui, &lookup("gui-minecraft-width"));
        assert!(
            width.x >= column_left - 1.0,
            "scale {scale}: the page stacked"
        );
        ui.window().set_size(PhysicalSize::new(2400, 1800));
    }
}

/// No row's content reaches into the label of the row below it.
fn assert_rows_do_not_overlap(ui: &AppWindow, scale: f32) {
    let labels = ["Window size", "Field of view", "Cloud status"];
    let resets = rects(ui, &lookup("gui-minecraft-forget"));
    let description = one(ui, DESCRIPTION);
    let unavailable = one(ui, UNAVAILABLE);
    let open = one(ui, &lookup("gui-minecraft-window-open"));
    let tops: Vec<f32> = labels.iter().map(|label| one(ui, label).y).collect();
    let bottoms = [
        resets[0]
            .bottom()
            .max(description.bottom())
            .max(open.bottom()),
        resets[1].bottom(),
        resets[2].bottom().max(unavailable.bottom()),
    ];
    for index in 0..2 {
        assert!(
            bottoms[index] <= tops[index + 1],
            "scale {scale}: {} reaches {} and {} starts at {}",
            labels[index],
            bottoms[index],
            labels[index + 1],
            tops[index + 1]
        );
    }
}

fn one(ui: &AppWindow, label: &str) -> Rect {
    rects(ui, label)
        .first()
        .copied()
        .expect("an element with that label is on screen")
}

/// Every element carrying the label, one per place on screen: a control and
/// the text inside it are both labeled, and only the outer one counts.
fn rects(ui: &AppWindow, label: &str) -> Vec<Rect> {
    let mut found: Vec<Rect> = ElementHandle::find_by_accessible_label(ui, label)
        .map(|element| {
            let (position, size) = (element.absolute_position(), element.size());
            Rect {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
            }
        })
        .collect();
    let all = found.clone();
    found.retain(|rect| {
        !all.iter().any(|other| {
            (other.width > rect.width || other.height > rect.height)
                && other.x <= rect.x
                && other.y <= rect.y
                && other.right() >= rect.right()
                && other.bottom() >= rect.bottom()
        })
    });
    found
}

fn rows() -> Vec<MinecraftPreferenceRow> {
    vec![
        MinecraftPreferenceRow {
            id: "window".into(),
            label: "Window size".into(),
            technical_id: "window.size".into(),
            description: DESCRIPTION.into(),
            editor: "window".into(),
            can_reset: true,
            overridden: true,
            text_value: "854".into(),
            height_text: "480".into(),
            aspect_index: 0,
            aspect_locked: true,
            status: "Supported".into(),
            status_kind: "supported".into(),
            enum_index: -1,
            ..MinecraftPreferenceRow::default()
        },
        MinecraftPreferenceRow {
            id: "fov".into(),
            label: "Field of view".into(),
            technical_id: "fov".into(),
            editor: "slider".into(),
            can_reset: true,
            overridden: true,
            dirty: true,
            advanced: true,
            number_min: 30.0,
            number_max: 110.0,
            number_step: 1.0,
            number_value: 70.0,
            number_readout: "70".into(),
            status: "Not representable".into(),
            status_kind: "unrepresentable".into(),
            enum_index: -1,
            ..MinecraftPreferenceRow::default()
        },
        MinecraftPreferenceRow {
            id: "clouds".into(),
            label: "Cloud status".into(),
            technical_id: "clouds".into(),
            editor: "toggle".into(),
            can_reset: true,
            state_text: "Not overridden".into(),
            status: "Unavailable".into(),
            status_kind: "unavailable".into(),
            status_detail: UNAVAILABLE.into(),
            enum_index: -1,
            ..MinecraftPreferenceRow::default()
        },
    ]
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window(width: u32, height: u32) -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(width, height));
    ui.show().expect("show");
    ui.set_loading(false);
    ui.invoke_open_settings("Minecraft".into());
    ui.set_minecraft_preference_rows(ModelRc::new(VecModel::from(rows())));
    settle(&ui);
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
