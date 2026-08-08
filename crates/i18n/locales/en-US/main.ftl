# Ephervex — English (United States) source resource.
#
# This file is the fallback language: every message id used anywhere in the
# application must exist here, and `fl!()` checks that at compile time.
#
# Rules for anything added below:
#   * Provider names (Modrinth, CurseForge, Forge, NeoForge, Prism), technical
#     ids, filesystem paths, version strings, and Minecraft log text stay
#     untranslated. They are passed in as arguments, never baked into a message.
#   * Message ids are stable. Rewording English is free; renaming an id breaks
#     every other locale, so treat renames as deletions plus additions.
#   * Errors get a concise summary and, where one exists, an actionable next
#     step as a `.action` attribute.

## Application identity

app-name = Ephervex
app-tagline = Disposable Minecraft instances for mod development and bug reproduction

## Languages
#
# Language names are written in their own language so a user who cannot read
# the current interface language can still find their own.

language-en-US = English (United States)

## Storage classes 

storage-class-temporary = Temporary
storage-class-persistent = Persistent
storage-class-temporary-description = Lives under the system temporary directory. The operating system decides when it is cleaned up; Ephervex never deletes it on its own.
storage-class-persistent-description = Lives under the configured instance directory and survives reboots until you delete it.

## Instance and server listings

instances-list-empty = No instances yet.
servers-list-empty = No servers yet.

instances-list-count = { $count ->
        [one] { $count } instance
       *[other] { $count } instances
    }

servers-list-count = { $count ->
        [one] { $count } server
       *[other] { $count } servers
    }

# $name is user-authored and $version is a Minecraft version string; neither is
# translated.
instance-summary = { $name } — Minecraft { $version }, { $loader }

## Startup reconciliation (ADR 0006)
#
# A vanished temporary root is the expected result of the operating system
# clearing its temporary directory. It is reported as information, never as an
# error. A vanished persistent root is the opposite and keeps a warning.

reconcile-removed-stale-temporary = { $count ->
        [one] Removed { $count } temporary instance whose directory no longer exists.
       *[other] Removed { $count } temporary instances whose directories no longer exist.
    }

reconcile-missing-persistent = { $count ->
        [one] { $count } persistent instance is missing its directory.
       *[other] { $count } persistent instances are missing their directories.
    }
.action = Ephervex kept the record so you can restore the directory from a backup. Delete the instance explicitly if it is gone for good.

reconcile-reindexed = { $count ->
        [one] Recovered { $count } instance found on disk but missing from the index.
       *[other] Recovered { $count } instances found on disk but missing from the index.
    }

reconcile-nothing-to-do = Everything in the index matches what is on disk.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-persistent-root-missing-detail = { $name } expected its files at { $path }.

## Attention reasons 
#
# Ephervex never presents an instance as faithfully runnable when it is not.

attention-missing-root = The instance directory is missing.
attention-unsupported-loader = { $loader } is not a supported mod loader.
attention-unsupported-launch-customization = The source used launch settings Ephervex cannot reproduce: { $detail }
attention-ignored-unsafe-customization = An imported command or hook was found and deliberately not run: { $detail }
attention-best-effort-server-conversion = This server was generated from a client instance and may still contain client-only mods.
attention-manual-download-required = Some files must be downloaded manually: { $detail }

## Configuration errors 

error-config-unreadable = The configuration file could not be read.
.action = Check that { $path } exists and is readable, or delete it to start from defaults.

error-config-invalid = The configuration file is not valid TOML.
.action = Fix the syntax at { $path }, or delete the file to start from defaults.

error-config-unwritable = The configuration file could not be saved.
.action = Check that { $path } is writable and that the filesystem is not full.

error-config-unsupported-schema = This configuration was written by a newer version of { app-name }.
.action = Update { app-name }, or move { $path } aside to start from defaults. Ephervex will not guess at a schema it does not know.

# ADR 0004: secrets belong in the desktop keyring, never in a file that users
# routinely attach to bug reports.
error-config-secret-rejected = Refusing to save a credential into the configuration file.
.action = The key { $key } looks like a secret. Secrets are stored in the system keyring instead, and configuration only records that one exists.

## Path resolution errors (ADR 0003)

error-paths-no-home-directory = Ephervex could not work out where your home directory is.
.action = Set HOME, or set the XDG base directory variables explicitly.

error-paths-directory-creation = A required directory could not be created.
.action = Check permissions on { $path }.

## Index and migration errors 

error-index-unopenable = The launcher index database could not be opened.
.action = Check permissions on { $path }. Instances are recoverable without it: their own metadata files are the source of truth.

error-index-query-failed = The launcher index database reported an error.

error-index-migration-failed = The launcher index database could not be upgraded to the current schema.
.action = Move { $path } aside and restart. Ephervex rebuilds the index by scanning your persistent instances.

error-index-schema-from-future = The launcher index database was written by a newer version of { app-name }.
.action = Update { app-name }. Ephervex will not downgrade a schema it does not understand.

## Registry errors 

error-registry-instance-unknown = No such instance.
error-registry-server-unknown = No such server.

error-registry-metadata-unwritable = The instance metadata file could not be written.
.action = Check that { $path } is writable. Ephervex writes this file before touching the index so a crash leaves a recoverable instance rather than a dangling record.

error-registry-metadata-unreadable = The instance metadata file could not be read.

error-registry-metadata-invalid = The instance metadata file is not valid Ephervex metadata.

error-registry-metadata-unsupported-version = The instance metadata at { $path } was written by a newer version of { app-name }.

error-registry-root-occupied = That instance directory already contains files.
.action = Choose a different name, or remove { $path } first.

# A persistent instance whose directory vanished keeps its record rather than
# being deleted, so "the files are gone" has to read differently
# from "no such instance".
error-registry-root-missing = That instance's files are no longer on disk.
.action = Ephervex expected them at { $path } and has kept the record. Restore the directory, or delete the instance to remove the record too.

## Instance duplication and folders 

instance-duplicated = Duplicated as { $name }.
# Following a symlinked folder could copy a shared mods directory, or walk out
# of the instance entirely, so the copy skips it and says so.
instance-duplicate-skipped-links = { $count ->
        [one] { $count } linked folder was not copied.
       *[other] { $count } linked folders were not copied.
    }
.action = The copy points at nothing where the original had a link. Recreate it by hand if the instance needs it.

# Used for both a client instance and a server, so it names neither.
instance-folder-unavailable = That folder does not exist yet.
.action = { $folder } appears once the game has written it, and Ephervex will not create an empty one that suggests otherwise.

## Global Minecraft preferences 

preferences-none-managed = No global Minecraft preferences are applied to new instances.
preferences-managed-count = { $count ->
        [one] { $count } preference is applied to new instances.
       *[other] { $count } preferences are applied to new instances.
    }
preference-set = { $preference } is now applied to new instances, as { $value }.
preference-unmanaged = { $preference } is no longer applied to new instances. Its value is remembered.
# Applied once, when the instance is created; never synchronized afterwards.
preferences-applied = Applied { $count ->
        [one] { $count } global preference
       *[other] { $count } global preferences
    } to the new instance.
preferences-unsupported-for-version = Minecraft { $version } has no setting for { $preference }, so it was not applied.

error-options-unreadable = The instance's Minecraft settings file could not be read.
.action = Check that { $path } is readable. Ephervex will not replace a settings file it could not read first.

error-options-unwritable = The instance's Minecraft settings file could not be written.
.action = Check that { $path } is writable.

# $syntax is a literal input format, not prose to translate.
error-options-preference-value-invalid = That is not a usable value for this setting.
.action = Expected { $syntax }.

## Minecraft version metadata 
#
# Version ids, release channels, and file paths are never translated; they are
# passed in as arguments.

minecraft-versions-list-empty = No supported Minecraft releases were found.

minecraft-versions-list-count = { $count ->
        [one] { $count } supported release
       *[other] { $count } supported releases
    }

# Shown alongside a version list that could not be refreshed, so the user knows
# what they are looking at rather than watching a spinner.
minecraft-versions-stale = Showing the last version list Ephervex was able to fetch. Mojang could not be reached.

minecraft-version-id-unrecognized = { $version } uses a version-numbering scheme this build does not know. It is listed because Mojang published it after { $minimum }.

## Minecraft metadata errors 

error-minecraft-manifest-unavailable = The Minecraft version list could not be fetched, and no cached copy is available.
.action = Check your network connection and try again. Instances that are already installed can still be launched.

error-minecraft-manifest-invalid = Mojang returned something that is not a Minecraft version manifest.
.action = This is usually a captive portal or a proxy answering instead of Mojang. Try again once you have real internet access.

error-minecraft-version-unknown = That Minecraft version is not in Mojang's version list.
.action = Check the version id. Only released versions appear in the list.

error-minecraft-version-unsupported = That Minecraft version is older than Ephervex supports.
.action = Ephervex supports Minecraft releases from 1.12.2 onward.

error-minecraft-version-not-a-release = That is a snapshot, pre-release, or release candidate rather than a Minecraft release.
.action = Ephervex deliberately offers only released versions, so that a reproduction can be repeated later against the same build.

error-minecraft-version-metadata-unavailable = The metadata for that Minecraft version could not be fetched.
.action = Check your network connection and try again.

# Verifying the SHA-1 Mojang publishes for each version document is a release
# blocker, not a nicety.
error-minecraft-version-metadata-corrupt = The metadata Mojang served for that version did not match the checksum Mojang published for it.
.action = Ephervex refused the file rather than using it. If this repeats, something between you and Mojang is rewriting the response.

error-minecraft-version-metadata-invalid = The metadata for that Minecraft version could not be understood.

error-minecraft-version-metadata-unreadable = The cached metadata for that Minecraft version could not be read.
.action = Check permissions on { $path }, or delete the file so Ephervex fetches it again.

error-minecraft-metadata-incomplete = That version's metadata is missing something a launch needs.
.action = Mod-loader version files extend an exact vanilla parent rather than standing alone. Ephervex refuses a raw or mismatched child document.

error-minecraft-metadata-url-invalid = Mojang's metadata contains a download address Ephervex will not use.
.action = Ephervex only downloads over HTTPS and never from an address that embeds credentials.

error-minecraft-cache-key-invalid = A download could not be given a safe location in the cache.

## Java runtime errors 

# the missing-compatible-Java case: the next step is a compatible runtime or a
# managed download, never a near-miss the launcher picked on the user's behalf.
error-java-no-compatible-runtime = None of the Java runtimes on this computer can run that Minecraft version.
.action = Let Ephervex download a compatible Java runtime, or install one yourself and refresh the list.

error-java-pinned-incompatible = The Java runtime chosen for this instance cannot run that Minecraft version.
.action = Clear the Java override so Ephervex picks a compatible runtime, or point it at one that matches.

error-java-runtime-unusable = That path is not a usable Java runtime.
.action = Point Ephervex at the `java` program itself, usually inside a `bin` folder.

error-java-auto-detect-disabled = Automatic Java detection is turned off and no Java path is set.
.action = Turn automatic detection back on in Settings, or set a preferred Java path.

error-java-managed-download-disabled = No installed Java runtime is compatible, and automatic Java downloads are turned off.
.action = Turn on "Auto-download compatible Java" in Settings, or install a matching Java runtime yourself.

error-java-managed-arch-unsupported = No Java runtime is published for this computer's processor architecture.
.action = Install a Java runtime through your distribution's package manager instead.

error-java-managed-api-unusable = The Java download service address is not usable.

error-java-managed-index-unavailable = The list of downloadable Java runtimes could not be fetched.
.action = Check your network connection and try again. Java runtimes already installed still work.

error-java-managed-index-invalid = The Java download service returned something Ephervex could not understand.

error-java-managed-release-missing = The Java download service publishes no build of that Java version for this computer.

error-java-managed-release-malformed = The published Java build is missing a download address or a checksum.
.action = Ephervex refuses to install a Java runtime it cannot verify.

error-java-managed-download-failed = The Java runtime could not be downloaded.
.action = Check your network connection and try again.

# Extraction limits and traversal checks are security boundaries, and
# a refusal has to say that something was actually wrong with the archive.
error-java-archive-unsafe = The downloaded Java archive tried to write files outside its own folder.
.action = Ephervex refused the archive and installed nothing. If this repeats, something between you and the download service is altering it.

error-java-archive-too-large = The downloaded Java archive is larger than Ephervex is willing to unpack.

error-java-archive-unreadable = The downloaded Java archive could not be read.

error-java-archive-unwritable = A file could not be written while unpacking the Java runtime.
.action = Check free space and permissions on { $path }.

error-java-archive-empty = The downloaded Java archive contained nothing Ephervex could install.

error-java-managed-extraction-failed = Unpacking the Java runtime did not finish.

error-java-managed-runtime-unusable = The downloaded Java runtime could not be started.
.action = Ephervex did not use it. Try again, or install a Java runtime through your distribution's package manager.

error-java-managed-cache-key-invalid = The Java runtime could not be given a safe location in the cache.

error-java-io = A file needed for the Java runtime could not be written.
.action = Check free space and permissions on { $path }.

error-java-probe-not-executable = That Java program could not be started.

error-java-probe-unintelligible = That program did not report a Java version.

error-java-probe-timed-out = That Java program did not respond in time.

## Java runtime output 

java-runtimes-list-empty = No Java runtimes were found on this computer.

java-runtimes-list-count = { $count ->
        [one] { $count } Java runtime
       *[other] { $count } Java runtimes
    }

java-runtime-probe-failed = { $path } could not be identified as a Java runtime.

## Authentication errors 
#
# The refusals here are shaped around what the user has to go and do, because
# that is what actually differs between them. "Authentication failed" would be
# accurate for all of these and useful for none.

error-auth-client-id-missing = Ephervex has no Microsoft application ID to sign in with.
.action = Set a Microsoft Client ID in Settings → APIs, or reinstall a build that ships one.

error-auth-client-id-invalid = That is not a valid Microsoft application ID.
.action = A Client ID looks like 00001111-aaaa-2222-bbbb-3333cccc4444. Copy it from the Overview page of your Azure app registration.

error-auth-endpoint-unusable = A Microsoft sign-in address could not be built.

error-auth-unreachable = Ephervex could not reach the Microsoft sign-in service.
.action = Check your network connection and try again. Instances you already have still work offline.

error-auth-unexpected-response = The sign-in service returned something Ephervex could not understand.
.action = If it keeps happening, the sign-in process has probably changed and Ephervex needs an update.

# Split from the message above on purpose: only one of the two is fixed by
# waiting, and promising that it is when it is not sends the user into a retry
# loop that can never succeed.
error-auth-service-unavailable = A sign-in service is temporarily unavailable.
.action = Try again in a few minutes.

error-auth-microsoft-rejected = Microsoft refused the sign-in request.

error-auth-declined = The sign-in was canceled.
.action = Start the sign-in again and approve the permission request to continue.

error-auth-device-code-expired = The sign-in code expired before it was used.
.action = Start the sign-in again. A code is only valid for about fifteen minutes.

error-auth-timed-out = Nobody finished signing in.
.action = Start the sign-in again and complete it in the browser window that opens.

error-auth-state-mismatch = That sign-in response did not belong to this request.
.action = Close any leftover sign-in tabs and start again.

error-auth-redirect-listener = Ephervex could not open a local port to receive the sign-in.
.action = Sign in with a device code instead, which needs no local port.

error-auth-canceled = The sign-in was stopped.

error-auth-xbox-authentication-failed = Xbox Live would not accept this Microsoft account.
.action = Check that you signed in with the account that owns Minecraft, and try again.

# The XSTS refusals. Each one sends the user somewhere different, which is the
# entire reason they are not one message.
error-auth-xbox-no-account = This Microsoft account has no Xbox profile.
.action = Create an Xbox profile for this account at xbox.com, then sign in again.

error-auth-xbox-account-banned = This account is banned from Xbox Live.
.action = Minecraft sign-in goes through Xbox Live, so this has to be resolved with Xbox support.

error-auth-xbox-country-unavailable = Xbox Live is not available in this account's country or region.

error-auth-xbox-adult-verification = This account needs to complete age verification before it can use Xbox Live.
.action = Finish the verification at xbox.com, then sign in again.

error-auth-xbox-child-account = This is a child account and must be added to a Microsoft Family before it can sign in.
.action = Ask the family organiser to add this account to their Family group.

error-auth-xbox-denied = Xbox Live refused to authorize this account for Minecraft.

error-auth-xbox-user-hash-missing = Xbox Live's response was missing information Ephervex needs to continue.

error-auth-minecraft-login-rejected = Minecraft would not accept the Xbox sign-in.
.action = Sign in again. If it keeps happening, check that this account can play Minecraft: Java Edition at minecraft.net.

error-auth-minecraft-unavailable = Minecraft's sign-in service is temporarily unavailable.
.action = Try again in a few minutes.

# Not a problem with the user, the account, or the sign-in — a property of the
# Azure application registration. No amount of signing in again changes it, so
# the next step is the approval form and nothing else. $url and $client_id are
# technical values shown verbatim.
error-auth-minecraft-api-not-approved = Minecraft has not approved this copy of Ephervex to sign you in.
.action = Microsoft must approve an application before it may use the Minecraft API. Apply at { $url } using Client ID { $client_id }. Signing in again will not help until it is approved.

error-auth-game-not-owned = This Microsoft account does not own Minecraft: Java Edition.
.action = Sign in with the account that owns the game, or buy it at minecraft.net.

# Not the same refusal, and telling this user they do not own the game would be
# false: they do, through Game Pass. They just have no Java Edition profile yet.
error-auth-game-pass-setup-required = This account reaches Minecraft through Game Pass but has no Java Edition profile yet.
.action = Open the official Minecraft Launcher once and start Java Edition to create the profile, then sign in here again.

error-auth-profile-unavailable = Your Minecraft profile could not be read.
.action = Check your network connection and try again.

error-auth-profile-malformed = Minecraft returned a profile Ephervex could not read.

error-auth-secret-storage-unavailable = Ephervex cannot store your sign-in securely on this computer.
.action = Install and unlock a keyring such as GNOME Keyring or KWallet, or sign in for this session only. Ephervex will never write your credentials to a file.

error-auth-credential-store-failed = The system keyring could not be used.
.action = Check that your keyring is unlocked, then try again.

error-auth-not-signed-in = No Microsoft account is signed in.
.action = Sign in with your Microsoft account before launching Minecraft.

error-auth-reauthentication-required = Your saved Microsoft sign-in is no longer valid.
.action = Sign in again. This happens when a sign-in expires or is revoked.

## Authentication progress and status 

auth-stage-microsoft = Signing in with Microsoft
auth-stage-xbox-live = Authenticating with Xbox Live
auth-stage-xsts = Authorizing for Minecraft
auth-stage-minecraft-login = Signing in to Minecraft
auth-stage-entitlements = Checking your Minecraft entitlements
auth-stage-profile = Reading your Minecraft profile

auth-storage-keyring = Saved in the system keyring
auth-storage-session-only = This session only

auth-signed-out = Not signed in.
# $name is the account's Minecraft profile name and is never translated.
auth-signed-in = Signed in as { $name }.
auth-session-only-warning = This sign-in is not saved and will be forgotten when Ephervex closes.

# $url is an address to open; shown verbatim.
auth-open-browser = Open this address to sign in: { $url }
# $code is Microsoft's short device code and $url is where to enter it.
auth-device-code-prompt = Go to { $url } and enter the code { $code }.
auth-logged-out = Signed out and removed the saved credential.
auth-nothing-to-log-out-of = No account was signed in.

## Accounts 
#
# Several accounts may exist at once, and exactly one of them is the default a
# launch uses. Which kind an account is has to be visible everywhere it is
# shown: an offline account cannot join an online-mode server, and a user who
# cannot tell them apart will not understand why.

account-kind-microsoft = Microsoft
account-kind-offline = Offline

accounts-list-empty = No accounts yet.

accounts-list-count = { $count ->
        [one] { $count } account
       *[other] { $count } accounts
    }

# $name is a Minecraft player name and is never translated.
account-added-offline = Added the offline account { $name }.
account-default-set = { $name } is now the default account.

# An offline account is not "signed in" to anything, and saying so would imply
# it can do things it cannot — such as join an online-mode server.
account-active-offline = Using the offline account { $name }.
account-removed = Removed the account { $name }.
account-removed-with-credential = Removed the account { $name } and deleted its saved credential.

# An offline account has no credential at all — this is not a missing or
# degraded one. Offline accounts have no credential material to store.
auth-storage-none-needed = No credential needed
auth-storage-not-saved = Not saved

error-auth-offline-name-invalid = That is not a usable Minecraft player name.
.action = Use 3 to 16 characters: letters, digits, or underscores. Minecraft does not accept spaces or punctuation.

error-auth-offline-account-exists = An offline account with that name already exists.
.action = An offline account's identity is derived from its name, so a second one would be the same player. Pick a different name.

error-auth-account-unknown = No account matches that name or ID.
.action = Run `ephervex account list` to see the accounts this installation has.

error-auth-account-ambiguous = More than one account has that name.
.action = Use the account's UUID instead. `ephervex account list` shows it.

## Launching 
#
# Minecraft's own log output, version ids, paths, and Java flags are never
# translated; they reach the user verbatim.

# Deliberately unparameterized: `LaunchEvent::Preparing` and `Starting` are
# emitted before and after the version has been resolved respectively, and a
# message that asks for an argument its event cannot supply renders the
# placeholder in the user's face.
launch-preparing = Preparing the launch…
launch-starting = Starting Minecraft…

# $name is a Minecraft player name and is never translated.
launch-account-offline = Launching as { $name } (offline). This account cannot join online-mode servers.
launch-account-microsoft = Launching as { $name }.

# $code is the process exit status.
launch-exited-cleanly = Minecraft exited normally.
launch-exited-with-code = Minecraft exited with code { $code }.
# $signal is a POSIX signal number.
launch-exited-with-signal = Minecraft was stopped by signal { $signal }.

launch-dry-run = Prepared the launch without starting Minecraft.

# The command line is shown with the access token replaced, always
#.
launch-command-redacted = The access token is not shown.

operation-installing-minecraft = Installing Minecraft

## Launch errors 

error-launch-already-running = That instance is already being launched.
.action = Close the running copy first. Two Minecraft processes in one instance directory overwrite each other's saves and options.

error-launch-lock-unusable = Ephervex could not claim the instance for launching.
.action = Check permissions on { $path }.

error-launch-invalid-version-metadata = That version's metadata does not describe something Ephervex can launch.
.action = The loader metadata may not match its declared Minecraft parent. Ephervex refuses rather than constructing an approximate launch.

# $loader is a product name and stays untranslated.
error-launch-loader-not-supported = This build does not support the { $loader } loader.
.action = Ephervex refuses to substitute Vanilla or another loader because that would not reproduce the selected instance.

error-launch-download-failed = A file Minecraft needs could not be downloaded.
.action = Check your network connection and try again. Nothing partial or unverified is kept, so a retry starts clean.

error-loader-metadata-unavailable = Loader version information is unavailable.
.action = Check the network connection. Ephervex will use a cached copy when one exists.
error-loader-metadata-invalid = Loader version information could not be understood.
.action = The provider may have changed its format. Ephervex refused to guess.
error-loader-version-unavailable = The requested loader version is unavailable.
.action = Choose an available exact build. Stable is not replaced with Latest automatically.
error-loader-identity-mismatch = The downloaded loader installer does not match the requested Minecraft or loader version.
.action = Ephervex refused the installer instead of installing different software.
error-loader-installer-invalid = The loader installer uses an unsupported or unsafe format.
.action = Check for an Ephervex update before trying this installer again.
error-loader-download-failed = A file required by the loader could not be downloaded or verified.
.action = Check the network connection and retry. Partial or unverified files are not kept.
error-loader-version-metadata-invalid = The loader's launch metadata could not be merged with Minecraft.
.action = Ephervex refused to launch with incomplete or conflicting metadata.
error-loader-io = The loader installation could not be written.
.action = Check permissions and free disk space at { $path }.
error-loader-processor-failed = A loader installation processor failed.
.action = Review the technical details. Ephervex removed the incomplete staging directory.
error-loader-server-target-occupied = The server installation target is not empty.
.action = Choose an empty directory so existing server data cannot be overwritten.

error-launch-asset-index-invalid = The asset index for that version could not be understood.
.action = Ephervex refused it rather than launching a game with no sounds or textures and no reason on screen.

error-launch-natives-unreadable = One of the native library archives could not be read.
.action = Delete { $path } so Ephervex fetches it again.

error-launch-natives-unsafe-path = A native library archive contained a file that would be written outside the launcher's own directories.
.action = Ephervex refused the archive. This should never happen with Mojang's own files; please report it.

error-launch-natives-too-large = A native library archive is larger than Ephervex will unpack.
.action = Ephervex refused it rather than filling the disk.

error-launch-natives-write = The native libraries could not be written.
.action = Check permissions and free space at { $path }.

# $placeholder is the unresolved token, verbatim from Mojang's metadata.
error-launch-unresolved-placeholder = Minecraft's launch arguments contain a value this build of Ephervex does not know how to fill in.
.action = This means Mojang added something new. Ephervex refuses rather than passing { $placeholder } to the game unchanged. Please report it.

error-launch-game-directory = The instance's game directory could not be prepared.
.action = Check permissions and free space at { $path }.

error-launch-process-start = Minecraft could not be started.
.action = Check that { $path } is a working Java runtime. `ephervex java list` shows the ones Ephervex found.

error-launch-process-lost = Ephervex lost track of the Minecraft process.
.action = Minecraft may still be running. Check your process list before launching again.

error-launch-canceled = The launch was canceled.

## Pack import 
#
# Pack names, file paths inside a pack, loader ids, and provider names are all
# untranslated technical text passed in as arguments.

pack-analysis-summary = { $name } — Minecraft { $version }, { $loader }, from { $provider }.
pack-analysis-file-count = { $count ->
        [one] { $count } file is installed automatically.
       *[other] { $count } files are installed automatically.
    }
pack-analysis-optional-count = { $count ->
        [one] { $count } optional file can be selected.
       *[other] { $count } optional files can be selected.
    }
# a client import keeps the server side of the pack rather than
# installing it, so a server can be created from the same recipe later.
pack-analysis-server-preserved = The pack's server files are recorded for later rather than installed.
pack-import-completed = Imported { $name } as { $instance }.
pack-import-files-preserved = { $count ->
        [one] { $count } file was recorded but not installed for this side.
       *[other] { $count } files were recorded but not installed for this side.
    }
# a refused address is reported rather than silently dropped.
pack-import-rejected-addresses = { $count ->
        [one] { $count } download address was refused by the download policy.
       *[other] { $count } download addresses were refused by the download policy.
    }
.action = Ephervex only downloads pack files from the hosts Modrinth's own pack validation accepts.
pack-recipe-missing = This instance was not imported from a pack, so it has no pack recipe.
pack-recipe-layer-missing = One of the pack's file layers was not kept, so a server made from this recipe would be missing it.

pack-requirement-required = Required
pack-requirement-optional = Optional
pack-requirement-unsupported = Not supported

error-pack-archive-unreadable = The pack file could not be read.
.action = Check that { $path } exists and is readable.

error-pack-archive-invalid = That file is not a readable archive.
.action = A pack is a ZIP file. Check that { $path } downloaded completely.

error-pack-format-unrecognized = Ephervex does not recognize that file as a modpack.
.action = Ephervex imports Modrinth `.mrpack` files, CurseForge exports, and Prism instance exports.

# a recognized format with no importer yet is said out loud rather than
# reported as a broken file.
error-pack-format-not-implemented = This build of Ephervex cannot import { $format } packs yet.
.action = Modrinth `.mrpack` import is available now; the other formats arrive in a later version.

error-pack-manifest-missing = The pack is missing the file that describes it.
.action = Ephervex expected { $entry } inside the archive.

error-pack-manifest-invalid = The pack's description file could not be understood.
.action = Ephervex refuses a pack it cannot read completely rather than importing part of one.

error-pack-manifest-unsupported-version = That pack uses a newer pack format than this build of Ephervex understands.
.action = Update Ephervex. Guessing at a format revision would risk importing the wrong files.

error-pack-game-unsupported = That pack is not a Minecraft pack.

error-pack-minecraft-missing = The pack does not say which Minecraft version it is for.
.action = Ephervex reproduces exact versions and will not guess one.

# an unsupported-loader error: named, not silently downgraded to vanilla.
error-pack-loader-unsupported = { $loader } is not a mod loader Ephervex supports.
.action = Ephervex supports Vanilla, Forge, and NeoForge. The pack was not imported rather than being imported as something it is not.

error-pack-loader-ambiguous = The pack declares two different mod loaders.
.action = { $first } and { $second } cannot both apply. Ephervex will not pick one on the pack author's behalf.

error-pack-entry-unsafe = The pack contains a file path that would write outside the instance.
.action = Ephervex refused the whole pack. Please report this to whoever produced it.

error-pack-entry-reserved = The pack contains a file that would overwrite Ephervex's own instance metadata.
.action = Ephervex refused the whole pack. Pack contents may never write into the.launcher folder.

error-pack-entry-type-unsupported = The pack contains an archive entry Ephervex will not extract.
.action = Ephervex extracts ordinary files and folders only.

error-pack-limit-exceeded = The pack is larger than Ephervex is willing to unpack.
.action = Ephervex stopped rather than filling the disk. The limits are deliberately generous.

error-pack-file-hashes-missing = One of the pack's files has no checksum Ephervex can verify.
.action = Ephervex will not download a file it cannot verify.

error-pack-file-downloads-missing = One of the pack's files has no download address.

error-pack-download-url-rejected = One of the pack's files is hosted somewhere Ephervex will not download from.
.action = Ephervex only downloads pack files over HTTPS from the hosts Modrinth's own pack validation accepts.

# a hash mismatch is a hard failure, and the bad file is deleted.
error-pack-download-failed = A file from the pack could not be downloaded or did not match its checksum.
.action = Ephervex deleted the bad file rather than installing it. Check the network connection and try again.

error-pack-write-failed = A file from the pack could not be written.
.action = Check permissions and free space at { $path }.

error-pack-recipe-unreadable = This instance's retained pack recipe could not be read.
.action = Check that { $path } is readable. Without it, a server cannot be created from this instance's pack.

error-pack-recipe-invalid = This instance's retained pack recipe is not valid.

error-pack-recipe-unsupported-version = This instance's pack recipe was written by a newer version of { app-name }.
.action = Update Ephervex rather than letting this build read part of it.

error-pack-canceled = The import was canceled.
