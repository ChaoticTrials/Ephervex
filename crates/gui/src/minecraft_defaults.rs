//! The Minecraft defaults settings screen's non-visual state.
//!
//! # What this screen configures
//!
//! Global defaults, meaning the values a *newly created or imported* client
//! instance comes up with. Saving here never rewrites an instance that already
//! exists, and nothing in this module can reach one: the only mutation it ever
//! produces is a batch of [`PreferenceEdit`]s for
//! `App::save_global_minecraft_preferences`.
//!
//! # Where the rows come from
//!
//! Exactly one place: the bundled product catalog, delivered through
//! [`MinecraftPreferenceSnapshot`]. Compatibility knowledge - remote or
//! bundled - can change what a release does with a row, and can never invent
//! one. An id configured by a newer Ephervex has no descriptor here, so it is
//! never a row and never part of a save batch, which is what leaves it
//! untouched in the configuration file.
//!
//! # One draft, keyed by identity
//!
//! Every edit is recorded against a [`PreferenceId`], never a row position, so
//! changing category, typing in the filter, toggling the advanced tier, or
//! taking a fresh snapshot cannot move an edit onto a different preference.
//! The draft is an *overlay*: a row the user has not touched is not in it at
//! all, and an edit that restores a row to what core last confirmed is removed
//! from it rather than saved as a no-op.

use std::collections::{BTreeMap, BTreeSet};

use ephervex_core::{
    InputBinding, LanguageEntry, MinecraftPreferenceDescriptor, MinecraftPreferenceEntry,
    MinecraftPreferencePreview, MinecraftPreferenceSnapshot, MinecraftPreferenceValue, MouseButton,
    PhysicalKey, PreferenceCategory, PreferenceId, PreferencePresentation, PreferenceValueShape,
    PresentationTier, ReleaseSupport, SkipReason, WindowSize,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ModelRc, SharedString, VecModel};

use crate::ui::MinecraftPreferenceRow;

/// The settings screen's tabs, in the order they are shown.
///
/// The catalog's own categories, listed once here so the tab strip and the row
/// filter cannot disagree about which tab a preference belongs on.
pub const CATEGORIES: [PreferenceCategory; 5] = [
    PreferenceCategory::Gameplay,
    PreferenceCategory::Video,
    PreferenceCategory::SoundAndMusic,
    PreferenceCategory::Controls,
    PreferenceCategory::LanguageAndAccessibility,
];

/// One background question, and the single answer that would still be an
/// answer to it.
///
/// Two facts rather than one, because a counter alone cannot express them
/// both. `next` never goes backwards and never repeats, so no two requests
/// can share an identity. `pending` is the one identity currently worth
/// listening for, and it is dropped the moment a newer question is asked -
/// *including* when minting the newer identity fails. A counter compared
/// against itself would keep accepting the exhausted identity's answer as
/// though it addressed the question that could not be sent.
#[derive(Debug, Default)]
struct RequestChannel {
    /// The last identity handed out.
    next: u64,
    /// The identity whose answer is still wanted, if any.
    pending: Option<u64>,
}

impl RequestChannel {
    /// Retires the outstanding request and mints the identity of its
    /// successor.
    ///
    /// `None` when no further identity exists. The retirement happens either
    /// way and before anything else, so a question that could not be sent
    /// still supersedes the one before it: the alternative is answering a new
    /// question with an old reply.
    fn issue(&mut self) -> Option<u64> {
        self.pending = None;
        let minted = self.next.checked_add(1)?;
        self.next = minted;
        self.pending = Some(minted);
        Some(minted)
    }

    /// Drops the outstanding request without asking anything new.
    fn retire(&mut self) {
        self.pending = None;
    }

    /// Whether an answer carrying this identity is the one being waited on.
    const fn accepts(&self, generation: u64) -> bool {
        match self.pending {
            Some(pending) => pending == generation,
            None => false,
        }
    }
}

/// The tab label for one category.
///
/// The catalog names categories in a stable machine form; this is the only
/// place that turns one into words, so the tab strip and any other reader of a
/// category agree.
#[must_use]
pub fn category_label(category: PreferenceCategory) -> String {
    lookup(match category {
        PreferenceCategory::Gameplay => "gui-minecraft-category-gameplay",
        PreferenceCategory::Video => "gui-minecraft-category-video",
        PreferenceCategory::SoundAndMusic => "gui-minecraft-category-sound",
        PreferenceCategory::Controls => "gui-minecraft-category-controls",
        PreferenceCategory::LanguageAndAccessibility => "gui-minecraft-category-language",
    })
}

/// The order sections are shown in, one list per category.
///
/// A stable product decision rather than anything the catalog states: the
/// generated `section` names are unordered, and listing key bindings as
/// movement-then-gameplay-then-the-rest reads better than alphabetically.
///
/// `None` is the preferences that carry no section, shown under the "General"
/// heading. It is listed here like any other entry, because where it belongs is
/// a judgement per category rather than a rule: Sound leads with its volumes
/// and leaves its four remaining settings after them, while every other
/// category opens with its general ones.
///
/// To reorder a category, reorder its list. A section missing from it sorts
/// after every listed one, by name, so a section the catalog grows later still
/// lands somewhere deterministic.
fn section_order(category: PreferenceCategory) -> &'static [Option<&'static str>] {
    match category {
        // Neither category has ever carried a section.
        PreferenceCategory::Video | PreferenceCategory::LanguageAndAccessibility => &[None],
        PreferenceCategory::SoundAndMusic => &[Some("volume"), None],
        PreferenceCategory::Gameplay => &[None, Some("skin_parts")],
        PreferenceCategory::Controls => &[
            None,
            Some("movement"),
            Some("gameplay"),
            Some("inventory"),
            Some("multiplayer"),
            Some("creative"),
            Some("spectator"),
            Some("misc"),
            Some("debug"),
        ],
    }
}

/// Product concepts whose readable name is not derivable from their id.
///
/// Deliberately tiny. Everything else is named by [`humanize_id`], because a
/// hand-written table of nearly two hundred labels is a second copy of the
/// catalog that can disagree with it.
const SPECIAL_LABELS: &[(&str, &str)] = &[("fov", "gui-minecraft-preference-fov")];

/// Preferences whose zero is a named mode rather than a magnitude.
///
/// `guiScale` is Minecraft's own: `0` means "pick a scale that fits the
/// window". The domain has a proven minimum and no proven maximum, so this
/// names the one value that needs a word instead of inventing a list of
/// scales the compatibility research never established.
const ZERO_IS_AUTOMATIC: &[&str] = &["gui_scale"];

/// The `ephervex-i18n` id prefix curated preference labels live under.
const LABEL_MESSAGE_PREFIX: &str = "gui-minecraft-preference-";
/// The `ephervex-i18n` id prefix curated section labels live under.
const SECTION_MESSAGE_PREFIX: &str = "gui-minecraft-section-";

// ------------------------------------------------------------------- editors

/// Which reusable editor renders one descriptor's value.
///
/// Derived from the catalog's closed [`PreferencePresentation`], so a
/// presentation kind and its editor cannot drift apart, and no descriptor can
/// fall through to a free-text field: an enumeration, a language, and a key
/// binding each have exactly one editor that can express them.
#[derive(Debug, Clone, PartialEq)]
pub enum PreferenceEditor {
    Toggle,
    Numeric(NumericEditor),
    Enumeration,
    Language,
    KeyBinding,
    WindowSize,
    Text,
}

impl PreferenceEditor {
    /// A short stable name, which is also the tag the Slint row switches on.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Numeric(numeric) => {
                if numeric.slider {
                    "slider"
                } else {
                    "number"
                }
            }
            Self::Enumeration => "enum",
            Self::Language => "language",
            Self::KeyBinding => "keybinding",
            Self::WindowSize => "window",
            Self::Text => "text",
        }
    }
}

/// How one numeric preference is presented.
///
/// Bounds come from the descriptor and are never invented: an absent bound
/// means the research proved none, and nothing here fills one in. A slider
/// appears only where the proven metadata supports one, because a slider whose
/// step cannot reach a legal value would quietly refuse values the release
/// accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericEditor {
    pub scale: NumericScale,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// Whether a bounded slider is safe for this preference.
    pub slider: bool,
    /// A word to show instead of `0`, for a domain where zero is a mode.
    pub zero_label: Option<&'static str>,
}

/// How a numeric value reads on screen.
///
/// One closed choice rather than a set of flags, because the readings are
/// mutually exclusive and most combinations of them would be nonsense: a
/// percentage is not also a whole number, and degrees are not also a
/// percentage. Every variant is presentation only - the typed value that
/// reaches core is the same either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericScale {
    /// A whole number, shown as itself.
    Whole,
    /// A decimal, shown as itself.
    Decimal,
    /// A decimal in a proven `0..=1` domain, shown as whole percent, as the
    /// sound volumes are stored.
    Percent,
    /// A decimal in degrees, with Minecraft's own names for the two ends of
    /// the proven range.
    Degrees,
}

impl NumericScale {
    /// Whether the typed value is a decimal rather than a whole number.
    #[must_use]
    pub const fn is_decimal(self) -> bool {
        !matches!(self, Self::Whole)
    }
}

/// The editor one descriptor is rendered with.
///
/// The three application-owned exceptions are all presentation, never storage:
/// a percentage view of a proven `0..=1` volume, a degrees slider for the one
/// preference whose proven domain is exactly Minecraft's own FOV range, and a
/// word for a zero that means "automatic". None of them changes the typed
/// value, and none of them is per-release.
#[must_use]
pub fn editor_for(descriptor: &MinecraftPreferenceDescriptor) -> PreferenceEditor {
    match &descriptor.presentation {
        PreferencePresentation::Toggle { .. } => PreferenceEditor::Toggle,
        PreferencePresentation::Integer { min, max, step, .. } => {
            PreferenceEditor::Numeric(NumericEditor {
                scale: NumericScale::Whole,
                min: min.map(whole_as_number),
                max: max.map(whole_as_number),
                step: step.map(whole_as_number),
                // A whole-number domain with a proven floor, ceiling and step can
                // reach every value it allows, so a slider loses nothing.
                slider: min.is_some() && max.is_some() && step.is_some(),
                zero_label: ZERO_IS_AUTOMATIC
                    .contains(&descriptor.id.as_str())
                    .then_some("gui-minecraft-scale-automatic"),
            })
        }
        PreferencePresentation::Decimal { min, max, step, .. } => {
            let scale = if is_normalized_volume(descriptor, *min, *max) {
                NumericScale::Percent
            } else if descriptor.id == "fov" {
                NumericScale::Degrees
            } else {
                NumericScale::Decimal
            };
            PreferenceEditor::Numeric(NumericEditor {
                scale,
                min: *min,
                max: *max,
                step: *step,
                // A decimal slider would have to snap to its step, and most
                // proven decimal steps here are coarser than the values the
                // release accepts. Two exceptions carry an explicit rule: a
                // normalized volume, shown in whole percent, and the field of
                // view, whose proven domain is whole degrees.
                slider: matches!(scale, NumericScale::Percent | NumericScale::Degrees),
                zero_label: None,
            })
        }
        PreferencePresentation::Enum { .. } => PreferenceEditor::Enumeration,
        PreferencePresentation::Language { .. } => PreferenceEditor::Language,
        PreferencePresentation::KeyBinding { .. } => PreferenceEditor::KeyBinding,
        PreferencePresentation::WindowSize => PreferenceEditor::WindowSize,
        PreferencePresentation::Text { .. } => PreferenceEditor::Text,
    }
}

/// A catalog bound as the presentation layer's own number type.
///
/// Every bound in the product catalog is a small settings figure - a render
/// distance, a scale, a frame cap - so the conversion is exact for every value
/// that can appear here. It is applied to a proven bound and never to a value
/// a user typed, which is checked against the descriptor's own domain instead.
#[allow(clippy::cast_precision_loss)]
const fn whole_as_number(value: i64) -> f64 {
    value as f64
}

/// A presentation number as the toolkit's own, for a control's bounds.
///
/// Slint measures in `f32`. Everything narrowed here is a bound or a slider
/// position, never a stored value: what reaches core keeps its exact type.
#[allow(clippy::cast_possible_truncation)]
fn control_number(value: f64) -> f32 {
    value as f32
}

/// Whether a decimal preference is one of the sound volumes, whose proven
/// domain is the normalized `0..=1` range a percentage reads naturally as.
fn is_normalized_volume(
    descriptor: &MinecraftPreferenceDescriptor,
    min: Option<f64>,
    max: Option<f64>,
) -> bool {
    descriptor.section.as_deref() == Some("volume")
        && min == Some(0.0)
        && max == Some(1.0)
        && descriptor.category == PreferenceCategory::SoundAndMusic
}

// -------------------------------------------------------------------- labels

/// The readable name for one preference.
///
/// Curated Ephervex text when there is any, and otherwise a deterministic
/// reading of the stable product id. Neither the canonical `options.txt` field
/// name nor Mojang's translation key is ever the label: both are technical
/// identities that happen to be nearby, and neither is written for a reader.
#[must_use]
pub fn preference_label(descriptor: &MinecraftPreferenceDescriptor) -> String {
    if let Some((_, message)) = SPECIAL_LABELS
        .iter()
        .find(|(id, _)| *id == descriptor.id.as_str())
        && ephervex_i18n::has(message)
    {
        return lookup(message);
    }
    let message = format!("{LABEL_MESSAGE_PREFIX}{}", descriptor.id.replace('_', "-"));
    if ephervex_i18n::has(&message) {
        return lookup(&message);
    }
    humanize_id(&descriptor.id)
}

/// The readable name for one section inside a category.
#[must_use]
pub fn section_label(section: Option<&str>) -> String {
    let Some(section) = section else {
        return lookup("gui-minecraft-section-general");
    };
    let message = format!("{SECTION_MESSAGE_PREFIX}{}", section.replace('_', "-"));
    if ephervex_i18n::has(&message) {
        return lookup(&message);
    }
    humanize_words(section)
}

/// Prefixes that name a family rather than the preference itself.
///
/// The section heading already says "Key bindings" or "Volume", so repeating
/// it in every row's label only makes the labels harder to scan.
const FAMILY_PREFIXES: &[&str] = &["keybind_", "sound_volume_", "model_part_"];

/// Turns a stable product id into readable text.
///
/// Deterministic and total: every id produces a label, the same one every
/// time, without a per-preference rule. Sentence case rather than title case,
/// which is how Minecraft's own option labels read.
#[must_use]
pub fn humanize_id(id: &str) -> String {
    let trimmed = FAMILY_PREFIXES
        .iter()
        .find_map(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id);
    humanize_words(trimmed)
}

/// Initialisms whose reading is not "capitalize the first letter".
const INITIALISMS: &[(&str, &str)] = &[
    ("fov", "FOV"),
    ("fps", "FPS"),
    ("gl", "GL"),
    ("gui", "GUI"),
    ("id", "ID"),
    ("ui", "UI"),
    ("vsync", "VSync"),
];

/// Reads an underscore-separated identifier as a sentence.
fn humanize_words(text: &str) -> String {
    let mut label = String::with_capacity(text.len());
    for (index, word) in text.split('_').filter(|word| !word.is_empty()).enumerate() {
        if index > 0 {
            label.push(' ');
        }
        match INITIALISMS
            .iter()
            .find_map(|(token, reading)| (*token == word).then_some(*reading))
        {
            Some(reading) => label.push_str(reading),
            None if index == 0 => {
                let mut characters = word.chars();
                if let Some(first) = characters.next() {
                    label.extend(first.to_uppercase());
                    label.push_str(characters.as_str());
                }
            }
            None => label.push_str(word),
        }
    }
    label
}

// ---------------------------------------------------------------- draft rows

/// What the screen currently holds for one preference.
///
/// The aspect ratios the window size picker offers.
///
/// Closed and UI-owned: this is how the picker projects one edge from the
/// other, not something Minecraft stores. Each fixed ratio is an exact pair of
/// whole numbers, so `16:10` never becomes `1.6` and two ratios that round to
/// the same decimal stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowAspect {
    /// Width and height move independently.
    #[default]
    Free,
    Ratio16x9,
    Ratio16x10,
    Ratio21x9,
    Ratio4x3,
}

/// Every ratio the picker offers, in the order it presents them.
pub const WINDOW_ASPECTS: [WindowAspect; 5] = [
    WindowAspect::Free,
    WindowAspect::Ratio16x9,
    WindowAspect::Ratio16x10,
    WindowAspect::Ratio21x9,
    WindowAspect::Ratio4x3,
];

impl WindowAspect {
    /// The exact whole-number pair this ratio projects with, or `None` for a
    /// free-form size.
    #[must_use]
    pub const fn ratio(self) -> Option<(u32, u32)> {
        match self {
            Self::Free => None,
            Self::Ratio16x9 => Some((16, 9)),
            Self::Ratio16x10 => Some((16, 10)),
            Self::Ratio21x9 => Some((21, 9)),
            Self::Ratio4x3 => Some((4, 3)),
        }
    }

    /// How the ratio reads on screen.
    #[must_use]
    pub fn label(self) -> String {
        match self.ratio() {
            None => lookup("gui-minecraft-aspect-free"),
            Some((width, height)) => lookup_args(
                "gui-minecraft-aspect-ratio",
                [
                    ("first", Arg::from(width.to_string().as_str())),
                    ("second", Arg::from(height.to_string().as_str())),
                ],
            ),
        }
    }

    /// The ratio at one position in [`WINDOW_ASPECTS`].
    #[must_use]
    pub fn at(index: usize) -> Option<Self> {
        WINDOW_ASPECTS.get(index).copied()
    }

    /// Where this ratio sits in [`WINDOW_ASPECTS`].
    #[must_use]
    pub fn position(self) -> usize {
        WINDOW_ASPECTS
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }
}

/// Which edge the user changed, and therefore which one holds still while the
/// other is projected from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAxis {
    Width,
    Height,
}

/// The edge a window manager moved between two sizes of the same window.
///
/// Both edges compared against the window's own previous size, not against
/// the value on record: while a ratio is held those two deliberately differ,
/// and it is the window the user is dragging.
///
/// A corner moves both edges at once, so the one that moved further in
/// proportion is taken as the one being driven. Deciding this once for a whole
/// gesture is what keeps it steady: recomputing it per report would let a
/// diagonal drag flip between edges and jitter the size it produces. `None`
/// when neither edge moved, which is nothing to anchor.
#[must_use]
pub fn window_anchor(previous: WindowSize, reported: WindowSize) -> Option<WindowAxis> {
    let moved = |before: u32, after: u32| {
        let before = f64::from(before.max(1));
        (f64::from(after) - before).abs() / before
    };
    let across = moved(previous.width, reported.width);
    let down = moved(previous.height, reported.height);
    if across == 0.0 && down == 0.0 {
        return None;
    }
    Some(if down > across {
        WindowAxis::Height
    } else {
        WindowAxis::Width
    })
}

/// The height an exact ratio gives one width.
///
/// Rounded to the nearest whole number with ties going up, which is the one
/// rounding rule every projection here uses. Widened to 64 bits first, so a
/// large edge cannot overflow on the way, and refused rather than moved when
/// the result is not a size this build accepts: a projection landing under the
/// minimum edge is not a size to quietly repair.
#[must_use]
pub fn project_height(width: u32, ratio: (u32, u32)) -> Option<u32> {
    project_edge(width, ratio.1, ratio.0)
}

/// The width an exact ratio gives one height, by the same rule.
#[must_use]
pub fn project_width(height: u32, ratio: (u32, u32)) -> Option<u32> {
    project_edge(height, ratio.0, ratio.1)
}

/// `edge * numerator / denominator`, rounded half up, or `None` when the
/// result is not a usable edge.
fn project_edge(edge: u32, numerator: u32, denominator: u32) -> Option<u32> {
    let denominator = u64::from(denominator);
    if denominator == 0 {
        return None;
    }
    let scaled = u64::from(edge).checked_mul(u64::from(numerator))?;
    let rounded = scaled.checked_add(denominator / 2)? / denominator;
    let rounded = u32::try_from(rounded).ok()?;
    (rounded >= WindowSize::MINIMUM_EDGE).then_some(rounded)
}

/// The window Minecraft itself opens at.
///
/// Somewhere to start and nothing more: it is the size the sizing window
/// appears at for a row that has none of its own, and it is never stored. An
/// override exists only once the user has actually resized that window.
pub const WINDOW_START: WindowSize = WindowSize {
    width: 854,
    height: 480,
};

/// What the screen holds for one preference.
///
/// The product has one axis: a preference either has an override this build
/// applies, or it has none. Choosing a value is what creates the override and
/// Reset is what removes it, so there is no second action to perform and no
/// state between the two.
///
/// `managed` is kept because configuration can still describe one: a value
/// stored by an older Ephervex that was deliberately not applied. That is read
/// faithfully and left alone, and is never something this screen produces. A
/// toggle's `Boolean(false)` is a real override, as distinct from having none
/// as it has always been.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DraftState {
    /// Whether Ephervex applies this preference to new instances.
    pub managed: bool,
    /// The stored value, or `None` when nothing is stored at all.
    pub value: Option<MinecraftPreferenceValue>,
}

impl DraftState {
    /// Whether this build applies a value here.
    ///
    /// A value configuration retains without applying is not an override: the
    /// row reads as having none, and what is stored stays stored until that
    /// exact row is edited or reset.
    #[must_use]
    pub const fn is_overridden(&self) -> bool {
        self.managed && self.value.is_some()
    }

    /// Whether anything is stored for this preference at all.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        self.value.is_none()
    }

    /// An override at one value.
    #[must_use]
    const fn overriding(value: MinecraftPreferenceValue) -> Self {
        Self {
            managed: true,
            value: Some(value),
        }
    }
}

/// Whether a row currently overrides what Minecraft would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueOrigin {
    /// This build applies the value shown.
    Overridden,
    /// This build applies nothing here, so the row shows what the catalog
    /// proved Minecraft does. Showing it stores nothing.
    NotOverridden,
}

impl ValueOrigin {
    /// The `ephervex-i18n` id describing this state to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Overridden => "gui-minecraft-state-overridden",
            Self::NotOverridden => "gui-minecraft-state-not-overridden",
        }
    }
}

/// What one exact release does with one row, as the preview shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityStatus {
    /// Proven for this release, and the drafted value encodes.
    Supported,
    /// This release provably has no such setting.
    Unavailable,
    /// Nothing is established about this release's handling of it.
    Unverified,
    /// The release has the setting and cannot express the drafted value.
    NotRepresentable,
}

impl CompatibilityStatus {
    /// A short stable name, which is also the tag the Slint badge switches on.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unavailable => "unavailable",
            Self::Unverified => "unverified",
            Self::NotRepresentable => "unrepresentable",
        }
    }

    /// The `ephervex-i18n` id for the badge's short text.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Supported => "gui-minecraft-compat-supported",
            Self::Unavailable => "gui-minecraft-compat-unavailable",
            Self::Unverified => "gui-minecraft-compat-unverified",
            Self::NotRepresentable => "gui-minecraft-compat-unrepresentable",
        }
    }

    /// The `ephervex-i18n` id for the sentence explaining a row that will not
    /// be written, or `None` when there is nothing to explain.
    #[must_use]
    pub const fn detail_message_id(self) -> Option<&'static str> {
        match self {
            Self::Supported => None,
            Self::Unavailable => Some("gui-minecraft-compat-unavailable-detail"),
            Self::Unverified => Some("gui-minecraft-compat-unverified-detail"),
            Self::NotRepresentable => Some("gui-minecraft-compat-unrepresentable-detail"),
        }
    }
}

/// What the language dropdown has to work with.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LanguageCatalogState {
    /// Nothing has been asked for yet.
    #[default]
    Idle,
    /// A request is in flight.
    Loading,
    /// The release's own catalog, in the order Mojang's metadata lists it.
    Available(Vec<LanguageEntry>),
    /// The catalog could not be resolved. Only the language row is affected.
    Unavailable,
}

/// The raw text a numeric or window-size field currently holds.
///
/// Kept separately from the typed draft so a partly typed number does not
/// fight the user by reformatting itself, and so text that is not a number at
/// all can be shown back with a validation state instead of being discarded.
#[derive(Debug, Clone, PartialEq)]
enum PendingText {
    Number(String),
    Window { width: String, height: String },
}

// -------------------------------------------------------------------- model

/// The whole settings screen's state.
///
/// The flags are independent switches over one screen (two filters, a save in
/// flight, a window held to a ratio), not a state machine a type could model.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
pub struct MinecraftDefaultsModel {
    /// Every shipped product preference, in catalog order, as core reports it.
    entries: Vec<MinecraftPreferenceEntry>,
    /// What core last confirmed, by id. Rows core has nothing stored for are
    /// simply absent.
    baseline: BTreeMap<PreferenceId, DraftState>,
    /// The user's unsaved edits. Only rows that actually differ from the
    /// baseline live here, so emptiness is exactly "nothing to save".
    overlay: BTreeMap<PreferenceId, DraftState>,
    /// Raw field text for rows being typed into.
    pending: BTreeMap<PreferenceId, PendingText>,
    /// The release the bundled catalog was generated from.
    catalog_minecraft_version: String,
    /// How many configured ids this build has no descriptor for.
    unrecognized: usize,
    /// How many configured values this build could not read.
    unusable: usize,
    category: usize,
    query: String,
    show_advanced: bool,
    only_overridden: bool,
    capturing: Option<PreferenceId>,
    preview: Option<MinecraftPreferencePreview>,
    preview_release: String,
    languages: LanguageCatalogState,
    /// The outstanding compatibility-preview request, and the outstanding
    /// language request.
    preview_requests: RequestChannel,
    language_requests: RequestChannel,
    /// Whether a save is in flight.
    ///
    /// The screen disables itself while one is, so only a completion may
    /// clear this. It lives beside the draft rather than in the view, so the
    /// flag and the draft it guards cannot drift apart.
    saving: bool,
    /// The window size picker's own state.
    ///
    /// How the picker projects one edge from the other, and nothing Minecraft
    /// stores: no edit carries it, no configuration row holds it, and it
    /// starts over with the screen.
    window_aspect: WindowAspect,
    window_locked: bool,
}

impl MinecraftDefaultsModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces what core has confirmed, keeping the user's unsaved edits.
    ///
    /// A refresh happens for reasons that have nothing to do with this screen,
    /// so it must never be able to throw away a draft. Edits that the new
    /// baseline has caught up with stop being edits, which is the same rule
    /// that applies when a user types a value back to what it was.
    pub fn apply_snapshot(&mut self, snapshot: MinecraftPreferenceSnapshot) {
        self.catalog_minecraft_version = snapshot.catalog_minecraft_version;
        self.unrecognized = snapshot.unrecognized;
        self.unusable = snapshot.unusable.len();
        self.baseline = snapshot
            .entries
            .iter()
            .filter_map(|entry| {
                let id = PreferenceId::parse(entry.id()).ok()?;
                entry.value.clone().map(|value| {
                    (
                        id,
                        DraftState {
                            managed: entry.managed,
                            value: Some(value),
                        },
                    )
                })
            })
            .collect();
        self.entries = snapshot.entries;
        self.overlay
            .retain(|id, state| *state != self.baseline.get(id).cloned().unwrap_or_default());
        if self.preview_release.is_empty() {
            self.preview_release
                .clone_from(&self.catalog_minecraft_version);
        }
    }

    /// Drops the raw field text, so every editor reads from the typed draft
    /// again. Used once a save has been confirmed.
    pub fn forget_pending_text(&mut self) {
        self.pending.clear();
    }

    /// Abandons every unsaved edit, restoring the last confirmed state.
    pub fn discard(&mut self) {
        self.overlay.clear();
        self.pending.clear();
        self.capturing = None;
    }

    /// Whether anything is unsaved.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        !self.overlay.is_empty()
    }

    /// How many preferences the draft would change.
    #[must_use]
    pub fn dirty_count(&self) -> usize {
        self.overlay.len()
    }

    /// Whether saving is allowed: something to save, nothing invalid, and no
    /// save already in flight.
    #[must_use]
    pub fn can_save(&self) -> bool {
        !self.saving && self.is_dirty() && self.invalid_ids().is_empty()
    }

    /// Whether a save is in flight.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.saving
    }

    /// Takes the batch to submit, marking a save as in flight.
    ///
    /// `None` when there is nothing to save, something invalid to save, or a
    /// save already running, so one click cannot become two batches.
    pub fn begin_save(&mut self) -> Option<Vec<ephervex_core::PreferenceEdit>> {
        if !self.can_save() {
            return None;
        }
        self.saving = true;
        Some(self.edits())
    }

    /// Records that a submitted save reached a terminal outcome.
    ///
    /// A refusal is as terminal as a success: core validates the whole batch
    /// before writing any of it, so a refused save changed nothing and the
    /// draft the user still has is the right thing to keep. All that ends
    /// either way is the screen holding itself disabled.
    pub fn finish_save(&mut self, succeeded: bool) {
        self.saving = false;
        if succeeded {
            // The confirmed snapshot has already folded in what core wrote,
            // so the only thing left over is raw field text.
            self.forget_pending_text();
        }
    }

    /// The one atomic batch this draft becomes.
    ///
    /// One edit per changed preference and nothing else: rows the user did not
    /// touch are not named, and ids with no descriptor cannot be named at all,
    /// which is what leaves both untouched in the configuration file.
    #[must_use]
    pub fn edits(&self) -> Vec<ephervex_core::PreferenceEdit> {
        use ephervex_core::{PreferenceChange, PreferenceEdit};
        self.overlay
            .iter()
            .map(|(id, state)| match &state.value {
                // Choosing a value is what creates the override, so every
                // value this screen sends is one it applies. There is no edit
                // it can make that records a value without applying it.
                Some(value) => PreferenceEdit {
                    id: id.clone(),
                    change: PreferenceChange::Set {
                        managed: true,
                        value: value.clone(),
                    },
                },
                None => PreferenceEdit::reset(id.clone()),
            })
            .collect()
    }

    /// What the screen holds for one preference right now.
    #[must_use]
    pub fn state_of(&self, id: &PreferenceId) -> DraftState {
        self.overlay
            .get(id)
            .or_else(|| self.baseline.get(id))
            .cloned()
            .unwrap_or_default()
    }

    /// Records one preference's state, or forgets the edit when it matches
    /// what core already has.
    fn set_state(&mut self, id: &PreferenceId, state: DraftState) {
        if state == self.baseline.get(id).cloned().unwrap_or_default() {
            self.overlay.remove(id);
        } else {
            self.overlay.insert(id.clone(), state);
        }
    }

    /// The descriptor for one id, when this build ships one.
    #[must_use]
    pub fn descriptor(&self, id: &PreferenceId) -> Option<&MinecraftPreferenceDescriptor> {
        self.entries
            .iter()
            .find(|entry| entry.id() == id.as_str())
            .map(|entry| &entry.descriptor)
    }

    /// The value shown for a row that has none of its own: the catalog's
    /// proven default, when the research established one.
    ///
    /// Presentation only. Nothing here writes it into the draft, so opening
    /// the screen cannot turn an absent preference into a remembered one.
    #[must_use]
    pub fn catalog_default(&self, id: &PreferenceId) -> Option<MinecraftPreferenceValue> {
        catalog_default_of(self.descriptor(id)?)
    }

    /// The value a row's editor shows: the draft's own, else the catalog's
    /// proven default, else nothing.
    #[must_use]
    pub fn shown_value(&self, id: &PreferenceId) -> Option<MinecraftPreferenceValue> {
        let state = self.state_of(id);
        if state.is_overridden() {
            return state.value;
        }
        // Including for a value configuration retains without applying: the
        // row is not overridden, so it shows what Minecraft would do rather
        // than a value nothing is going to write.
        self.catalog_default(id)
    }

    /// Whether this row overrides what Minecraft would do.
    #[must_use]
    pub fn origin_of(&self, id: &PreferenceId) -> ValueOrigin {
        if self.state_of(id).is_overridden() {
            ValueOrigin::Overridden
        } else {
            ValueOrigin::NotOverridden
        }
    }
}

#[cfg(test)]
impl MinecraftDefaultsModel {
    /// Places both request counters one step from exhaustion.
    ///
    /// Test-only: the boundary the allocators guard is otherwise unreachable,
    /// and it is the one case where reusing an identity would go unnoticed.
    fn exhaust_request_identities(&mut self) {
        self.preview_requests.next = u64::MAX - 1;
        self.language_requests.next = u64::MAX - 1;
    }
}

/// The proven default one descriptor carries, as a typed value.
///
/// `None` where the analyzer established no default. That absence is a fact,
/// not a zero: nothing downstream may fill it in.
#[must_use]
pub fn catalog_default_of(
    descriptor: &MinecraftPreferenceDescriptor,
) -> Option<MinecraftPreferenceValue> {
    match &descriptor.presentation {
        PreferencePresentation::Toggle { default } => {
            default.map(MinecraftPreferenceValue::Boolean)
        }
        PreferencePresentation::Integer { default, .. } => {
            default.map(MinecraftPreferenceValue::Integer)
        }
        PreferencePresentation::Decimal { default, .. } => {
            default.and_then(MinecraftPreferenceValue::decimal)
        }
        PreferencePresentation::Enum { default, .. } => default
            .as_ref()
            .map(|value| MinecraftPreferenceValue::Enum(value.clone())),
        PreferencePresentation::Language { default } => default
            .as_ref()
            .map(|code| MinecraftPreferenceValue::LanguageCode(code.clone())),
        PreferencePresentation::Text { default } => default
            .as_ref()
            .map(|text| MinecraftPreferenceValue::Text(text.clone())),
        PreferencePresentation::KeyBinding { default } => {
            let default = default.as_ref()?;
            if default.unbound {
                return Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound));
            }
            let serialized = default.serialized.as_deref()?;
            glfw_name_to_binding(serialized).map(MinecraftPreferenceValue::KeyBinding)
        }
        // Two canonical fields with no product-wide proven pairing: a default
        // window size would be the display's, which the catalog cannot know.
        PreferencePresentation::WindowSize => None,
    }
}

/// Reads the catalog's proven key-binding default back into the typed model.
///
/// The catalog records a default the way the analyzed release serialized it,
/// which from 1.13 onward is `key.keyboard.<name>` or `key.mouse.<name>`.
/// Anything else is left unresolved rather than guessed: a default nobody can
/// read is a missing default, not a reason to bind an action to something.
fn glfw_name_to_binding(serialized: &str) -> Option<InputBinding> {
    if serialized == "key.keyboard.unknown" {
        return Some(InputBinding::Unbound);
    }
    if let Some(name) = serialized.strip_prefix("key.keyboard.") {
        return PhysicalKey::from_name(name).map(InputBinding::Keyboard);
    }
    if let Some(name) = serialized.strip_prefix("key.mouse.") {
        return MouseButton::from_name(name).map(InputBinding::Mouse);
    }
    None
}

// ------------------------------------------------------------------- editing

/// Why a typed field's contents are not a usable value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldProblem {
    /// Not a number at all, or not two numbers for a window size.
    Unreadable,
    /// A number, but outside the domain the descriptor proved.
    OutOfDomain,
}

impl FieldProblem {
    /// The `ephervex-i18n` id describing this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Unreadable => "gui-minecraft-value-unreadable",
            Self::OutOfDomain => "gui-minecraft-value-out-of-domain",
        }
    }
}

/// What a captured input did to a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The binding now holds the captured input.
    Bound,
    /// The binding is now deliberately bound to nothing.
    Cleared,
    /// Capture ended with the binding exactly as it was.
    Canceled,
    /// Nothing Minecraft can name was pressed, so nothing changed and capture
    /// stays open.
    Refused,
}

impl MinecraftDefaultsModel {
    /// Chooses a boolean value, which overrides the preference.
    pub fn set_boolean(&mut self, id: &PreferenceId, on: bool) {
        self.set_state(
            id,
            DraftState::overriding(MinecraftPreferenceValue::Boolean(on)),
        );
    }

    /// Chooses one of an enumeration's proven values by position in the
    /// descriptor's own list.
    pub fn set_enumeration(&mut self, id: &PreferenceId, index: usize) {
        let Some(descriptor) = self.descriptor(id) else {
            return;
        };
        let PreferencePresentation::Enum { values, .. } = &descriptor.presentation else {
            return;
        };
        let Some(chosen) = values.get(index) else {
            return;
        };
        let value = MinecraftPreferenceValue::Enum(chosen.technical_value.clone());
        self.set_state(id, DraftState::overriding(value));
    }

    /// Chooses a language by position in the release's own catalog.
    pub fn set_language(&mut self, id: &PreferenceId, index: usize) {
        let LanguageCatalogState::Available(languages) = &self.languages else {
            return;
        };
        let Some(entry) = languages.get(index) else {
            return;
        };
        let value = MinecraftPreferenceValue::LanguageCode(entry.code.clone());
        self.set_state(id, DraftState::overriding(value));
    }

    /// Chooses a numeric value from a bounded control.
    ///
    /// A percentage control reports whole percent of a proven `0..=1` domain;
    /// the value stored is the exact decimal, never the percentage.
    pub fn set_number(&mut self, id: &PreferenceId, number: f64) {
        let Some(descriptor) = self.descriptor(id) else {
            return;
        };
        let PreferenceEditor::Numeric(editor) = editor_for(descriptor) else {
            return;
        };
        let value = match editor.scale {
            // The control reports whole percent; the value stays the exact
            // normalized decimal the release stores.
            NumericScale::Percent => MinecraftPreferenceValue::decimal(number.round() / 100.0),
            NumericScale::Decimal | NumericScale::Degrees => {
                MinecraftPreferenceValue::decimal(number.round())
            }
            NumericScale::Whole => {
                whole_from_control(number).map(MinecraftPreferenceValue::Integer)
            }
        };
        let Some(value) = value else {
            return;
        };
        self.pending.remove(id);
        self.set_state(id, DraftState::overriding(value));
    }

    /// Records what a numeric field currently holds.
    ///
    /// Text that reads as a number becomes the drafted value even when it is
    /// outside the proven domain: the row is then shown as invalid and cannot
    /// be saved, which is a different thing from quietly moving the number to
    /// the nearest value the descriptor would accept.
    pub fn set_number_text(&mut self, id: &PreferenceId, text: &str) {
        self.pending
            .insert(id.clone(), PendingText::Number(text.to_owned()));
        let Some(descriptor) = self.descriptor(id).cloned() else {
            return;
        };
        if let Ok(value) = read_number(&descriptor, text) {
            self.set_state(id, DraftState::overriding(value));
        }
    }

    /// Records what the two window-size fields currently hold.
    pub fn set_window_size(&mut self, id: &PreferenceId, width: &str, height: &str) {
        self.pending.insert(
            id.clone(),
            PendingText::Window {
                width: width.to_owned(),
                height: height.to_owned(),
            },
        );
        if let Ok(value) = read_window_size(width, height) {
            self.set_state(id, DraftState::overriding(value));
        }
    }

    /// The ratio the window size picker is projecting with.
    ///
    /// Editor state, not Minecraft data: nothing here reaches configuration,
    /// and it is expected to start over when the screen is reopened.
    #[must_use]
    pub const fn window_aspect(&self) -> WindowAspect {
        self.window_aspect
    }

    /// Whether the picker is holding its ratio while the size changes.
    #[must_use]
    pub const fn window_locked(&self) -> bool {
        self.window_locked
    }

    /// The exact size the picker is presenting as an override right now, if
    /// any.
    ///
    /// Deliberately fail-closed, because this is the only value the ratio and
    /// the drag handle are ever allowed to move. Three things that look like a
    /// size are refused here: the catalog's proven default, which is
    /// presentation and belongs to Minecraft rather than to this build; a
    /// value configuration retains without applying, which this screen shows
    /// as no override and must not quietly activate; and field text that does
    /// not yet read as a size, because moving a size the user is part-way
    /// through retyping would discard what they typed.
    fn active_window_size(&self, id: &PreferenceId) -> Option<WindowSize> {
        let state = self.state_of(id);
        if !state.is_overridden() || self.problem_of(id).is_some() {
            return None;
        }
        // Text in the fields is what the user is looking at, so it is what a
        // ratio or a drag continues from once it reads as a size.
        if let Some(text) = self.pending.get(id) {
            let PendingText::Window { width, height } = text else {
                return None;
            };
            return match read_window_size(width, height) {
                Ok(MinecraftPreferenceValue::WindowSize(size)) => Some(size),
                _ => None,
            };
        }
        match state.value {
            Some(MinecraftPreferenceValue::WindowSize(size)) => Some(size),
            _ => None,
        }
    }

    /// The size to open the sizing window at.
    ///
    /// The row's own size when it has one, else the window Minecraft itself
    /// opens at. Reading it stores nothing: opening a window is a way to look
    /// at a size, not a way to choose one.
    #[must_use]
    pub fn window_start(&self, id: &PreferenceId) -> WindowSize {
        self.active_window_size(id).unwrap_or(WINDOW_START)
    }

    /// Chooses the ratio the picker projects with.
    ///
    /// A fixed ratio holds itself on selection and keeps the width, projecting
    /// the height from it. Free releases the hold and leaves both edges
    /// exactly as they are. Either way the ratio itself is presentation: it
    /// moves a size that is already an override, and a row that has none gets
    /// none invented for it.
    pub fn set_window_aspect(&mut self, id: &PreferenceId, aspect: WindowAspect) {
        self.window_aspect = aspect;
        self.window_locked = aspect.ratio().is_some();
        self.apply_window_ratio(id, WindowAxis::Width);
    }

    /// Holds or releases the selected ratio.
    ///
    /// Holding projects the height from the width that is already there;
    /// releasing keeps the ratio selected and both edges untouched, so the
    /// next edit moves one edge on its own.
    pub fn set_window_locked(&mut self, id: &PreferenceId, locked: bool) {
        self.window_locked = locked;
        if locked {
            self.apply_window_ratio(id, WindowAxis::Width);
        }
    }

    /// Records the two fields and, while the ratio is held, projects the edge
    /// the user did not touch from the one they did.
    ///
    /// The edited field is the anchor: entering a width keeps that width and
    /// moves the height, and entering a height does the reverse.
    ///
    /// The pair on screen is read exactly once here, and that reading is the
    /// only thing a projection may continue from. Text that is not a size
    /// therefore ends the commit: it stays exactly as typed, the row stays
    /// invalid and unsaveable, and no earlier value is projected in its place.
    /// Half-finished input is a state to report, not one to repair.
    pub fn commit_window_size(
        &mut self,
        id: &PreferenceId,
        width: &str,
        height: &str,
        anchor: WindowAxis,
    ) {
        self.pending.insert(
            id.clone(),
            PendingText::Window {
                width: width.to_owned(),
                height: height.to_owned(),
            },
        );
        let Ok(MinecraftPreferenceValue::WindowSize(size)) = read_window_size(width, height) else {
            return;
        };
        match self.projection_of(size, anchor) {
            Some(projected) => self.store_window_size(id, projected),
            None => self.set_state(
                id,
                DraftState::overriding(MinecraftPreferenceValue::WindowSize(size)),
            ),
        }
    }

    /// One size with the edge opposite `anchor` projected from the selected
    /// ratio, or `None` when there is nothing to change.
    ///
    /// Pure, and the single place a ratio turns into a size. `None` covers
    /// three cases that all mean "leave it alone": no ratio is held, the
    /// projection already agrees with the size, and the ratio cannot express
    /// this edge as a size this build accepts. The last one is refused rather
    /// than clamped, since a size the user did not choose is not a repair.
    fn projection_of(&self, size: WindowSize, anchor: WindowAxis) -> Option<WindowSize> {
        let ratio = self.window_aspect.ratio().filter(|_| self.window_locked)?;
        let projected = match anchor {
            WindowAxis::Width => WindowSize {
                width: size.width,
                height: project_height(size.width, ratio)?,
            },
            WindowAxis::Height => WindowSize {
                width: project_width(size.height, ratio)?,
                height: size.height,
            },
        };
        (projected != size).then_some(projected)
    }

    /// Projects the held ratio onto the size this row is already overriding.
    fn apply_window_ratio(&mut self, id: &PreferenceId, anchor: WindowAxis) {
        let Some(size) = self.active_window_size(id) else {
            return;
        };
        let Some(projected) = self.projection_of(size, anchor) else {
            return;
        };
        self.store_window_size(id, projected);
    }

    /// Records the size the window manager gave the sizing window.
    ///
    /// The window on screen is the value: whatever size it was dragged to is
    /// the exact size stored, with no scaling in between. `anchor` is the edge
    /// the user is driving, so a held ratio moves the other one.
    ///
    /// Returns the exact size now stored when it differs from `reported`, so
    /// the caller knows the window is not yet showing what the row holds;
    /// `None` when the two already agree, or when the size was refused. A size
    /// under the smallest window this build accepts is refused rather than
    /// grown: the window carries that minimum as its own limit, and a manager
    /// that ignores it does not get to store a size Minecraft would not take.
    pub fn fit_window(
        &mut self,
        id: &PreferenceId,
        reported: WindowSize,
        anchor: WindowAxis,
    ) -> Option<WindowSize> {
        if reported.width < WindowSize::MINIMUM_EDGE || reported.height < WindowSize::MINIMUM_EDGE {
            return None;
        }
        let exact = self.projection_of(reported, anchor).unwrap_or(reported);
        self.store_window_size(id, exact);
        (exact != reported).then_some(exact)
    }

    /// Records one exact size, dropping the field text it supersedes so both
    /// fields read back what was actually stored.
    fn store_window_size(&mut self, id: &PreferenceId, size: WindowSize) {
        self.pending.remove(id);
        self.set_state(
            id,
            DraftState::overriding(MinecraftPreferenceValue::WindowSize(size)),
        );
    }

    /// Records free text for the one presentation kind that takes it.
    pub fn set_text(&mut self, id: &PreferenceId, text: &str) {
        self.set_state(
            id,
            DraftState::overriding(MinecraftPreferenceValue::Text(text.to_owned())),
        );
    }

    /// Returns one row to "Ephervex has no opinion", in the draft only.
    ///
    /// Not the same as turning management off: that keeps the chosen value so
    /// it can be switched back on. This forgets it, which is what makes the
    /// next instance take whatever Minecraft itself would do.
    pub fn reset_row(&mut self, id: &PreferenceId) {
        self.pending.remove(id);
        if self.capturing.as_ref() == Some(id) {
            self.capturing = None;
        }
        self.set_state(id, DraftState::default());
    }

    /// Every row whose current contents cannot be saved.
    #[must_use]
    pub fn invalid_ids(&self) -> BTreeSet<PreferenceId> {
        let mut invalid = BTreeSet::new();
        for (id, text) in &self.pending {
            let Some(descriptor) = self.descriptor(id) else {
                continue;
            };
            let readable = match text {
                PendingText::Number(number) => read_number(descriptor, number).is_ok(),
                PendingText::Window { width, height } => read_window_size(width, height).is_ok(),
            };
            if !readable {
                invalid.insert(id.clone());
            }
        }
        for (id, state) in &self.overlay {
            let (Some(descriptor), Some(value)) = (self.descriptor(id), state.value.as_ref())
            else {
                continue;
            };
            if descriptor.validate_value(value).is_err() {
                invalid.insert(id.clone());
            }
        }
        invalid
    }

    /// The problem one row's contents have, when it has one.
    #[must_use]
    pub fn problem_of(&self, id: &PreferenceId) -> Option<FieldProblem> {
        let descriptor = self.descriptor(id)?;
        if let Some(text) = self.pending.get(id) {
            let read = match text {
                PendingText::Number(number) => read_number(descriptor, number).err(),
                PendingText::Window { width, height } => read_window_size(width, height).err(),
            };
            if let Some(problem) = read {
                return Some(problem);
            }
        }
        let value = self.overlay.get(id)?.value.as_ref()?;
        descriptor
            .validate_value(value)
            .err()
            .map(|_| FieldProblem::OutOfDomain)
    }
}

/// Reads a numeric field's text against one descriptor's proven domain.
fn read_number(
    descriptor: &MinecraftPreferenceDescriptor,
    text: &str,
) -> Result<MinecraftPreferenceValue, FieldProblem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(FieldProblem::Unreadable);
    }
    let value = match descriptor.value_shape() {
        PreferenceValueShape::Integer => MinecraftPreferenceValue::Integer(
            trimmed.parse().map_err(|_| FieldProblem::Unreadable)?,
        ),
        PreferenceValueShape::Decimal => {
            let number: f64 = trimmed.parse().map_err(|_| FieldProblem::Unreadable)?;
            MinecraftPreferenceValue::decimal(number).ok_or(FieldProblem::Unreadable)?
        }
        _ => return Err(FieldProblem::Unreadable),
    };
    descriptor
        .validate_value(&value)
        .map_err(|_| FieldProblem::OutOfDomain)?;
    Ok(value)
}

/// Reads the two window-size fields as one value.
///
/// A window size is one preference, so a half-filled pair is unreadable rather
/// than a width with a guessed height.
fn read_window_size(width: &str, height: &str) -> Result<MinecraftPreferenceValue, FieldProblem> {
    let width: u32 = width.trim().parse().map_err(|_| FieldProblem::Unreadable)?;
    let height: u32 = height
        .trim()
        .parse()
        .map_err(|_| FieldProblem::Unreadable)?;
    if width < WindowSize::MINIMUM_EDGE || height < WindowSize::MINIMUM_EDGE {
        return Err(FieldProblem::OutOfDomain);
    }
    Ok(MinecraftPreferenceValue::WindowSize(WindowSize {
        width,
        height,
    }))
}

// ------------------------------------------------------------- key capture

/// Reads one focused key press as a physical key Minecraft can name.
///
/// The fallback path, used on a backend whose raw keyboard events this build
/// cannot reach. Where it can, the key itself is read instead and every key
/// the game names can be bound; see the `physical_keys` module.
///
/// # What the toolkit actually supplies
///
/// Slint's `KeyEvent` carries a `text`, a set of modifier booleans, and a
/// repeat flag. There is no scan code, no physical key, and no key location
/// anywhere in its public API, and its winit backend turns every printable
/// key into `Key::Character(text)` with the location discarded. So a printed
/// character identifies a *character*, never a key: the same `1` comes from
/// the number row and from the keypad, the same `z` comes from a different
/// physical key on QWERTY and on QWERTZ, and `/` is not the slash key on most
/// layouts in Europe.
///
/// A key is therefore only accepted here when the code Slint reports picks out
/// exactly one member of the shipped model. That is true of the function
/// keys, of the modifiers the backend disambiguates by location, and of a
/// handful of keys with no twin of any kind. It is not true of anything else,
/// and everything else is refused.
///
/// # What that rules out, and why
///
/// * **Every printable character.** Promoting `a` to `A`, `7` to `Digit7`, or
///   `/` to `Slash` reads a US layout into evidence that does not contain
///   one, and would store the keypad `7` as the number-row `7`.
/// * **Enter, the arrows, Insert, Home, End, Page Up and Page Down.** The
///   backend matches these without a location guard, so the keypad key of the
///   same name arrives as the main-row one. The model names both, and
///   choosing between them is not this frontend's to make.
/// * **Alt.** The backend guards Shift, Control and Meta by location and does
///   not guard Alt, so a left and a right Alt are the same event here.
///   `AltGr` is a different key rather than a position of that one, and is
///   the one right-hand modifier the toolkit does identify.
///
/// Refusing costs a binding the user could otherwise have set. Guessing costs
/// them a binding they did not set, in a file Ephervex writes into instances
/// on their behalf, and only one of those is recoverable by pressing another
/// key.
#[must_use]
pub fn physical_key_of(character: char) -> Option<PhysicalKey> {
    use slint::platform::Key;

    // Every entry is a code the backend produces for exactly one member of
    // the shipped model. A key needing a location the backend does not send,
    // or sharing its code with a keypad key, is deliberately absent.
    let identified = [
        (Key::F1, PhysicalKey::F1),
        (Key::F2, PhysicalKey::F2),
        (Key::F3, PhysicalKey::F3),
        (Key::F4, PhysicalKey::F4),
        (Key::F5, PhysicalKey::F5),
        (Key::F6, PhysicalKey::F6),
        (Key::F7, PhysicalKey::F7),
        (Key::F8, PhysicalKey::F8),
        (Key::F9, PhysicalKey::F9),
        (Key::F10, PhysicalKey::F10),
        (Key::F11, PhysicalKey::F11),
        (Key::F12, PhysicalKey::F12),
        // Guarded by `KeyLocation` in the backend, so left and right are
        // genuinely separate events rather than one standing for both.
        (Key::Shift, PhysicalKey::LeftShift),
        (Key::ShiftR, PhysicalKey::RightShift),
        (Key::Control, PhysicalKey::LeftControl),
        (Key::ControlR, PhysicalKey::RightControl),
        (Key::Meta, PhysicalKey::LeftWin),
        (Key::MetaR, PhysicalKey::RightWin),
        // The level-three modifier, which is its own key rather than a
        // position of Alt. A right Alt on a layout without one reports plain
        // Alt instead, and plain Alt is refused.
        (Key::AltGr, PhysicalKey::RightAlt),
        // No keypad twin, no left and right, no layout involvement.
        (Key::Tab, PhysicalKey::Tab),
        (Key::Space, PhysicalKey::Space),
        (Key::CapsLock, PhysicalKey::CapsLock),
        (Key::ScrollLock, PhysicalKey::ScrollLock),
        (Key::Pause, PhysicalKey::Pause),
        (Key::SysReq, PhysicalKey::PrintScreen),
    ];
    identified
        .into_iter()
        .find_map(|(code, key)| (char::from(code) == character).then_some(key))
}

/// Reads one focused mouse press as a button Minecraft can name.
///
/// The reported button becomes the zero-based index Minecraft's own input
/// layer uses, and that index is the only way in to [`MouseButton`], so a
/// button this build cannot name produces nothing rather than the button that
/// happens to sit next to it.
///
/// # What the toolkit actually proves
///
/// Slint's `PointerEvent` carries a `PointerEventButton`, which is a closed
/// set of five named buttons plus `Other`, and its backend discards the raw
/// index when it builds that value. So `Other` arrives with no way to tell
/// which button it was, and the buttons past Forward - `Button6` through
/// `Button8` in the shipped model - cannot be captured here at all. They stay
/// representable, storable, and writable; this frontend simply has no event
/// that identifies them, and refuses rather than guessing.
#[must_use]
pub fn mouse_button_of(reported: &str) -> Option<MouseButton> {
    let index = match reported {
        "left" => 0,
        "right" => 1,
        "middle" => 2,
        "back" => 3,
        "forward" => 4,
        // Including "other", which the toolkit reports without an index.
        _ => return None,
    };
    MouseButton::from_index(index)
}

/// How one binding reads on screen.
///
/// Never the durable configuration text: `key:left.shift` is what a file
/// holds, and "Left Shift" is what a person reads.
#[must_use]
pub fn binding_label(binding: Option<InputBinding>) -> String {
    match binding {
        None => lookup("gui-minecraft-binding-unset"),
        Some(InputBinding::Unbound) => lookup("gui-minecraft-binding-unbound"),
        Some(InputBinding::Keyboard(key)) => humanize_words(&key.name().replace('.', "_")),
        Some(InputBinding::Mouse(button)) => match button {
            MouseButton::Left => lookup("gui-minecraft-mouse-left"),
            MouseButton::Right => lookup("gui-minecraft-mouse-right"),
            MouseButton::Middle => lookup("gui-minecraft-mouse-middle"),
            numbered => lookup_args(
                "gui-minecraft-mouse-numbered",
                [("value", Arg::from(numbered.name()))],
            ),
        },
    }
}

impl MinecraftDefaultsModel {
    /// The row currently waiting for an input, if any.
    #[must_use]
    pub fn capturing(&self) -> Option<&PreferenceId> {
        self.capturing.as_ref()
    }

    /// Puts one binding row into capture mode.
    pub fn begin_capture(&mut self, id: &PreferenceId) {
        self.capturing = Some(id.clone());
    }

    /// Leaves capture mode with the binding exactly as it was.
    ///
    /// The one exit that changes nothing, used by Escape and by the control
    /// losing focus alike, so an abandoned capture never rebinds anything.
    pub fn cancel_capture(&mut self) {
        self.capturing = None;
    }

    /// Offers one focused key press to the row that is capturing.
    ///
    /// The fallback path, taking the character the toolkit reported. Escape
    /// leaves the binding alone and Delete or Backspace clears it. Anything a
    /// character does not identify exactly is refused, leaving capture open so
    /// another key can be tried. See [`physical_key_of`].
    pub fn capture_key(&mut self, id: &PreferenceId, character: char) -> CaptureOutcome {
        use slint::platform::Key;

        if self.capturing.as_ref() != Some(id) {
            return CaptureOutcome::Canceled;
        }
        if character == char::from(Key::Escape) {
            self.capturing = None;
            return CaptureOutcome::Canceled;
        }
        // Both leave capture without binding either key, so the fact that the
        // backend cannot tell a keypad Delete from a main-row one does not
        // matter: neither is being stored.
        if character == char::from(Key::Delete) || character == char::from(Key::Backspace) {
            self.capturing = None;
            self.set_binding(id, InputBinding::Unbound);
            return CaptureOutcome::Cleared;
        }
        let Some(key) = physical_key_of(character) else {
            return CaptureOutcome::Refused;
        };
        self.capturing = None;
        self.set_binding(id, InputBinding::Keyboard(key));
        CaptureOutcome::Bound
    }

    /// Offers one physical key press to the row that is capturing.
    ///
    /// The same three exits [`Self::capture_key`] has: Escape leaves the
    /// binding alone, Delete and Backspace clear it, and anything else binds.
    /// There is no fourth, because the windowing layer names the key and there
    /// is nothing left to refuse. Those three stay unbindable in exchange,
    /// which is what the game does with Escape itself and what the row's own
    /// clear action covers for the other two.
    pub fn capture_physical_key(&mut self, id: &PreferenceId, key: PhysicalKey) -> CaptureOutcome {
        if self.capturing.as_ref() != Some(id) {
            return CaptureOutcome::Canceled;
        }
        if key == PhysicalKey::Escape {
            self.capturing = None;
            return CaptureOutcome::Canceled;
        }
        if key == PhysicalKey::Delete || key == PhysicalKey::Backspace {
            self.capturing = None;
            self.set_binding(id, InputBinding::Unbound);
            return CaptureOutcome::Cleared;
        }
        self.capturing = None;
        self.set_binding(id, InputBinding::Keyboard(key));
        CaptureOutcome::Bound
    }

    /// Offers one mouse button to the row that is capturing.
    ///
    /// Takes the button rather than the name the toolkit reported, so a press
    /// anywhere in the window binds. The control says "press any key or mouse
    /// button", and a click that only counted on the control itself made that
    /// a smaller promise than it reads as.
    pub fn capture_mouse_button(
        &mut self,
        id: &PreferenceId,
        button: MouseButton,
    ) -> CaptureOutcome {
        if self.capturing.as_ref() != Some(id) {
            return CaptureOutcome::Canceled;
        }
        self.capturing = None;
        self.set_binding(id, InputBinding::Mouse(button));
        CaptureOutcome::Bound
    }

    /// Offers one focused mouse press to the row that is capturing.
    pub fn capture_mouse(&mut self, id: &PreferenceId, reported: &str) -> CaptureOutcome {
        if self.capturing.as_ref() != Some(id) {
            return CaptureOutcome::Canceled;
        }
        let Some(button) = mouse_button_of(reported) else {
            return CaptureOutcome::Refused;
        };
        self.capturing = None;
        self.set_binding(id, InputBinding::Mouse(button));
        CaptureOutcome::Bound
    }

    /// Binds one row to nothing at all, which is a chosen value.
    pub fn clear_binding(&mut self, id: &PreferenceId) {
        self.capturing = None;
        self.set_binding(id, InputBinding::Unbound);
    }

    fn set_binding(&mut self, id: &PreferenceId, binding: InputBinding) {
        self.set_state(
            id,
            DraftState::overriding(MinecraftPreferenceValue::KeyBinding(binding)),
        );
    }

    /// Which explicitly chosen bindings clash, and with what.
    ///
    /// Minecraft itself allows two actions on one key and shows the clash, so
    /// this warns and never rewrites. Only values the draft actually holds
    /// take part: a row showing a catalog default has not been chosen, and
    /// claiming a conflict against one would be claiming something nobody
    /// asked for. Being bound to nothing is not a clash.
    #[must_use]
    pub fn binding_conflicts(&self) -> BTreeMap<PreferenceId, Vec<String>> {
        let mut chosen: BTreeMap<InputBinding, Vec<(PreferenceId, String)>> = BTreeMap::new();
        for entry in &self.entries {
            let Ok(id) = PreferenceId::parse(entry.id()) else {
                continue;
            };
            let Some(MinecraftPreferenceValue::KeyBinding(binding)) = self.state_of(&id).value
            else {
                continue;
            };
            if binding == InputBinding::Unbound {
                continue;
            }
            chosen
                .entry(binding)
                .or_default()
                .push((id, preference_label(&entry.descriptor)));
        }
        let mut conflicts = BTreeMap::new();
        for holders in chosen.values().filter(|holders| holders.len() > 1) {
            for (id, _) in holders {
                let others = holders
                    .iter()
                    .filter(|(other, _)| other != id)
                    .map(|(_, label)| label.clone())
                    .collect();
                conflicts.insert(id.clone(), others);
            }
        }
        conflicts
    }
}

// ------------------------------------------------------- screen-level state

impl MinecraftDefaultsModel {
    /// Which category tab is selected.
    #[must_use]
    pub const fn category_index(&self) -> usize {
        self.category
    }

    /// The category tab's own typed identity.
    #[must_use]
    pub fn category(&self) -> PreferenceCategory {
        CATEGORIES
            .get(self.category)
            .copied()
            .unwrap_or(PreferenceCategory::Gameplay)
    }

    /// Selects a category tab. Draft state is untouched: a tab decides what is
    /// shown, never what is chosen.
    pub fn select_category(&mut self, index: usize) {
        if index < CATEGORIES.len() {
            self.category = index;
            self.capturing = None;
        }
    }

    /// The current filter text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Filters the visible rows. Draft state is untouched.
    pub fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.query);
        self.capturing = None;
    }

    /// Whether the catalog has any developer-facing row, which is all the
    /// advanced switch reveals.
    #[must_use]
    pub fn has_advanced_rows(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.descriptor.tier == PresentationTier::Advanced)
    }

    /// Whether developer-facing rows are shown alongside the ordinary ones.
    #[must_use]
    pub const fn show_advanced(&self) -> bool {
        self.show_advanced
    }

    /// Shows or hides the advanced tier. Draft state is untouched, so an
    /// advanced row edited and then hidden is still saved.
    pub fn set_show_advanced(&mut self, show: bool) {
        self.show_advanced = show;
        self.capturing = None;
    }

    /// Whether only the preferences Ephervex writes are shown.
    #[must_use]
    pub const fn only_overridden(&self) -> bool {
        self.only_overridden
    }

    /// Narrows the list to what Ephervex writes, or widens it again. Draft
    /// state is untouched, exactly as the tier filter leaves it.
    pub fn set_only_overridden(&mut self, only: bool) {
        self.only_overridden = only;
        self.capturing = None;
    }

    /// Whether this preference survives the "only overridden" filter.
    ///
    /// An unsaved edit counts even when it is a reset: a row that vanished the
    /// moment it was cleared would take its own Reset button with it, leaving
    /// no way to undo the clearing.
    fn is_kept_by_override_filter(&self, id: &PreferenceId) -> bool {
        self.origin_of(id) == ValueOrigin::Overridden || self.overlay.contains_key(id)
    }

    /// The release the compatibility preview is resolved against.
    #[must_use]
    pub fn preview_release(&self) -> &str {
        &self.preview_release
    }

    /// The release the bundled product catalog was generated from, which is
    /// the preview's starting point.
    #[must_use]
    pub fn catalog_minecraft_version(&self) -> &str {
        &self.catalog_minecraft_version
    }

    /// Points the preview at one exact release.
    ///
    /// Only ever changes what the rows *say*: no configured value moves, and
    /// nothing is persisted, because global defaults are release-neutral.
    /// Whatever the previous release resolved to is dropped, so a row cannot
    /// keep showing an answer for a release nobody is looking at.
    pub fn set_preview_release(&mut self, release: &str) -> bool {
        if self.preview_release == release {
            return false;
        }
        release.clone_into(&mut self.preview_release);
        self.preview = None;
        self.languages = LanguageCatalogState::Loading;
        // Whatever either channel was waiting for was asked about the release
        // just left behind, so neither answer addresses anything now. Retiring
        // here rather than relying on the next request being minted keeps that
        // true even when no further identity can be handed out.
        self.preview_requests.retire();
        self.language_requests.retire();
        true
    }

    /// Records a resolved preview.
    pub fn apply_preview(&mut self, preview: MinecraftPreferencePreview) {
        self.preview = Some(preview);
    }

    /// Retires the outstanding compatibility preview and mints the identity
    /// of its successor, or refuses when no identity is left.
    pub fn next_preview_request(&mut self) -> Option<u64> {
        self.preview_requests.issue()
    }

    /// Whether a preview answer is the one currently being waited on.
    ///
    /// Both halves matter: the identity has to be the outstanding request
    /// rather than merely the highest ever minted, and the release has to be
    /// the one still selected.
    #[must_use]
    pub fn accepts_preview_result(&self, generation: u64, release: &str) -> bool {
        self.preview_requests.accepts(generation) && release == self.preview_release
    }

    /// Retires the outstanding language request and mints the identity of its
    /// successor, or refuses when no identity is left.
    pub fn next_language_request(&mut self) -> Option<u64> {
        self.language_requests.issue()
    }

    /// Whether a language answer is the one currently being waited on.
    #[must_use]
    pub fn accepts_language_result(&self, generation: u64, release: &str) -> bool {
        self.language_requests.accepts(generation) && release == self.preview_release
    }

    /// Whether the previewed release is known to offer the drafted language.
    ///
    /// The one preference whose validity is not a property of `options.txt`.
    /// Nothing is claimed without a catalog: an unresolved list leaves this
    /// unverified, which is what keeps a language failure from reaching any
    /// other preference.
    #[must_use]
    pub fn language_availability(&self) -> ephervex_core::LanguageAvailability {
        use ephervex_core::LanguageAvailability;

        let LanguageCatalogState::Available(languages) = &self.languages else {
            return LanguageAvailability::Unverified;
        };
        let drafted = self.entries.iter().find_map(|entry| {
            let id = PreferenceId::parse(entry.id()).ok()?;
            let state = self.state_of(&id);
            // Same rule as the drafted values themselves: a language
            // configuration retains without applying is not the language this
            // screen is choosing, so it settles nothing about a release.
            if !state.is_overridden() {
                return None;
            }
            match state.value {
                Some(MinecraftPreferenceValue::LanguageCode(code)) => Some(code),
                _ => None,
            }
        });
        match drafted {
            None => LanguageAvailability::Unverified,
            Some(code) if languages.iter().any(|entry| entry.code == code) => {
                LanguageAvailability::Verified
            }
            Some(_) => LanguageAvailability::Absent,
        }
    }

    /// Records what the language dropdown has to work with.
    pub fn apply_languages(&mut self, languages: LanguageCatalogState) {
        self.languages = languages;
    }

    /// What the language dropdown has to work with.
    #[must_use]
    pub const fn languages(&self) -> &LanguageCatalogState {
        &self.languages
    }

    /// How many configured ids this build has no descriptor for.
    #[must_use]
    pub const fn unrecognized(&self) -> usize {
        self.unrecognized
    }

    /// How many configured values this build could not read.
    #[must_use]
    pub const fn unusable(&self) -> usize {
        self.unusable
    }

    /// Every shipped product preference, in catalog order.
    #[must_use]
    pub fn entries(&self) -> &[MinecraftPreferenceEntry] {
        &self.entries
    }

    /// The draft as a set of configured preferences, for the release preview.
    ///
    /// Exactly what the user has chosen, saved or not: core resolves it
    /// against a release and decides representability, so no codec knowledge
    /// has to exist here.
    #[must_use]
    pub fn draft_preferences(&self) -> ephervex_core::ConfiguredPreferences {
        use ephervex_core::{ConfiguredPreference, ConfiguredPreferences};

        let mut draft = ConfiguredPreferences::new();
        for entry in &self.entries {
            let Ok(id) = PreferenceId::parse(entry.id()) else {
                continue;
            };
            let state = self.state_of(&id);
            // Only what this screen would actually apply. A value
            // configuration retains without applying is not part of the
            // question being asked: the row shows what Minecraft would do, so
            // answering about the retained value instead would attach a
            // compatibility error to a value nothing is going to write and
            // that the user cannot even see.
            if !state.is_overridden() {
                continue;
            }
            let Some(value) = state.value else {
                continue;
            };
            draft.insert(id, ConfiguredPreference::managed(value));
        }
        draft
    }

    /// What the previewed release does with one row.
    ///
    /// `None` while no preview has been resolved yet, which is a different
    /// thing from a release about which nothing is established.
    #[must_use]
    pub fn status_of(&self, id: &PreferenceId) -> Option<CompatibilityStatus> {
        let preview = self.preview.as_ref()?;
        if preview.unrepresentable.contains_key(id.as_str()) {
            return Some(CompatibilityStatus::NotRepresentable);
        }
        Some(match preview.support.get(id.as_str())? {
            ReleaseSupport::Authoritative => CompatibilityStatus::Supported,
            ReleaseSupport::Unsupported => CompatibilityStatus::Unavailable,
            ReleaseSupport::Unknown => CompatibilityStatus::Unverified,
        })
    }

    /// Why the previewed release cannot express one row's drafted value.
    #[must_use]
    pub fn skip_reason_of(&self, id: &PreferenceId) -> Option<SkipReason> {
        self.preview
            .as_ref()?
            .unrepresentable
            .get(id.as_str())
            .copied()
    }
}

/// Where one section sorts inside its category.
fn section_rank(category: PreferenceCategory, section: Option<&str>) -> (usize, String) {
    let order = section_order(category);
    match order.iter().position(|known| *known == section) {
        Some(index) => (index, String::new()),
        // Unlisted sorts after everything listed, by name, which keeps the
        // order total. Every category lists `None`, so an unlisted section
        // always has a name to be ordered by.
        None => (order.len(), section.unwrap_or_default().to_owned()),
    }
}

/// One enumeration value's readable label.
///
/// The upstream constant name when the catalog carries one, read as a word;
/// otherwise the exact technical value, which is a fallback rather than an
/// invention. Either way the value the draft stores is the technical one.
#[must_use]
pub fn enum_label(value: &ephervex_core::EnumValuePresentation) -> String {
    match value.enum_constant.as_deref() {
        Some(constant) if !constant.is_empty() => humanize_words(&constant.to_ascii_lowercase()),
        _ => value.technical_value.clone(),
    }
}

/// How one language reads in the dropdown.
#[must_use]
pub fn language_label(entry: &LanguageEntry) -> String {
    if entry.region.is_empty() {
        lookup_args(
            "gui-minecraft-language-label",
            [
                ("first", Arg::from(entry.name.as_str())),
                ("second", Arg::from(entry.code.as_str())),
            ],
        )
    } else {
        lookup_args(
            "gui-minecraft-language-label-region",
            [
                ("first", Arg::from(entry.name.as_str())),
                ("second", Arg::from(entry.region.as_str())),
                ("third", Arg::from(entry.code.as_str())),
            ],
        )
    }
}

// ---------------------------------------------------------------- rendering

/// How a field of view reads: degrees, plus Minecraft's own name for either
/// end of the proven range.
///
/// Presentation only. The typed value stays the number of degrees, and how a
/// given release persists that number is the codec's business, not this
/// screen's.
fn degrees_readout(number: f64, max: Option<f64>) -> String {
    /// Degrees are whole numbers, so anything closer than this is the same
    /// setting. Comparing exactly would make the reading depend on how a
    /// value happened to be rounded on its way here.
    const SAME_DEGREE: f64 = 0.5;
    /// The field of view Minecraft itself calls normal.
    const NORMAL_DEGREES: f64 = 70.0;

    let degrees = lookup_args(
        "gui-minecraft-degrees",
        [("value", Arg::from(trim_number(number).as_str()))],
    );
    let same = |other: f64| (number - other).abs() < SAME_DEGREE;
    let endpoint = if max.is_some_and(same) {
        Some(lookup("gui-fov-quake-pro"))
    } else if same(NORMAL_DEGREES) {
        Some(lookup("gui-fov-normal"))
    } else {
        None
    };
    match endpoint {
        Some(name) => lookup_args(
            "gui-minecraft-readout-named",
            [
                ("first", Arg::from(degrees.as_str())),
                ("second", Arg::from(name.as_str())),
            ],
        ),
        None => degrees,
    }
}

/// How one numeric value reads, given how its preference is presented.
fn numeric_readout(number: f64, numeric: &NumericEditor) -> String {
    if let Some(word) = numeric.zero_label
        && number.abs() < f64::EPSILON
    {
        return lookup(word);
    }
    match numeric.scale {
        NumericScale::Percent => lookup_args(
            "gui-minecraft-percent",
            [("value", Arg::from(trim_number(number).as_str()))],
        ),
        NumericScale::Degrees => degrees_readout(number, numeric.max),
        NumericScale::Whole | NumericScale::Decimal => trim_number(number),
    }
}

/// A control's reported position as the whole number it stands for.
///
/// `None` when the position is not a whole number this build can express,
/// which no bounded control can produce: refusing is still cheaper than
/// storing a number nobody chose.
#[allow(clippy::cast_possible_truncation)]
fn whole_from_control(number: f64) -> Option<i64> {
    let rounded = number.round();
    (rounded.abs() < 9.007_199_254_740_992e15).then_some(rounded as i64)
}

/// Formats a number without a trailing run of zeros.
fn trim_number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    text
}

impl MinecraftDefaultsModel {
    /// Every product preference the selected tab, tier, and filter show.
    ///
    /// Identity is the descriptor's own, so the same preference is the same
    /// row whatever the filter is doing, and every shipped descriptor is
    /// reachable through exactly one category.
    #[must_use]
    pub fn visible_entries(&self) -> Vec<&MinecraftPreferenceEntry> {
        let category = self.category();
        let query = self.query.trim().to_lowercase();
        let mut visible: Vec<&MinecraftPreferenceEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.descriptor.category == category)
            .filter(|entry| {
                self.show_advanced || entry.descriptor.tier == PresentationTier::Standard
            })
            .filter(|entry| query.is_empty() || matches_query(&entry.descriptor, &query))
            .filter(|entry| {
                !self.only_overridden
                    || PreferenceId::parse(&entry.descriptor.id)
                        .is_ok_and(|id| self.is_kept_by_override_filter(&id))
            })
            .collect();
        visible.sort_by_key(|entry| section_rank(category, entry.descriptor.section.as_deref()));
        visible
    }

    /// The rows the settings screen renders.
    #[must_use]
    pub fn rows(&self) -> Vec<MinecraftPreferenceRow> {
        let conflicts = self.binding_conflicts();
        let invalid = self.invalid_ids();
        let mut heading: Option<Option<String>> = None;
        let mut rows = Vec::new();
        for entry in self.visible_entries() {
            let section = entry.descriptor.section.clone();
            let section_heading = if heading.as_ref() == Some(&section) {
                String::new()
            } else {
                heading = Some(section.clone());
                section_label(section.as_deref())
            };
            rows.push(self.row_for(entry, section_heading, &conflicts, &invalid));
        }
        rows
    }

    /// One visible row rebuilt on its own, with the position it occupies.
    ///
    /// For a gesture that has to keep the element it is happening in: the row
    /// is written back in place instead of the list being replaced.
    #[must_use]
    pub fn row_at(&self, id: &PreferenceId) -> Option<(usize, MinecraftPreferenceRow)> {
        let mut heading: Option<Option<String>> = None;
        for (index, entry) in self.visible_entries().into_iter().enumerate() {
            let section = entry.descriptor.section.clone();
            let repeated = heading.as_ref() == Some(&section);
            if !repeated {
                heading = Some(section.clone());
            }
            if entry.id() != id.as_str() {
                continue;
            }
            let section_heading = if repeated {
                String::new()
            } else {
                section_label(section.as_deref())
            };
            return Some((
                index,
                self.row_for(
                    entry,
                    section_heading,
                    &self.binding_conflicts(),
                    &self.invalid_ids(),
                ),
            ));
        }
        None
    }

    #[allow(clippy::too_many_lines)]
    fn row_for(
        &self,
        entry: &MinecraftPreferenceEntry,
        section_heading: String,
        conflicts: &BTreeMap<PreferenceId, Vec<String>>,
        invalid: &BTreeSet<PreferenceId>,
    ) -> MinecraftPreferenceRow {
        let descriptor = &entry.descriptor;
        let Ok(id) = PreferenceId::parse(&descriptor.id) else {
            return MinecraftPreferenceRow::default();
        };
        let editor = editor_for(descriptor);
        let state = self.state_of(&id);
        let shown = self.shown_value(&id);
        let origin = self.origin_of(&id);
        let status = self.status_of(&id);

        let mut row = MinecraftPreferenceRow {
            id: descriptor.id.as_str().into(),
            label: preference_label(descriptor).into(),
            technical_id: descriptor.id.as_str().into(),
            section_heading: section_heading.into(),
            editor: editor.as_str().into(),
            advanced: descriptor.tier == PresentationTier::Advanced,
            overridden: origin == ValueOrigin::Overridden,
            // An overridden row says so with its pill and its Reset action,
            // so the caption is only worth spending a line on for a row that
            // is showing what Minecraft would do.
            state_text: match origin {
                ValueOrigin::Overridden => SharedString::default(),
                ValueOrigin::NotOverridden => lookup(origin.message_id()).into(),
            },
            // Offered whenever configuration holds anything for this row,
            // including a value an older Ephervex stored without applying, so
            // there is always a way to clear it.
            can_reset: !state.is_absent(),
            dirty: self.overlay.contains_key(&id),
            enum_index: -1,
            enum_labels: ModelRc::new(VecModel::<SharedString>::default()),
            ..MinecraftPreferenceRow::default()
        };

        match (&editor, shown.as_ref()) {
            (PreferenceEditor::Toggle, value) => {
                row.bool_value = matches!(value, Some(MinecraftPreferenceValue::Boolean(true)));
            }
            (PreferenceEditor::Numeric(numeric), value) => {
                let number = match value {
                    Some(MinecraftPreferenceValue::Integer(number)) => {
                        Some(whole_as_number(*number))
                    }
                    Some(MinecraftPreferenceValue::Decimal(number)) => Some(number.get()),
                    _ => None,
                };
                let percent = numeric.scale == NumericScale::Percent;
                let displayed = number.map(|number| if percent { number * 100.0 } else { number });
                let (low, high, step) = if percent {
                    (0.0, 100.0, 1.0)
                } else {
                    (
                        numeric.min.unwrap_or(0.0),
                        numeric.max.unwrap_or(0.0),
                        numeric.step.unwrap_or(1.0),
                    )
                };
                row.number_min = control_number(low);
                row.number_max = control_number(high);
                row.number_step = control_number(step);
                row.number_value = control_number(displayed.unwrap_or(low));
                row.number_readout = match displayed {
                    None => lookup("gui-minecraft-value-unset").into(),
                    Some(number) => numeric_readout(number, numeric).into(),
                };
                // A hint only where the reading is not simply the number
                // written out, so an ordinary field is not captioned with a
                // copy of its own contents.
                row.number_hint = match displayed {
                    Some(number) if row.number_readout != trim_number(number).as_str() => {
                        row.number_readout.clone()
                    }
                    _ => SharedString::default(),
                };
                row.text_value = match self.pending.get(&id) {
                    Some(PendingText::Number(text)) => text.as_str().into(),
                    _ => number.map(trim_number).unwrap_or_default().into(),
                };
            }
            (PreferenceEditor::Enumeration, value) => {
                let PreferencePresentation::Enum { values, .. } = &descriptor.presentation else {
                    return row;
                };
                row.enum_labels = ModelRc::new(VecModel::from(
                    values
                        .iter()
                        .map(|candidate| SharedString::from(enum_label(candidate)))
                        .collect::<Vec<_>>(),
                ));
                if let Some(MinecraftPreferenceValue::Enum(technical)) = value {
                    row.enum_index = values
                        .iter()
                        .position(|candidate| candidate.technical_value == *technical)
                        .and_then(|index| i32::try_from(index).ok())
                        .unwrap_or(-1);
                }
            }
            (PreferenceEditor::Language, value) => {
                let code = match value {
                    Some(MinecraftPreferenceValue::LanguageCode(code)) => Some(code.as_str()),
                    _ => None,
                };
                row.text_value = code.unwrap_or_default().into();
                match &self.languages {
                    LanguageCatalogState::Available(languages) => {
                        row.enum_labels = ModelRc::new(VecModel::from(
                            languages
                                .iter()
                                .map(|entry| SharedString::from(language_label(entry)))
                                .collect::<Vec<_>>(),
                        ));
                        let position = code
                            .and_then(|code| languages.iter().position(|entry| entry.code == code));
                        row.enum_index = position
                            .and_then(|index| i32::try_from(index).ok())
                            .unwrap_or(-1);
                        if let (Some(code), None) = (code, position) {
                            row.notice = lookup_args(
                                "gui-minecraft-language-absent",
                                [
                                    ("first", Arg::from(code)),
                                    ("second", Arg::from(self.preview_release.as_str())),
                                ],
                            )
                            .into();
                        }
                    }
                    LanguageCatalogState::Loading => {
                        row.notice = lookup("gui-minecraft-language-loading").into();
                    }
                    LanguageCatalogState::Idle | LanguageCatalogState::Unavailable => {
                        row.notice = lookup("gui-minecraft-language-unavailable").into();
                    }
                }
            }
            (PreferenceEditor::KeyBinding, value) => {
                let binding = match value {
                    Some(MinecraftPreferenceValue::KeyBinding(binding)) => Some(*binding),
                    _ => None,
                };
                row.binding_label = binding_label(binding).into();
                if let Some(others) = conflicts.get(&id) {
                    row.notice = lookup_args(
                        "gui-minecraft-binding-conflict",
                        [("value", Arg::from(others.join(", ").as_str()))],
                    )
                    .into();
                }
            }
            (PreferenceEditor::WindowSize, value) => {
                row.aspect_index = i32::try_from(self.window_aspect.position()).unwrap_or(0);
                row.aspect_locked = self.window_locked;
                let size = match value {
                    Some(MinecraftPreferenceValue::WindowSize(size)) => Some(*size),
                    _ => None,
                };
                if let Some(PendingText::Window { width, height }) = self.pending.get(&id) {
                    row.text_value = width.as_str().into();
                    row.height_text = height.as_str().into();
                } else {
                    row.text_value = size
                        .map(|size| size.width.to_string())
                        .unwrap_or_default()
                        .into();
                    row.height_text = size
                        .map(|size| size.height.to_string())
                        .unwrap_or_default()
                        .into();
                }
            }
            (PreferenceEditor::Text, value) => {
                row.text_value = match value {
                    Some(MinecraftPreferenceValue::Text(text)) => text.as_str().into(),
                    _ => SharedString::default(),
                };
            }
        }

        if invalid.contains(&id) {
            row.invalid = true;
            row.validation = self
                .problem_of(&id)
                .map(|problem| lookup(problem.message_id()))
                .unwrap_or_default()
                .into();
        }
        if let Some(status) = status {
            row.status = lookup(status.message_id()).into();
            row.status_kind = status.as_str().into();
            row.status_detail = status
                .detail_message_id()
                .map(|message| {
                    lookup_args(
                        message,
                        [("value", Arg::from(self.preview_release.as_str()))],
                    )
                })
                .unwrap_or_default()
                .into();
        }
        row
    }
}

/// Whether one descriptor answers the filter.
///
/// Matches the readable label, the stable product id, and the section's own
/// label: what a user can see or type, never remote metadata.
fn matches_query(descriptor: &MinecraftPreferenceDescriptor, query: &str) -> bool {
    preference_label(descriptor).to_lowercase().contains(query)
        || descriptor.id.to_lowercase().contains(query)
        || section_label(descriptor.section.as_deref())
            .to_lowercase()
            .contains(query)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::collections::BTreeSet;

    use ephervex_core::{MinecraftPreferenceCatalog, PreferenceChange, ReleaseSupport, SkipReason};
    use slint::Model as _;

    use super::*;

    /// The real bundled catalog, presented the way core presents it.
    ///
    /// The product surface under test is the shipped one: a second catalog
    /// written for the tests could agree with itself while disagreeing with
    /// what users actually get.
    fn bundled_snapshot() -> MinecraftPreferenceSnapshot {
        let catalog = MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog");
        MinecraftPreferenceSnapshot {
            catalog_minecraft_version: catalog.canonical_minecraft_version.clone(),
            entries: catalog
                .descriptors
                .iter()
                .map(|descriptor| MinecraftPreferenceEntry {
                    descriptor: descriptor.clone(),
                    managed: false,
                    value: None,
                })
                .collect(),
            unusable: Vec::new(),
            unrecognized: 0,
        }
    }

    fn loaded_model() -> MinecraftDefaultsModel {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(bundled_snapshot());
        model
    }

    /// The shipped catalog with one row promoted to the advanced tier, because
    /// the shipped catalog has none and the switch needs a row to reveal.
    fn model_with_advanced_row(name: &str) -> MinecraftDefaultsModel {
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == name {
                entry.descriptor.tier = PresentationTier::Advanced;
            }
        }
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot);
        model
    }

    fn id(text: &str) -> PreferenceId {
        PreferenceId::parse(text).expect("a well-formed id")
    }

    /// Every row the screen can reach, across every tab, with advanced shown.
    fn all_rows(model: &mut MinecraftDefaultsModel) -> Vec<String> {
        model.set_show_advanced(true);
        let mut reachable = Vec::new();
        for index in 0..CATEGORIES.len() {
            model.select_category(index);
            reachable.extend(
                model
                    .visible_entries()
                    .iter()
                    .map(|entry| entry.id().to_owned()),
            );
        }
        reachable
    }

    // --------------------------------------------------- the product surface

    #[test]
    fn every_shipped_preference_is_reachable_through_exactly_one_category() {
        let mut model = loaded_model();
        let catalog = MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog");
        let reachable = all_rows(&mut model);

        let unique: BTreeSet<&String> = reachable.iter().collect();
        assert_eq!(
            unique.len(),
            reachable.len(),
            "a preference reachable from two tabs would be two rows for one setting"
        );
        assert_eq!(
            reachable.len(),
            catalog.descriptors.len(),
            "every shipped descriptor is a product row"
        );
    }

    #[test]
    fn no_row_exists_for_a_setting_the_catalog_does_not_offer() {
        let mut model = loaded_model();
        let catalog = MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog");
        let offered: BTreeSet<&str> = catalog
            .descriptors
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect();

        let reachable = all_rows(&mut model);

        assert!(!reachable.is_empty());
        for row in &reachable {
            assert!(
                offered.contains(row.as_str()),
                "`{row}` is not an offered preference"
            );
        }
        assert!(
            !reachable.iter().any(|row| row == "sound_device"),
            "the machine-specific sound device must stay out of Sound & Music"
        );
    }

    #[test]
    fn a_row_belongs_to_the_category_its_descriptor_names() {
        let mut model = loaded_model();
        model.set_show_advanced(true);
        for (index, category) in CATEGORIES.iter().enumerate() {
            model.select_category(index);
            for entry in model.visible_entries() {
                assert_eq!(
                    entry.descriptor.category,
                    *category,
                    "`{}` was shown under the wrong tab",
                    entry.id()
                );
            }
        }
    }

    #[test]
    fn window_size_is_one_product_row_rather_than_a_width_and_a_height() {
        let mut model = loaded_model();
        let reachable = all_rows(&mut model);

        assert_eq!(
            reachable.iter().filter(|id| *id == "window_size").count(),
            1
        );
        assert!(!reachable.iter().any(|id| id == "overrideWidth"));
        assert!(!reachable.iter().any(|id| id == "overrideHeight"));
    }

    #[test]
    fn an_id_written_by_a_newer_build_is_reported_but_never_editable() {
        let mut model = MinecraftDefaultsModel::new();
        let mut snapshot = bundled_snapshot();
        snapshot.unrecognized = 3;
        model.apply_snapshot(snapshot);

        assert_eq!(model.unrecognized(), 3);
        let reachable = all_rows(&mut model);
        assert_eq!(
            reachable.len(),
            model.entries().len(),
            "an unrecognized id has no descriptor, so it cannot become a row"
        );
        assert!(
            model.edits().is_empty(),
            "and nothing about it can reach a save batch"
        );
    }

    // ----------------------------------------------------- the draft's shape

    #[test]
    fn a_preference_with_nothing_stored_shows_its_catalog_default_without_storing_it() {
        let model = loaded_model();
        let fov = id("fov");

        assert_eq!(model.origin_of(&fov), ValueOrigin::NotOverridden);
        assert_eq!(model.state_of(&fov), DraftState::default());
        assert_eq!(
            model.shown_value(&fov),
            MinecraftPreferenceValue::decimal(70.0)
        );
        assert!(
            !model.is_dirty(),
            "opening the screen must not turn an absent preference into a stored one"
        );
        assert!(model.edits().is_empty());
    }

    #[test]
    fn choosing_the_value_the_catalog_proves_is_still_an_explicit_override() {
        let mut model = loaded_model();
        let fov = id("fov");
        let default = model.catalog_default(&fov).expect("a proven default");

        model.set_number(&fov, 70.0);

        assert_eq!(model.state_of(&fov).value, Some(default));
        assert!(
            model.state_of(&fov).is_overridden(),
            "an explicit choice is an override even where it agrees with Minecraft"
        );
        assert_eq!(
            model.edits().len(),
            1,
            "and it is saved rather than optimized away"
        );
    }

    #[test]
    fn a_row_is_either_overridden_or_not_and_choosing_a_value_is_what_overrides_it() {
        let mut model = loaded_model();
        let scale = id("gui_scale");

        assert_eq!(model.origin_of(&scale), ValueOrigin::NotOverridden);
        assert!(model.state_of(&scale).is_absent());

        // One action, not two: the value is the override.
        model.set_number_text(&scale, "3");
        assert_eq!(model.origin_of(&scale), ValueOrigin::Overridden);
        assert!(model.state_of(&scale).is_overridden());
        assert_eq!(
            model.state_of(&scale).value,
            Some(MinecraftPreferenceValue::Integer(3))
        );

        model.reset_row(&scale);
        assert_eq!(
            model.origin_of(&scale),
            ValueOrigin::NotOverridden,
            "and Reset is the only thing that takes it away"
        );
        assert!(model.state_of(&scale).is_absent());
    }

    #[test]
    fn a_toggle_set_to_false_is_a_real_override_rather_than_the_absence_of_one() {
        let mut model = loaded_model();
        let fullscreen = id("fullscreen");

        model.set_boolean(&fullscreen, false);

        let state = model.state_of(&fullscreen);
        assert!(state.is_overridden(), "false is a value like any other");
        assert_eq!(state.value, Some(MinecraftPreferenceValue::Boolean(false)));
        assert_eq!(
            model.edits(),
            vec![ephervex_core::PreferenceEdit::manage(
                fullscreen,
                MinecraftPreferenceValue::Boolean(false)
            )],
            "applying a toggle at false is a choice, not an absence"
        );
    }

    #[test]
    fn a_row_with_no_proven_default_starts_blank_and_overrides_once_a_value_is_given() {
        let mut model = loaded_model();
        // The catalog proves no default window size: it would be the display's.
        let window = id("window_size");
        assert_eq!(model.origin_of(&window), ValueOrigin::NotOverridden);

        assert!(model.state_of(&window).is_absent());
        assert!(!model.is_dirty(), "and showing the row stores nothing");

        model.set_window_size(&window, "1280", "720");
        assert!(
            model.state_of(&window).is_overridden(),
            "one compound value, overriding in one action"
        );
        assert_eq!(model.edits().len(), 1);
    }

    #[test]
    fn returning_a_preference_to_its_saved_state_stops_it_being_unsaved() {
        let mut model = MinecraftDefaultsModel::new();
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "render_distance" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(16));
            }
        }
        model.apply_snapshot(snapshot);
        let render = id("render_distance");

        model.set_number_text(&render, "20");
        assert!(model.is_dirty());

        model.set_number_text(&render, "16");
        assert!(
            !model.is_dirty(),
            "an edit that restores what core already has is not a change"
        );
        assert!(model.edits().is_empty());
    }

    // ------------------------------------------------ what the view cannot do

    #[test]
    fn changing_category_filter_or_tier_never_disturbs_the_draft() {
        let mut model = loaded_model();
        let volume = id("sound_volume_master");
        let binding = id("keybind_sneak");
        model.set_number(&volume, 40.0);
        model.set_binding(&binding, InputBinding::Keyboard(PhysicalKey::LeftControl));
        let before = model.edits();
        assert_eq!(before.len(), 2);

        for index in 0..CATEGORIES.len() {
            model.select_category(index);
        }
        model.set_query("render");
        model.set_show_advanced(true);
        model.set_query("");
        model.set_show_advanced(false);
        model.select_category(0);

        assert_eq!(model.edits(), before, "the view decides what is shown only");
        assert_eq!(
            model.state_of(&volume).value,
            MinecraftPreferenceValue::decimal(0.4)
        );
    }

    #[test]
    fn the_filter_matches_the_label_the_id_and_the_section() {
        let mut model = loaded_model();
        model.select_category(3);
        model.set_show_advanced(true);

        model.set_query("sneak");
        let by_label: Vec<String> = model
            .visible_entries()
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect();
        assert!(by_label.contains(&"keybind_sneak".to_owned()));

        model.set_query("keybind_hotbar_4");
        assert_eq!(model.visible_entries().len(), 1);

        model.set_query("movement");
        assert!(
            model
                .visible_entries()
                .iter()
                .all(|entry| entry.descriptor.section.as_deref() == Some("movement"))
        );

        model.set_query("zzz-nothing-matches-this");
        assert!(model.visible_entries().is_empty());
    }

    #[test]
    fn advanced_rows_stay_available_without_crowding_the_default_view() {
        let mut model = model_with_advanced_row("keybind_use");
        model.select_category(3);

        model.set_show_advanced(false);
        let standard = model.visible_entries().len();
        assert!(
            model
                .visible_entries()
                .iter()
                .all(|entry| entry.descriptor.tier == PresentationTier::Standard)
        );

        model.set_show_advanced(true);
        let everything = model.visible_entries().len();
        assert!(
            everything > standard,
            "the advanced tier is hidden by default, not deleted"
        );
    }

    #[test]
    fn only_overridden_narrows_the_list_to_what_ephervex_writes() {
        let mut model = loaded_model();
        model.set_show_advanced(true);
        let category = category_of(&mut model, "fullscreen");
        model.select_category(category);
        let everything = model.visible_entries().len();

        // Nothing is configured yet, so the filter has nothing to show.
        model.set_only_overridden(true);
        assert!(model.visible_entries().is_empty());

        model.set_boolean(&id("fullscreen"), true);
        assert_eq!(
            model
                .visible_entries()
                .iter()
                .map(|entry| entry.id().to_owned())
                .collect::<Vec<_>>(),
            ["fullscreen"],
            "an unsaved override is still an override"
        );

        // Undoing the edit leaves nothing configured, so the row is honestly
        // out of scope again.
        model.reset_row(&id("fullscreen"));
        assert!(model.visible_entries().is_empty());

        model.set_only_overridden(false);
        assert_eq!(
            model.visible_entries().len(),
            everything,
            "the filter narrows the list, it does not delete rows from it"
        );
    }

    #[test]
    fn only_overridden_keeps_a_row_whose_override_is_being_cleared() {
        let mut model = MinecraftDefaultsModel::new();
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "fullscreen" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Boolean(true));
            }
        }
        model.apply_snapshot(snapshot);
        let category = category_of(&mut model, "fullscreen");
        model.select_category(category);
        model.set_only_overridden(true);

        // Pressing Reset on a saved override is an unsaved edit until it is
        // saved. The row has to stay on screen while that is true, or its own
        // Save and its own way back both disappear with it.
        model.reset_row(&id("fullscreen"));
        assert!(model.is_dirty());
        assert_eq!(
            model
                .visible_entries()
                .iter()
                .map(|entry| entry.id().to_owned())
                .collect::<Vec<_>>(),
            ["fullscreen"]
        );
    }

    /// Which tab a preference is reachable through, since every descriptor
    /// belongs to exactly one. Leaves the model on that tab.
    fn category_of(model: &mut MinecraftDefaultsModel, preference: &str) -> usize {
        model.set_show_advanced(true);
        for index in 0..CATEGORIES.len() {
            model.select_category(index);
            if model
                .visible_entries()
                .iter()
                .any(|entry| entry.id() == preference)
            {
                return index;
            }
        }
        panic!("the preference is reachable through some category")
    }

    #[test]
    fn an_advanced_row_edited_and_then_hidden_is_still_saved() {
        let mut model = model_with_advanced_row("keybind_use");
        model.select_category(3);
        model.set_show_advanced(true);
        assert!(
            model
                .visible_entries()
                .iter()
                .any(|entry| entry.id() == "keybind_use"),
            "the advanced row is reachable before it is hidden"
        );
        model.set_binding(&id("keybind_use"), InputBinding::Keyboard(PhysicalKey::F4));

        model.set_show_advanced(false);
        assert!(
            !model
                .visible_entries()
                .iter()
                .any(|entry| entry.id() == "keybind_use")
        );
        assert_eq!(model.edits().len(), 1, "hidden is not discarded");
    }

    #[test]
    fn the_advanced_switch_is_offered_only_when_the_catalog_has_advanced_rows() {
        assert!(
            !loaded_model().has_advanced_rows(),
            "the shipped catalog marks nothing advanced, so the switch would do nothing"
        );
        assert!(model_with_advanced_row("keybind_use").has_advanced_rows());
    }

    // -------------------------------------------------------- save and discard

    #[test]
    fn the_draft_becomes_one_batch_naming_only_what_changed() {
        let mut model = loaded_model();
        model.set_boolean(&id("fullscreen"), true);
        model.set_number(&id("sound_volume_music"), 25.0);

        let edits = model.edits();
        assert_eq!(
            edits.len(),
            2,
            "one edit per changed preference, and no more"
        );
        let named: BTreeSet<String> = edits.iter().map(|edit| edit.id.to_string()).collect();
        assert!(named.contains("fullscreen"));
        assert!(named.contains("sound_volume_music"));
    }

    #[test]
    fn resetting_a_configured_preference_asks_core_to_forget_it() {
        let mut model = MinecraftDefaultsModel::new();
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "gui_scale" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(2));
            }
        }
        model.apply_snapshot(snapshot);

        model.reset_row(&id("gui_scale"));
        assert_eq!(
            model.edits(),
            vec![ephervex_core::PreferenceEdit::reset(id("gui_scale"))],
            "reset means no stored row, not an unmanaged one"
        );
    }

    /// A snapshot in which one preference is configured the way an older
    /// Ephervex could leave it: a value stored without being applied.
    fn snapshot_with_retained_value(
        name: &str,
        value: &MinecraftPreferenceValue,
    ) -> MinecraftPreferenceSnapshot {
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == name {
                entry.managed = false;
                entry.value = Some(value.clone());
            }
        }
        snapshot
    }

    #[test]
    fn a_value_configuration_retains_without_applying_reads_as_not_overridden() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(2),
        ));
        let scale = id("gui_scale");

        assert_eq!(
            model.origin_of(&scale),
            ValueOrigin::NotOverridden,
            "nothing is applied here, so the row does not claim to override"
        );
        assert_eq!(
            model.shown_value(&scale),
            model.catalog_default(&scale),
            "and it shows what Minecraft would do rather than the retained value"
        );
        assert!(
            !model.is_dirty() && model.edits().is_empty(),
            "reading it changes nothing"
        );
    }

    #[test]
    fn a_retained_value_survives_a_save_that_does_not_name_its_row() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(2),
        ));

        model.set_boolean(&id("fullscreen"), true);
        let edits = model.edits();

        assert_eq!(edits.len(), 1, "only the row that was edited is named");
        assert_eq!(edits[0].id.as_str(), "fullscreen");
        assert!(
            !edits.iter().any(|edit| edit.id.as_str() == "gui_scale"),
            "so the retained value is left exactly as configuration has it"
        );
    }

    #[test]
    fn editing_a_retained_row_turns_it_into_an_applied_override() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(2),
        ));

        model.set_number_text(&id("gui_scale"), "4");

        assert!(model.state_of(&id("gui_scale")).is_overridden());
        assert_eq!(
            model.edits(),
            vec![ephervex_core::PreferenceEdit::manage(
                id("gui_scale"),
                MinecraftPreferenceValue::Integer(4)
            )]
        );
    }

    #[test]
    fn resetting_a_retained_row_removes_it_rather_than_leaving_it_stored() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(2),
        ));

        model.reset_row(&id("gui_scale"));

        assert_eq!(
            model.edits(),
            vec![ephervex_core::PreferenceEdit::reset(id("gui_scale"))],
            "an explicit reset clears what configuration was holding"
        );
    }

    #[test]
    fn no_action_on_this_screen_can_store_a_value_without_applying_it() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(2),
        ));

        // One of every editor, plus a reset, plus a row left alone.
        model.set_boolean(&id("fullscreen"), false);
        model.set_number_text(&id("render_distance"), "16");
        model.set_number(&id("sound_volume_master"), 40.0);
        model.set_enumeration(&id("cloud_status"), 2);
        model.set_window_size(&id("window_size"), "1280", "720");
        model.set_binding(
            &id("keybind_sneak"),
            InputBinding::Keyboard(PhysicalKey::F5),
        );
        model.set_text(&id("gui_scale"), "ignored");
        model.reset_row(&id("gui_scale"));

        for edit in model.edits() {
            match edit.change {
                PreferenceChange::Set { managed, .. } => {
                    assert!(managed, "`{}` was stored without being applied", edit.id);
                }
                PreferenceChange::Reset => {}
                PreferenceChange::Unmanage => {
                    panic!("`{}` was unmanaged, which this screen cannot do", edit.id)
                }
            }
        }
    }

    #[test]
    fn discarding_restores_exactly_what_core_last_confirmed() {
        let mut model = MinecraftDefaultsModel::new();
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "render_distance" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(16));
            }
        }
        model.apply_snapshot(snapshot);

        model.set_number_text(&id("render_distance"), "32");
        model.set_boolean(&id("fullscreen"), true);
        assert!(model.is_dirty());

        model.discard();
        assert!(!model.is_dirty());
        assert!(model.edits().is_empty());
        assert_eq!(
            model.state_of(&id("render_distance")).value,
            Some(MinecraftPreferenceValue::Integer(16))
        );
        assert!(model.state_of(&id("fullscreen")).is_absent());
    }

    #[test]
    fn a_refreshed_snapshot_keeps_unsaved_edits_and_retires_the_ones_it_caught_up_with() {
        let mut model = loaded_model();
        model.set_number_text(&id("render_distance"), "24");
        model.set_boolean(&id("fullscreen"), true);
        assert_eq!(model.dirty_count(), 2);

        // Core now confirms one of the two, as a save or another window would.
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "render_distance" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(24));
            }
        }
        model.apply_snapshot(snapshot);

        assert_eq!(model.dirty_count(), 1, "the confirmed edit stops being one");
        assert_eq!(
            model.state_of(&id("fullscreen")).value,
            Some(MinecraftPreferenceValue::Boolean(true)),
            "a background refresh must never discard a draft"
        );
    }

    #[test]
    fn an_unreadable_value_blocks_saving_and_keeps_the_draft() {
        let mut model = loaded_model();
        let render = id("render_distance");
        model.set_number_text(&render, "16");
        assert!(model.can_save());

        model.set_number_text(&render, "not a number");
        assert!(!model.can_save(), "an invalid field cannot be saved");
        assert!(model.is_dirty(), "and the draft is not thrown away for it");
        assert_eq!(model.problem_of(&render), Some(FieldProblem::Unreadable));
        assert!(model.invalid_ids().contains(&render));
    }

    #[test]
    fn a_value_outside_the_proven_domain_is_reported_rather_than_moved() {
        let mut model = loaded_model();
        model.select_category(1);
        let fov = id("fov");
        model.set_number_text(&fov, "500");

        assert_eq!(model.problem_of(&fov), Some(FieldProblem::OutOfDomain));
        assert!(!model.can_save(), "an out-of-domain value cannot be saved");
        assert_eq!(
            model.state_of(&fov).value,
            None,
            "and it never becomes a typed draft value, so no batch can carry it"
        );

        let row = model
            .rows()
            .into_iter()
            .find(|row| row.id == "fov")
            .expect("the field of view row");
        assert_eq!(
            row.text_value, "500",
            "the field still shows exactly what was typed rather than a repaired number"
        );
        assert!(row.invalid);
    }

    // ------------------------------------------------------- control mapping

    #[test]
    fn every_presentation_kind_reaches_the_editor_that_can_express_it() {
        let model = loaded_model();
        let editor_of =
            |name: &str| editor_for(model.descriptor(&id(name)).expect("a shipped descriptor"));

        assert_eq!(editor_of("fullscreen"), PreferenceEditor::Toggle);
        assert!(matches!(
            editor_of("render_distance"),
            PreferenceEditor::Numeric(NumericEditor {
                scale: NumericScale::Whole,
                ..
            })
        ));
        assert!(matches!(
            editor_of("gamma"),
            PreferenceEditor::Numeric(NumericEditor {
                scale: NumericScale::Decimal,
                ..
            })
        ));
        assert_eq!(editor_of("cloud_status"), PreferenceEditor::Enumeration);
        assert_eq!(editor_of("language"), PreferenceEditor::Language);
        assert_eq!(editor_of("keybind_attack"), PreferenceEditor::KeyBinding);
        assert_eq!(editor_of("window_size"), PreferenceEditor::WindowSize);
    }

    #[test]
    fn no_closed_domain_ever_falls_back_to_a_free_text_editor() {
        let model = loaded_model();
        for entry in model.entries() {
            let editor = editor_for(&entry.descriptor);
            let closed = matches!(
                entry.descriptor.presentation,
                PreferencePresentation::Enum { .. }
                    | PreferencePresentation::Language { .. }
                    | PreferencePresentation::KeyBinding { .. }
            );
            if closed {
                assert_ne!(
                    editor,
                    PreferenceEditor::Text,
                    "`{}` must never be edited as free text",
                    entry.id()
                );
            }
        }
    }

    #[test]
    fn a_slider_appears_only_where_the_proven_metadata_supports_one() {
        let model = loaded_model();
        for entry in model.entries() {
            let PreferenceEditor::Numeric(numeric) = editor_for(&entry.descriptor) else {
                continue;
            };
            if !numeric.slider {
                continue;
            }
            assert!(
                numeric.scale == NumericScale::Percent
                    || numeric.min.is_some() && numeric.max.is_some(),
                "`{}` has a slider with no proven range",
                entry.id()
            );
            if !numeric.scale.is_decimal() {
                assert!(
                    numeric.step.is_some(),
                    "`{}` has a slider with no proven step",
                    entry.id()
                );
            }
        }
    }

    #[test]
    fn an_open_ended_numeric_domain_gets_a_validated_field_rather_than_an_invented_slider() {
        let model = loaded_model();
        let PreferenceEditor::Numeric(scale) =
            editor_for(model.descriptor(&id("gui_scale")).expect("gui scale"))
        else {
            panic!("gui scale is numeric");
        };

        assert_eq!(scale.min, Some(0.0));
        assert_eq!(scale.max, None, "no maximum was ever proven");
        assert!(!scale.slider, "and none may be fabricated to make one");
        assert_eq!(scale.zero_label, Some("gui-minecraft-scale-automatic"));
    }

    /// The headings a category shows, in the order it shows them.
    fn headings(category_index: usize) -> Vec<String> {
        let mut model = loaded_model();
        model.set_show_advanced(true);
        model.select_category(category_index);
        let mut seen: Vec<String> = Vec::new();
        for entry in model.visible_entries() {
            let heading = section_label(entry.descriptor.section.as_deref());
            if seen.last() != Some(&heading) {
                seen.push(heading);
            }
        }
        seen
    }

    /// Where the unsectioned preferences sit is a decision per category, not a
    /// rule. Sound leads with its volumes, because they are what the category
    /// is for; every other category opens with its general settings.
    #[test]
    fn each_category_orders_its_sections_for_itself() {
        assert_eq!(headings(2), ["Volume", "General"], "sound and music");
        assert_eq!(headings(0), ["General"], "gameplay");
        assert_eq!(headings(1), ["General"], "video");
        assert_eq!(headings(4), ["General"], "language and accessibility");
        assert_eq!(
            headings(3),
            ["General", "Movement", "Gameplay", "Inventory"],
            "controls"
        );
    }

    /// A section the catalog grows later still lands somewhere deterministic:
    /// after everything the category lists, ordered by name.
    #[test]
    fn a_section_no_category_lists_sorts_last_by_name() {
        let listed = section_rank(PreferenceCategory::SoundAndMusic, Some("volume"));
        let unlisted = section_rank(PreferenceCategory::SoundAndMusic, Some("ambience"));
        let later = section_rank(PreferenceCategory::SoundAndMusic, Some("binaural"));

        assert!(listed < unlisted);
        assert!(section_rank(PreferenceCategory::SoundAndMusic, None) < unlisted);
        assert!(
            unlisted < later,
            "and unlisted sections are ordered by name"
        );
    }

    // ------------------------------------------------------- Sound and Music

    #[test]
    fn sound_and_music_exposes_every_manageable_setting_with_eleven_volumes() {
        let mut model = loaded_model();
        model.set_show_advanced(true);
        model.select_category(2);
        let rows = model.visible_entries();

        assert_eq!(rows.len(), 15, "the category must be complete");
        let volumes: Vec<&str> = rows
            .iter()
            .map(|entry| entry.id())
            .filter(|id| id.starts_with("sound_volume_"))
            .collect();
        assert_eq!(volumes.len(), 11);
        for entry in &rows {
            assert!(
                entry.id() != "soundDevice",
                "the excluded sound device must not appear"
            );
        }
    }

    #[test]
    fn every_sound_volume_is_shown_as_a_percentage_of_its_exact_decimal() {
        let mut model = loaded_model();
        model.set_show_advanced(true);
        model.select_category(2);

        let mut checked = 0;
        for entry in model.visible_entries() {
            if !entry.id().starts_with("sound_volume_") {
                continue;
            }
            let PreferenceEditor::Numeric(numeric) = editor_for(&entry.descriptor) else {
                panic!("a volume is numeric");
            };
            assert_eq!(
                numeric.scale,
                NumericScale::Percent,
                "`{}` is not shown as a percentage",
                entry.id()
            );
            checked += 1;
        }
        assert_eq!(checked, 11);

        let master = id("sound_volume_master");
        model.set_number(&master, 75.0);
        assert_eq!(
            model.state_of(&master).value,
            MinecraftPreferenceValue::decimal(0.75),
            "the percentage is presentation; the stored value stays normalized"
        );
        let row = model
            .rows()
            .into_iter()
            .find(|row| row.id == "sound_volume_master")
            .expect("the master volume row");
        assert_eq!(row.number_readout, "75%");
    }

    #[test]
    fn a_sound_enumeration_offers_its_proven_values_rather_than_a_text_field() {
        let mut model = loaded_model();
        model.select_category(2);
        let frequency = id("music_frequency");

        let row = model
            .rows()
            .into_iter()
            .find(|row| row.id == "music_frequency")
            .expect("the music frequency row");
        assert_eq!(row.editor, "enum");
        assert_eq!(row.enum_labels.row_count(), 3);

        model.set_enumeration(&frequency, 2);
        assert_eq!(
            model.state_of(&frequency).value,
            Some(MinecraftPreferenceValue::Enum("CONSTANT".to_owned())),
            "the select keeps the exact technical value"
        );
    }

    // ------------------------------------------------------------- video

    #[test]
    fn the_field_of_view_reads_in_degrees_with_minecrafts_own_endpoint_names() {
        let mut model = loaded_model();
        model.select_category(1);
        let fov = id("fov");

        let readout = |model: &MinecraftDefaultsModel| {
            model
                .rows()
                .into_iter()
                .find(|row| row.id == "fov")
                .expect("the field of view row")
                .number_readout
                .to_string()
        };

        assert_eq!(readout(&model), "70 deg (Normal)");
        model.set_number(&fov, 110.0);
        assert_eq!(readout(&model), "110 deg (Quake Pro)");
        model.set_number(&fov, 85.0);
        assert_eq!(readout(&model), "85 deg");
        assert_eq!(
            model.state_of(&fov).value,
            MinecraftPreferenceValue::decimal(85.0),
            "the typed value is degrees; how a release stores it is core's business"
        );
    }

    #[test]
    fn a_gui_scale_of_zero_reads_as_automatic() {
        let mut model = loaded_model();
        model.select_category(1);
        let row = |model: &MinecraftDefaultsModel| {
            model
                .rows()
                .into_iter()
                .find(|row| row.id == "gui_scale")
                .expect("the gui scale row")
        };

        assert_eq!(row(&model).number_readout, "Auto");
        model.set_number_text(&id("gui_scale"), "3");
        assert_eq!(row(&model).number_readout, "3");
    }

    #[test]
    fn the_window_size_editor_takes_a_width_and_a_height_as_one_value() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        model.set_window_size(&window, "1600", "900");
        assert_eq!(
            model.state_of(&window).value,
            Some(MinecraftPreferenceValue::WindowSize(WindowSize {
                width: 1600,
                height: 900
            }))
        );

        let row = model
            .rows()
            .into_iter()
            .find(|row| row.id == "window_size")
            .expect("the window size row");
        assert_eq!(row.editor, "window");
        assert_eq!(row.text_value, "1600");
        assert_eq!(row.height_text, "900");

        model.set_window_size(&window, "1600", "");
        assert_eq!(
            model.problem_of(&window),
            Some(FieldProblem::Unreadable),
            "half a window size is not a window size"
        );
    }

    // -------------------------------------------------- the window size picker

    /// A model whose window size is a value an older Ephervex stored without
    /// applying it, which this screen reads as no override at all.
    fn retained_window_model(size: WindowSize) -> MinecraftDefaultsModel {
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "window_size" {
                entry.managed = false;
                entry.value = Some(MinecraftPreferenceValue::WindowSize(size));
            }
        }
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot);
        model.select_category(1);
        model
    }

    fn window_size_of(model: &MinecraftDefaultsModel, id: &PreferenceId) -> Option<WindowSize> {
        match model.state_of(id).value {
            Some(MinecraftPreferenceValue::WindowSize(size)) => Some(size),
            _ => None,
        }
    }

    #[test]
    fn each_offered_aspect_is_an_exact_whole_number_pair() {
        assert_eq!(WindowAspect::Free.ratio(), None);
        assert_eq!(WindowAspect::Ratio16x9.ratio(), Some((16, 9)));
        assert_eq!(WindowAspect::Ratio16x10.ratio(), Some((16, 10)));
        assert_eq!(WindowAspect::Ratio21x9.ratio(), Some((21, 9)));
        assert_eq!(WindowAspect::Ratio4x3.ratio(), Some((4, 3)));

        for (index, aspect) in WINDOW_ASPECTS.iter().enumerate() {
            assert_eq!(WindowAspect::at(index), Some(*aspect));
            assert_eq!(aspect.position(), index);
        }
        assert_eq!(WindowAspect::at(WINDOW_ASPECTS.len()), None);
        assert_ne!(
            WindowAspect::Ratio16x9.ratio(),
            WindowAspect::Ratio16x10.ratio(),
            "two ratios that round to the same decimal stay distinct"
        );
    }

    #[test]
    fn projection_rounds_half_up_and_refuses_what_it_cannot_express() {
        // 1620 * 10 / 16 is exactly 1012.5, so the tie decides the value.
        assert_eq!(project_height(1620, (16, 10)), Some(1013));
        // 321 * 16 / 9 is 570.67, which rounds to the nearer whole number.
        assert_eq!(project_width(321, (16, 9)), Some(571));
        // A wide ratio can imply a height under the smallest window this
        // build accepts. That is refused rather than moved up to the minimum.
        assert_eq!(project_height(400, (21, 9)), None);
        // And an edge whose projection leaves the representable range fails
        // closed instead of wrapping.
        assert_eq!(project_width(u32::MAX, (21, 9)), None);
        assert_eq!(project_height(u32::MAX, (16, 9)), Some(2_415_919_103));
    }

    #[test]
    fn a_free_size_moves_each_edge_on_its_own() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        model.set_window_aspect(&window, WindowAspect::Free);
        model.commit_window_size(&window, "1600", "1000", WindowAxis::Width);

        assert!(!model.window_locked(), "free never holds a ratio");
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 1600,
                height: 1000
            }),
            "neither edge is projected from the other"
        );
    }

    #[test]
    fn a_held_ratio_projects_the_edge_the_user_did_not_type() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "1000");

        model.set_window_aspect(&window, WindowAspect::Ratio16x9);
        assert!(model.window_locked(), "choosing a ratio holds it");
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 1600,
                height: 900
            }),
            "the width is kept and the height follows the ratio"
        );

        model.commit_window_size(&window, "1600", "1200", WindowAxis::Height);
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 2133,
                height: 1200
            }),
            "typing a height keeps it and projects the width"
        );

        model.commit_window_size(&window, "1280", "1200", WindowAxis::Width);
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 1280,
                height: 720
            }),
            "typing a width keeps it and projects the height"
        );
    }

    #[test]
    fn releasing_and_holding_a_ratio_again_only_projects_while_it_is_held() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "1000");
        model.set_window_aspect(&window, WindowAspect::Ratio16x10);

        let held = WindowSize {
            width: 1600,
            height: 1000,
        };
        assert_eq!(window_size_of(&model, &window), Some(held), "already 16:10");

        model.set_window_locked(&window, false);
        model.commit_window_size(&window, "1600", "1200", WindowAxis::Height);
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 1600,
                height: 1200
            }),
            "released, the ratio is only shown and each edge stands alone"
        );

        model.set_window_locked(&window, true);
        assert_eq!(
            window_size_of(&model, &window),
            Some(WindowSize {
                width: 1600,
                height: 1000
            }),
            "holding it again projects the height from the width that is there"
        );
    }

    #[test]
    fn window_text_that_is_not_a_size_survives_a_commit_on_either_edge() {
        let earlier = WindowSize {
            width: 1600,
            height: 1000,
        };
        for (anchor, width, height, kept_width, kept_height) in [
            (WindowAxis::Width, "", "1000", "", "1000"),
            (WindowAxis::Height, "1600", "12x0", "1600", "12x0"),
        ] {
            let mut model = loaded_model();
            model.select_category(1);
            let window = id("window_size");
            model.set_window_size(&window, "1600", "1000");
            model.set_window_aspect(&window, WindowAspect::Ratio16x9);
            model.commit_window_size(&window, "1600", "900", WindowAxis::Width);
            model.commit_window_size(&window, "1600", "1000", WindowAxis::Width);
            model.set_window_locked(&window, false);
            model.set_window_locked(&window, true);
            // Whatever the ratio did earlier, the size on record is the one
            // the next commit must not silently project.
            let recorded = window_size_of(&model, &window);

            model.commit_window_size(&window, width, height, anchor);

            let row = model
                .rows()
                .into_iter()
                .find(|row| row.id == "window_size")
                .expect("the window size row");
            assert_eq!(row.text_value, kept_width, "the width text stays as typed");
            assert_eq!(
                row.height_text, kept_height,
                "the height text stays as typed"
            );
            assert!(
                model.problem_of(&window).is_some(),
                "the row still reports the problem"
            );
            assert!(
                model.invalid_ids().contains(&window),
                "and saving stays blocked"
            );
            assert_eq!(
                window_size_of(&model, &window),
                recorded,
                "no earlier size is projected in place of what failed to read"
            );
            assert_ne!(
                recorded,
                Some(earlier),
                "the earlier size is not what a ratio would have made of it"
            );
            assert_eq!(
                model.window_start(&window),
                WINDOW_START,
                "and the sizing window will not open onto a size the fields \
                 have already been typed past"
            );
        }
    }

    #[test]
    fn the_sizing_window_opens_on_the_rows_own_size_and_stores_nothing_to_do_it() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        assert_eq!(
            model.window_start(&window),
            WINDOW_START,
            "a row with no size opens on the window Minecraft itself opens"
        );
        assert!(model.state_of(&window).is_absent());
        assert!(!model.is_dirty(), "looking at a size is not choosing one");
        assert!(model.edits().is_empty());

        model.set_window_size(&window, "1600", "900");
        assert_eq!(
            model.window_start(&window),
            WindowSize {
                width: 1600,
                height: 900
            },
            "and a row with one opens on exactly that"
        );
    }

    #[test]
    fn a_free_sizing_window_stores_exactly_the_size_it_was_given() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        let given = WindowSize {
            width: 1731,
            height: 973,
        };
        assert_eq!(
            model.fit_window(&window, given, WindowAxis::Width),
            None,
            "nothing to correct, so the window is left where the user put it"
        );
        assert_eq!(window_size_of(&model, &window), Some(given));
        assert_eq!(model.origin_of(&window), ValueOrigin::Overridden);

        let edits = model.edits();
        assert_eq!(edits.len(), 1, "one preference, one edit");
        assert_eq!(edits[0].id.as_str(), "window_size");
        assert!(matches!(
            edits[0].change,
            PreferenceChange::Set {
                value: MinecraftPreferenceValue::WindowSize(stored),
                ..
            } if stored == given
        ));
    }

    #[test]
    fn the_edge_a_window_manager_moved_is_the_edge_the_user_is_driving() {
        let from = WindowSize {
            width: 1600,
            height: 900,
        };
        let anchor = |width, height| window_anchor(from, WindowSize { width, height });

        assert_eq!(anchor(1600, 900), None, "a window that did not move");
        assert_eq!(
            anchor(1900, 900),
            Some(WindowAxis::Width),
            "the right border: the width is chosen and the height follows"
        );
        assert_eq!(
            anchor(1600, 1100),
            Some(WindowAxis::Height),
            "the bottom border: the height is chosen and the width follows"
        );
        // A corner moves both, so the edge that moved further in proportion is
        // the one being driven. Comparing raw pixels would make the long edge
        // win every diagonal drag on a wide window.
        assert_eq!(
            anchor(1700, 1400),
            Some(WindowAxis::Height),
            "6% wider against 56% taller"
        );
        assert_eq!(anchor(2400, 950), Some(WindowAxis::Width));
        assert_eq!(
            anchor(2400, 1350),
            Some(WindowAxis::Width),
            "both edges by half: an exact tie keeps the width rather than \
             flipping edge to edge"
        );
    }

    #[test]
    fn a_diagonal_drag_keeps_one_edge_for_the_whole_gesture() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        let opened = WindowSize {
            width: 1600,
            height: 900,
        };
        model.set_window_size(&window, "1600", "900");
        model.set_window_aspect(&window, WindowAspect::Ratio16x9);

        // What the window reports as the pointer travels out and down. It
        // starts out mostly sideways and turns downwards part way, which is
        // where deciding the edge report by report would change its mind.
        let dragged = [
            WindowSize {
                width: 1900,
                height: 950,
            },
            WindowSize {
                width: 2000,
                height: 1400,
            },
            WindowSize {
                width: 2100,
                height: 1450,
            },
        ];
        let anchor = window_anchor(opened, dragged[0]).expect("the window moved");
        assert_eq!(anchor, WindowAxis::Width, "the drag began sideways");
        assert_eq!(
            window_anchor(dragged[0], dragged[1]),
            Some(WindowAxis::Height),
            "and turned downwards, which is the report that would flip it"
        );

        for reported in dragged {
            model.fit_window(&window, reported, anchor);
            let size = window_size_of(&model, &window).expect("a size");
            assert_eq!(
                size.width, reported.width,
                "the edge the gesture is driving stays the user's throughout"
            );
            assert_eq!(
                size.height,
                project_height(reported.width, (16, 9)).expect("a 16:9 height"),
                "and the other edge is the ratio's, every step of the way"
            );
        }
    }

    #[test]
    fn a_held_ratio_follows_the_edge_the_window_manager_did_not_move() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "900");
        model.set_window_aspect(&window, WindowAspect::Ratio16x9);

        // The right border moved: the width is what the user chose, so the
        // height is what follows.
        let correction = model.fit_window(
            &window,
            WindowSize {
                width: 1280,
                height: 900,
            },
            WindowAxis::Width,
        );
        assert_eq!(
            correction,
            Some(WindowSize {
                width: 1280,
                height: 720
            })
        );
        assert_eq!(window_size_of(&model, &window), correction);

        // The bottom border moved: the height stands and the width follows.
        let correction = model.fit_window(
            &window,
            WindowSize {
                width: 1280,
                height: 1000,
            },
            WindowAxis::Height,
        );
        assert_eq!(
            correction,
            Some(WindowSize {
                width: 1778,
                height: 1000
            }),
            "1000 * 16 / 9 is 1777.8, rounded to the nearer whole number"
        );
        assert_eq!(window_size_of(&model, &window), correction);
    }

    #[test]
    fn a_corrected_window_reports_its_corrected_size_and_settles() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "900");
        model.set_window_aspect(&window, WindowAspect::Ratio16x10);

        let reported = WindowSize {
            width: 1728,
            height: 1003,
        };
        let corrected = model
            .fit_window(&window, reported, WindowAxis::Width)
            .expect("the ratio moves the height");
        assert_eq!(
            corrected,
            WindowSize {
                width: 1728,
                height: 1080
            }
        );
        assert_eq!(
            model.fit_window(&window, corrected, WindowAxis::Width),
            None,
            "putting the window on the corrected size settles in one step \
             rather than chasing itself"
        );
        assert_eq!(window_size_of(&model, &window), Some(corrected));
    }

    #[test]
    fn a_window_smaller_than_this_build_accepts_stores_nothing() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "900");
        let held = window_size_of(&model, &window);

        for refused in [
            WindowSize {
                width: WindowSize::MINIMUM_EDGE - 1,
                height: 900,
            },
            WindowSize {
                width: 1600,
                height: WindowSize::MINIMUM_EDGE - 1,
            },
            WindowSize {
                width: 1,
                height: 1,
            },
        ] {
            assert_eq!(model.fit_window(&window, refused, WindowAxis::Width), None);
            assert_eq!(
                window_size_of(&model, &window),
                held,
                "a window under the minimum is refused rather than grown into one"
            );
        }
    }

    #[test]
    fn a_size_the_held_ratio_cannot_express_is_kept_rather_than_moved() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");
        model.set_window_size(&window, "1600", "900");
        model.set_window_aspect(&window, WindowAspect::Ratio21x9);

        // 21:9 of this width is a height under the minimum, so there is no
        // projection to apply and the exact window stands.
        let given = WindowSize {
            width: 400,
            height: 400,
        };
        assert_eq!(model.fit_window(&window, given, WindowAxis::Width), None);
        assert_eq!(window_size_of(&model, &window), Some(given));
    }

    #[test]
    fn choosing_a_ratio_or_holding_it_writes_nothing_on_an_untouched_row() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        for aspect in WINDOW_ASPECTS {
            model.set_window_aspect(&window, aspect);
            model.set_window_locked(&window, false);
            model.set_window_locked(&window, true);
        }

        assert_eq!(model.origin_of(&window), ValueOrigin::NotOverridden);
        assert!(model.state_of(&window).is_absent());
        assert!(!model.is_dirty(), "the picker's own state is not a value");
        assert!(model.edits().is_empty());
        assert_eq!(
            model.window_start(&window),
            WINDOW_START,
            "and there is no size of its own for a window to open onto"
        );
    }

    #[test]
    fn a_retained_window_size_stays_inactive_through_every_picker_interaction() {
        let retained = WindowSize {
            width: 1600,
            height: 1000,
        };
        let window = id("window_size");
        let unchanged = DraftState {
            managed: false,
            value: Some(MinecraftPreferenceValue::WindowSize(retained)),
        };

        let mut model = retained_window_model(retained);
        assert_eq!(model.origin_of(&window), ValueOrigin::NotOverridden);
        assert_eq!(model.state_of(&window), unchanged);
        assert_eq!(
            model.shown_value(&window),
            None,
            "a value nothing writes is not the value the row shows"
        );

        for aspect in WINDOW_ASPECTS {
            model.set_window_aspect(&window, aspect);
            model.set_window_locked(&window, false);
            model.set_window_locked(&window, true);
            assert_eq!(
                model.window_start(&window),
                WINDOW_START,
                "and a sizing window never opens onto a retained value"
            );
        }

        assert_eq!(
            model.state_of(&window),
            unchanged,
            "the retained configuration is left exactly as it was found"
        );
        assert_eq!(model.origin_of(&window), ValueOrigin::NotOverridden);
        assert!(!model.is_dirty());
        assert!(
            model.edits().is_empty(),
            "presentation alone never reaches a save batch"
        );

        // Only an explicit size makes it an override, and it is still one row.
        model.set_window_aspect(&window, WindowAspect::Free);
        model.commit_window_size(&window, "1280", "720", WindowAxis::Width);
        assert_eq!(model.origin_of(&window), ValueOrigin::Overridden);
        let edits = model.edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].id.as_str(), "window_size");
        assert!(matches!(
            edits[0].change,
            PreferenceChange::Set {
                value: MinecraftPreferenceValue::WindowSize(WindowSize {
                    width: 1280,
                    height: 720
                }),
                ..
            }
        ));
    }

    #[test]
    fn resetting_the_window_size_leaves_no_override_behind() {
        let mut model = loaded_model();
        model.select_category(1);
        let window = id("window_size");

        model.commit_window_size(&window, "1280", "720", WindowAxis::Width);
        assert_eq!(model.origin_of(&window), ValueOrigin::Overridden);

        model.reset_row(&window);
        assert_eq!(model.origin_of(&window), ValueOrigin::NotOverridden);
        assert!(model.state_of(&window).is_absent());
        assert_eq!(model.window_start(&window), WINDOW_START);
        assert!(
            model.edits().is_empty(),
            "back to what core already holds, so there is nothing to save"
        );
    }

    // ------------------------------------------------------------- labelling

    #[test]
    fn no_row_is_labelled_with_a_canonical_field_name_or_a_translation_key() {
        let mut model = loaded_model();
        for row in all_rows(&mut model)
            .into_iter()
            .filter_map(|id| PreferenceId::parse(&id).ok())
        {
            let descriptor = model.descriptor(&row).expect("a shipped descriptor");
            let label = preference_label(descriptor);
            assert!(!label.is_empty(), "`{row}` has no label");
            assert!(
                !label.contains('.'),
                "`{row}` is labelled with a technical identity: {label}"
            );
            assert!(
                !label.contains('_'),
                "`{row}` is labelled with its raw id: {label}"
            );
            if let Some(key) = &descriptor.translation_key {
                assert_ne!(&label, key, "`{row}` shows Mojang's translation key");
            }
        }
    }

    #[test]
    fn a_label_reads_a_stable_product_id_the_same_way_every_time() {
        assert_eq!(
            humanize_id("advanced_item_tooltips"),
            "Advanced item tooltips"
        );
        assert_eq!(humanize_id("gui_scale"), "GUI scale");
        assert_eq!(humanize_id("enable_vsync"), "Enable VSync");
        assert_eq!(humanize_id("sound_volume_master"), "Master");
        assert_eq!(humanize_id("sound_volume_ui"), "UI");
        assert_eq!(
            humanize_id("keybind_toggle_perspective"),
            "Toggle perspective"
        );
        assert_eq!(humanize_id("model_part_left_sleeve"), "Left sleeve");
    }

    #[test]
    fn every_category_and_section_has_readable_text() {
        for category in CATEGORIES {
            let label = category_label(category);
            assert!(!label.is_empty());
            assert!(!label.contains('_'));
        }
        assert_eq!(section_label(None), "General");
        assert_eq!(section_label(Some("skin_parts")), "Skin parts");
        assert_eq!(section_label(Some("debug")), "Debug");
    }

    #[test]
    fn a_section_heading_opens_each_group_exactly_once() {
        let mut model = loaded_model();
        model.select_category(3);
        model.set_show_advanced(true);
        let rows = model.rows();

        let headings: Vec<String> = rows
            .iter()
            .filter(|row| !row.section_heading.is_empty())
            .map(|row| row.section_heading.to_string())
            .collect();
        let unique: BTreeSet<&String> = headings.iter().collect();
        assert_eq!(
            headings.len(),
            unique.len(),
            "a group that opened twice is a group that was not sorted together"
        );
        assert!(
            headings.len() > 1,
            "Controls must not be one undifferentiated wall"
        );
        assert_eq!(
            rows.first().map(|row| row.section_heading.as_str()),
            Some("General"),
            "ungrouped rows come first"
        );
    }

    // ------------------------------------------------------ save completion

    #[test]
    fn a_submitted_save_holds_the_screen_until_it_completes() {
        let mut model = loaded_model();
        model.set_boolean(&id("fullscreen"), true);
        assert!(model.can_save());

        let edits = model.begin_save().expect("a batch");
        assert_eq!(edits.len(), 1);
        assert!(model.is_saving());
        assert!(!model.can_save(), "the screen holds itself while in flight");
        assert_eq!(
            model.begin_save(),
            None,
            "so one click cannot become two batches"
        );
    }

    #[test]
    fn a_confirmed_save_releases_the_screen_and_drops_only_the_field_text() {
        let mut model = loaded_model();
        model.set_number_text(&id("render_distance"), "24");
        model.begin_save().expect("a batch");

        // Core confirmed it, and the refreshed snapshot arrives with it.
        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "render_distance" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(24));
            }
        }
        model.apply_snapshot(snapshot);
        model.finish_save(true);

        assert!(!model.is_saving());
        assert!(!model.is_dirty(), "the confirmed edit stopped being one");
        assert!(model.edits().is_empty());
    }

    #[test]
    fn a_refused_save_releases_the_screen_without_touching_the_draft() {
        let mut model = loaded_model();
        model.set_number_text(&id("render_distance"), "24");
        model.set_boolean(&id("fullscreen"), true);
        let submitted = model.begin_save().expect("a batch");
        assert!(model.is_saving());

        // Core refused the batch, so no snapshot follows it.
        model.finish_save(false);

        assert!(!model.is_saving(), "the screen is usable again");
        assert_eq!(model.dirty_count(), 2, "and the draft is exactly as it was");
        assert_eq!(
            model.state_of(&id("render_distance")).value,
            Some(MinecraftPreferenceValue::Integer(24))
        );
        assert_eq!(
            model.state_of(&id("fullscreen")),
            DraftState {
                managed: true,
                value: Some(MinecraftPreferenceValue::Boolean(true))
            }
        );
        // The same batch is offered again, unchanged.
        assert!(model.can_save(), "a refused save can be retried");
        assert_eq!(model.begin_save(), Some(submitted));
    }

    #[test]
    fn a_refused_save_keeps_text_a_user_is_part_way_through_typing() {
        let mut model = loaded_model();
        model.select_category(1);
        model.set_number_text(&id("render_distance"), "24");
        model.begin_save().expect("a batch");

        model.finish_save(false);

        assert_eq!(
            model
                .rows()
                .into_iter()
                .find(|row| row.id == "render_distance")
                .map(|row| row.text_value.to_string()),
            Some("24".to_owned()),
            "the field still shows exactly what was typed into it"
        );
    }

    #[test]
    fn a_confirmed_save_is_the_only_thing_that_drops_the_field_text() {
        let mut model = loaded_model();
        model.select_category(1);
        model.set_number_text(&id("render_distance"), "0024");
        model.begin_save().expect("a batch");

        let mut snapshot = bundled_snapshot();
        for entry in &mut snapshot.entries {
            if entry.id() == "render_distance" {
                entry.managed = true;
                entry.value = Some(MinecraftPreferenceValue::Integer(24));
            }
        }
        model.apply_snapshot(snapshot);
        model.finish_save(true);

        assert_eq!(
            model
                .rows()
                .into_iter()
                .find(|row| row.id == "render_distance")
                .map(|row| row.text_value.to_string()),
            Some("24".to_owned()),
            "a confirmed value is shown as core stores it"
        );
    }

    #[test]
    fn a_refused_save_leaves_discarding_available() {
        let mut model = loaded_model();
        model.set_boolean(&id("fullscreen"), true);
        model.begin_save().expect("a batch");
        model.finish_save(false);

        assert!(model.is_dirty(), "there is still something to discard");
        model.discard();
        assert!(!model.is_dirty());
        assert!(!model.is_saving());
        assert!(model.state_of(&id("fullscreen")).is_absent());
    }

    #[test]
    fn a_refused_batch_writes_no_part_of_itself_to_the_configuration() {
        use ephervex_core::{App, PreferenceEdit};

        let base = tempfile::tempdir().expect("a disposable root");
        let mut app = App::open_at(ephervex_core::Paths::rooted_at(base.path())).expect("an app");
        app.save_global_minecraft_preferences(&[PreferenceEdit::manage(
            id("gui_scale"),
            MinecraftPreferenceValue::Integer(2),
        )])
        .expect("the first batch is valid");
        let settled = app.config().minecraft_preferences.clone();

        // A batch whose first edit is fine and whose second is outside the
        // descriptor's proven domain.
        let refused = app.save_global_minecraft_preferences(&[
            PreferenceEdit::manage(id("fullscreen"), MinecraftPreferenceValue::Boolean(true)),
            PreferenceEdit::manage(
                id("fov"),
                MinecraftPreferenceValue::decimal(500.0).expect("a real number"),
            ),
        ]);

        assert!(refused.is_err(), "the batch is refused");
        assert_eq!(
            app.config().minecraft_preferences,
            settled,
            "and not one edit of it reached the configuration file"
        );
    }

    // -------------------------------------------------- compatibility preview

    /// A preview resolved the way core resolves one, without a release feed.
    fn preview_of(
        release: &str,
        support: &[(&str, ReleaseSupport)],
        unrepresentable: &[(&str, SkipReason)],
    ) -> MinecraftPreferencePreview {
        MinecraftPreferencePreview {
            minecraft_version: release.to_owned(),
            source: ephervex_core::CompatibilitySource::Bundled,
            support: support
                .iter()
                .map(|(id, support)| ((*id).to_owned(), *support))
                .collect(),
            unrepresentable: unrepresentable
                .iter()
                .map(|(id, reason)| ((*id).to_owned(), *reason))
                .collect(),
        }
    }

    fn status_row(model: &MinecraftDefaultsModel, wanted: &str) -> MinecraftPreferenceRow {
        model
            .rows()
            .into_iter()
            .find(|row| row.id == wanted)
            .unwrap_or_else(|| panic!("the `{wanted}` row"))
    }

    #[test]
    fn the_three_release_answers_stay_three_separate_sentences() {
        let mut model = loaded_model();
        model.set_preview_release("1.12.2");
        model.apply_preview(preview_of(
            "1.12.2",
            &[
                ("fullscreen", ReleaseSupport::Authoritative),
                ("simulation_distance", ReleaseSupport::Unsupported),
                ("gamma", ReleaseSupport::Unknown),
            ],
            &[],
        ));

        assert_eq!(
            model.status_of(&id("fullscreen")),
            Some(CompatibilityStatus::Supported)
        );
        assert_eq!(
            model.status_of(&id("simulation_distance")),
            Some(CompatibilityStatus::Unavailable)
        );
        assert_eq!(
            model.status_of(&id("gamma")),
            Some(CompatibilityStatus::Unverified),
            "nobody having analyzed a release is not the release lacking the setting"
        );
    }

    #[test]
    fn an_authoritative_row_whose_value_cannot_encode_says_so_rather_than_unsupported() {
        let mut model = loaded_model();
        model.set_preview_release("1.12.2");
        model.set_binding(
            &id("keybind_attack"),
            InputBinding::Mouse(MouseButton::Left),
        );
        model.apply_preview(preview_of(
            "1.12.2",
            &[("keybind_attack", ReleaseSupport::Authoritative)],
            &[("keybind_attack", SkipReason::InputUnavailable)],
        ));

        assert_eq!(
            model.status_of(&id("keybind_attack")),
            Some(CompatibilityStatus::NotRepresentable)
        );
        assert_eq!(
            model.skip_reason_of(&id("keybind_attack")),
            Some(SkipReason::InputUnavailable),
            "the pre-1.13 mouse form is deliberately unproven, not guessed"
        );
    }

    #[test]
    fn a_release_nobody_has_analyzed_leaves_every_row_unverified() {
        let mut model = loaded_model();
        model.set_preview_release("99.9");
        model.apply_preview(preview_of(
            "99.9",
            &model
                .entries()
                .iter()
                .map(|entry| (entry.id(), ReleaseSupport::Unknown))
                .collect::<Vec<_>>(),
            &[],
        ));

        model.select_category(1);
        for row in model.rows() {
            assert_eq!(row.status_kind, "unverified");
            assert!(!row.status_detail.is_empty(), "the reason must be readable");
        }
    }

    #[test]
    fn rows_a_release_cannot_take_stay_visible_with_their_reason() {
        let mut model = loaded_model();
        model.set_preview_release("1.12.2");
        model.apply_preview(preview_of(
            "1.12.2",
            &[("simulation_distance", ReleaseSupport::Unsupported)],
            &[],
        ));
        model.select_category(0);

        let row = status_row(&model, "simulation_distance");
        assert_eq!(row.status_kind, "unavailable");
        assert!(
            row.status_detail.contains("1.12.2"),
            "a user has to see which release will not receive this"
        );
    }

    #[test]
    fn changing_the_previewed_release_moves_no_configured_value() {
        let mut model = loaded_model();
        model.set_number(&id("sound_volume_master"), 30.0);
        let before = model.edits();

        assert!(model.set_preview_release("1.20.1"));
        assert!(model.set_preview_release("1.12.2"));
        assert!(model.set_preview_release("1.20.1"));

        assert_eq!(
            model.edits(),
            before,
            "a preview is a question, not an edit"
        );
        assert_eq!(
            model.state_of(&id("sound_volume_master")).value,
            MinecraftPreferenceValue::decimal(0.3)
        );
    }

    #[test]
    fn a_new_release_drops_the_previous_releases_answer_rather_than_reusing_it() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        model.apply_preview(preview_of(
            "1.20.1",
            &[("fullscreen", ReleaseSupport::Authoritative)],
            &[],
        ));
        assert!(model.status_of(&id("fullscreen")).is_some());

        model.set_preview_release("1.12.2");
        assert_eq!(
            model.status_of(&id("fullscreen")),
            None,
            "a row must not keep showing an answer for a release nobody is looking at"
        );
    }

    #[test]
    fn window_size_carries_one_product_status_rather_than_two() {
        let mut model = loaded_model();
        model.set_preview_release("1.21.1");
        model.apply_preview(preview_of(
            "1.21.1",
            &[("window_size", ReleaseSupport::Authoritative)],
            &[],
        ));
        model.select_category(1);

        assert_eq!(
            model.status_of(&id("window_size")),
            Some(CompatibilityStatus::Supported)
        );
        assert_eq!(
            model
                .rows()
                .iter()
                .filter(|row| row.id == "window_size")
                .count(),
            1
        );
    }

    #[test]
    fn the_preview_is_asked_about_the_draft_rather_than_about_what_is_saved() {
        let mut model = loaded_model();
        model.set_boolean(&id("fullscreen"), true);
        model.set_number(&id("sound_volume_music"), 20.0);

        let draft = model.draft_preferences();
        assert_eq!(draft.len(), 2, "exactly what the user has chosen so far");
        assert_eq!(
            draft.get(&id("fullscreen")).map(|entry| entry.managed),
            Some(true)
        );
        assert_eq!(
            draft
                .get(&id("sound_volume_music"))
                .map(|entry| entry.managed),
            Some(true),
            "choosing a value is what applies it, so both travel as applied"
        );
    }

    #[test]
    fn a_value_configuration_retains_without_applying_is_not_part_of_the_preview() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(3),
        ));
        model.set_boolean(&id("fullscreen"), true);

        let draft = model.draft_preferences();

        assert_eq!(
            draft.get(&id("gui_scale")),
            None,
            "a value nothing is going to write is not part of the question"
        );
        assert_eq!(
            draft.get(&id("fullscreen")).map(|entry| entry.managed),
            Some(true),
            "and an active override is, as something that would be applied"
        );
        assert_eq!(draft.len(), 1);
    }

    #[test]
    fn a_retained_value_a_release_cannot_express_never_marks_the_row_unrepresentable() {
        let mut model = MinecraftDefaultsModel::new();
        // A binding no pre-1.13 release can store, retained without being
        // applied by an older Ephervex.
        model.apply_snapshot(snapshot_with_retained_value(
            "keybind_attack",
            &MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Left)),
        ));
        model.set_preview_release("1.12.2");

        let draft = model.draft_preferences();
        assert!(
            draft.get(&id("keybind_attack")).is_none(),
            "core is never asked about a value this screen would not write"
        );

        // Core answering only about support, as it now would.
        model.apply_preview(preview_of(
            "1.12.2",
            &[("keybind_attack", ReleaseSupport::Authoritative)],
            &[],
        ));

        assert_eq!(
            model.status_of(&id("keybind_attack")),
            Some(CompatibilityStatus::Supported),
            "the row must not report a problem with a value it is not showing"
        );
        assert_eq!(
            model.origin_of(&id("keybind_attack")),
            ValueOrigin::NotOverridden
        );
    }

    #[test]
    fn editing_a_retained_row_brings_it_back_into_the_preview() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "gui_scale",
            &MinecraftPreferenceValue::Integer(3),
        ));
        assert!(model.draft_preferences().get(&id("gui_scale")).is_none());

        model.set_number_text(&id("gui_scale"), "4");

        let draft = model.draft_preferences();
        assert_eq!(
            draft.get(&id("gui_scale")).map(|entry| entry.value.clone()),
            Some(MinecraftPreferenceValue::Integer(4)),
            "an override is asked about, and at the value the user chose"
        );
        assert_eq!(
            draft.get(&id("gui_scale")).map(|entry| entry.managed),
            Some(true)
        );
    }

    // ---------------------------------------------------- language metadata

    fn language(code: &str, name: &str, region: &str, bidirectional: bool) -> LanguageEntry {
        LanguageEntry {
            code: code.to_owned(),
            name: name.to_owned(),
            region: region.to_owned(),
            bidirectional,
        }
    }

    fn official_languages() -> Vec<LanguageEntry> {
        vec![
            language("en_us", "English", "United States", false),
            language("de_de", "Deutsch", "Deutschland", false),
            language(
                "ar_sa",
                "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
                "\u{627}\u{644}\u{645}\u{645}\u{644}\u{643}\u{629} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} \u{627}\u{644}\u{633}\u{639}\u{648}\u{62f}\u{64a}\u{629}",
                true,
            ),
        ]
    }

    #[test]
    fn the_language_row_is_a_select_over_the_releases_own_catalog() {
        let mut model = loaded_model();
        model.select_category(4);
        model.apply_languages(LanguageCatalogState::Available(official_languages()));

        let row = status_row(&model, "language");
        assert_eq!(row.editor, "language");
        assert_eq!(row.enum_labels.row_count(), 3);
        assert!(
            row.enum_labels
                .row_data(1)
                .is_some_and(|label| label.contains("de_de")),
            "the exact code stays visible beside the native name"
        );

        model.set_language(&id("language"), 1);
        assert_eq!(
            model.state_of(&id("language")).value,
            Some(MinecraftPreferenceValue::LanguageCode("de_de".to_owned()))
        );
    }

    #[test]
    fn long_native_and_right_to_left_names_survive_the_model_conversion() {
        let entries = official_languages();
        let arabic = entries.last().expect("an entry");
        assert!(arabic.bidirectional);
        let label = language_label(arabic);
        assert!(label.contains(&arabic.name));
        assert!(label.contains("ar_sa"));
        assert!(!label.is_empty());
    }

    #[test]
    fn an_unresolvable_language_list_affects_the_language_row_and_nothing_else() {
        let mut model = loaded_model();
        model.apply_languages(LanguageCatalogState::Unavailable);
        model.select_category(4);

        let row = status_row(&model, "language");
        assert_eq!(row.editor, "language", "never a free-text code field");
        assert!(
            !row.notice.is_empty(),
            "the row says the list is unavailable"
        );

        // Every other preference stays editable and saveable.
        model.set_boolean(&id("auto_jump"), false);
        assert!(model.can_save());
        assert!(
            model
                .edits()
                .iter()
                .any(|edit| edit.id.as_str() == "auto_jump")
        );
        assert_eq!(
            model.language_availability(),
            ephervex_core::LanguageAvailability::Unverified,
            "nothing may be claimed about a language without a catalog"
        );
    }

    #[test]
    fn a_drafted_language_missing_from_the_release_is_reported_rather_than_hidden() {
        let mut model = loaded_model();
        model.select_category(4);
        // Chosen while a release that offers it was previewed...
        model.apply_languages(LanguageCatalogState::Available(official_languages()));
        model.set_language(&id("language"), 1);
        assert_eq!(
            model.state_of(&id("language")).value,
            Some(MinecraftPreferenceValue::LanguageCode("de_de".to_owned()))
        );

        // ...and then previewed against one whose own catalog does not list it.
        model.set_preview_release("1.12.2");
        model.apply_languages(LanguageCatalogState::Available(vec![language(
            "en_us",
            "English",
            "United States",
            false,
        )]));

        let row = status_row(&model, "language");
        assert_eq!(row.editor, "language", "still a select, never a text field");
        assert_eq!(row.enum_index, -1);
        assert!(
            row.notice.contains("de_de"),
            "the chosen code is reported rather than silently dropped"
        );
        assert_eq!(
            model.language_availability(),
            ephervex_core::LanguageAvailability::Absent
        );
        assert_eq!(
            model.state_of(&id("language")).value,
            Some(MinecraftPreferenceValue::LanguageCode("de_de".to_owned())),
            "and the choice itself is untouched by which release is previewed"
        );
    }

    #[test]
    fn a_retained_language_settles_nothing_about_a_release() {
        let mut model = MinecraftDefaultsModel::new();
        // A code no release lists, retained without being applied.
        model.apply_snapshot(snapshot_with_retained_value(
            "language",
            &MinecraftPreferenceValue::LanguageCode("qya_aa".to_owned()),
        ));
        model.select_category(4);
        model.apply_languages(LanguageCatalogState::Available(official_languages()));

        assert_eq!(
            model.language_availability(),
            ephervex_core::LanguageAvailability::Unverified,
            "a language this screen is not applying cannot be absent from a release"
        );
        assert!(model.draft_preferences().get(&id("language")).is_none());

        let row = status_row(&model, "language");
        assert!(
            row.notice.is_empty(),
            "and the row reports no problem with a code it is not showing"
        );
        assert_eq!(model.origin_of(&id("language")), ValueOrigin::NotOverridden);
    }

    #[test]
    fn choosing_a_language_makes_it_the_one_a_release_is_asked_about() {
        let mut model = MinecraftDefaultsModel::new();
        model.apply_snapshot(snapshot_with_retained_value(
            "language",
            &MinecraftPreferenceValue::LanguageCode("qya_aa".to_owned()),
        ));
        model.select_category(4);
        model.apply_languages(LanguageCatalogState::Available(official_languages()));

        model.set_language(&id("language"), 1);

        assert_eq!(
            model.state_of(&id("language")).value,
            Some(MinecraftPreferenceValue::LanguageCode("de_de".to_owned()))
        );
        assert_eq!(
            model.language_availability(),
            ephervex_core::LanguageAvailability::Verified
        );
        assert_eq!(
            model
                .draft_preferences()
                .get(&id("language"))
                .map(|entry| entry.managed),
            Some(true)
        );
    }

    #[test]
    fn a_late_answer_for_a_release_already_moved_off_is_refused() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        let first = model.next_language_request().expect("an identity");

        model.set_preview_release("1.12.2");
        let second = model.next_language_request().expect("an identity");

        // Back to where it started: the release matches the first request
        // again, and the first request is still stale.
        model.set_preview_release("1.20.1");
        let third = model.next_language_request().expect("an identity");

        assert!(!model.accepts_language_result(first, "1.20.1"));
        assert!(!model.accepts_language_result(second, "1.12.2"));
        assert!(model.accepts_language_result(third, "1.20.1"));
        assert!(
            !model.accepts_language_result(third, "1.12.2"),
            "the generation alone is not enough: the release has to still match"
        );
    }

    #[test]
    fn a_late_preview_for_a_release_already_moved_off_is_refused() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        let first = model.next_preview_request().expect("an identity");
        model.set_preview_release("1.12.2");
        let second = model.next_preview_request().expect("an identity");
        model.set_preview_release("1.20.1");
        let third = model.next_preview_request().expect("an identity");

        assert!(!model.accepts_preview_result(first, "1.20.1"));
        assert!(!model.accepts_preview_result(second, "1.12.2"));
        assert!(model.accepts_preview_result(third, "1.20.1"));
    }

    #[test]
    fn a_request_identity_is_never_handed_out_twice_even_at_the_counter_boundary() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        model.exhaust_request_identities();

        let last_preview = model.next_preview_request().expect("the last identity");
        let last_language = model.next_language_request().expect("the last identity");
        assert_eq!(last_preview, u64::MAX);
        assert_eq!(last_language, u64::MAX);
        assert!(model.accepts_preview_result(last_preview, "1.20.1"));
        assert!(model.accepts_language_result(last_language, "1.20.1"));

        // Past the boundary nothing is minted, so no request is sent and no
        // identity is reused.
        assert_eq!(model.next_preview_request(), None);
        assert_eq!(model.next_language_request(), None);
        assert_eq!(model.next_preview_request(), None, "and stays refused");
        assert_eq!(model.next_language_request(), None);
    }

    #[test]
    fn a_request_that_could_not_be_minted_still_supersedes_the_one_before_it() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        model.exhaust_request_identities();
        let stranded_preview = model.next_preview_request().expect("the last identity");
        let stranded_language = model.next_language_request().expect("the last identity");

        // The draft changes, so the screen asks a logically new question about
        // the same release. Neither channel can mint an identity for it.
        model.set_boolean(&id("fullscreen"), true);
        assert_eq!(model.next_preview_request(), None);
        assert_eq!(model.next_language_request(), None);

        // The answer still in flight was asked about the previous draft, and
        // it is no longer an answer to anything.
        assert!(
            !model.accepts_preview_result(stranded_preview, "1.20.1"),
            "an exhausted allocator must not leave the old reply acceptable"
        );
        assert!(!model.accepts_language_result(stranded_language, "1.20.1"));
    }

    #[test]
    fn an_exhausted_channel_cannot_resurrect_an_old_reply_by_returning_to_its_release() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        model.exhaust_request_identities();
        let stranded_preview = model.next_preview_request().expect("the last identity");
        let stranded_language = model.next_language_request().expect("the last identity");

        // Away and back, with no identity left to mint on either leg.
        assert!(model.set_preview_release("1.12.2"));
        assert_eq!(model.next_preview_request(), None);
        assert!(model.set_preview_release("1.20.1"));
        assert_eq!(model.next_preview_request(), None);
        assert_eq!(model.next_language_request(), None);

        assert_eq!(
            model.preview_release(),
            "1.20.1",
            "the release matches again"
        );
        assert!(
            !model.accepts_preview_result(stranded_preview, "1.20.1"),
            "but the request it was asked for is long retired"
        );
        assert!(!model.accepts_language_result(stranded_language, "1.20.1"));
    }

    #[test]
    fn an_answer_is_tied_to_the_request_that_was_actually_issued() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");

        let issued = model.next_preview_request().expect("an identity");
        assert!(model.accepts_preview_result(issued, "1.20.1"));

        // A newer question retires the previous one the moment it is asked.
        let newer = model.next_preview_request().expect("an identity");
        assert!(!model.accepts_preview_result(issued, "1.20.1"));
        assert!(model.accepts_preview_result(newer, "1.20.1"));
        // Nothing that was never issued is ever acceptable.
        assert!(!model.accepts_preview_result(newer + 1, "1.20.1"));
    }

    #[test]
    fn a_language_retry_supersedes_the_catalog_request_before_it() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");

        let first = model.next_language_request().expect("an identity");
        let retry = model.next_language_request().expect("an identity");

        assert!(
            !model.accepts_language_result(first, "1.20.1"),
            "the retry is the question now"
        );
        assert!(model.accepts_language_result(retry, "1.20.1"));
    }

    #[test]
    fn a_counter_is_not_rewound_merely_because_the_release_changed() {
        let mut model = loaded_model();
        model.set_preview_release("1.20.1");
        let first = model.next_preview_request().expect("an identity");

        model.set_preview_release("1.12.2");
        let second = model.next_preview_request().expect("an identity");

        assert!(
            second > first,
            "a new release asks a new question, not the same one again"
        );
    }

    #[test]
    fn selecting_the_release_already_selected_asks_nothing_new() {
        let mut model = loaded_model();
        assert!(model.set_preview_release("1.21.1"));
        assert!(
            !model.set_preview_release("1.21.1"),
            "a repeated selection must not mint another request"
        );
        model.apply_languages(LanguageCatalogState::Available(official_languages()));
        assert!(!model.set_preview_release("1.21.1"));
        assert!(
            matches!(model.languages(), LanguageCatalogState::Available(_)),
            "and must not throw away the answer already in hand"
        );
    }

    // ---------------------------------------------------------- key capture

    #[test]
    fn a_focused_key_press_the_toolkit_identifies_becomes_a_binding() {
        use slint::platform::Key;

        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        model.begin_capture(&sneak);
        assert_eq!(model.capturing(), Some(&sneak));

        assert_eq!(
            model.capture_key(&sneak, char::from(Key::F6)),
            CaptureOutcome::Bound
        );
        assert_eq!(
            model.state_of(&sneak).value,
            Some(MinecraftPreferenceValue::KeyBinding(
                InputBinding::Keyboard(PhysicalKey::F6)
            ))
        );
        assert_eq!(model.capturing(), None, "capture ends when it succeeds");
    }

    #[test]
    fn every_accepted_code_names_exactly_the_key_it_is_meant_to() {
        use slint::platform::Key;

        for (code, key) in [
            (Key::F1, PhysicalKey::F1),
            (Key::F12, PhysicalKey::F12),
            (Key::Shift, PhysicalKey::LeftShift),
            (Key::ShiftR, PhysicalKey::RightShift),
            (Key::Control, PhysicalKey::LeftControl),
            (Key::ControlR, PhysicalKey::RightControl),
            (Key::Meta, PhysicalKey::LeftWin),
            (Key::MetaR, PhysicalKey::RightWin),
            (Key::AltGr, PhysicalKey::RightAlt),
            (Key::Tab, PhysicalKey::Tab),
            (Key::Space, PhysicalKey::Space),
            (Key::CapsLock, PhysicalKey::CapsLock),
            (Key::ScrollLock, PhysicalKey::ScrollLock),
            (Key::Pause, PhysicalKey::Pause),
            (Key::SysReq, PhysicalKey::PrintScreen),
        ] {
            assert_eq!(
                physical_key_of(char::from(code)),
                Some(key),
                "an accepted code named the wrong key"
            );
        }
    }

    #[test]
    fn a_printable_character_never_becomes_a_key_on_its_own() {
        // The toolkit turns every printable key into its character and throws
        // the location away, so none of these identifies a key: the same text
        // comes from the keypad, and from a different key on another layout.
        for glyph in [
            'a', 'z', 'w', 'Q', '0', '1', '7', '9', '/', ',', '.', ';', '\'', '[', ']', '\\', '`',
            '-', '=', '?', '&', '~', '+', '*',
        ] {
            assert_eq!(
                physical_key_of(glyph),
                None,
                "`{glyph}` was promoted to a key from its text alone"
            );
        }
    }

    #[test]
    fn a_code_the_keypad_shares_is_refused_rather_than_stored_as_the_main_row_key() {
        use slint::platform::Key;

        // The backend matches these without a location guard, so a keypad
        // press arrives as the main-row code. The model names both keys, so
        // storing either one would be a substitution.
        for code in [
            Key::Return,
            Key::UpArrow,
            Key::DownArrow,
            Key::LeftArrow,
            Key::RightArrow,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ] {
            assert_eq!(
                physical_key_of(char::from(code)),
                None,
                "a code the keypad shares was stored as the main-row key"
            );
        }
        // The keypad's own printable keys are refused by the same rule, so
        // none of them lands on the number row either.
        for glyph in ['1', '7', '.', '/', '*', '-', '+', '='] {
            assert_eq!(physical_key_of(glyph), None);
        }
        // Alt carries no location either, so neither side of it is claimed.
        assert_eq!(physical_key_of(char::from(Key::Alt)), None);
        assert_eq!(physical_key_of(char::from(Key::Menu)), None);
    }

    /// The whole reason the key is read rather than the character it prints:
    /// a keypad 7 and a number-row 7 print the same thing and are two keys the
    /// game binds separately.
    #[test]
    fn a_key_the_windowing_layer_names_becomes_that_exact_key() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");

        model.begin_capture(&sneak);
        assert_eq!(
            model.capture_physical_key(&sneak, PhysicalKey::Keypad7),
            CaptureOutcome::Bound,
            "an ordinary key is nothing to refuse once it has been named"
        );
        assert_eq!(
            model.state_of(&sneak).value,
            Some(MinecraftPreferenceValue::KeyBinding(
                InputBinding::Keyboard(PhysicalKey::Keypad7)
            )),
            "and the keypad key is not stored as the number-row one"
        );

        model.begin_capture(&sneak);
        assert_eq!(
            model.capture_physical_key(&sneak, PhysicalKey::Digit7),
            CaptureOutcome::Bound
        );
        assert_eq!(
            model.state_of(&sneak).value,
            Some(MinecraftPreferenceValue::KeyBinding(
                InputBinding::Keyboard(PhysicalKey::Digit7)
            ))
        );
    }

    #[test]
    fn escape_and_delete_keep_their_meaning_when_the_key_itself_is_read() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        model.set_binding(&sneak, InputBinding::Keyboard(PhysicalKey::LeftShift));
        let before = model.state_of(&sneak);

        model.begin_capture(&sneak);
        assert_eq!(
            model.capture_physical_key(&sneak, PhysicalKey::Escape),
            CaptureOutcome::Canceled
        );
        assert_eq!(model.state_of(&sneak), before, "escape changes nothing");

        model.begin_capture(&sneak);
        assert_eq!(
            model.capture_physical_key(&sneak, PhysicalKey::Delete),
            CaptureOutcome::Cleared
        );
        assert_eq!(
            model.state_of(&sneak).value,
            Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound))
        );
    }

    #[test]
    fn a_physical_key_offered_to_a_row_that_is_not_capturing_changes_nothing() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        model.set_binding(&sneak, InputBinding::Keyboard(PhysicalKey::LeftShift));
        let before = model.state_of(&sneak);

        assert_eq!(
            model.capture_physical_key(&sneak, PhysicalKey::W),
            CaptureOutcome::Canceled
        );
        assert_eq!(model.state_of(&sneak), before);
    }

    /// The character path, which is what a backend whose raw keyboard events
    /// this build cannot reach still falls back to.
    #[test]
    fn a_character_that_identifies_no_key_leaves_the_binding_alone() {
        use slint::platform::Key;

        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        model.set_binding(&sneak, InputBinding::Keyboard(PhysicalKey::LeftShift));
        let before = model.state_of(&sneak);

        model.begin_capture(&sneak);
        assert_eq!(
            model.capture_key(&sneak, 'k'),
            CaptureOutcome::Refused,
            "a character identifies no key"
        );

        assert_eq!(model.state_of(&sneak), before, "so nothing was rebound");
        assert_eq!(
            model.capturing(),
            Some(&sneak),
            "and capture stays open for an input that can be identified"
        );
        assert_eq!(
            model.capture_key(&sneak, char::from(Key::F9)),
            CaptureOutcome::Bound
        );
        assert_eq!(
            model.state_of(&sneak).value,
            Some(MinecraftPreferenceValue::KeyBinding(
                InputBinding::Keyboard(PhysicalKey::F9)
            ))
        );
    }

    #[test]
    fn a_named_mouse_button_is_captured_and_an_unnamed_one_is_refused() {
        let mut model = loaded_model();
        let attack = id("keybind_attack");

        model.begin_capture(&attack);
        assert_eq!(model.capture_mouse(&attack, "left"), CaptureOutcome::Bound);
        assert_eq!(
            model.state_of(&attack).value,
            Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(
                MouseButton::Left
            )))
        );

        model.begin_capture(&attack);
        assert_eq!(model.capture_mouse(&attack, "back"), CaptureOutcome::Bound);
        assert_eq!(
            model.state_of(&attack).value,
            Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(
                MouseButton::Button4
            )))
        );

        model.begin_capture(&attack);
        assert_eq!(
            model.capture_mouse(&attack, "other"),
            CaptureOutcome::Refused,
            "a button with no Minecraft name is refused rather than guessed"
        );
        assert_eq!(
            model.state_of(&attack).value,
            Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(
                MouseButton::Button4
            ))),
            "and nothing about the binding changes"
        );
        assert_eq!(
            model.capturing(),
            Some(&attack),
            "capture stays open so another button can be pressed"
        );
    }

    #[test]
    fn every_button_the_toolkit_identifies_arrives_through_the_validated_index() {
        // Slint reports a closed set of five named buttons and its backend
        // discards the raw index for anything else, so these five are the
        // whole of what can be captured. Each still enters the model through
        // the index rather than by name.
        for (reported, index) in [
            ("left", 0_u8),
            ("right", 1),
            ("middle", 2),
            ("back", 3),
            ("forward", 4),
        ] {
            let button = mouse_button_of(reported).expect("a nameable button");
            assert_eq!(button, MouseButton::from_index(index).expect("an index"));
        }
        // `Other` reaches this frontend without an index, so it identifies
        // nothing and must produce nothing.
        assert_eq!(mouse_button_of("other"), None);
        assert_eq!(mouse_button_of(""), None);
        assert_eq!(mouse_button_of("button6"), None);
    }

    #[test]
    fn the_buttons_past_forward_stay_storable_though_this_frontend_cannot_capture_them() {
        // A limit of the toolkit's event rather than of the shipped model:
        // these buttons still round-trip through configuration, so a value
        // that reaches the draft another way keeps working.
        for index in 5..=MouseButton::MAXIMUM_INDEX {
            let button = MouseButton::from_index(index).expect("a shipped button");
            let binding = InputBinding::Mouse(button);
            assert_eq!(
                InputBinding::parse_stored(&binding.to_stored_text()),
                Some(binding)
            );
        }
        assert_eq!(
            MouseButton::from_index(MouseButton::MAXIMUM_INDEX + 1),
            None
        );
    }

    #[test]
    fn escape_leaves_capture_with_the_binding_exactly_as_it_was() {
        let mut model = loaded_model();
        let jump = id("keybind_jump");
        model.set_binding(&jump, InputBinding::Keyboard(PhysicalKey::Space));
        let before = model.state_of(&jump);

        model.begin_capture(&jump);
        assert_eq!(
            model.capture_key(&jump, char::from(slint::platform::Key::Escape)),
            CaptureOutcome::Canceled
        );
        assert_eq!(model.state_of(&jump), before);
        assert_eq!(model.capturing(), None);
    }

    #[test]
    fn delete_and_backspace_bind_an_action_to_nothing_at_all() {
        use slint::platform::Key;

        for clearing in [Key::Delete, Key::Backspace] {
            let mut model = loaded_model();
            let drop_item = id("keybind_drop");
            model.begin_capture(&drop_item);

            assert_eq!(
                model.capture_key(&drop_item, char::from(clearing)),
                CaptureOutcome::Cleared
            );
            assert_eq!(
                model.state_of(&drop_item).value,
                Some(MinecraftPreferenceValue::KeyBinding(InputBinding::Unbound)),
                "unbound is a chosen state, not an absent one"
            );
            assert!(!model.state_of(&drop_item).is_absent());
        }
    }

    #[test]
    fn losing_focus_cancels_capture_without_binding_anything() {
        let mut model = loaded_model();
        let sprint = id("keybind_sprint");
        model.begin_capture(&sprint);
        let before = model.state_of(&sprint);

        model.cancel_capture();

        assert_eq!(model.capturing(), None);
        assert_eq!(model.state_of(&sprint), before);
        assert!(!model.is_dirty());
    }

    #[test]
    fn a_key_offered_to_a_row_that_is_not_capturing_changes_nothing() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");

        assert_eq!(model.capture_key(&sneak, 'q'), CaptureOutcome::Canceled);
        assert!(model.state_of(&sneak).is_absent());
    }

    #[test]
    fn a_binding_reads_as_words_rather_than_as_its_durable_configuration_text() {
        assert_eq!(
            binding_label(Some(InputBinding::Keyboard(PhysicalKey::LeftShift))),
            "Left shift"
        );
        assert_eq!(
            binding_label(Some(InputBinding::Keyboard(PhysicalKey::F3))),
            "F3"
        );
        assert_eq!(
            binding_label(Some(InputBinding::Mouse(MouseButton::Button4))),
            "Mouse 4"
        );
        assert_eq!(binding_label(Some(InputBinding::Unbound)), "Unbound");
        assert_eq!(binding_label(None), "Not set");
        for label in [
            binding_label(Some(InputBinding::Keyboard(PhysicalKey::LeftShift))),
            binding_label(Some(InputBinding::Mouse(MouseButton::Left))),
        ] {
            assert!(
                !label.contains("key:"),
                "durable text is not an editing label"
            );
            assert!(!label.contains("mouse:"));
        }
    }

    // ------------------------------------------------------------ conflicts

    #[test]
    fn two_explicitly_chosen_identical_bindings_warn_without_blocking_a_save() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        let sprint = id("keybind_sprint");
        model.set_binding(&sneak, InputBinding::Keyboard(PhysicalKey::LeftControl));
        model.set_binding(&sprint, InputBinding::Keyboard(PhysicalKey::LeftControl));

        let conflicts = model.binding_conflicts();
        assert_eq!(conflicts.len(), 2, "each side names the other");
        assert!(conflicts[&sneak].iter().any(|label| label == "Sprint"));
        assert!(conflicts[&sprint].iter().any(|label| label == "Sneak"));
        assert!(
            model.can_save(),
            "Minecraft permits a clash, so Ephervex must not be stricter"
        );

        model.select_category(3);
        let row = status_row(&model, "keybind_sneak");
        assert!(row.notice.contains("Sprint"));
        assert!(
            !row.invalid,
            "a conflict is a warning, not a validation failure"
        );
    }

    #[test]
    fn a_chosen_binding_never_conflicts_with_a_catalog_default_nobody_chose() {
        let mut model = loaded_model();
        // The proven default for attacking is the left mouse button; choosing
        // it for another action must not claim a clash against a value the
        // user has not actually configured.
        model.set_binding(&id("keybind_use"), InputBinding::Mouse(MouseButton::Left));

        assert!(
            model.binding_conflicts().is_empty(),
            "only values the draft actually holds take part"
        );
    }

    #[test]
    fn two_actions_bound_to_nothing_are_not_a_conflict() {
        let mut model = loaded_model();
        model.set_binding(&id("keybind_sneak"), InputBinding::Unbound);
        model.set_binding(&id("keybind_sprint"), InputBinding::Unbound);

        assert!(
            model.binding_conflicts().is_empty(),
            "being bound to nothing is not being bound to the same thing"
        );
    }

    #[test]
    fn a_conflict_follows_the_preference_rather_than_the_row_position() {
        let mut model = loaded_model();
        let sneak = id("keybind_sneak");
        let sprint = id("keybind_sprint");
        model.set_binding(&sneak, InputBinding::Keyboard(PhysicalKey::G));
        model.set_binding(&sprint, InputBinding::Keyboard(PhysicalKey::G));

        model.select_category(3);
        model.set_query("sprint");
        let filtered = model.rows();
        assert!(
            !filtered.iter().any(|row| row.id == "keybind_sneak"),
            "the other side of the clash is filtered away"
        );
        let sprint_row = filtered
            .iter()
            .find(|row| row.id == "keybind_sprint")
            .expect("the sprint binding row");
        assert!(
            sprint_row.notice.contains("Sneak"),
            "and the clash survives, because it follows the preference, not the row"
        );
    }
}
