//! Every button the interface can disable says why, and every icon-only
//! button has the name its tooltip shows.
//!
//! These read the markup, because a button added later that forgets its
//! reason compiles and works, and is only noticed by someone who hovers it.

use crate::test_support::ui_markup;

const BUTTONS: [&str; 4] = [
    "AppButton",
    "IconButton",
    "FavoriteToggle",
    "LaunchAsButton",
];

/// What an instance of a control sets on itself, with nested blocks removed.
struct Instance {
    control: &'static str,
    line: usize,
    own: String,
}

impl Instance {
    fn sets(&self, property: &str) -> bool {
        let mut rest = self.own.as_str();
        while let Some(at) = rest.find(property) {
            let before = rest[..at].chars().next_back();
            let after = rest[at + property.len()..].trim_start();
            if before.is_none_or(|c| c.is_whitespace() || c == ';' || c == '{')
                && after.starts_with(':')
            {
                return true;
            }
            rest = &rest[at + property.len()..];
        }
        false
    }
}

fn instances(markup: &str) -> Vec<Instance> {
    let mut found = Vec::new();
    for control in BUTTONS {
        let opening = format!("{control} {{");
        for (at, _) in markup.match_indices(&opening) {
            let before = markup[..at].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '-') {
                continue;
            }
            let mut depth = 0_usize;
            let mut own = String::new();
            for c in markup[at + opening.len() - 1..].chars() {
                match c {
                    '{' => {
                        depth += 1;
                        if depth == 1 {
                            continue;
                        }
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                if depth == 1 {
                    own.push(c);
                }
            }
            found.push(Instance {
                control,
                line: markup[..at].matches('\n').count() + 1,
                own,
            });
        }
    }
    found
}

#[test]
fn every_button_that_can_be_disabled_gives_the_reason() {
    let missing: Vec<String> = instances(ui_markup())
        .into_iter()
        .filter(|instance| instance.sets("enabled") && !instance.sets("hint"))
        .map(|instance| format!("{} at joined line {}", instance.control, instance.line))
        .collect();
    assert!(
        missing.is_empty(),
        "buttons with an enabled binding and no hint: {missing:?}"
    );
}

#[test]
fn every_icon_only_button_has_a_name_to_show() {
    let nameless: Vec<usize> = instances(ui_markup())
        .into_iter()
        .filter(|instance| instance.control == "IconButton" && !instance.sets("accessible-name"))
        .map(|instance| instance.line)
        .collect();
    assert!(
        nameless.is_empty(),
        "icon buttons without an accessible-name at joined lines {nameless:?}"
    );
}

#[test]
fn the_scan_finds_the_buttons_it_is_meant_to_check() {
    let all = instances(ui_markup());
    for control in BUTTONS {
        assert!(
            all.iter().any(|instance| instance.control == control),
            "no {control} found in the markup"
        );
    }
    assert!(
        all.iter()
            .any(|instance| instance.sets("enabled") && instance.sets("hint")),
        "no button with both an enabled binding and a hint"
    );
}
