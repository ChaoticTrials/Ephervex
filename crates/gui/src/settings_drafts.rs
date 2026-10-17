//! Settings fields that keep unsaved input while snapshots keep arriving.

use std::collections::VecDeque;

/// A group of settings fields the user saves together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSave {
    JavaSettings,
    StorageRoot,
    MicrosoftClientId,
    UserAgent,
    ArtifactCacheLimit,
}

/// One field's last confirmed value, to tell the user's edit from the stored value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftField<T> {
    confirmed: Option<T>,
    /// Saves started and not yet finished, oldest first.
    submitted: VecDeque<T>,
}

impl<T> Default for DraftField<T> {
    fn default() -> Self {
        Self {
            confirmed: None,
            submitted: VecDeque::new(),
        }
    }
}

impl<T: PartialEq + Clone> DraftField<T> {
    /// Takes the newly confirmed value and returns what the field should show.
    ///
    /// `None` means the field holds an unsaved edit and must be left alone.
    pub fn fold(&mut self, shown: &T, incoming: T) -> Option<T> {
        let unedited = self.confirmed.as_ref().is_none_or(|stored| stored == shown);
        self.confirmed = Some(incoming.clone());
        unedited.then_some(incoming)
    }

    /// Notes the value a save was started with.
    pub fn submit(&mut self, value: T) {
        self.submitted.push_back(value);
    }

    /// Whether a save started from this field has not finished yet.
    pub fn is_saving(&self) -> bool {
        !self.submitted.is_empty()
    }

    /// Forgets the newest submission, for a save that never reached the backend.
    pub fn withdraw(&mut self) {
        self.submitted.pop_back();
    }

    /// What a finished save puts in the field: the stored value, unless the
    /// user has typed since pressing Save or another save is still running.
    pub fn finish(&mut self, shown: &T, succeeded: bool) -> Option<T> {
        let submitted = self.submitted.pop_front()?;
        (succeeded && self.submitted.is_empty() && submitted == *shown)
            .then(|| self.confirmed.clone())
            .flatten()
    }
}

/// The confirmed value behind every settings field that has its own Save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsDrafts {
    pub java_auto_detect: DraftField<bool>,
    pub java_auto_download: DraftField<bool>,
    pub java_memory_min: DraftField<String>,
    pub java_memory_max: DraftField<String>,
    pub java_preferred_path: DraftField<String>,
    pub persistent_root: DraftField<String>,
    pub microsoft_client_id: DraftField<String>,
    pub user_agent: DraftField<String>,
    pub artifact_cache_limit_index: DraftField<i32>,
    pub artifact_cache_custom_value: DraftField<String>,
    pub artifact_cache_custom_unit: DraftField<String>,
    /// The `server.properties` defaults page, with its unsaved edits.
    pub(crate) server_defaults: crate::server_property_form::ServerDefaultsForm,
}

impl SettingsDrafts {
    /// Whether a save of this group has started and not finished.
    pub fn is_saving(&self, save: SettingsSave) -> bool {
        match save {
            SettingsSave::JavaSettings => {
                self.java_auto_detect.is_saving()
                    || self.java_auto_download.is_saving()
                    || self.java_memory_min.is_saving()
                    || self.java_memory_max.is_saving()
                    || self.java_preferred_path.is_saving()
            }
            SettingsSave::StorageRoot => self.persistent_root.is_saving(),
            SettingsSave::MicrosoftClientId => self.microsoft_client_id.is_saving(),
            SettingsSave::UserAgent => self.user_agent.is_saving(),
            SettingsSave::ArtifactCacheLimit => {
                self.artifact_cache_limit_index.is_saving()
                    || self.artifact_cache_custom_value.is_saving()
                    || self.artifact_cache_custom_unit.is_saving()
            }
        }
    }

    /// Forgets the newest save of this group, for one that never reached the backend.
    pub fn withdraw(&mut self, save: SettingsSave) {
        match save {
            SettingsSave::JavaSettings => {
                self.java_auto_detect.withdraw();
                self.java_auto_download.withdraw();
                self.java_memory_min.withdraw();
                self.java_memory_max.withdraw();
                self.java_preferred_path.withdraw();
            }
            SettingsSave::StorageRoot => self.persistent_root.withdraw(),
            SettingsSave::MicrosoftClientId => self.microsoft_client_id.withdraw(),
            SettingsSave::UserAgent => self.user_agent.withdraw(),
            SettingsSave::ArtifactCacheLimit => {
                self.artifact_cache_limit_index.withdraw();
                self.artifact_cache_custom_value.withdraw();
                self.artifact_cache_custom_unit.withdraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> String {
        value.to_owned()
    }

    #[test]
    fn the_first_confirmed_value_is_shown() {
        let mut field = DraftField::default();
        assert_eq!(field.fold(&text("512"), text("2048")), Some(text("2048")));
    }

    #[test]
    fn an_unedited_field_follows_the_stored_value() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        assert_eq!(field.fold(&text("a"), text("b")), Some(text("b")));
    }

    #[test]
    fn an_edited_field_keeps_its_text_when_an_unrelated_value_arrives() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        assert_eq!(field.fold(&text("typed"), text("a")), None);
        assert_eq!(field.fold(&text("typed"), text("b")), None);
    }

    #[test]
    fn an_edit_that_matches_the_stored_value_stops_being_an_edit() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        assert_eq!(field.fold(&text("b"), text("b")), None);
        assert_eq!(field.fold(&text("b"), text("c")), Some(text("c")));
    }

    #[test]
    fn a_completed_save_puts_back_the_stored_form_of_what_was_submitted() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("0512"));
        assert_eq!(field.fold(&text("0512"), text("512")), None);
        assert_eq!(field.finish(&text("0512"), true), Some(text("512")));
    }

    #[test]
    fn a_completed_save_keeps_what_was_typed_after_it_started() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("b"));
        assert_eq!(field.fold(&text("c"), text("b")), None);
        assert_eq!(field.finish(&text("c"), true), None);
        assert_eq!(field.fold(&text("c"), text("b")), None);
    }

    #[test]
    fn a_refused_save_keeps_the_typed_text_and_is_forgotten() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("bad"));
        assert_eq!(field.finish(&text("bad"), false), None);
        field.submit(text(" b "));
        assert_eq!(field.fold(&text(" b "), text("b")), None);
        assert_eq!(field.finish(&text(" b "), true), Some(text("b")));
    }

    #[test]
    fn an_earlier_save_finishing_leaves_a_later_submission_alone() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("b"));
        field.submit(text("c"));
        assert_eq!(field.fold(&text("c"), text("b")), None);
        assert_eq!(field.finish(&text("c"), true), None);
        assert_eq!(field.fold(&text("c"), text("c")), None);
        assert_eq!(field.finish(&text("c"), true), Some(text("c")));
    }

    #[test]
    fn a_save_settles_only_once() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("a"));
        assert_eq!(field.finish(&text("a"), true), Some(text("a")));
        assert_eq!(field.finish(&text("a"), true), None);
    }

    #[test]
    fn a_toggle_flipped_by_the_user_is_kept() {
        let mut field = DraftField::default();
        field.fold(&false, true);
        assert_eq!(field.fold(&false, true), None);
        assert_eq!(field.fold(&true, true), Some(true));
    }

    #[test]
    fn a_field_is_saving_from_submission_until_its_save_finishes() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        assert!(!field.is_saving());
        field.submit(text("b"));
        assert!(field.is_saving());
        field.finish(&text("b"), true);
        assert!(!field.is_saving());
    }

    #[test]
    fn a_refused_save_stops_the_field_saving() {
        let mut field = DraftField::default();
        field.fold(&text(""), text("a"));
        field.submit(text("bad"));
        field.finish(&text("bad"), false);
        assert!(!field.is_saving());
    }

    #[test]
    fn a_withdrawn_submission_stops_the_field_saving_and_keeps_earlier_ones() {
        let mut field = DraftField::default();
        field.submit(text("b"));
        field.submit(text("c"));
        field.withdraw();
        assert!(field.is_saving());
        field.withdraw();
        assert!(!field.is_saving());
    }

    #[test]
    fn a_group_is_saving_while_any_of_its_fields_is() {
        let mut drafts = SettingsDrafts::default();
        drafts.java_memory_max.submit(text("4096"));
        assert!(drafts.is_saving(SettingsSave::JavaSettings));
        assert!(!drafts.is_saving(SettingsSave::StorageRoot));
        drafts.artifact_cache_custom_unit.submit(text("GiB"));
        assert!(drafts.is_saving(SettingsSave::ArtifactCacheLimit));
        drafts.withdraw(SettingsSave::JavaSettings);
        assert!(!drafts.is_saving(SettingsSave::JavaSettings));
    }

    #[test]
    fn the_two_provider_identifiers_save_independently() {
        let mut drafts = SettingsDrafts::default();
        drafts.microsoft_client_id.submit(text("id"));
        assert!(drafts.is_saving(SettingsSave::MicrosoftClientId));
        assert!(!drafts.is_saving(SettingsSave::UserAgent));
    }
}
