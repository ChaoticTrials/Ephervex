//! Live statistics a running Minecraft server reports about itself on its
//! console: tick rate and the players online.
//!
//! Everything here is read from captured console text, which the server
//! process, its mods, and its players control. Each value is checked and
//! bounded before it is kept, and a line that does not match a known report
//! exactly is ignored.

use std::collections::VecDeque;

use crate::capture::CapturedLine;

/// How many tick-rate reports are kept, oldest dropped first.
pub const MAX_TICK_RATE_SAMPLES: usize = 120;
/// How many player names are kept from one report.
pub const MAX_LISTED_PLAYERS: usize = 256;
/// The longest name kept, in characters. Minecraft names are 16 long; the
/// margin covers prefixes that bridge plugins add.
pub const MAX_NAME_CHARS: usize = 48;
/// The highest tick rate accepted. A server cannot report more than its
/// configured rate, so anything above this is not a real report.
pub const MAX_PLAUSIBLE_TICK_RATE: f32 = 1_000.0;
/// The highest player limit accepted.
pub const MAX_PLAUSIBLE_PLAYER_COUNT: u32 = 1_000_000;

/// The players a server last reported.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerRoster {
    /// The names kept, in the order reported. At most [`MAX_LISTED_PLAYERS`].
    pub names: Vec<String>,
    /// How many players the server counts online, which can exceed `names`.
    pub online: u32,
    /// The server's player limit, when its report gave one.
    pub max: Option<u32>,
}

/// What a server's console reported since its latest start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerStatistics {
    tick_rates: VecDeque<f32>,
    players: Option<PlayerRoster>,
    /// The count and limit of a `list` header that prints its names on the
    /// next line, as servers before 1.13 do.
    awaiting_names: Option<(u32, u32)>,
}

impl ServerStatistics {
    /// Statistics read from a run's captured lines, oldest first.
    #[must_use]
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a CapturedLine>) -> Self {
        let mut statistics = Self::default();
        for line in lines {
            statistics.observe(&line.text);
        }
        statistics
    }

    /// Takes one console line. Returns whether it changed the statistics.
    pub fn observe(&mut self, text: &str) -> bool {
        let text = strip_terminal_escapes(text);
        let message = console_message(&text);
        if let Some((online, max)) = self.awaiting_names.take()
            && let Some(roster) = parse_legacy_names(message, online, max)
        {
            self.players = Some(roster);
            return true;
        }
        if let Some(rate) = parse_tick_rate(message) {
            if self.tick_rates.len() == MAX_TICK_RATE_SAMPLES {
                self.tick_rates.pop_front();
            }
            self.tick_rates.push_back(rate);
            return true;
        }
        if let Some(roster) = parse_player_list(message) {
            self.players = Some(roster);
            return true;
        }
        if let Some((online, max)) = parse_legacy_list_header(message) {
            if online == 0 {
                self.players = Some(PlayerRoster {
                    names: Vec::new(),
                    online,
                    max: Some(max),
                });
                return true;
            }
            self.awaiting_names = Some((online, max));
            return false;
        }
        if let Some((name, joined)) = parse_player_movement(message) {
            return self.move_player(name, joined);
        }
        false
    }

    /// The tick rates reported, oldest first, in ticks per second.
    #[must_use]
    pub fn tick_rates(&self) -> Vec<f32> {
        self.tick_rates.iter().copied().collect()
    }

    /// The newest roster the server reported, kept current by joins and
    /// leaves seen since. `None` until a report arrived.
    #[must_use]
    pub const fn players(&self) -> Option<&PlayerRoster> {
        self.players.as_ref()
    }

    /// A join or leave only adjusts a roster that a report established: the
    /// run's earlier joins are unknown otherwise.
    fn move_player(&mut self, name: &str, joined: bool) -> bool {
        let Some(roster) = self.players.as_mut() else {
            return false;
        };
        let position = roster.names.iter().position(|known| known == name);
        match (joined, position) {
            (true, None) => {
                roster.online = roster.online.saturating_add(1);
                if roster.names.len() < MAX_LISTED_PLAYERS {
                    roster.names.push(name.to_owned());
                }
                true
            }
            (false, Some(index)) => {
                roster.names.remove(index);
                roster.online = roster.online.saturating_sub(1);
                true
            }
            _ => false,
        }
    }
}

/// Shortens text from a server to a name that is safe to show: control
/// characters become spaces, and a long value is cut at [`MAX_NAME_CHARS`].
#[must_use]
pub fn bounded_display_name(text: &str) -> String {
    text.chars()
        .take(MAX_NAME_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Removes terminal escape sequences, which a console with colors prints
/// around its text.
fn strip_terminal_escapes(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            kept.push(character);
            continue;
        }
        if characters.next_if_eq(&'[').is_some() {
            // A control sequence ends at its first byte in `@` through `~`.
            for next in characters.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    kept
}

/// The message of a console line, without the log prefix.
///
/// The prefix ends at the first `]: `, which precedes anything a player or
/// mod put in the message, so text later in the line cannot pose as a prefix.
fn console_message(line: &str) -> &str {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.starts_with('[')
        && let Some(end) = line.find("]: ")
    {
        return &line[end + 3..];
    }
    line
}

/// The tick rate in a report, from the formats the supported servers print.
///
/// Forge prints `Overall: Mean tick time: 5.2 ms. Mean TPS: 20.000` for its
/// `tps` command, and Forge 1.12 puts a space before the colon. NeoForge prints
/// `Overall: 20.000 TPS (5.2 ms/tick)`. Vanilla prints
/// `Average time per tick: 2.1ms (Target: 50.0ms)` for `tick query`, where the
/// rate is bounded by the target.
fn parse_tick_rate(message: &str) -> Option<f32> {
    if let Some(rest) = message.strip_prefix("Overall") {
        let rest = rest.trim_start().strip_prefix(':')?.trim_start();
        if let Some(rest) = rest.strip_prefix("Mean tick time: ") {
            let rate = rest.split_once("Mean TPS: ")?.1;
            return plausible_rate(leading_number(rate)?);
        }
        let end = number_end(rest);
        rest[end..].strip_prefix(" TPS (")?;
        return plausible_rate(parse_number(&rest[..end])?);
    }
    let rest = message.strip_prefix("Average time per tick: ")?;
    let (average, rest) = rest.split_once("ms (Target: ")?;
    let target = rest.strip_suffix("ms)")?;
    let average = parse_number(average)?;
    let target = parse_number(target)?;
    if target <= 0.0 || average < 0.0 {
        return None;
    }
    let achievable = if average == 0.0 {
        f32::INFINITY
    } else {
        1_000.0 / average
    };
    plausible_rate(achievable.min(1_000.0 / target))
}

fn plausible_rate(rate: f32) -> Option<f32> {
    (rate.is_finite() && (0.0..=MAX_PLAUSIBLE_TICK_RATE).contains(&rate)).then_some(rate)
}

/// A decimal number. Forge and NeoForge format theirs with the JVM's locale,
/// so a comma may stand where the point does; a number with both, or with more
/// than one separator, is not one.
fn parse_number(text: &str) -> Option<f32> {
    let text = text.trim();
    let separators = text
        .bytes()
        .filter(|byte| matches!(byte, b'.' | b','))
        .count();
    if text.is_empty()
        || separators > 1
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b','))
    {
        return None;
    }
    text.replace(',', ".").parse().ok()
}

fn leading_number(text: &str) -> Option<f32> {
    parse_number(&text[..number_end(text)])
}

/// Where the number at the start of `text` ends.
fn number_end(text: &str) -> usize {
    text.bytes()
        .position(|byte| !(byte.is_ascii_digit() || matches!(byte, b'.' | b',')))
        .unwrap_or(text.len())
}

/// The roster in a `list` report:
/// `There are 2 of a max of 20 players online: Alice, Bob`.
fn parse_player_list(message: &str) -> Option<PlayerRoster> {
    let rest = message.strip_prefix("There are ")?;
    let (online, rest) = rest.split_once(" of a max of ")?;
    let (max, names) = rest.split_once(" players online:")?;
    let online = count(online)?;
    let max = count(max)?;
    let mut roster = PlayerRoster {
        names: Vec::new(),
        online,
        max: Some(max),
    };
    for name in names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !is_player_name(name) {
            return None;
        }
        if roster.names.len() < MAX_LISTED_PLAYERS {
            roster.names.push(name.to_owned());
        }
    }
    Some(roster)
}

/// The header of a `list` report before 1.13, `There are 2/20 players online:`,
/// whose names follow as a message of their own.
fn parse_legacy_list_header(message: &str) -> Option<(u32, u32)> {
    let counts = message
        .strip_prefix("There are ")?
        .strip_suffix(" players online:")?;
    let (online, max) = counts.split_once('/')?;
    Some((count(online)?, count(max)?))
}

/// The names line after a legacy header. It must name exactly the players the
/// header counted, each a valid name, so no other line can stand in for it.
fn parse_legacy_names(message: &str, online: u32, max: u32) -> Option<PlayerRoster> {
    let mut names = Vec::new();
    let mut total = 0_usize;
    for name in message
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !is_player_name(name) {
            return None;
        }
        total += 1;
        if names.len() < MAX_LISTED_PLAYERS {
            names.push(name.to_owned());
        }
    }
    (total == usize::try_from(online).ok()?).then_some(PlayerRoster {
        names,
        online,
        max: Some(max),
    })
}

fn count(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse()
        .ok()
        .filter(|value| *value <= MAX_PLAUSIBLE_PLAYER_COUNT)
}

/// A join or leave notice: `Alice joined the game`. The message must be the
/// notice alone, so a chat line (`<Alice> ...`) or a broadcast
/// (`[Server] ...`) never matches.
fn parse_player_movement(message: &str) -> Option<(&str, bool)> {
    let (name, joined) = if let Some(name) = message.strip_suffix(" joined the game") {
        (name, true)
    } else {
        (message.strip_suffix(" left the game")?, false)
    };
    is_player_name(name).then_some((name, joined))
}

/// Whether text can be one player's name. Minecraft names are letters,
/// digits, and underscores; bridged accounts add a leading mark, so only the
/// characters that would let other text pose as a name are refused.
fn is_player_name(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= MAX_NAME_CHARS
        && !text.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '<' | '>' | '[' | ']' | ':' | ',')
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn statistics(lines: &[&str]) -> ServerStatistics {
        let mut statistics = ServerStatistics::default();
        for line in lines {
            statistics.observe(line);
        }
        statistics
    }

    #[test]
    fn a_forge_tps_report_is_read_through_its_log_prefix() {
        let read = statistics(&[
            "[25Oct2026 12:00:00.123] [Server thread/INFO] [minecraft/MinecraftServer]: Dim minecraft:overworld (minecraft:overworld): Mean tick time: 9.1 ms. Mean TPS: 20.000",
            "[25Oct2026 12:00:00.124] [Server thread/INFO] [minecraft/MinecraftServer]: Overall: Mean tick time: 12.5 ms. Mean TPS: 20.000",
            "[12:00:30] [Server thread/INFO]: Overall: Mean tick time: 80.0 ms. Mean TPS: 12.500",
        ]);
        assert_eq!(read.tick_rates(), [20.0, 12.5]);
    }

    #[test]
    fn a_neoforge_tps_report_and_the_older_forge_spelling_are_read() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: minecraft:overworld: 20.000 TPS (1.234 ms/tick)",
            "[12:00:00] [Server thread/INFO]: Overall: 17.500 TPS (57.100 ms/tick)",
            "[12:00:30] [Server thread/INFO]: Overall : Mean tick time: 80.0 ms. Mean TPS: 12.500",
        ]);
        assert_eq!(read.tick_rates(), [17.5, 12.5]);
    }

    #[test]
    fn a_malformed_neoforge_report_is_ignored() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: Overall: fast TPS (1 ms/tick)",
            "[12:00:00] [Server thread/INFO]: Overall: 20.000 ticks",
            "[12:00:00] [Server thread/INFO]: Overall: 99999 TPS (1 ms/tick)",
            "[12:00:00] [Server thread/INFO]: Overall: 20.000",
        ]);
        assert!(read.tick_rates().is_empty());
    }

    #[test]
    fn a_vanilla_tick_query_reports_the_rate_the_average_allows() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: Average time per tick: 2.1ms (Target: 50.0ms)",
            "[12:00:30] [Server thread/INFO]: Average time per tick: 100.0ms (Target: 50.0ms)",
            "[12:01:00] [Server thread/INFO]: Average time per tick: 0.0ms (Target: 50.0ms)",
        ]);
        assert_eq!(read.tick_rates(), [20.0, 10.0, 20.0]);
    }

    #[test]
    fn implausible_or_malformed_tick_reports_are_ignored() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: Overall: Mean tick time: 1 ms. Mean TPS: 99999.0",
            "[12:00:00] [Server thread/INFO]: Overall: Mean tick time: 1 ms. Mean TPS: NaN",
            "[12:00:00] [Server thread/INFO]: Overall: Mean tick time: 1 ms. Mean TPS: -3",
            "[12:00:00] [Server thread/INFO]: Average time per tick: 4ms (Target: 0.0ms)",
            "[12:00:00] [Server thread/INFO]: Average time per tick: fastms (Target: 50.0ms)",
        ]);
        assert!(read.tick_rates().is_empty());
    }

    #[test]
    fn tick_rate_history_keeps_only_the_newest_samples() {
        let mut read = ServerStatistics::default();
        for step in 0..(MAX_TICK_RATE_SAMPLES + 5) {
            read.observe(&format!(
                "[12:00:00] [Server thread/INFO]: Overall: Mean tick time: 1 ms. Mean TPS: {step}.0"
            ));
        }
        let rates = read.tick_rates();
        assert_eq!(rates.len(), MAX_TICK_RATE_SAMPLES);
        assert_eq!(rates.first().copied(), Some(5.0));
    }

    #[test]
    fn a_list_report_establishes_the_roster() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 2 of a max of 20 players online: Alice, Bob_2",
        ]);
        let roster = read.players().expect("roster");
        assert_eq!(roster.names, ["Alice", "Bob_2"]);
        assert_eq!((roster.online, roster.max), (2, Some(20)));
    }

    #[test]
    fn decimal_commas_in_a_report_are_read_as_decimal_points() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: Overall: Mean tick time: 57,100 ms. Mean TPS: 12,500",
            "[12:00:01] [Server thread/INFO]: Overall: 17,500 TPS (57,100 ms/tick)",
            "[12:00:02] [Server thread/INFO]: Overall: 1,234.5 TPS (1 ms/tick)",
        ]);
        assert_eq!(read.tick_rates(), [12.5, 17.5]);
    }

    #[test]
    fn a_legacy_list_report_takes_its_names_from_the_next_line() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 2/20 players online:",
            "[12:00:00] [Server thread/INFO]: Alice, Bob",
        ]);
        let roster = read.players().expect("roster");
        assert_eq!(roster.names, ["Alice", "Bob"]);
        assert_eq!((roster.online, roster.max), (2, Some(20)));
    }

    #[test]
    fn a_legacy_report_of_nobody_needs_no_names_line() {
        let read = statistics(&["[12:00:00] [Server thread/INFO]: There are 0/20 players online:"]);
        let roster = read.players().expect("roster");
        assert!(roster.names.is_empty());
        assert_eq!(roster.max, Some(20));
    }

    #[test]
    fn only_a_line_naming_the_counted_players_completes_a_legacy_report() {
        for following in [
            "[12:00:01] [Server thread/INFO]: <Mallory> Eve, Frank",
            "[12:00:01] [Server thread/INFO]: Eve",
            "[12:00:01] [Server thread/INFO]: Eve, Frank, Gus",
            "[12:00:01] [Server thread/INFO]: Saved the game",
        ] {
            let read = statistics(&[
                "[12:00:00] [Server thread/INFO]: There are 2/20 players online:",
                following,
            ]);
            assert!(read.players().is_none(), "{following}");
        }
    }

    #[test]
    fn a_names_line_without_a_header_is_ignored() {
        let read = statistics(&["[12:00:01] [Server thread/INFO]: Alice, Bob"]);
        assert!(read.players().is_none());
    }

    #[test]
    fn an_empty_list_report_is_an_empty_roster() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 0 of a max of 20 players online: ",
        ]);
        let roster = read.players().expect("roster");
        assert!(roster.names.is_empty());
        assert_eq!(roster.online, 0);
    }

    #[test]
    fn joins_and_leaves_keep_an_established_roster_current() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 1 of a max of 20 players online: Alice",
            "[12:00:10] [Server thread/INFO]: Bob joined the game",
            "[12:00:20] [Server thread/INFO]: Alice left the game",
            "[12:00:21] [Server thread/INFO]: Alice left the game",
        ]);
        let roster = read.players().expect("roster");
        assert_eq!(roster.names, ["Bob"]);
        assert_eq!(roster.online, 1);
    }

    #[test]
    fn joins_before_any_report_do_not_invent_a_roster() {
        let read = statistics(&["[12:00:10] [Server thread/INFO]: Bob joined the game"]);
        assert!(read.players().is_none());
    }

    #[test]
    fn chat_and_broadcasts_cannot_pose_as_player_notices() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 0 of a max of 20 players online: ",
            "[12:00:01] [Server thread/INFO]: <Mallory> Eve joined the game",
            "[12:00:02] [Server thread/INFO]: [Server] Eve joined the game",
            "[12:00:03] [Server thread/INFO]: <Mallory> hi]: Eve joined the game",
            "[12:00:04] [Server thread/INFO]: Eve and Frank joined the game",
            "[12:00:05] [Server thread/INFO]: <Mallory> There are 9 of a max of 9 players online: Eve",
        ]);
        let roster = read.players().expect("roster");
        assert!(roster.names.is_empty());
        assert_eq!(roster.online, 0);
    }

    #[test]
    fn a_report_naming_something_that_is_not_a_player_is_ignored() {
        let read = statistics(&[
            "[12:00:00] [Server thread/INFO]: There are 1 of a max of 20 players online: <b>x</b>",
        ]);
        assert!(read.players().is_none());
    }

    #[test]
    fn roster_and_counts_are_bounded() {
        let names: Vec<String> = (0..(MAX_LISTED_PLAYERS + 10))
            .map(|index| format!("P{index}"))
            .collect();
        let read = statistics(&[&format!(
            "[12:00:00] [Server thread/INFO]: There are {} of a max of 4294967295 players online: {}",
            names.len(),
            names.join(", ")
        )]);
        assert!(
            read.players().is_none(),
            "a limit above the bound is refused"
        );
        let read = statistics(&[&format!(
            "[12:00:00] [Server thread/INFO]: There are {} of a max of 500 players online: {}",
            names.len(),
            names.join(", ")
        )]);
        let roster = read.players().expect("roster");
        assert_eq!(roster.names.len(), MAX_LISTED_PLAYERS);
        assert_eq!(roster.online as usize, names.len());
    }

    #[test]
    fn terminal_colors_around_a_report_are_ignored() {
        let read = statistics(&[
            "\u{1b}[33;1m[12:00:00 INFO]\u{1b}[m: Overall: Mean tick time: 5 ms. Mean TPS: 20.0",
        ]);
        assert_eq!(read.tick_rates(), [20.0]);
    }

    #[test]
    fn a_line_without_a_log_prefix_is_read_as_its_message() {
        let read = statistics(&["Overall: Mean tick time: 5 ms. Mean TPS: 20.0"]);
        assert_eq!(read.tick_rates(), [20.0]);
    }

    #[test]
    fn display_names_are_cut_and_stripped_of_control_characters() {
        let name = bounded_display_name(&format!("a\nb{}", "x".repeat(100)));
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
        assert!(!name.contains('\n'));
    }
}
