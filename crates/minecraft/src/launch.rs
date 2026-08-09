//! Building the command line that starts Minecraft.
//!
//! Everything here is a pure function of the metadata, the installation plan,
//! the account, and the settings. Nothing reads the network, nothing touches
//! the filesystem beyond creating the game directory, and nothing spawns a
//! process — [`crate::process`] does that. The split is what lets the whole of
//! argument selection, rule evaluation, classpath order, and placeholder
//! substitution be tested against real Mojang metadata without a JVM
//!
//! # The placeholder set is closed, and an unknown one is a failure
//!
//! Verified against every `type == "release"` document at or above 1.12.2 on
//! **2026-08-08** — 51 versions — the templates name exactly twenty-one
//! placeholders and no others. That is what makes [`LaunchError::
//! UnresolvedPlaceholder`](crate::error::LaunchError::UnresolvedPlaceholder)
//! the right answer for anything else. Leaving a `${…}` in place sends the
//! literal text to the game; blanking it changes what the game is told without
//! saying so. Both are the silent approximation Ephervex does not allow, and a
//! new placeholder is exactly the signal that this build has fallen behind
//! Mojang.
//!
//! # `default-user-jvm` is a default, not a requirement
//!
//! The 26.x releases added a third argument group holding Mojang's recommended
//! heap and garbage-collector flags. Ephervex gives the *user* the memory
//! settings and a JVM-arguments box, so:
//!
//! * heap flags from the group are always dropped — the user's Minimum and
//!   Maximum RAM are what a launcher's memory settings mean;
//! * the remaining flags are included only when the user has supplied no JVM
//!   arguments of their own, because the custom JVM argument box replaces rather than
//!   addition.
//!
//! The group is never mixed into the required `jvm` arguments, which the game
//! genuinely needs.
//!
//! # 1.12.2 declares no JVM arguments at all
//!
//! The legacy form is a bare `minecraftArguments` string: no `-cp`, no
//! `-Djava.library.path`, no launcher brand. A launcher is expected to know
//! them, so [`LEGACY_JVM_ARGUMENTS`] supplies exactly the set 1.13 later wrote
//! down — which is how the two eras end up producing the same shape of command
//! line rather than two subtly different ones.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ephervex_model::{APP_ID, APP_VERSION, ConnectionTarget};
use tracing::debug;

use crate::arguments::{ArgumentForm, ArgumentGroup};
use crate::error::{LaunchError, LaunchResult};
use crate::install::InstallationPlan;
use crate::platform::Platform;
use crate::rules::{RuleContext, rules_allow};
use crate::version::VersionMetadata;

/// The JVM arguments a pre-1.13 version does not declare and still needs.
///
/// Written as templates rather than as resolved strings so they go through
/// exactly the same substitution as the modern `jvm` group — one code path, one
/// set of placeholders, one place a mistake can be made. These are the four
/// 1.13 turned into metadata, minus the ones only newer LWJGL reads.
pub const LEGACY_JVM_ARGUMENTS: &[&str] = &[
    "-Djava.library.path=${natives_directory}",
    "-Dminecraft.launcher.brand=${launcher_name}",
    "-Dminecraft.launcher.version=${launcher_version}",
    "-cp",
    "${classpath}",
];

/// The separator between classpath entries.
///
/// A constant rather than `std::path::MAIN_SEPARATOR`-adjacent guesswork: this
/// is the JVM's `path.separator`, which is `:` on every platform Ephervex
/// targets and is not the same question as the filesystem separator.
const CLASSPATH_SEPARATOR: &str = ":";

/// Who the game is told it is running as.
///
/// Deliberately not `ephervex-auth`'s `Session`: this crate does not depend on
/// the auth crate, and the launch pipeline needs six strings rather than a
/// token chain. `ephervex-core` maps a session onto one of these, which is also
/// where the two account kinds are forced to be handled explicitly.
#[derive(Clone, PartialEq, Eq)]
pub struct PlayerIdentity {
    pub name: String,
    /// The profile UUID, hyphenated.
    pub uuid: String,
    /// The Minecraft access token, or [`OFFLINE_ACCESS_TOKEN`] for an offline
    /// account.
    pub access_token: String,
    /// `msa` or `legacy` — see [`Self::microsoft`] and [`Self::offline`].
    pub user_type: String,
    /// The Xbox user id, when one is known. Empty otherwise.
    pub xuid: String,
    /// The launcher's client id for telemetry. Empty: Ephervex sends none.
    pub client_id: String,
}

/// What an offline account presents as an access token.
///
/// Not the empty string. An empty `--accessToken` is indistinguishable from a
/// Microsoft launch whose token substitution failed, and the game's own
/// argument parser treats a missing value differently from a present one. `0`
/// is the value offline launches have used since long before this launcher, and
/// it fails session validation exactly as it should.
pub const OFFLINE_ACCESS_TOKEN: &str = "0";

impl PlayerIdentity {
    /// An identity backed by a real Minecraft session.
    ///
    /// `user_type` is `msa`, which is what Mojang's own launcher passes for
    /// every version in the supported range — including 1.12.2, whose
    /// `Session.Type` predates the name.
    #[must_use]
    pub fn microsoft(
        name: impl Into<String>,
        uuid: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            uuid: uuid.into(),
            access_token: access_token.into(),
            user_type: "msa".to_owned(),
            xuid: String::new(),
            client_id: String::new(),
        }
    }

    /// A local identity that proves nothing.
    ///
    /// `user_type` is `legacy` rather than `msa`: it is a constant every
    /// version from 1.12.2 to 26.x defines, and claiming `msa` for an account
    /// that never authenticated would be the launcher asserting something
    /// untrue about the player.
    #[must_use]
    pub fn offline(name: impl Into<String>, uuid: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            uuid: uuid.into(),
            access_token: OFFLINE_ACCESS_TOKEN.to_owned(),
            user_type: "legacy".to_owned(),
            xuid: String::new(),
            client_id: String::new(),
        }
    }

    /// Whether this identity carries a real credential.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.access_token != OFFLINE_ACCESS_TOKEN && !self.access_token.is_empty()
    }
}

/// Redacts the token. A launch failure walks the error chain and a launcher
/// logs the identity it launched with; neither may print a credential
impl std::fmt::Debug for PlayerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerIdentity")
            .field("name", &self.name)
            .field("uuid", &self.uuid)
            .field("access_token", &"<redacted>")
            .field("user_type", &self.user_type)
            .finish_non_exhaustive()
    }
}

/// The launcher features a launch has turned on, and the values they need.
///
/// A map rather than a struct of `bool`s because Mojang adds feature flags
/// without warning — the quick-play family arrived in 1.20 — and because the
/// same values have to answer both a rule and a placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchFeatures {
    flags: BTreeMap<String, bool>,
    values: BTreeMap<String, String>,
    direct_connect: Option<ConnectionTarget>,
}

impl LaunchFeatures {
    /// Nothing enabled: an ordinary launch.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Runs the demo, as `--demo`.
    #[must_use]
    pub fn demo(mut self) -> Self {
        self.flags.insert("is_demo_user".to_owned(), true);
        self
    }

    /// Opens the window at a chosen size, as `--width` and `--height`.
    #[must_use]
    pub fn custom_resolution(mut self, width: u32, height: u32) -> Self {
        self.flags.insert("has_custom_resolution".to_owned(), true);
        self.values
            .insert("resolution_width".to_owned(), width.to_string());
        self.values
            .insert("resolution_height".to_owned(), height.to_string());
        self
    }

    /// Joins one server immediately after startup.
    ///
    /// Modern metadata receives Mojang's Quick Play feature and value. Older
    /// releases use the long-standing `--server` and `--port` game arguments
    /// in [`build_command`], so both paths remain in the normal launch layer.
    #[must_use]
    pub fn direct_connect(mut self, target: ConnectionTarget) -> Self {
        self.flags
            .insert("is_quick_play_multiplayer".to_owned(), true);
        self.values
            .insert("quickPlayMultiplayer".to_owned(), target.to_string());
        self.direct_connect = Some(target);
        self
    }

    /// The rules context these features and a platform describe.
    #[must_use]
    pub fn rule_context(&self, platform: Platform) -> RuleContext {
        RuleContext {
            platform,
            features: self.flags.clone(),
        }
    }

    /// Whether a flag is on.
    #[must_use]
    pub fn is_enabled(&self, flag: &str) -> bool {
        self.flags.get(flag).copied().unwrap_or(false)
    }
}

/// The memory and argument half of independent of how it is
/// stored.
///
/// A mirror of `ephervex-java`'s `EffectiveJvmSettings` rather than the type
/// itself, for the same reason that type mirrors the configuration file: the
/// dependency runs `java` → `minecraft`, so this crate cannot name it.
/// `ephervex-core` bridges the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmOptions {
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    /// The user's own JVM arguments. When non-empty they replace Mojang's
    /// recommended `default-user-jvm` flags, per the "Use global settings" option.
    pub extra_arguments: Vec<String>,
}

impl Default for JvmOptions {
    fn default() -> Self {
        Self {
            min_memory_mib: 512,
            max_memory_mib: 4096,
            extra_arguments: Vec::new(),
        }
    }
}

/// Everything needed to turn a plan into a command line.
#[derive(Debug, Clone)]
pub struct LaunchSettings {
    /// The `java` executable Java runtime support selected.
    pub java: PathBuf,
    /// The instance's own game directory: saves, options, logs, crash reports.
    pub game_directory: PathBuf,
    pub identity: PlayerIdentity,
    pub features: LaunchFeatures,
    pub jvm: JvmOptions,
}

/// One argument of the command line.
#[derive(Clone, PartialEq, Eq)]
struct Argument {
    value: String,
    /// Whether printing this argument would disclose a credential.
    sensitive: bool,
}

/// A resolved command line, ready to spawn.
///
/// Its `Debug` and [`Display`](std::fmt::Display) are redacted, because the one
/// thing a launcher inevitably does with a command line is log it, and one of
/// its arguments is a Minecraft access token.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub java: PathBuf,
    pub working_directory: PathBuf,
    pub main_class: String,
    arguments: Vec<Argument>,
    /// Where the `--` between JVM arguments and game arguments falls, so a
    /// caller can show the two halves apart.
    jvm_argument_count: usize,
}

impl LaunchCommand {
    /// Every argument, in order, as the JVM receives them.
    pub fn arguments(&self) -> impl Iterator<Item = &str> {
        self.arguments
            .iter()
            .map(|argument| argument.value.as_str())
    }

    /// The JVM half: the flags before the main class.
    pub fn jvm_arguments(&self) -> impl Iterator<Item = &str> {
        self.arguments
            .iter()
            .take(self.jvm_argument_count)
            .map(|argument| argument.value.as_str())
    }

    /// The game half: everything the main class is handed.
    pub fn game_arguments(&self) -> impl Iterator<Item = &str> {
        self.arguments
            .iter()
            .skip(self.jvm_argument_count + 1)
            .map(|argument| argument.value.as_str())
    }

    /// The arguments with every credential replaced by `<redacted>`.
    ///
    /// The only form that may be logged, printed by `--dry-run`, or attached to
    /// a bug report.
    #[must_use]
    pub fn redacted_arguments(&self) -> Vec<String> {
        self.arguments
            .iter()
            .map(|argument| {
                if argument.sensitive {
                    "<redacted>".to_owned()
                } else {
                    argument.value.clone()
                }
            })
            .collect()
    }
}

impl std::fmt::Debug for LaunchCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchCommand")
            .field("java", &self.java)
            .field("working_directory", &self.working_directory)
            .field("main_class", &self.main_class)
            .field("arguments", &self.redacted_arguments())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for LaunchCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.java.display())?;
        for argument in self.redacted_arguments() {
            write!(f, " {argument}")?;
        }
        Ok(())
    }
}

/// Builds the command line for one launch.
///
/// The order is the one Mojang's own launcher produces, and it is load-bearing:
/// the user's memory settings first so a metadata flag could override them, the
/// version's required JVM arguments next (which is where `-cp` lives), the
/// user's own arguments last so they win, then the main class, then the game
/// arguments.
pub fn build_command(
    metadata: &VersionMetadata,
    plan: &InstallationPlan,
    settings: &LaunchSettings,
) -> LaunchResult<LaunchCommand> {
    let context = settings.features.rule_context(plan.platform.clone());
    let classpath = plan
        .classpath()
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(CLASSPATH_SEPARATOR);

    let mut values: BTreeMap<&str, String> = BTreeMap::new();
    // -- jvm --
    values.insert("natives_directory", display(&plan.natives_dir));
    values.insert("launcher_name", APP_ID.to_owned());
    values.insert("launcher_version", APP_VERSION.to_owned());
    values.insert("classpath", classpath);
    values.insert("library_directory", display(&plan.libraries_dir));
    values.insert("classpath_separator", CLASSPATH_SEPARATOR.to_owned());
    // -- game --
    values.insert("auth_player_name", settings.identity.name.clone());
    values.insert("version_name", metadata.id.clone());
    values.insert("game_directory", display(&settings.game_directory));
    values.insert("assets_root", display(&plan.assets_dir));
    values.insert("assets_index_name", plan.asset_index_id.clone());
    values.insert("auth_uuid", settings.identity.uuid.clone());
    values.insert("auth_access_token", settings.identity.access_token.clone());
    values.insert("clientid", settings.identity.client_id.clone());
    values.insert("auth_xuid", settings.identity.xuid.clone());
    values.insert("user_type", settings.identity.user_type.clone());
    values.insert("version_type", metadata.release_type.as_str().to_owned());
    for (name, value) in &settings.features.values {
        values.insert(name.as_str(), value.clone());
    }

    let mut arguments: Vec<Argument> = Vec::new();

    // 1. The user's memory settings.
    arguments.push(plain(format!("-Xms{}m", settings.jvm.min_memory_mib)));
    arguments.push(plain(format!("-Xmx{}m", settings.jvm.max_memory_mib)));

    // 2. Mojang's recommended defaults, where the user has not replaced them.
    for value in recommended_jvm_arguments(metadata, &context, &settings.jvm) {
        arguments.push(argument(&substitute(&value, &values)?, &values));
    }

    // 3. The arguments the version genuinely requires, including `-cp`.
    let declared = metadata.arguments.group(&ArgumentGroup::Jvm);
    if declared.is_empty() && metadata.arguments.form() == ArgumentForm::Legacy {
        for template in LEGACY_JVM_ARGUMENTS {
            arguments.push(argument(&substitute(template, &values)?, &values));
        }
    } else {
        for value in expand(declared, &context) {
            let resolved = substitute(&value, &values)?;
            let resolved = adapt_classpath_filename_argument(&resolved, plan);
            arguments.push(argument(&resolved, &values));
        }
    }

    // 4. The log4j configuration the release ships.
    if let Some(logging) = plan.logging.as_ref() {
        let mut path_value = BTreeMap::new();
        path_value.insert("path", display(&logging.artifact.path));
        arguments.push(plain(substitute(&logging.argument, &path_value)?));
    }

    // 5. The user's own JVM arguments, last so they override.
    for value in &settings.jvm.extra_arguments {
        arguments.push(plain(value.clone()));
    }

    let jvm_argument_count = arguments.len();
    arguments.push(plain(metadata.main_class.clone()));

    // 6. The game arguments.
    for value in expand(metadata.arguments.group(&ArgumentGroup::Game), &context) {
        arguments.push(argument(&substitute(&value, &values)?, &values));
    }
    if let Some(target) = settings.features.direct_connect
        && !arguments
            .iter()
            .skip(jvm_argument_count + 1)
            .any(|argument| argument.value == "--quickPlayMultiplayer")
    {
        arguments.push(plain("--server".to_owned()));
        arguments.push(plain(target.address.to_string()));
        arguments.push(plain("--port".to_owned()));
        arguments.push(plain(target.port.to_string()));
    }

    debug!(
        version = %metadata.id,
        java = %settings.java.display(),
        jvm_arguments = jvm_argument_count,
        game_arguments = arguments.len() - jvm_argument_count - 1,
        classpath_entries = plan.libraries.len() + 1,
        authenticated = settings.identity.is_authenticated(),
        "built a launch command"
    );

    Ok(LaunchCommand {
        java: settings.java.clone(),
        working_directory: settings.game_directory.clone(),
        main_class: metadata.main_class.clone(),
        arguments,
        jvm_argument_count,
    })
}

/// Mojang's `default-user-jvm` group, filtered to what the user has not
/// replaced.
fn recommended_jvm_arguments(
    metadata: &VersionMetadata,
    context: &RuleContext,
    jvm: &JvmOptions,
) -> Vec<String> {
    if !jvm.extra_arguments.is_empty() {
        // Custom JVM arguments replace generated arguments; they are not appended.
        return Vec::new();
    }
    expand(
        metadata.arguments.group(&ArgumentGroup::DefaultUserJvm),
        context,
    )
    .into_iter()
    // The launcher's Minimum and Maximum RAM settings are what a memory
    // setting means; Mojang's suggestion is exactly the default they replace.
    .filter(|value| !value.starts_with("-Xms") && !value.starts_with("-Xmx"))
    .collect()
}

/// Flattens an argument group to the values that apply to this launch.
fn expand(entries: &[crate::arguments::ArgumentEntry], context: &RuleContext) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| rules_allow(&entry.rules, context))
        .flat_map(|entry| entry.values.iter().cloned())
        .collect()
}

/// Replaces every `${name}` with its value, failing on one it does not know.
fn substitute(template: &str, values: &BTreeMap<&str, String>) -> LaunchResult<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let Some(end) = tail.find('}') else {
            // An unclosed `${` is literal text, not a placeholder. Mojang has
            // never published one, and treating it as a failure would refuse a
            // launch over a `$` in a path.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &tail[..end];
        let value = values
            .get(name)
            .ok_or_else(|| LaunchError::UnresolvedPlaceholder {
                placeholder: name.to_owned(),
                argument: template.to_owned(),
            })?;
        out.push_str(value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Wraps a resolved value, marking it sensitive when it contains a credential.
///
/// Checked against the value rather than against the template, so an argument
/// that merely *embeds* the token — `--accessToken=<token>`, which some loader
/// metadata writes — is caught too.
fn argument(value: &str, values: &BTreeMap<&str, String>) -> Argument {
    let token = values.get("auth_access_token").map(String::as_str);
    let sensitive = token.is_some_and(|token| {
        !token.is_empty() && token != OFFLINE_ACCESS_TOKEN && value.contains(token)
    });
    Argument {
        value: value.to_owned(),
        sensitive,
    }
}

const fn plain(value: String) -> Argument {
    Argument {
        value,
        sensitive: false,
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Adapts loader metadata whose semantics depend on classpath *filenames* to
/// Ephervex's content-addressed client-jar path.
///
/// Forge's BootstrapLauncher 1.1.x scans the legacy classpath and turns every
/// entry not matched by `-DignoreList` into a Java module. Its official version
/// metadata ignores `${version_name}.jar`, which is the filename used by the
/// official launcher. Ephervex deliberately stores the same verified bytes as
/// `<sha1>.jar`; without adding that actual filename, Forge also loads the
/// vanilla client beside its patched `minecraft` module and Java rejects their
/// overlapping packages. BootstrapLauncher uses `filename.starts_with(token)`,
/// so preserving every official token and appending the real filename exactly
/// retains the metadata's intended exclusion.
fn adapt_classpath_filename_argument(value: &str, plan: &InstallationPlan) -> String {
    let Some(ignore_list) = value.strip_prefix("-DignoreList=") else {
        return value.to_owned();
    };
    let Some(filename) = plan.client_jar.path.file_name() else {
        return value.to_owned();
    };
    let filename = filename.to_string_lossy();
    if ignore_list
        .split(',')
        .any(|prefix| filename.starts_with(prefix))
    {
        value.to_owned()
    } else {
        format!("{value},{filename}")
    }
}

/// Builds a command around an arbitrary executable, for the process tests.
///
/// Ephervex does not allow a test that depends on Minecraft actually starting, and
/// [`crate::process`] is about spawning, streaming, and exit codes — none of
/// which care what is on the other end. Going through [`build_command`] there
/// would make a process test depend on the whole metadata pipeline as well.
#[cfg(test)]
pub(crate) fn test_command(
    executable: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
) -> LaunchCommand {
    let count = arguments.len();
    LaunchCommand {
        java: executable,
        working_directory,
        main_class: String::new(),
        arguments: arguments.into_iter().map(plain).collect(),
        jvm_argument_count: count,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_download::CacheLayout;

    use super::*;
    use crate::rules::OsName;
    use crate::test_support::fixture;

    fn layout() -> CacheLayout {
        CacheLayout::new("/cache/ephervex")
    }

    fn metadata(name: &str) -> VersionMetadata {
        VersionMetadata::parse(fixture(name))
            .unwrap_or_else(|error| panic!("fixture `{name}` should parse: {error}"))
    }

    fn platform() -> Platform {
        Platform::new(OsName::Linux, "x86_64", "6.8.0")
    }

    fn settings(identity: PlayerIdentity) -> LaunchSettings {
        LaunchSettings {
            java: PathBuf::from("/usr/lib/jvm/java-21/bin/java"),
            game_directory: PathBuf::from("/tmp/ephervex/instances/abc/minecraft"),
            identity,
            features: LaunchFeatures::none(),
            jvm: JvmOptions::default(),
        }
    }

    fn offline() -> PlayerIdentity {
        PlayerIdentity::offline("Notch", "b50ad385-829d-3141-a216-7e7d7539ba7f")
    }

    fn microsoft() -> PlayerIdentity {
        PlayerIdentity::microsoft(
            "Notch",
            "069a79f4-44e9-4726-a5be-fca90e38aaf5",
            "eyJhbGciOiJIUzI1-SECRET-TOKEN",
        )
    }

    fn command(name: &str, settings: &LaunchSettings) -> LaunchCommand {
        let metadata = metadata(name);
        let context = settings.features.rule_context(platform());
        let plan = InstallationPlan::resolve(&metadata, &context, &layout())
            .unwrap_or_else(|error| panic!("`{name}` should plan: {error}"));
        build_command(&metadata, &plan, settings)
            .unwrap_or_else(|error| panic!("`{name}` should build: {error}"))
    }

    fn all(command: &LaunchCommand) -> Vec<String> {
        command.arguments().map(str::to_owned).collect()
    }

    #[test]
    fn every_era_builds_a_command_with_a_classpath_and_the_game_main_class() {
        for name in ["1.12.2.json", "1.20.1.json", "1.21.1.json", "26.1.json"] {
            let command = command(name, &settings(offline()));
            let arguments = all(&command);

            assert_eq!(
                command.main_class, "net.minecraft.client.main.Main",
                "{name}"
            );
            let cp = arguments
                .iter()
                .position(|argument| argument == "-cp")
                .unwrap_or_else(|| panic!("{name} must pass a classpath"));
            assert!(
                arguments[cp + 1].contains("/cache/ephervex/libraries/"),
                "{name}: the classpath references the shared library store"
            );
            assert!(
                std::path::Path::new(&arguments[cp + 1])
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jar")),
                "{name}: the client jar goes last"
            );
            assert!(
                cp < arguments
                    .iter()
                    .position(|argument| argument == &command.main_class)
                    .expect("main class"),
                "{name}: `-cp` has to precede the main class"
            );
        }
    }

    #[test]
    fn the_legacy_form_gets_the_jvm_arguments_1_13_later_wrote_down() {
        // 1.12.2 declares none at all; a launcher is expected to know them.
        let command = command("1.12.2.json", &settings(offline()));
        let arguments = all(&command);

        assert_eq!(
            metadata("1.12.2.json").arguments.form(),
            ArgumentForm::Legacy
        );
        assert!(arguments.iter().any(|argument| argument
            == "-Djava.library.path=/cache/ephervex/natives/1.12.2/linux-x86_64"));
        assert!(
            arguments
                .iter()
                .any(|argument| argument == "-Dminecraft.launcher.brand=ephervex")
        );
        assert!(arguments.contains(&"-cp".to_owned()));
    }

    #[test]
    fn the_modern_form_uses_the_versions_own_jvm_arguments() {
        let command = command("1.21.1.json", &settings(offline()));
        let arguments = all(&command);
        assert!(
            arguments
                .iter()
                .any(|argument| argument
                    == "-Djna.tmpdir=/cache/ephervex/natives/1.21.1/linux-x86_64"),
            "the natives directory reaches every flag that names it: {arguments:?}"
        );
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "-XstartOnFirstThread"),
            "a macOS-only flag must not be on a Linux command line"
        );
    }

    #[test]
    fn loader_library_placeholders_resolve_to_the_shared_maven_root() {
        let parent = metadata("1.20.1.json");
        let child = VersionMetadata::parse(
            r#"{
              "id":"1.20.1-forge-test","inheritsFrom":"1.20.1","type":"release",
              "releaseTime":"2026-08-08T00:00:00Z","time":"2026-08-08T00:00:00Z",
              "mainClass":"example.Loader","arguments":{"game":[],"jvm":[
                "-DlibraryDirectory=${library_directory}",
                "-Dpair=a${classpath_separator}b"
              ]}
            }"#,
        )
        .expect("child");
        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");
        let settings = settings(offline());
        let context = settings.features.rule_context(platform());
        let plan = InstallationPlan::resolve(&merged, &context, &layout()).expect("plan");
        let arguments = all(&build_command(&merged, &plan, &settings).expect("command"));
        assert!(arguments.contains(&"-DlibraryDirectory=/cache/ephervex/libraries".to_owned()));
        assert!(arguments.contains(&format!("-Dpair=a{CLASSPATH_SEPARATOR}b")));
    }

    #[test]
    fn forge_1_20_ignore_list_covers_the_content_addressed_client_jar() {
        let parent = metadata("1.20.1.json");
        let child = VersionMetadata::parse(include_str!(
            "../../../fixtures/loaders/forge-1.20.1-version.json"
        ))
        .expect("official Forge child fixture");
        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");
        let settings = settings(offline());
        let context = settings.features.rule_context(platform());
        let plan = InstallationPlan::resolve(&merged, &context, &layout()).expect("plan");
        let client_filename = plan
            .client_jar
            .path
            .file_name()
            .expect("client filename")
            .to_string_lossy()
            .into_owned();
        let arguments = all(&build_command(&merged, &plan, &settings).expect("command"));
        let ignore = arguments
            .iter()
            .find_map(|argument| argument.strip_prefix("-DignoreList="))
            .expect("Forge ignore list");

        assert!(
            ignore
                .split(',')
                .any(|prefix| client_filename.starts_with(prefix)),
            "BootstrapLauncher must not turn the vanilla client into a second minecraft module: {ignore}"
        );
        assert!(
            ignore.contains("1.20.1-forge-47.4.10.jar"),
            "the official installer token remains intact"
        );
        assert!(
            ignore.contains(&client_filename),
            "the content-addressed launch path is added without renaming its verified artifact"
        );
    }

    #[test]
    fn the_account_placeholders_are_substituted_for_an_offline_account() {
        let command = command("1.12.2.json", &settings(offline()));
        let game: Vec<String> = command.game_arguments().map(str::to_owned).collect();

        assert_eq!(pair(&game, "--username"), Some("Notch".to_owned()));
        assert_eq!(
            pair(&game, "--uuid"),
            Some("b50ad385-829d-3141-a216-7e7d7539ba7f".to_owned()),
            "the derived offline uuid a server computes for itself"
        );
        assert_eq!(pair(&game, "--accessToken"), Some("0".to_owned()));
        assert_eq!(
            pair(&game, "--userType"),
            Some("legacy".to_owned()),
            "an offline identity must never claim to be an authenticated one"
        );
        assert_eq!(pair(&game, "--versionType"), Some("release".to_owned()));
        assert_eq!(
            pair(&game, "--assetsDir"),
            Some("/cache/ephervex/assets".to_owned()),
            "assets are shared rather than copied into the instance"
        );
        assert_eq!(
            pair(&game, "--gameDir"),
            Some("/tmp/ephervex/instances/abc/minecraft".to_owned()),
            "saves and options stay instance-local"
        );
        assert_eq!(pair(&game, "--assetIndex"), Some("1.12".to_owned()));
    }

    #[test]
    fn the_account_placeholders_are_substituted_for_a_microsoft_account() {
        let command = command("1.21.1.json", &settings(microsoft()));
        let game: Vec<String> = command.game_arguments().map(str::to_owned).collect();

        assert_eq!(
            pair(&game, "--accessToken"),
            Some("eyJhbGciOiJIUzI1-SECRET-TOKEN".to_owned())
        );
        assert_eq!(pair(&game, "--userType"), Some("msa".to_owned()));
        assert_eq!(
            pair(&game, "--uuid"),
            Some("069a79f4-44e9-4726-a5be-fca90e38aaf5".to_owned())
        );
        // The modern form asks for two more that Ephervex has no value for;
        // they are still substituted rather than left as literals.
        assert_eq!(pair(&game, "--xuid"), Some(String::new()));
        assert_eq!(pair(&game, "--clientId"), Some(String::new()));
    }

    #[test]
    fn no_placeholder_survives_into_a_built_command_for_any_era() {
        for name in ["1.12.2.json", "1.20.1.json", "1.21.1.json", "26.1.json"] {
            for identity in [offline(), microsoft()] {
                for argument in all(&command(name, &settings(identity))) {
                    assert!(
                        !argument.contains("${"),
                        "{name}: `{argument}` reached the game unsubstituted"
                    );
                }
            }
        }
    }

    #[test]
    fn the_access_token_is_redacted_everywhere_a_command_can_be_printed() {
        // The one thing a launcher inevitably does with a command line is log
        // it.
        let command = command("1.21.1.json", &settings(microsoft()));
        let secret = "eyJhbGciOiJIUzI1-SECRET-TOKEN";

        assert!(command.arguments().any(|argument| argument == secret));
        assert!(
            !command
                .redacted_arguments()
                .iter()
                .any(|argument| argument.contains(secret))
        );
        assert!(!format!("{command:?}").contains(secret));
        assert!(!command.to_string().contains(secret));
        assert!(!format!("{:?}", settings(microsoft()).identity).contains(secret));
        assert!(
            command
                .redacted_arguments()
                .contains(&"<redacted>".to_owned())
        );
    }

    #[test]
    fn an_offline_token_is_not_treated_as_a_secret_worth_hiding() {
        // Redacting `0` would hide the one fact worth seeing in a log: that
        // this launch was not authenticated.
        let command = command("1.21.1.json", &settings(offline()));
        assert!(
            !command
                .redacted_arguments()
                .contains(&"<redacted>".to_owned())
        );
        assert!(command.to_string().contains(" 0"));
    }

    #[test]
    fn a_feature_rule_decides_whether_its_arguments_appear_at_all() {
        let ordinary = command("1.20.1.json", &settings(offline()));
        assert!(!all(&ordinary).contains(&"--demo".to_owned()));

        let mut demo = settings(offline());
        demo.features = LaunchFeatures::none().demo().custom_resolution(1280, 720);
        let command = command("1.20.1.json", &demo);
        let game: Vec<String> = command.game_arguments().map(str::to_owned).collect();

        assert!(game.contains(&"--demo".to_owned()));
        assert_eq!(pair(&game, "--width"), Some("1280".to_owned()));
        assert_eq!(pair(&game, "--height"), Some("720".to_owned()));
    }

    #[test]
    fn modern_quick_play_receives_one_unambiguous_direct_connect_argument() {
        let mut connected = settings(offline());
        connected.features = LaunchFeatures::none().direct_connect(ConnectionTarget {
            address: "::1".parse().expect("address"),
            port: std::num::NonZeroU16::new(25_566).expect("port"),
        });
        let command = command("1.20.1.json", &connected);
        let game: Vec<String> = command.game_arguments().map(str::to_owned).collect();

        assert_eq!(
            game.iter()
                .filter(|argument| argument.as_str() == "--quickPlayMultiplayer")
                .count(),
            1
        );
        assert_eq!(
            pair(&game, "--quickPlayMultiplayer"),
            Some("[::1]:25566".to_owned())
        );
        assert!(!game.iter().any(|argument| argument == "--server"));
        assert!(!game.iter().any(|argument| argument == "--port"));
    }

    #[test]
    fn legacy_direct_connect_arguments_appear_once_in_the_game_layer() {
        let mut connected = settings(microsoft());
        connected.features = LaunchFeatures::none().direct_connect(ConnectionTarget {
            address: "127.0.0.1".parse().expect("address"),
            port: std::num::NonZeroU16::new(25_567).expect("port"),
        });
        let command = command("1.12.2.json", &connected);
        let game: Vec<String> = command.game_arguments().map(str::to_owned).collect();

        assert_eq!(
            game.iter()
                .filter(|argument| argument.as_str() == "--server")
                .count(),
            1
        );
        assert_eq!(
            game.iter()
                .filter(|argument| argument.as_str() == "--port")
                .count(),
            1
        );
        assert_eq!(pair(&game, "--server"), Some("127.0.0.1".to_owned()));
        assert_eq!(pair(&game, "--port"), Some("25567".to_owned()));
        assert_eq!(pair(&game, "--userType"), Some("msa".to_owned()));
        assert_eq!(
            command.java,
            PathBuf::from("/usr/lib/jvm/java-21/bin/java"),
            "direct connect does not select another runtime"
        );
        assert!(command.jvm_arguments().all(|argument| {
            argument != "--server"
                && argument != "--port"
                && argument != "127.0.0.1"
                && argument != "25567"
        }));
    }

    #[test]
    fn the_users_memory_settings_come_first_and_their_own_arguments_come_last() {
        let mut custom = settings(offline());
        custom.jvm = JvmOptions {
            min_memory_mib: 1024,
            max_memory_mib: 8192,
            extra_arguments: vec!["-XX:+UseG1GC".to_owned()],
        };
        let command = command("1.20.1.json", &custom);
        let jvm: Vec<String> = command.jvm_arguments().map(str::to_owned).collect();

        assert_eq!(jvm.first().map(String::as_str), Some("-Xms1024m"));
        assert_eq!(jvm.get(1).map(String::as_str), Some("-Xmx8192m"));
        assert_eq!(
            jvm.last().map(String::as_str),
            Some("-XX:+UseG1GC"),
            "the user's arguments go last so a duplicate flag resolves their way"
        );
    }

    #[test]
    fn mojangs_recommended_defaults_are_used_but_never_its_heap_sizes() {
        // 26.x's `default-user-jvm` suggests `-Xms2G -Xmx4G`; Ephervex exposes those
        // to the user's Minimum and Maximum RAM settings instead.
        let command = command("26.1.json", &settings(offline()));
        let jvm: Vec<String> = command.jvm_arguments().map(str::to_owned).collect();

        assert!(jvm.contains(&"-Xms512m".to_owned()) && jvm.contains(&"-Xmx4096m".to_owned()));
        assert!(
            !jvm.iter()
                .any(|argument| argument == "-Xms2G" || argument == "-Xmx4G")
        );
        assert!(
            jvm.contains(&"-XX:+UseZGC".to_owned()),
            "the recommended collector applies on linux: {jvm:?}"
        );
        assert!(
            !jvm.iter()
                .any(|argument| argument.contains("G1NewSizePercent")),
            "and the windows-only alternative does not"
        );
    }

    #[test]
    fn a_user_who_supplied_jvm_arguments_replaces_mojangs_recommendations() {
        let mut custom = settings(offline());
        custom.jvm.extra_arguments = vec!["-XX:+UseSerialGC".to_owned()];
        let jvm: Vec<String> = command("26.1.json", &custom)
            .jvm_arguments()
            .map(str::to_owned)
            .collect();

        assert!(jvm.contains(&"-XX:+UseSerialGC".to_owned()));
        assert!(
            !jvm.contains(&"-XX:+UseZGC".to_owned()),
            "custom JVM arguments replace generated arguments rather than extending them"
        );
    }

    #[test]
    fn the_log4j_configuration_the_release_ships_is_passed_to_the_jvm() {
        for name in ["1.12.2.json", "1.20.1.json", "26.1.json"] {
            let jvm: Vec<String> = command(name, &settings(offline()))
                .jvm_arguments()
                .map(str::to_owned)
                .collect();
            assert!(
                jvm.iter().any(|argument| argument
                    .starts_with("-Dlog4j.configurationFile=/cache/ephervex/downloads/")),
                "{name}: {jvm:?}"
            );
        }
    }

    #[test]
    fn an_argument_naming_a_placeholder_this_build_cannot_resolve_fails_loudly() {
        let mut values = BTreeMap::new();
        values.insert("known", "yes".to_owned());

        assert_eq!(
            substitute("--a ${known}", &values).expect("known"),
            "--a yes"
        );
        let error = substitute("--a ${from_the_future}", &values)
            .expect_err("an unknown placeholder is a launch this build cannot reproduce");
        assert!(matches!(
            error,
            LaunchError::UnresolvedPlaceholder { ref placeholder,.. }
                if placeholder == "from_the_future"
        ));
        assert_eq!(error.message_id(), "error-launch-unresolved-placeholder");
    }

    #[test]
    fn substitution_handles_several_placeholders_and_text_around_them() {
        // 26.2 writes `-Djna.tmpdir=${natives_directory}/jna`.
        let mut values = BTreeMap::new();
        values.insert("natives_directory", "/n".to_owned());
        values.insert("classpath", "/a:/b".to_owned());
        assert_eq!(
            substitute("-Djna.tmpdir=${natives_directory}/jna", &values).expect("substitutes"),
            "-Djna.tmpdir=/n/jna"
        );
        assert_eq!(
            substitute("${natives_directory}:${classpath}", &values).expect("substitutes"),
            "/n:/a:/b"
        );
        assert_eq!(
            substitute("a $ b ${", &values).expect("a lone dollar is literal text"),
            "a $ b ${"
        );
    }

    /// The value following a flag in a game-argument list.
    fn pair(arguments: &[String], flag: &str) -> Option<String> {
        let index = arguments.iter().position(|value| value == flag)?;
        arguments.get(index + 1).cloned()
    }
}
