//! A host's Java runtime section: what it runs, the runtimes it can switch to,
//! and the install it offers.
//!
//! Choosing a runtime never changes the host here. It opens the same review
//! a Diagnostics fix opens, and only Apply there changes anything.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use ephervex_core::{DiagnosticFix, InstalledHost};
use ephervex_i18n::lookup;
use slint::SharedString;

use super::{Workspace, diagnosis_flow, model, send};
use crate::backend::{self, Command, HostJavaData};
use crate::java_view::{DEFAULT_CHOICE, JavaChoiceView};
use crate::ui::{AppWindow, JavaChoiceRow};

/// Asks for a host's Java status, and for its picker's entries when the
/// runtime section shows them.
pub(super) fn load(commands: &Sender<Command>, host: InstalledHost, choices: bool) {
    send(commands, Command::LoadHostJava { host, choices });
}

/// Asks every shown host for its Java again, once the runtimes they can use
/// may have changed.
pub(super) fn reload_shown(workspace: &Workspace) {
    for host in workspace.state.borrow().shown() {
        load(&workspace.commands, host, true);
    }
}

fn row(choice: &JavaChoiceView) -> JavaChoiceRow {
    JavaChoiceRow {
        key: choice.key.as_str().into(),
        title: choice.title.as_str().into(),
        detail: choice.detail.as_str().into(),
        compatible: choice.compatible,
        current: choice.current,
        reason: choice.reason.as_str().into(),
    }
}

pub(super) fn apply_host_java(workspace: &Workspace, data: &HostJavaData) {
    workspace.update(data.host, |view| {
        match &data.status {
            Ok(status) => {
                view.java_tone = status.tone.as_str().into();
                view.java_tone_label = status.tone_label.as_str().into();
                view.java_runtime = status.runtime.as_str().into();
                view.java_path = status.path.as_str().into();
                view.java_origin = status.origin.as_str().into();
                view.java_expected = status.expected.as_str().into();
                view.java_problem = status.problem.as_str().into();
                view.java_summary = status.summary.as_str().into();
                view.java_install = status.install.as_str().into();
                view.java_error = SharedString::default();
            }
            // Nothing the last good read said stands once a newer one failed.
            Err(error) => {
                view.java_tone = SharedString::default();
                view.java_tone_label = SharedString::default();
                view.java_runtime = SharedString::default();
                view.java_path = SharedString::default();
                view.java_origin = SharedString::default();
                view.java_expected = SharedString::default();
                view.java_problem = SharedString::default();
                view.java_summary = SharedString::default();
                view.java_install = SharedString::default();
                view.java_error = error.as_str().into();
            }
        }
        if let Some(choices) = &data.choices {
            view.java_choices = model(choices.iter().map(row).collect());
        }
    });
}

pub(super) fn apply_path_checked(
    workspace: &Workspace,
    host: InstalledHost,
    result: &Result<JavaChoiceView, String>,
) {
    workspace.update(host, |view| {
        view.java_custom_checking = false;
        match result {
            Ok(choice) => {
                view.java_custom = model(vec![row(choice)]);
                view.java_custom_error = SharedString::default();
            }
            Err(error) => {
                view.java_custom = model(Vec::new());
                view.java_custom_error = error.as_str().into();
            }
        }
    });
}

/// A runtime was installed for a host. When the host would not use it on its
/// own, the change that pins it is put up for review, never applied.
pub(super) fn apply_installed(workspace: &Workspace, host: InstalledHost, pin: Option<PathBuf>) {
    if let Some(path) = pin {
        diagnosis_flow::review_change(
            workspace,
            &workspace.commands,
            host,
            lookup("gui-java-use-installed"),
            DiagnosticFix::PinJava(path),
        );
    }
    load(&workspace.commands, host, true);
}

pub(super) fn apply_install_settled(workspace: &Workspace, host: InstalledHost) {
    workspace.update(host, |view| view.java_installing = false);
}

/// The fix choosing a picker entry proposes.
fn fix_for(key: &str) -> DiagnosticFix {
    if key == DEFAULT_CHOICE {
        DiagnosticFix::UseAutomaticJava
    } else {
        DiagnosticFix::PinJava(PathBuf::from(key))
    }
}

pub(super) fn register(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    ui.on_load_host_java(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            load(&sender, host, true);
        }
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_choose_host_java(move |kind, id, key| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            diagnosis_flow::review_change(
                &views,
                &sender,
                host,
                lookup("gui-java-change-title"),
                fix_for(&key),
            );
        }
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_check_host_java_path(move |kind, id, path| {
        let path = path.trim();
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        if path.is_empty() {
            return;
        }
        views.update(host, |view| {
            view.java_custom_checking = true;
            view.java_custom_error = SharedString::default();
        });
        send(
            &sender,
            Command::CheckHostJavaPath {
                host,
                path: PathBuf::from(path),
            },
        );
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_install_host_java(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            views.update(host, |view| view.java_installing = true);
            send(&sender, Command::InstallHostJava(host));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_entry_clears_the_host_pin_and_a_runtime_entry_pins_its_path() {
        assert_eq!(fix_for(DEFAULT_CHOICE), DiagnosticFix::UseAutomaticJava);
        assert_eq!(
            fix_for("/opt/jdk-21/bin/java"),
            DiagnosticFix::PinJava(PathBuf::from("/opt/jdk-21/bin/java"))
        );
    }
}
