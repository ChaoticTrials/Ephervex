//! The version ranges Forge and NeoForge mods declare for their dependencies.
//!
//! They follow Maven's version range syntax
//! (<https://maven.apache.org/enforcer/enforcer-rules/versionRanges.html>)
//! and version order
//! (<https://maven.apache.org/pom.html#version-order-specification>). Only
//! what can be compared without guessing is combined: single intervals whose
//! bounds are numbers, compared only where a plain numeric reading agrees with
//! Maven's order. Maven treats `.` and `-` differently (`1-1` sorts before
//! `1.1`) and orders qualifiers such as `beta` by rules of its own, so a range
//! that relies on either is never narrowed.

use std::cmp::Ordering;

/// The one range every given range accepts, as narrow as they require.
///
/// `None` when that cannot be worked out with certainty: a range this reads
/// no single interval from, bounds that cannot be compared for certain, or
/// ranges that accept no version in common. The ranges themselves are then
/// what to show.
#[must_use]
pub(crate) fn narrowest_range<'a>(ranges: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut distinct: Vec<&str> = Vec::new();
    for range in ranges {
        if !distinct.contains(&range) {
            distinct.push(range);
        }
    }
    if let [only] = distinct.as_slice() {
        return Some((*only).to_owned());
    }
    let mut combined = Interval {
        lower: None,
        upper: None,
    };
    for range in distinct {
        let interval = Interval::parse(range)?;
        combined.lower = tighter(combined.lower, interval.lower, Ordering::Greater).ok()?;
        combined.upper = tighter(combined.upper, interval.upper, Ordering::Less).ok()?;
    }
    combined.is_satisfiable()?.then(|| combined.to_string())
}

#[derive(Debug, Clone)]
struct Bound {
    /// As the range wrote it.
    version: String,
    numbers: Vec<u64>,
    /// The separator before each number after the first.
    separators: Vec<char>,
    inclusive: bool,
}

impl Bound {
    fn parse(version: &str, inclusive: bool) -> Option<Self> {
        let version = version.trim();
        let numbers = version
            .split(['.', '-'])
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            version: version.to_owned(),
            numbers,
            separators: version.chars().filter(|c| matches!(c, '.' | '-')).collect(),
            inclusive,
        })
    }

    /// Maven's order, where a numeric reading is certain to agree with it:
    /// two dotted versions, whose missing trailing parts count as zero as in
    /// Maven (`1.0` equals `1.0.0`), or two versions with `.` and `-` in the
    /// same places. `None` for any other pair.
    fn compare(&self, other: &Self) -> Option<Ordering> {
        let dotted = |bound: &Self| bound.separators.iter().all(|&c| c == '.');
        if !(dotted(self) && dotted(other) || self.separators == other.separators) {
            return None;
        }
        let length = self.numbers.len().max(other.numbers.len());
        let at = |numbers: &[u64], index: usize| numbers.get(index).copied().unwrap_or(0);
        Some(
            (0..length)
                .map(|index| at(&self.numbers, index).cmp(&at(&other.numbers, index)))
                .find(|ordering| ordering.is_ne())
                .unwrap_or(Ordering::Equal),
        )
    }
}

/// Two bounds whose order cannot be known for certain.
#[derive(Debug)]
struct Uncertain;

/// Of two lower bounds the greater is tighter, of two upper bounds the lesser.
/// At the same version an exclusive bound is the tighter one.
fn tighter(
    current: Option<Bound>,
    next: Option<Bound>,
    tighter_side: Ordering,
) -> Result<Option<Bound>, Uncertain> {
    Ok(match (current, next) {
        (None, next) => next,
        (current, None) => current,
        (Some(current), Some(next)) => match next.compare(&current).ok_or(Uncertain)? {
            Ordering::Equal if !next.inclusive => Some(next),
            Ordering::Equal => Some(current),
            ordering if ordering == tighter_side => Some(next),
            _ => Some(current),
        },
    })
}

#[derive(Debug)]
struct Interval {
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl Interval {
    fn parse(range: &str) -> Option<Self> {
        let range = range.trim();
        let inner = range.get(1..range.len().checked_sub(1)?)?;
        let opens_inclusive = match range.chars().next()? {
            '[' => true,
            '(' => false,
            _ => return None,
        };
        let closes_inclusive = match range.chars().last()? {
            ']' => true,
            ')' => false,
            _ => return None,
        };
        // More than one interval, such as `[1,2),[3,4)`.
        if inner.contains(['[', ']', '(', ')']) {
            return None;
        }
        let Some((lower, upper)) = inner.split_once(',') else {
            // `[1.0]` is exactly one version.
            if !(opens_inclusive && closes_inclusive) {
                return None;
            }
            let exact = Bound::parse(inner, true)?;
            return Some(Self {
                lower: Some(exact.clone()),
                upper: Some(exact),
            });
        };
        if upper.contains(',') {
            return None;
        }
        let side = |version: &str, inclusive: bool| -> Option<Option<Bound>> {
            if version.trim().is_empty() {
                // An unbounded side is always written open.
                (!inclusive).then_some(None)
            } else {
                Bound::parse(version, inclusive).map(Some)
            }
        };
        Some(Self {
            lower: side(lower, opens_inclusive)?,
            upper: side(upper, closes_inclusive)?,
        })
    }

    /// Whether any version lies between the bounds, or `None` when the two
    /// bounds cannot be compared for certain.
    fn is_satisfiable(&self) -> Option<bool> {
        match (&self.lower, &self.upper) {
            (Some(lower), Some(upper)) => Some(match lower.compare(upper)? {
                Ordering::Less => true,
                Ordering::Equal => lower.inclusive && upper.inclusive,
                Ordering::Greater => false,
            }),
            _ => Some(true),
        }
    }
}

impl std::fmt::Display for Interval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let (Some(lower), Some(upper)) = (&self.lower, &self.upper)
            && lower.compare(upper) == Some(Ordering::Equal)
        {
            return write!(formatter, "[{}]", lower.version);
        }
        let (open, lower) = self.lower.as_ref().map_or(("(", ""), |bound| {
            (
                if bound.inclusive { "[" } else { "(" },
                bound.version.as_str(),
            )
        });
        let (close, upper) = self.upper.as_ref().map_or((")", ""), |bound| {
            (
                if bound.inclusive { "]" } else { ")" },
                bound.version.as_str(),
            )
        });
        write!(formatter, "{open}{lower},{upper}{close}")
    }
}

#[cfg(test)]
mod tests {
    use super::narrowest_range;

    /// Every order of the same ranges.
    fn orders<'a>(ranges: &[&'a str]) -> Vec<Vec<&'a str>> {
        if ranges.len() <= 1 {
            return vec![ranges.to_vec()];
        }
        let mut orders = Vec::new();
        for (index, first) in ranges.iter().enumerate() {
            let mut rest = ranges.to_vec();
            rest.remove(index);
            for mut order in self::orders(&rest) {
                order.insert(0, first);
                orders.push(order);
            }
        }
        orders
    }

    #[test]
    fn open_ended_ranges_narrow_to_the_highest_minimum_in_any_order() {
        for order in orders(&[
            "[1.19.3-4.3.0,)",
            "[1.20.1-5.0.5,)",
            "[1.20.1-5.0.1,)",
            "[1.20.1-5.0.14,)",
        ]) {
            assert_eq!(
                narrowest_range(order.iter().copied()).as_deref(),
                Some("[1.20.1-5.0.14,)"),
                "{order:?}"
            );
        }
    }

    #[test]
    fn upper_and_lower_bounds_both_narrow() {
        assert_eq!(
            narrowest_range(["[1.0,3.0)", "[2.0,)", "(,2.5]"]).as_deref(),
            Some("[2.0,2.5]")
        );
        assert_eq!(
            narrowest_range(["[2.0,)", "(2.0,3)"]).as_deref(),
            Some("(2.0,3)"),
            "an exclusive bound at the same version is the tighter one"
        );
        assert_eq!(
            narrowest_range(["[1.0,2.0]", "[2.0,)"]).as_deref(),
            Some("[2.0]")
        );
    }

    #[test]
    fn one_range_is_its_own_answer_whatever_it_says() {
        assert_eq!(
            narrowest_range(["[47.1-beta,)", "[47.1-beta,)"]).as_deref(),
            Some("[47.1-beta,)")
        );
    }

    /// Maven sorts `1-1` before `1.1`, so a numeric reading that calls them
    /// equal would accept versions one requester refuses. Such bounds are left
    /// unnarrowed, whichever order the requests came in.
    #[test]
    fn a_dash_and_a_dot_in_the_same_place_are_never_treated_as_equal() {
        for order in orders(&["[1-1,)", "[1.1,)"]) {
            assert_eq!(narrowest_range(order.iter().copied()), None, "{order:?}");
        }
        for order in orders(&["[1-1]", "[1.1]"]) {
            assert_eq!(narrowest_range(order.iter().copied()), None, "{order:?}");
        }
        for order in orders(&["[1.20.1-5.0.1,)", "[1.20.1.5,)"]) {
            assert_eq!(narrowest_range(order.iter().copied()), None, "{order:?}");
        }
    }

    #[test]
    fn anything_uncertain_is_left_unnarrowed() {
        // A qualifier Maven orders by rules of its own.
        assert_eq!(narrowest_range(["[1.0-beta,)", "[1.0,)"]), None);
        // More than one interval.
        assert_eq!(narrowest_range(["[1,2),[3,4)", "[1,)"]), None);
        // A soft requirement rather than a range.
        assert_eq!(narrowest_range(["1.0", "[1.0,)"]), None);
        // Nothing both accept.
        assert_eq!(narrowest_range(["[1.0,2.0)", "[3.0,)"]), None);
        assert_eq!(narrowest_range(["[1.0,2.0)", "[2.0,)"]), None);
    }

    #[test]
    fn trailing_zeros_do_not_make_a_dotted_version_newer() {
        for order in orders(&["[1.0,)", "[1.0.0,)"]) {
            let narrowed = narrowest_range(order.iter().copied());
            assert!(
                matches!(narrowed.as_deref(), Some("[1.0,)" | "[1.0.0,)")),
                "{order:?}: {narrowed:?}"
            );
        }
    }
}
