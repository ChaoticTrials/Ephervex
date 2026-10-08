//! The UI states and regions used in documentation.

use ephervex_gui::ui::AppWindow;

use crate::fixtures;

#[derive(Clone, Copy)]
pub(crate) struct Region {
    pub(crate) name: &'static str,
    pub(crate) accessible_label: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub(crate) name: &'static str,
    pub(crate) show: fn(&AppWindow),
    pub(crate) regions: &'static [Region],
    /// A 1080p image may be resized from 1440p only after a state is known to
    /// preserve the same layout at both window sizes.
    pub(crate) same_layout_at_both_sizes: bool,
}

const INSTANCE_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
    },
    Region {
        name: "client-card",
        accessible_label: Some("CaveStone"),
    },
];
const SERVER_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
    },
    Region {
        name: "server-card",
        accessible_label: Some("CaveStone Server"),
    },
];
const ACCOUNT_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
    },
    Region {
        name: "offline-profile",
        accessible_label: Some("MelanX"),
    },
];
const WORKSPACE_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
}];

pub(crate) const STATES: &[State] = &[
    State {
        name: "instances-overview",
        show: fixtures::instances_overview,
        regions: INSTANCE_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "servers-overview",
        show: fixtures::servers_overview,
        regions: SERVER_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "accounts-settings",
        show: fixtures::accounts_settings,
        regions: ACCOUNT_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-mods",
        show: fixtures::client_mods,
        regions: WORKSPACE_REGIONS,
        same_layout_at_both_sizes: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_and_region_has_a_unique_safe_name() {
        let mut names = std::collections::HashSet::new();
        for state in STATES {
            assert!(names.insert(state.name));
            assert!(
                state
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            );
            let mut regions = std::collections::HashSet::new();
            for region in state.regions {
                assert!(regions.insert(region.name));
                assert!(
                    region
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
                );
            }
            assert!(regions.contains("full"));
        }
    }
}
