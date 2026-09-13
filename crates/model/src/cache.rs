//! The byte budget for the shared reusable artifact cache.
//!
//! The cache is a disposable optimization, never a source of truth: an
//! installed instance or a retained server owns its own files, so removing a
//! reusable cache entry costs a re-download and nothing else. That is what
//! makes a budget expressible at all, and it is why the budget is *soft* - a
//! required artifact is fetched whatever its size, and the limit only decides
//! what is worth keeping afterwards.
//!
//! # Why the limit is a type rather than a number
//!
//! "Unlimited" and "ten gibibytes" are different kinds of answer, and every
//! encoding that squeezes them into one integer needs a sentinel that some
//! caller eventually forgets to check. `u64::MAX` is the obvious candidate and
//! the worst one: it is also a perfectly valid arithmetic operand, so a missed
//! check produces a plausible-looking watermark instead of an error.

use std::fmt;

/// One kibibyte, and the base every unit below is a power of.
pub const KIB: u64 = 1024;
/// One mebibyte in bytes.
pub const MIB: u64 = KIB * 1024;
/// One gibibyte in bytes.
pub const GIB: u64 = MIB * 1024;
/// One tebibyte in bytes.
pub const TIB: u64 = GIB * 1024;
/// One pebibyte in bytes. Past anything a settings screen offers, and here so
/// the ceiling below and the sentence that reports it can be written exactly.
const PIB: u64 = TIB * 1024;
/// One exbibyte in bytes. See [`PIB`].
const EIB: u64 = PIB * 1024;

/// The budget a fresh installation gets.
///
/// Large enough that ordinary use never sees an eviction, small enough that a
/// launcher for disposable instances does not quietly fill a laptop's disk.
pub const DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES: u64 = 10 * GIB;

/// The largest byte budget Ephervex represents.
///
/// Bounded by what the configuration file can carry rather than by what a number
/// can hold: TOML integers are signed 64-bit, so a budget above `i64::MAX` bytes
/// could be set in memory and then fail to save. This is the largest whole power
/// of two inside that range, which keeps the ceiling a value the interface can
/// state exactly instead of a fraction of one. Four exbibytes is past every
/// storage device that exists, so nothing real is out of reach.
pub const MAX_ARTIFACT_CACHE_LIMIT_BYTES: u64 = 4 * EIB;

/// The budgets a settings screen offers as one-click choices, smallest first.
///
/// Presentation, not policy: the stored value is always a byte count, and a
/// custom size can be anything up to [`MAX_ARTIFACT_CACHE_LIMIT_BYTES`]. The list
/// ends at a hundred gibibytes because that is where one-click choices stop being
/// useful, not because it is a ceiling.
pub const ARTIFACT_CACHE_LIMIT_PRESETS: [u64; 6] =
    [2 * GIB, 5 * GIB, 10 * GIB, 25 * GIB, 50 * GIB, 100 * GIB];

/// The share of a limited budget a cleanup pass aims to leave behind.
///
/// A high/low-water pair rather than a single threshold: collecting down to the
/// limit itself would mean a tiny eviction after almost every download, and the
/// point of a cache is not to be busy.
pub const CLEANUP_TARGET_PERCENT: u64 = 80;

/// How much of the shared reusable artifact cache Ephervex will keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCacheLimit {
    /// Keep at most this many bytes of reusable artifacts.
    Limited(u64),
    /// Keep everything. Usage is still measured and a manual clear still works;
    /// only automatic collection is off.
    Unlimited,
}

impl Default for ArtifactCacheLimit {
    fn default() -> Self {
        Self::Limited(DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES)
    }
}

/// Why a requested cache limit was refused.
///
/// Refused rather than clamped: a user who typed a number that cannot be
/// represented should be told, not silently given a different budget than the
/// one on their screen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ArtifactCacheLimitError {
    /// The value times its unit does not fit the representable range.
    #[error("a cache limit of {value} {unit} is larger than this launcher can represent")]
    TooLarge { value: u64, unit: ByteUnit },
    /// A limit of zero bytes, which would evict every artifact the moment its
    /// operation finished.
    #[error("a cache limit has to be at least one {}", ByteUnit::Mebibytes)]
    TooSmall,
    /// The text in a custom-size field is not a whole number.
    #[error("`{value}` is not a whole number of bytes")]
    Malformed { value: String },
}

impl ArtifactCacheLimitError {
    /// The `ephervex-i18n` message id describing this refusal to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "error-artifact-cache-limit-too-large",
            Self::TooSmall => "error-artifact-cache-limit-too-small",
            Self::Malformed { .. } => "error-artifact-cache-limit-malformed",
        }
    }

    /// Values the localized message takes as arguments.
    ///
    /// Stated beside the message ids so a frontend supplies them without knowing
    /// what the bounds are, and so the sentence a user reads names the same
    /// numbers this type enforces.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::TooLarge { .. } => vec![(
                "maximum",
                format_binary_bytes(MAX_ARTIFACT_CACHE_LIMIT_BYTES),
            )],
            Self::TooSmall => vec![("minimum", format_binary_bytes(MIB))],
            Self::Malformed { .. } => Vec::new(),
        }
    }
}

/// A binary size unit, as displayed and as entered.
///
/// Binary throughout, never decimal: the cache reports what the filesystem
/// reports, and mixing 1000-based labels with 1024-based arithmetic is how a
/// "10 GB" limit ends up evicting at 9.3 of whatever the user thought they set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ByteUnit {
    Mebibytes,
    Gibibytes,
    Tebibytes,
}

impl ByteUnit {
    /// Every unit a custom size may be entered in, largest last.
    pub const ALL: [Self; 3] = [Self::Mebibytes, Self::Gibibytes, Self::Tebibytes];

    /// How many bytes one of this unit is.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Mebibytes => MIB,
            Self::Gibibytes => GIB,
            Self::Tebibytes => TIB,
        }
    }

    /// The IEC abbreviation, which is also what a unit selector shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mebibytes => "MiB",
            Self::Gibibytes => "GiB",
            Self::Tebibytes => "TiB",
        }
    }

    /// Resolves an abbreviation, so a frontend can round-trip its own labels.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        let trimmed = label.trim();
        Self::ALL
            .into_iter()
            .find(|unit| unit.label().eq_ignore_ascii_case(trimmed))
    }
}

impl fmt::Display for ByteUnit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

impl ArtifactCacheLimit {
    /// Builds a limited budget from a count of some unit.
    ///
    /// This is the entry point for a custom size: the frontend edits a number
    /// and a unit, and the canonical value is always the byte product. The
    /// multiplication is checked, so a number nobody could store is an error
    /// rather than a wrapped one.
    pub fn from_unit(value: u64, unit: ByteUnit) -> Result<Self, ArtifactCacheLimitError> {
        let bytes = value
            .checked_mul(unit.bytes())
            .filter(|bytes| *bytes <= MAX_ARTIFACT_CACHE_LIMIT_BYTES)
            .ok_or(ArtifactCacheLimitError::TooLarge { value, unit })?;
        Self::from_bytes(bytes)
    }

    /// Builds a limited budget from an exact byte count.
    pub fn from_bytes(bytes: u64) -> Result<Self, ArtifactCacheLimitError> {
        if bytes < MIB {
            return Err(ArtifactCacheLimitError::TooSmall);
        }
        if bytes > MAX_ARTIFACT_CACHE_LIMIT_BYTES {
            return Err(ArtifactCacheLimitError::TooLarge {
                value: bytes,
                unit: ByteUnit::Mebibytes,
            });
        }
        Ok(Self::Limited(bytes))
    }

    /// Parses the text of a custom-size field against a unit.
    pub fn parse_in_unit(value: &str, unit: ByteUnit) -> Result<Self, ArtifactCacheLimitError> {
        let trimmed = value.trim();
        // Parsed as `u64` rather than as a float: a budget is a whole number of
        // units, and going through `f64` would make 9007199254740993 MiB round
        // to something the user did not type.
        let count = trimmed
            .parse::<u64>()
            .map_err(|_| ArtifactCacheLimitError::Malformed {
                value: trimmed.to_owned(),
            })?;
        Self::from_unit(count, unit)
    }

    /// The configured ceiling in bytes, or `None` when unlimited.
    #[must_use]
    pub const fn bytes(self) -> Option<u64> {
        match self {
            Self::Limited(bytes) => Some(bytes),
            Self::Unlimited => None,
        }
    }

    /// The high-water mark: usage strictly above this starts a cleanup.
    #[must_use]
    pub const fn high_water(self) -> Option<u64> {
        self.bytes()
    }

    /// The low-water mark a cleanup collects down to.
    ///
    /// `floor(limit * 80 / 100)`, split so that no intermediate can overflow.
    ///
    /// Writing the product first would overflow for any limit above a fifth of
    /// `u64::MAX`, which is well inside the representable range. Dividing before
    /// multiplying and adding the remainder's share back is exact -
    /// `b = 100q + r` gives `80q + floor(80r/100)` - and every term stays below
    /// `b`.
    #[must_use]
    pub const fn cleanup_target(self) -> Option<u64> {
        match self.bytes() {
            Some(bytes) => {
                let whole = bytes / 100 * CLEANUP_TARGET_PERCENT;
                let remainder = bytes % 100 * CLEANUP_TARGET_PERCENT / 100;
                Some(whole + remainder)
            }
            None => None,
        }
    }

    /// Whether the measured usage is over the high-water mark.
    ///
    /// Deliberately strict: sitting exactly at the configured limit is within
    /// budget, and collecting there would make an exact-fit cache churn.
    #[must_use]
    pub const fn is_exceeded_by(self, usage: u64) -> bool {
        match self.high_water() {
            Some(limit) => usage > limit,
            None => false,
        }
    }
}

impl fmt::Display for ArtifactCacheLimit {
    /// The limit as a settings screen shows it, with "Unlimited" spelled the
    /// way the type means it.
    ///
    /// Frontends localize the unlimited case; this exists for logs and
    /// diagnostics, where an exact technical value is what is wanted.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.bytes() {
            Some(bytes) => formatter.write_str(&format_binary_bytes(bytes)),
            None => formatter.write_str("unlimited"),
        }
    }
}

/// Formats a byte count in IEC units.
///
/// One decimal place below a tebibyte and two above it, so a size stays
/// readable without the last digit flickering on every download. Exact multiples
/// lose their decimal entirely: a limit the user set as `10 GiB` should read back
/// as `10 GiB`, not as `10.0 GiB`.
///
/// The units above a tebibyte exist for one value - the representable ceiling -
/// and never appear in a real usage figure.
#[must_use]
pub fn format_binary_bytes(bytes: u64) -> String {
    const UNITS: [(u64, &str, usize); 6] = [
        (EIB, "EiB", 2),
        (PIB, "PiB", 2),
        (TIB, "TiB", 2),
        (GIB, "GiB", 1),
        (MIB, "MiB", 1),
        (KIB, "KiB", 1),
    ];

    for (scale, label, decimals) in UNITS {
        if bytes >= scale {
            if bytes % scale == 0 {
                return format!("{} {label}", bytes / scale);
            }
            // Scaled through `f64` only for presentation. The value is at most
            // eight exbibytes, so the ratio is far inside the range a double
            // renders faithfully at this precision.
            #[expect(
                clippy::cast_precision_loss,
                reason = "display rounding, never a stored value"
            )]
            let value = bytes as f64 / scale as f64;
            return format!("{value:.decimals$} {label}");
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_default_budget_is_ten_gibibytes_of_bytes() {
        assert_eq!(
            ArtifactCacheLimit::default(),
            ArtifactCacheLimit::Limited(10_737_418_240)
        );
        assert_eq!(DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn a_custom_five_hundred_gibibyte_budget_is_accepted_exactly() {
        let limit = ArtifactCacheLimit::from_unit(500, ByteUnit::Gibibytes).expect("500 GiB");
        assert_eq!(limit.bytes(), Some(536_870_912_000));
    }

    #[test]
    fn budgets_larger_than_five_hundred_gibibytes_are_accepted() {
        for (value, unit, bytes) in [
            (1, ByteUnit::Tebibytes, 1_099_511_627_776_u64),
            (4, ByteUnit::Tebibytes, 4_398_046_511_104),
            (2_048, ByteUnit::Gibibytes, 2_199_023_255_552),
        ] {
            let limit = ArtifactCacheLimit::from_unit(value, unit).expect("representable");
            assert_eq!(limit.bytes(), Some(bytes), "{value} {unit}");
        }
    }

    #[test]
    fn a_budget_past_the_representable_range_is_refused_rather_than_wrapped() {
        let error = ArtifactCacheLimit::from_unit(u64::MAX, ByteUnit::Tebibytes)
            .expect_err("an overflowing product must be refused");
        assert!(matches!(error, ArtifactCacheLimitError::TooLarge { .. }));

        // Just past what the settings format can carry: the product does not
        // overflow `u64`, so only the explicit ceiling catches it.
        let error = ArtifactCacheLimit::from_unit(8 * 1024 * 1024, ByteUnit::Tebibytes)
            .expect_err("a value beyond the persistable range must be refused");
        assert!(matches!(error, ArtifactCacheLimitError::TooLarge { .. }));

        assert!(ArtifactCacheLimit::from_bytes(MAX_ARTIFACT_CACHE_LIMIT_BYTES).is_ok());
        assert!(ArtifactCacheLimit::from_bytes(MAX_ARTIFACT_CACHE_LIMIT_BYTES + 1).is_err());
        // Whatever the ceiling is, the settings file has to be able to carry it.
        assert!(i64::try_from(MAX_ARTIFACT_CACHE_LIMIT_BYTES).is_ok());
    }

    #[test]
    fn a_budget_below_one_mebibyte_is_refused() {
        assert!(matches!(
            ArtifactCacheLimit::from_bytes(0),
            Err(ArtifactCacheLimitError::TooSmall)
        ));
        assert!(matches!(
            ArtifactCacheLimit::from_unit(0, ByteUnit::Gibibytes),
            Err(ArtifactCacheLimitError::TooSmall)
        ));
        assert_eq!(
            ArtifactCacheLimit::from_bytes(MIB).expect("one mebibyte"),
            ArtifactCacheLimit::Limited(MIB)
        );
    }

    #[test]
    fn custom_text_that_is_not_a_whole_number_is_refused() {
        for hostile in ["", "  ", "ten", "-5", "1.5", "1e9", "500 GiB", "٥٠٠"] {
            assert!(
                ArtifactCacheLimit::parse_in_unit(hostile, ByteUnit::Gibibytes).is_err(),
                "`{hostile}` should be refused"
            );
        }
        assert_eq!(
            ArtifactCacheLimit::parse_in_unit(" 500 ", ByteUnit::Gibibytes).expect("500 GiB"),
            ArtifactCacheLimit::Limited(500 * GIB)
        );
    }

    #[test]
    fn unlimited_has_no_watermarks_and_is_never_exceeded() {
        let limit = ArtifactCacheLimit::Unlimited;
        assert_eq!(limit.bytes(), None);
        assert_eq!(limit.high_water(), None);
        assert_eq!(limit.cleanup_target(), None);
        assert!(!limit.is_exceeded_by(u64::MAX));
    }

    #[test]
    fn the_cleanup_target_is_four_fifths_of_the_limit_for_every_representable_limit() {
        assert_eq!(
            ArtifactCacheLimit::Limited(10 * GIB).cleanup_target(),
            Some(8 * GIB)
        );
        assert_eq!(ArtifactCacheLimit::Limited(9).cleanup_target(), Some(7));
        // The product of these and 80 overflows `u64`, which is exactly the
        // arithmetic the 128-bit intermediate exists for.
        assert_eq!(
            ArtifactCacheLimit::Limited(MAX_ARTIFACT_CACHE_LIMIT_BYTES).cleanup_target(),
            Some(3_689_348_814_741_910_323)
        );
        assert_eq!(
            ArtifactCacheLimit::Limited(u64::MAX).cleanup_target(),
            Some(14_757_395_258_967_641_292)
        );
    }

    #[test]
    fn collection_starts_above_the_limit_and_not_at_it() {
        let limit = ArtifactCacheLimit::Limited(1_000);
        assert!(!limit.is_exceeded_by(999));
        assert!(!limit.is_exceeded_by(1_000));
        assert!(limit.is_exceeded_by(1_001));
    }

    #[test]
    fn sizes_are_rendered_in_binary_units_without_noisy_decimals() {
        assert_eq!(format_binary_bytes(0), "0 B");
        assert_eq!(format_binary_bytes(512), "512 B");
        assert_eq!(format_binary_bytes(10 * GIB), "10 GiB");
        assert_eq!(format_binary_bytes(500 * GIB), "500 GiB");
        assert_eq!(format_binary_bytes(TIB), "1 TiB");
        assert_eq!(format_binary_bytes(GIB + GIB / 2), "1.5 GiB");
        assert_eq!(format_binary_bytes(TIB + TIB / 4), "1.25 TiB");
        assert_eq!(format_binary_bytes(2 * MIB + 512 * KIB), "2.5 MiB");
        assert_eq!(format_binary_bytes(1536), "1.5 KiB");
        assert_eq!(
            format_binary_bytes(MAX_ARTIFACT_CACHE_LIMIT_BYTES),
            "4 EiB",
            "the representable ceiling has to be statable exactly"
        );
    }

    #[test]
    fn a_unit_label_round_trips_through_its_own_abbreviation() {
        for unit in ByteUnit::ALL {
            assert_eq!(ByteUnit::from_label(unit.label()), Some(unit));
        }
        assert_eq!(ByteUnit::from_label(" gib "), Some(ByteUnit::Gibibytes));
        assert_eq!(ByteUnit::from_label("GB"), None);
        assert_eq!(ByteUnit::from_label(""), None);
    }

    #[test]
    fn a_limit_displays_as_a_technical_value_a_log_can_be_read_from() {
        assert_eq!(ArtifactCacheLimit::default().to_string(), "10 GiB");
        assert_eq!(ArtifactCacheLimit::Unlimited.to_string(), "unlimited");
    }
}
