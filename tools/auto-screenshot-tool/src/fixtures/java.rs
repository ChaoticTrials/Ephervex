//! The Java runtimes the screenshots show as installed, which settings pin
//! them, and what each host's Java check concludes from them.

use ephervex_gui::java_view::reported_runtime_line;
use ephervex_gui::ui::{AppWindow, FixChangeRow, HostView, JavaChoiceRow, JavaRow};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute};
use slint::{Model, SharedString};

use super::rows;

/// One runtime of the Java inventory.
pub(crate) struct Runtime {
    pub(crate) version: &'static str,
    pub(crate) vendor: &'static str,
    pub(crate) path: &'static str,
    /// Where discovery found it, as Settings > Java names the origin.
    pub(crate) origin: &'static str,
    pub(crate) major: u8,
    pub(crate) update: u32,
}

const ARCH: &str = "amd64";

pub(crate) const JAVA_8: Runtime = Runtime {
    version: "1.8.0_432",
    vendor: "Azul Systems, Inc.",
    path: "/demo/usr/lib/jvm/zulu-8/bin/java",
    origin: "system",
    major: 8,
    update: 432,
};

/// The runtime CaveStone pins, at the version its recorded diagnosis reported.
pub(crate) const JAVA_17: Runtime = Runtime {
    version: "17.0.20.1",
    vendor: "Eclipse Adoptium",
    path: "/demo/ephervex/runtimes/temurin-17/bin/java",
    origin: "managed",
    major: 17,
    update: 20,
};

pub(crate) const JAVA_21: Runtime = Runtime {
    version: "21.0.5",
    vendor: "Eclipse Adoptium",
    path: "/demo/jvm/temurin-21/bin/java",
    origin: "java-home",
    major: 21,
    update: 5,
};

pub(crate) const JAVA_25: Runtime = Runtime {
    version: "25.0.1",
    vendor: "Eclipse Adoptium",
    path: "/demo/home/.sdkman/candidates/java/25-tem/bin/java",
    origin: "sdkman",
    major: 25,
    update: 1,
};

/// Newest first, the order discovery lists runtimes in.
pub(crate) const INVENTORY: [&Runtime; 4] = [&JAVA_25, &JAVA_21, &JAVA_17, &JAVA_8];

/// The instances that pin their own runtime, by key and name: CaveStone one
/// that fits, the horrors in the fog one that does not.
const PINS: [(&str, &str, &Runtime); 2] = [
    ("cavestone", "CaveStone", &JAVA_17),
    ("fog", "the horrors in the fog", &JAVA_25),
];

/// The newest Java 8 update legacy Forge accepts.
const LEGACY_FORGE_MAX_UPDATE: u32 = 320;

impl Runtime {
    /// "Java 17.0.20.1, Eclipse Adoptium, amd64".
    pub(crate) fn line(&self) -> String {
        reported_runtime_line(self.version, self.vendor, ARCH)
    }

    pub(crate) fn managed(&self) -> bool {
        self.origin == "managed"
    }

    /// The settings that name this runtime, as Settings > Java lists them.
    pub(crate) fn pinning(&self) -> String {
        PINS.iter()
            .filter(|(_, _, runtime)| runtime.path == self.path)
            .map(|(_, name, _)| {
                lookup_args("gui-java-pinned-instance", [("name", Arg::from(*name))])
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(crate) fn row(&self) -> JavaRow {
        let pinning = self.pinning();
        JavaRow {
            path: self.path.into(),
            version: self.version.into(),
            vendor: self.vendor.into(),
            origin: self.origin.into(),
            compatible: format!("Java {}", self.major).into(),
            title: self.line().into(),
            pinned_by: if pinning.is_empty() {
                pinning.clone()
            } else {
                lookup_args(
                    "gui-java-pinned-by",
                    [("hosts", Arg::from(pinning.as_str()))],
                )
            }
            .into(),
            pins_known: true,
            pinning: pinning.into(),
        }
    }

    /// The runtime with how it was chosen: "Java 21.0.5, ... (chosen automatically)".
    fn chosen(&self, pinned: bool) -> String {
        format!("{} ({})", self.line(), origin(pinned, self.managed()))
    }
}

fn origin(pinned: bool, managed: bool) -> String {
    lookup(match (pinned, managed) {
        (false, false) => "gui-java-origin-automatic",
        (false, true) => "gui-java-origin-automatic-managed",
        (true, false) => "gui-java-origin-host",
        (true, true) => "gui-java-origin-host-managed",
    })
}

/// The Java a host's Minecraft release and loader expect.
struct Expected {
    major: u8,
    /// The newest update the loader accepts, when it bounds one.
    max_update: Option<u32>,
}

impl Expected {
    fn of(minecraft: &str, loader_kind: &str) -> Option<Self> {
        let major = match minecraft {
            "1.12.2" => 8,
            "1.20.1" => 17,
            "1.21.1" => 21,
            _ => return None,
        };
        let max_update = (major == 8 && loader_kind == "forge").then_some(LEGACY_FORGE_MAX_UPDATE);
        Some(Self { major, max_update })
    }

    fn text(&self) -> String {
        let major = Arg::from(self.major.to_string());
        match self.max_update {
            Some(update) => lookup_args(
                "gui-java-expected-at-most",
                [
                    ("major", major),
                    ("update", Arg::from(update.to_string())),
                    ("loader", Arg::from("Forge")),
                ],
            ),
            None => lookup_args("gui-java-expected", [("major", major)]),
        }
    }

    /// Why this host refuses `runtime`, as the status line says it.
    fn mismatch(&self, runtime: &Runtime) -> Option<String> {
        if runtime.major != self.major {
            return Some(lookup_args(
                "gui-diagnosis-java-major",
                [("found", Arg::from(runtime.major.to_string()))],
            ));
        }
        let maximum = self
            .max_update
            .filter(|maximum| runtime.update > *maximum)?;
        Some(lookup_args(
            "gui-diagnosis-java-too-new",
            [
                ("found", Arg::from(runtime.version)),
                ("maximum", Arg::from(format!("1.8.0_{maximum}"))),
            ],
        ))
    }

    /// Why the picker disables `runtime`: a wrong major names what the host
    /// requires, since the row already shows the runtime's version.
    fn choice_reason(&self, runtime: &Runtime) -> Option<String> {
        if runtime.major != self.major {
            return Some(lookup_args(
                "gui-java-choice-requires",
                [("required", Arg::from(self.major.to_string()))],
            ));
        }
        self.mismatch(runtime)
    }

    /// The runtime automatic selection picks: the newest that fits.
    fn automatic(&self) -> Option<&'static Runtime> {
        INVENTORY
            .iter()
            .copied()
            .find(|runtime| self.mismatch(runtime).is_none())
    }
}

/// The host's own pin, if it has one.
fn pin(view: &HostView) -> Option<&'static Runtime> {
    (view.kind == "client")
        .then(|| {
            PINS.iter()
                .find(|(key, _, _)| view.id == *key)
                .map(|(_, _, runtime)| *runtime)
        })
        .flatten()
}

fn expected(view: &HostView) -> Option<Expected> {
    if view.kind == "client" {
        Expected::of(&view.client.minecraft, &view.client.loader_kind)
    } else {
        Expected::of(&view.server.minecraft, &view.server.loader_kind)
    }
}

/// Fills the host's Java status, header summary, and picker the way the
/// launcher does for every host it shows.
pub(crate) fn fill_host(view: &mut HostView) {
    let Some(expected) = expected(view) else {
        return;
    };
    let pinned = pin(view);
    let automatic = expected.automatic();
    view.java_expected = expected.text().into();
    match (pinned, automatic) {
        (Some(runtime), _) | (None, Some(runtime)) => {
            let problem = expected.mismatch(runtime);
            view.java_tone = match (&problem, pinned) {
                (Some(_), _) => "problem",
                (None, Some(_)) => "customized",
                (None, None) => "healthy",
            }
            .into();
            view.java_runtime = runtime.line().into();
            view.java_path = runtime.path.into();
            view.java_origin = origin(pinned.is_some(), runtime.managed()).into();
            view.java_summary = lookup_args(
                "gui-java-summary",
                [
                    ("version", Arg::from(runtime.version)),
                    ("vendor", Arg::from(runtime.vendor)),
                ],
            )
            .into();
            view.java_problem = problem.unwrap_or_default().into();
        }
        (None, None) => {
            let major = Arg::from(expected.major.to_string());
            view.java_tone = "healthy".into();
            view.java_runtime =
                lookup_args("gui-fix-java-download", [("major", major.clone())]).into();
            view.java_origin = origin(false, true).into();
            view.java_summary =
                lookup_args("gui-java-summary-download", [("major", major.clone())]).into();
            view.java_install = lookup_args("gui-java-install-for-host", [("major", major)]).into();
        }
    }
    view.java_tone_label = lookup(match view.java_tone.as_str() {
        "problem" => "gui-java-tone-problem",
        "customized" => "gui-java-tone-customized",
        _ => "gui-java-tone-healthy",
    })
    .into();
    view.java_choices = rows(choices(&expected, pinned, automatic));
}

/// The picker: the default entry, then every runtime that fits before every
/// one that does not, leaving out the one the default already resolves to
/// unless the host pins it.
fn choices(
    expected: &Expected,
    pinned: Option<&Runtime>,
    automatic: Option<&Runtime>,
) -> Vec<JavaChoiceRow> {
    let default = JavaChoiceRow {
        key: "default".into(),
        title: lookup("gui-java-choice-automatic").into(),
        detail: automatic
            .map_or_else(
                || {
                    lookup_args(
                        "gui-fix-java-download",
                        [("major", Arg::from(expected.major.to_string()))],
                    )
                },
                |runtime| format!("{} ({})", runtime.line(), runtime.path),
            )
            .into(),
        compatible: true,
        current: pinned.is_none(),
        reason: SharedString::default(),
    };
    let mut listed = INVENTORY
        .iter()
        .copied()
        .filter(|runtime| {
            pinned.is_some_and(|pin| pin.path == runtime.path)
                || automatic.is_none_or(|resolved| resolved.path != runtime.path)
        })
        .collect::<Vec<_>>();
    listed.sort_by_key(|runtime| expected.mismatch(runtime).is_some());
    std::iter::once(default)
        .chain(listed.into_iter().map(|runtime| {
            let reason = expected.choice_reason(runtime);
            JavaChoiceRow {
                key: runtime.path.into(),
                title: runtime.line().into(),
                detail: if runtime.managed() {
                    lookup_args(
                        "gui-java-choice-managed",
                        [("path", Arg::from(runtime.path))],
                    )
                } else {
                    runtime.path.to_owned()
                }
                .into(),
                compatible: reason.is_none(),
                current: pinned.is_some_and(|pin| pin.path == runtime.path),
                reason: reason.unwrap_or_default().into(),
            }
        }))
        .collect()
}

/// Every shown host the state left without a Java status gets the one the
/// launcher would load for it.
pub(crate) fn fill_shown_hosts(ui: &AppWindow) {
    let views = ui.get_host_views();
    for index in 0..views.row_count() {
        let Some(mut view) = views.row_data(index) else {
            continue;
        };
        if view.java_tone.is_empty() {
            fill_host(&mut view);
            views.set_row_data(index, view);
        }
    }
}

/// The review that switches a pinned host back to automatic selection.
pub(crate) fn review_automatic(view: &mut HostView) {
    let (Some(expected), Some(pinned)) = (expected(view), pin(view)) else {
        return;
    };
    let Some(automatic) = expected.automatic() else {
        return;
    };
    let current_detail = match expected.mismatch(pinned) {
        Some(reason) => lookup_args(
            "gui-java-refused-detail",
            [
                ("path", Arg::from(pinned.path)),
                ("reason", Arg::from(reason)),
            ],
        ),
        None => pinned.path.to_owned(),
    };
    view.fix_state = "ready".into();
    view.fix_title = lookup("gui-java-change-title").into();
    view.fix_changes = rows(vec![
        FixChangeRow {
            action: lookup("gui-fix-current").into(),
            name: pinned.chosen(true).into(),
            detail: current_detail.into(),
        },
        FixChangeRow {
            action: lookup("gui-fix-proposed").into(),
            name: automatic.chosen(false).into(),
            detail: automatic.path.into(),
        },
    ]);
    view.fix_notes = rows(vec![
        lookup_args(
            "gui-fix-java-expected",
            [("expected", Arg::from(expected.text()))],
        )
        .into(),
        lookup("gui-fix-java-note").into(),
    ]);
}

/// What a launch refused for the host's pinned runtime says, after the
/// refusal's own message.
pub(crate) fn refusal(view: &HostView) -> Option<String> {
    let expected = expected(view)?;
    let pinned = pin(view)?;
    let mismatch = expected.mismatch(pinned)?;
    let mut message = lookup("error-java-pinned-incompatible");
    if let Some(action) = lookup_attribute("error-java-pinned-incompatible", "action") {
        message.push('\n');
        message.push_str(&action);
    }
    let sentences = [
        lookup_args(
            "gui-java-refused-tried",
            [
                ("runtime", Arg::from(pinned.line())),
                ("origin", Arg::from(origin(true, pinned.managed()))),
            ],
        ),
        lookup_args(
            "gui-java-refused-expected",
            [("expected", Arg::from(expected.text()))],
        ),
        mismatch,
        lookup("gui-java-refused-next"),
    ];
    Some(format!("{message} {}", sentences.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::recorded;

    fn host(key: &str, minecraft: &str, loader_kind: &str) -> HostView {
        let mut view = HostView {
            kind: "client".into(),
            id: key.into(),
            ..HostView::default()
        };
        view.client.minecraft = minecraft.into();
        view.client.loader_kind = loader_kind.into();
        fill_host(&mut view);
        view
    }

    #[test]
    fn the_pinned_runtime_is_the_one_the_recorded_diagnosis_reported() {
        assert_eq!(JAVA_17.version, recorded::diagnostics().java);
    }

    #[test]
    fn each_pin_is_named_on_its_runtime_and_nowhere_else() {
        for runtime in INVENTORY {
            let pinned = PINS.iter().any(|(_, _, pin)| pin.path == runtime.path);
            assert_eq!(
                !runtime.row().pinned_by.is_empty(),
                pinned,
                "{}",
                runtime.path
            );
        }
    }

    #[test]
    fn a_fitting_pin_is_customized_and_a_wrong_major_is_a_problem() {
        assert_eq!(host("cavestone", "1.20.1", "forge").java_tone, "customized");
        let fog = host("fog", "1.20.1", "forge");
        assert_eq!(fog.java_tone, "problem");
        assert!(!fog.java_problem.is_empty());
        assert_eq!(host("photon", "1.21.1", "neoforge").java_tone, "healthy");
    }

    #[test]
    fn legacy_forge_with_no_accepted_java_8_offers_an_install() {
        let view = host("super-tiny", "1.12.2", "forge");
        assert_eq!(view.java_tone, "healthy");
        assert!(!view.java_install.is_empty());
        assert!(view.java_path.is_empty());
    }

    #[test]
    fn the_picker_lists_fitting_runtimes_first_and_disables_the_rest() {
        let view = host("fog", "1.20.1", "forge");
        let choices = view.java_choices.iter().collect::<Vec<_>>();
        assert_eq!(choices[0].key, "default");
        let first_refused = choices
            .iter()
            .position(|choice| !choice.compatible)
            .unwrap_or(choices.len());
        assert!(
            choices[first_refused..]
                .iter()
                .all(|choice| !choice.compatible && !choice.reason.is_empty())
        );
        assert_eq!(choices.iter().filter(|choice| choice.current).count(), 1);
    }
}
