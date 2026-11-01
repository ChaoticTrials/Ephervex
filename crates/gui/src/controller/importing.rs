//! Instances a pack import is still creating, opened before they exist.
//!
//! An import keeps its instance unpublished until every file is in place, so
//! nothing may act on it yet. The window opens it anyway: it shows what the
//! pack declares, and holds the changes the user makes here until the import
//! publishes the instance, then sends them in the order they were made. A
//! failed or canceled import drops them.

use std::collections::HashMap;

use ephervex_core::{CaseNotes, CaseRefusal};
use ephervex_i18n::lookup;
use ephervex_model::{InstanceId, LoaderKind, ModOrigin};
use slint::SharedString;

use crate::backend::{CaseChange, Command, PendingClientTarget};
use crate::presentation::PackModPresentation;
use crate::ui::{CaseStepRow, InstanceRow, ModRow};

/// What an import is creating, as far as the window knows before it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImportingFacts {
    pub request_id: String,
    pub name: String,
    pub minecraft: String,
    /// The loader and its build, as the import dialog showed it.
    pub loader: String,
    pub loader_kind: LoaderKind,
    pub persistent: bool,
    /// The issue link the import itself stores on the instance.
    pub issue_link: String,
    /// The jars the pack gives this client, with the optional ones the user
    /// left out already removed.
    pub mods: Vec<PackModPresentation>,
}

/// A save of the Settings form, held as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeldRuntime {
    pub min_memory_mib: String,
    pub max_memory_mib: String,
    pub jvm_args: String,
}

#[derive(Debug)]
struct ImportingInstance {
    facts: ImportingFacts,
    runtime: Option<HeldRuntime>,
    issue_link: Option<String>,
    /// The case as the held changes leave it, built by the same rules the
    /// stored case follows, so what is shown is what will be stored.
    case: CaseNotes,
    /// The case changes in the order they were made. Replayed on the new
    /// instance's empty case, they give every step the id it has here.
    case_changes: Vec<CaseChange>,
    /// The import published the instance and the held changes were sent.
    published: bool,
}

impl ImportingInstance {
    fn held_changes(&self) -> usize {
        usize::from(self.runtime.is_some())
            + usize::from(self.issue_link.is_some())
            + self.case_changes.len()
    }
}

/// Every import the window has started and not yet seen finish.
#[derive(Debug, Default)]
pub(super) struct ImportingInstances {
    instances: HashMap<InstanceId, ImportingInstance>,
}

impl ImportingInstances {
    pub(super) fn begin(&mut self, id: InstanceId, facts: ImportingFacts) {
        self.instances.insert(
            id,
            ImportingInstance {
                facts,
                runtime: None,
                issue_link: None,
                case: CaseNotes::default(),
                case_changes: Vec::new(),
                published: false,
            },
        );
    }

    /// Whether changes to `id` have to be held: an import is creating it and
    /// has not published it yet.
    pub(super) fn holds(&self, id: InstanceId) -> bool {
        self.instances
            .get(&id)
            .is_some_and(|instance| !instance.published)
    }

    /// Whether an import is creating `id`, or published it and its row has
    /// not arrived yet.
    pub(super) fn knows(&self, id: InstanceId) -> bool {
        self.instances.contains_key(&id)
    }

    /// The instance the import under the pending card `request_id` creates.
    pub(super) fn instance_of(&self, request_id: &str) -> Option<InstanceId> {
        self.instances
            .iter()
            .find(|(_, instance)| instance.facts.request_id == request_id)
            .map(|(&id, _)| id)
    }

    /// The exact target the catalogs are searched on for `id`.
    pub(super) fn target(&self, id: InstanceId) -> Option<PendingClientTarget> {
        let instance = self.instances.get(&id)?;
        Some(PendingClientTarget {
            minecraft_version: instance.facts.minecraft.clone(),
            loader: instance.facts.loader_kind,
        })
    }

    /// Holds a save of the Settings form. Only the newest save is kept: it is
    /// the whole form, not a change to it.
    pub(super) fn hold_runtime(&mut self, id: InstanceId, runtime: HeldRuntime) -> bool {
        let Some(instance) = self.held_mut(id) else {
            return false;
        };
        instance.runtime = Some(runtime);
        true
    }

    pub(super) fn runtime(&self, id: InstanceId) -> Option<&HeldRuntime> {
        self.instances.get(&id)?.runtime.as_ref()
    }

    pub(super) fn hold_issue_link(&mut self, id: InstanceId, link: String) -> bool {
        let Some(instance) = self.held_mut(id) else {
            return false;
        };
        instance.issue_link = Some(link);
        true
    }

    /// Holds a change to the case, refused here for the same reasons the
    /// stored case would refuse it. A count of reproductions is not held:
    /// nothing can have been reproduced on an instance that does not exist.
    pub(super) fn hold_case(
        &mut self,
        id: InstanceId,
        change: CaseChange,
    ) -> Result<bool, CaseRefusal> {
        let Some(instance) = self.held_mut(id) else {
            return Ok(false);
        };
        let applied = match &change {
            CaseChange::AddStep(text) => instance.case.add_step(text).map(|_| true)?,
            CaseChange::SetStepDone { step, done } => instance.case.set_step_done(*step, *done),
            CaseChange::RemoveStep(step) => instance.case.remove_step(*step),
            CaseChange::SetNote(note) => {
                instance.case.set_note(note)?;
                true
            }
            CaseChange::CountReproduced | CaseChange::ResetReproduced => false,
        };
        if applied {
            instance.case_changes.push(change);
        }
        Ok(applied)
    }

    /// The import published `id`: answers the held changes as the commands
    /// that make them, in the order they were made. The instance stays known
    /// until its row arrives, so the window never shows it as gone.
    pub(super) fn publish(&mut self, id: InstanceId) -> Vec<Command> {
        let Some(instance) = self.instances.get_mut(&id) else {
            return Vec::new();
        };
        if instance.published {
            return Vec::new();
        }
        instance.published = true;
        let mut commands = Vec::new();
        if let Some(runtime) = instance.runtime.take() {
            commands.push(Command::SaveHostRuntime {
                host: ephervex_core::InstalledHost::Instance(id),
                min_memory_mib: runtime.min_memory_mib,
                max_memory_mib: runtime.max_memory_mib,
                jvm_args: runtime.jvm_args,
            });
        }
        if let Some(link) = instance.issue_link.take() {
            commands.push(Command::SetIssueLink { client: id, link });
        }
        commands.extend(
            instance
                .case_changes
                .drain(..)
                .map(|change| Command::ChangeCase { client: id, change }),
        );
        commands
    }

    /// The import of `id` ended without publishing it. Answers how many held
    /// changes that discards.
    pub(super) fn fail(&mut self, id: InstanceId) -> usize {
        self.instances
            .remove(&id)
            .filter(|instance| !instance.published)
            .map_or(0, |instance| instance.held_changes())
    }

    /// Forgets every published instance `listed` says has a row now, and
    /// answers them: from here on the window reads them like any other.
    pub(super) fn settle(&mut self, listed: impl Fn(InstanceId) -> bool) -> Vec<InstanceId> {
        let settled = self
            .instances
            .iter()
            .filter(|&(&id, instance)| instance.published && listed(id))
            .map(|(&id, _)| id)
            .collect::<Vec<_>>();
        for id in &settled {
            self.instances.remove(id);
        }
        settled
    }

    /// The rows of every instance still being imported, by instance.
    pub(super) fn rows(&self) -> HashMap<InstanceId, InstanceRow> {
        self.instances
            .keys()
            .filter_map(|&id| Some((id, self.row(id)?)))
            .collect()
    }

    /// The row the workspace shows for `id` in place of the one the instance
    /// list will have.
    pub(super) fn row(&self, id: InstanceId) -> Option<InstanceRow> {
        let instance = self.instances.get(&id)?;
        let facts = &instance.facts;
        let steps = instance
            .case
            .steps
            .iter()
            .map(|step| CaseStepRow {
                id: i32::try_from(step.id).unwrap_or(i32::MAX),
                text: step.text.as_str().into(),
                done: step.done,
            })
            .collect::<Vec<_>>();
        let done = steps.iter().filter(|step| step.done).count();
        let issue_link = instance
            .issue_link
            .as_deref()
            .unwrap_or(facts.issue_link.as_str());
        Some(InstanceRow {
            id: id.to_string().into(),
            name: facts.name.as_str().into(),
            minecraft: facts.minecraft.as_str().into(),
            loader: facts.loader.as_str().into(),
            loader_kind: crate::presentation::loader_kind_name(facts.loader_kind).into(),
            storage: lookup(if facts.persistent {
                "gui-persistent"
            } else {
                "gui-temporary"
            })
            .into(),
            storage_kind: if facts.persistent {
                "persistent"
            } else {
                "temporary"
            }
            .into(),
            mods: i32::try_from(facts.mods.len()).unwrap_or(i32::MAX),
            state: lookup("gui-state-importing").into(),
            state_kind: "importing".into(),
            case_steps: super::model(steps),
            case_done: i32::try_from(done).unwrap_or(i32::MAX),
            case_note: instance.case.note.as_str().into(),
            issue_link: issue_link.into(),
            group_index: -1,
            icon_slot: -1,
            ..InstanceRow::default()
        })
    }

    /// The pack's mods as the importing instance's Mods surface lists them.
    /// None can be changed: their files do not exist yet.
    pub(super) fn mod_rows(&self, id: InstanceId) -> Vec<ModRow> {
        let Some(instance) = self.instances.get(&id) else {
            return Vec::new();
        };
        instance
            .facts
            .mods
            .iter()
            .map(|entry| ModRow {
                project_id: entry.project_id.as_str().into(),
                logical_path: crate::mod_path_key::encode(std::path::Path::new(
                    &entry.logical_path,
                ))
                .into(),
                title: entry.file_name.as_str().into(),
                provider: entry.provider.map_or_else(
                    || lookup("gui-mod-provider-unknown").into(),
                    |provider| SharedString::from(provider.to_string()),
                ),
                provider_kind: entry
                    .provider
                    .map_or_else(SharedString::default, |provider| provider.as_str().into()),
                origin: lookup(ModOrigin::PackDeclared.message_id()).into(),
                origin_kind: "pack".into(),
                compatibility: lookup("gui-mod-importing").into(),
                enabled: true,
                pack_owned: true,
                ..ModRow::default()
            })
            .collect()
    }

    fn held_mut(&mut self, id: InstanceId) -> Option<&mut ImportingInstance> {
        self.instances
            .get_mut(&id)
            .filter(|instance| !instance.published)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    fn facts() -> ImportingFacts {
        ImportingFacts {
            request_id: "creation-1".to_owned(),
            name: "Example Pack".to_owned(),
            minecraft: "1.21.1".to_owned(),
            loader: "NeoForge 21.1.42".to_owned(),
            loader_kind: LoaderKind::NeoForge,
            persistent: false,
            issue_link: String::new(),
            mods: vec![PackModPresentation {
                logical_path: "mods/example.jar".to_owned(),
                file_name: "example.jar".to_owned(),
                provider: Some(ephervex_model::ModProvider::CurseForge),
                project_id: "1234".to_owned(),
                optional: false,
            }],
        }
    }

    fn importing() -> (ImportingInstances, InstanceId) {
        let mut instances = ImportingInstances::default();
        let id = InstanceId::new();
        instances.begin(id, facts());
        (instances, id)
    }

    #[test]
    fn changes_are_sent_in_the_order_they_were_made_once_the_import_publishes() {
        let (mut instances, id) = importing();
        instances
            .hold_case(id, CaseChange::AddStep("Open the world".to_owned()))
            .expect("step is held");
        assert!(instances.hold_issue_link(id, "https://example.com/1".to_owned()));
        instances
            .hold_case(
                id,
                CaseChange::SetStepDone {
                    step: 1,
                    done: true,
                },
            )
            .expect("step is checked");

        let commands = instances.publish(id);

        assert!(matches!(
            commands.as_slice(),
            [
                Command::SetIssueLink { .. },
                Command::ChangeCase {
                    change: CaseChange::AddStep(_),
                    ..
                },
                Command::ChangeCase {
                    change: CaseChange::SetStepDone {
                        step: 1,
                        done: true
                    },
                    ..
                },
            ]
        ));
        assert!(!instances.holds(id), "a published instance holds nothing");
        assert!(instances.publish(id).is_empty(), "nothing is sent twice");
    }

    #[test]
    fn only_the_newest_settings_save_is_sent() {
        let (mut instances, id) = importing();
        for max in ["2048", "4096"] {
            assert!(instances.hold_runtime(
                id,
                HeldRuntime {
                    min_memory_mib: String::new(),
                    max_memory_mib: max.to_owned(),
                    jvm_args: String::new(),
                },
            ));
        }

        let commands = instances.publish(id);

        assert_eq!(commands.len(), 1);
        let Command::SaveHostRuntime { max_memory_mib, .. } = &commands[0] else {
            panic!("a settings save is sent");
        };
        assert_eq!(max_memory_mib, "4096");
    }

    #[test]
    fn a_step_the_stored_case_would_refuse_is_refused_while_held() {
        let (mut instances, id) = importing();

        let refused = instances.hold_case(id, CaseChange::AddStep("   ".to_owned()));

        assert_eq!(refused, Err(CaseRefusal::StepEmpty));
        assert!(instances.publish(id).is_empty());
    }

    #[test]
    fn a_failed_import_discards_what_was_held_and_says_how_much() {
        let (mut instances, id) = importing();
        instances
            .hold_case(id, CaseChange::SetNote("Crashes on load".to_owned()))
            .expect("note is held");
        assert!(instances.hold_issue_link(id, "https://example.com/2".to_owned()));

        assert_eq!(instances.fail(id), 2);
        assert!(!instances.knows(id));
        assert!(!instances.hold_issue_link(id, "https://example.com/3".to_owned()));
    }

    #[test]
    fn held_steps_are_shown_on_the_row_with_the_ids_the_stored_case_gives_them() {
        let (mut instances, id) = importing();
        for text in ["First", "Second", "Third"] {
            instances
                .hold_case(id, CaseChange::AddStep(text.to_owned()))
                .expect("step is held");
        }
        instances
            .hold_case(id, CaseChange::RemoveStep(2))
            .expect("step is removed");

        let row = instances.row(id).expect("an importing row");
        let steps = slint::Model::iter(&row.case_steps)
            .map(|step| (step.id, step.text.to_string()))
            .collect::<Vec<_>>();

        assert_eq!(steps, [(1, "First".to_owned()), (3, "Third".to_owned())]);
        assert_eq!(row.state_kind, "importing");
        assert!(!row.can_launch);
    }

    #[test]
    fn a_published_instance_is_kept_until_the_list_has_its_row() {
        let (mut instances, id) = importing();
        instances.publish(id);

        assert!(instances.settle(|_| false).is_empty());
        assert!(instances.knows(id));
        assert_eq!(instances.settle(|listed| listed == id), [id]);
        assert!(!instances.knows(id));
    }

    #[test]
    fn a_reproduction_count_is_not_held() {
        let (mut instances, id) = importing();

        assert_eq!(
            instances.hold_case(id, CaseChange::CountReproduced),
            Ok(false)
        );
        assert!(instances.publish(id).is_empty());
    }
}
