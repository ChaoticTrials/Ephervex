# Ephervex - English (United States) source resource.
#
# This file is the fallback language: every message id used anywhere in the
# application must exist here, and `fl!()` checks that at compile time.
#
# Rules for anything added below:
#   * Provider names (Modrinth, CurseForge, Forge, NeoForge), technical
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
instance-summary = { $name } - Minecraft { $version }, { $loader }

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

reconcile-unfinished-instances = { $count ->
        [one] { $count } instance directory was left unfinished by an interrupted creation or import.
       *[other] { $count } instance directories were left unfinished by interrupted creations or imports.
    }
.action = Ephervex did not add them to the instance list, because their files are incomplete. Delete the directories when you no longer need what they hold.

# An instance whose files were found already standing at the persistent root
# they were being moved to. Good news, not a warning: the record followed them
# rather than being forgotten (ADR 0025).
reconcile-relocated = { $count ->
        [one] Finished moving { $count } instance into your persistent instance folder.
       *[other] Finished moving { $count } instances into your persistent instance folder.
    }

reconcile-nothing-to-do = Everything in the index matches what is on disk.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-persistent-root-missing-detail = { $name } expected its files at { $path }.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-unfinished-instance-detail = { $name } was never finished at { $path }.

## Attention reasons 
#
# Ephervex never presents an instance as faithfully runnable when it is not.

attention-missing-root = The instance directory is missing.
attention-unsupported-loader = { $loader } is not a supported mod loader.
attention-unsupported-launch-customization = The source used launch settings Ephervex cannot reproduce: { $detail }
attention-ignored-unsafe-customization = An imported command or hook was found and deliberately not run: { $detail }
attention-best-effort-server-conversion = This server was generated from a client instance and may still contain client-only mods.
attention-manual-download-required = Some files must be downloaded manually: { $detail }
attention-unknown-server-applicability = Server applicability is undeclared for { $path }, and this server was created without it.

error-pack-recipe-missing = The client no longer has the retained recipe needed to create its server.
.action = Restore the client's launcher metadata. The original pack archive is not used as a fallback.
error-pack-retained-layer-unavailable = The retained { $scope } override layer is unavailable.
.action = Restore { $path } from the client instance's retained launcher source.
error-pack-unknown-server-selection = The confirmed path is not an unknown-server entry in the retained recipe.
.action = Inspect the retained recipe and pass an exact logical path shown as unknown for the server.
error-pack-recipe-target-mismatch = The retained recipe does not match the linked client's exact Minecraft or loader version.
.action = Restore matching client metadata and recipe data. Ephervex will not substitute versions.
error-pack-recipe-destination-collision = Two selected recipe files use the same server path.
.action = Fix the retained desired state; Ephervex will not choose one file to overwrite the other.
error-pack-recipe-destination-occupied = A selected recipe file would overwrite part of the installed server.
.action = Fix the retained recipe path collision; Ephervex will not overwrite the base server silently.

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

## Artifact cache errors

# The cache holds re-downloadable copies, so every one of these leaves the cache
# larger than its budget rather than risking a file something is still using.
error-artifact-cache-busy = The reusable artifact cache is in use right now.
.action = Wait for the current download or installation to finish, then try again.

error-artifact-cache-lock-unusable = Ephervex could not claim the artifact cache for maintenance.
.action = Check permissions on { $path }.

error-artifact-cache-scan-failed = The reusable artifact cache could not be read.
.action = Check permissions on { $path }. Nothing was removed.

error-artifact-cache-limit-too-large = That cache limit is larger than Ephervex can store.
.action = Choose at most { $maximum }. The limit you had is unchanged.

error-artifact-cache-limit-too-small = That cache limit is too small to be useful.
.action = Choose at least { $minimum }. The limit you had is unchanged.

error-artifact-cache-limit-malformed = That cache limit is not a whole number.
.action = Enter a whole number and pick a unit. The limit you had is unchanged.

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

## Instance duplication, storage, and folders 

instance-duplicated = Duplicated as { $name }.

# $value is a URL with its query and fragment removed.
import-downloaded-pack = Downloaded the pack from { $value }.

# $name is user-authored, $path is a filesystem path. Untranslated.
# Moving preserves the instance, so there is no counterpart to
# instance-duplicate-skipped-links below: a move keeps a linked folder as a
# link rather than copying what it points at.
instance-made-persistent = { $name } is now stored permanently at { $path }.
server-made-persistent = { $name } moved with it, to { $path }.
server-not-made-persistent = { $name } could not be moved and is still temporary: { $value }
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

## Logs and crash reports
#
# File names, paths, byte counts, and the text of a log or crash report are
# shown exactly as written and are never translated.

diagnostics-nothing-recorded = Nothing has been written here yet.

# $returned and $total are byte counts.
diagnostics-window-truncated = Showing { $returned } of { $total } bytes.
diagnostics-window-lossy = Some bytes are not text and were replaced so the rest stays readable.

diagnostics-search-no-matches = Nothing in this part of the file matches.
# $matches is how many matches were shown before the limit stopped the search.
diagnostics-search-truncated = Stopped after { $matches } matches. Narrow the search or raise the limit to see more.

error-diagnostics-directory-unreadable = That log folder could not be read.
.action = Check the folder and its permissions.
error-diagnostics-file-missing = That file is no longer there.
.action = Logs rotate and crash reports can be deleted. Refresh the list and try again.
error-diagnostics-file-unreadable = That file could not be read.
.action = Check the file and its permissions.
error-diagnostics-file-refused = Ephervex will not open that file.
.action = Only regular files inside the instance's, the server's, or the launcher's own log and crash-report folders can be opened.

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
# "Unknown" is not "unsupported": nobody has established what this release does
# with the setting, which is what happens the day a Minecraft release appears.
preferences-unverified-for-version = How Minecraft { $version } stores { $preference } has not been verified, so it was not applied.
preferences-value-not-applied = Minecraft { $version } cannot store the configured { $preference }, so it was not applied.
preferences-language-unverified = The languages Minecraft { $version } offers could not be checked, so { $preference } was not applied.
preferences-language-absent = Minecraft { $version } does not offer that language, so { $preference } was not applied.

error-options-compatibility-invalid = The Minecraft settings compatibility data could not be used.
.action = Ephervex is using the data it shipped with. No settings file was changed.

error-options-feed-unavailable = The published Minecraft settings compatibility data could not be updated.
.action = Ephervex is using the data it already has. Newly released Minecraft versions may not receive managed settings until this succeeds.

error-options-unreadable = The instance's Minecraft settings file could not be read.
.action = Check that { $path } is readable. Ephervex will not replace a settings file it could not read first.

error-options-unwritable = The instance's Minecraft settings file could not be written.
.action = Check that { $path } is writable.

# $syntax is a literal input format, not prose to translate.
error-options-preference-value-invalid = That is not a usable value for this setting.
.action = Expected { $syntax }.

# $preference is a stable setting id and is never translated.
error-options-preference-value-unusable = The configured value for { $preference } cannot be used for this setting.
.action = Choose a value this setting accepts and save again.

error-options-preference-unrecognized = That is not a Minecraft setting this version of Ephervex offers.
.action = Check the setting name, or update Ephervex.

error-options-preference-catalog-invalid = The Minecraft settings this version of Ephervex offers could not be read.
.action = No settings file was changed. Reinstall Ephervex if this persists.

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

error-minecraft-language-catalog-unavailable = The list of languages that Minecraft version offers could not be resolved.
.action = The instance is unaffected. Ephervex leaves the managed language setting alone rather than claiming a language the release may not have.

error-minecraft-asset-index-invalid = That version's asset index could not be understood.

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

error-java-settings-invalid = Java memory settings are invalid: minimum { $minimum } MiB, maximum { $maximum } MiB.
    .action = Use positive values and make the maximum at least the minimum.

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

# Not a problem with the user, the account, or the sign-in - a property of the
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

error-auth-credential-blank = No value was entered.
.action = Paste the credential and try again. Nothing was changed.

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
# it can do things it cannot - such as join an online-mode server.
account-active-offline = Using the offline account { $name }.
account-removed = Removed the account { $name }.
account-removed-with-credential = Removed the account { $name } and deleted its saved credential.

# An offline account has no credential at all - this is not a missing or
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
launch-connect-starting-server = Starting linked server { $server }…
launch-connect-reusing-server = Reusing linked server { $server }.
launch-connect-waiting = Waiting for the server at { $target }…
launch-connect-ready = The server is ready at { $target }.

operation-launching-minecraft = Launching Minecraft
operation-starting-server = Starting the server
# The phases a long operation moves through, named so a waiting user can see
# which one is running.
operation-stage-reading-archive = Reading the archive
operation-stage-resolving-files = Resolving files with the provider
operation-stage-downloading-files = Downloading files
operation-stage-extracting-overrides = Extracting the pack's own files
operation-stage-retaining-source = Keeping the pack's source
operation-stage-applying-settings = Applying settings
operation-stage-publishing = Finishing up
operation-stage-installing-server = Installing the linked server
operation-stage-installing-server-pack = Installing the server files
operation-stage-relocating-files = Moving the files
operation-importing-pack = Importing modpack
operation-installing-mods = Installing mods
operation-creating-server = Creating server
operation-promoting-instance = Making the instance persistent
operation-downloading-pack = Downloading the pack
operation-restoring-mod = Putting the mod back
operation-creating-instance = Creating instance

# $operation identifies one background task. It is shown so a user can find the
# same task in the launcher log, and is never translated.
operation-identifier = Operation { $operation }

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
# $since is the oldest Minecraft release the loader was ever published for.
error-loader-never-published = { $loader } has no builds for Minecraft { $minecraft }.
.action = { $loader } was first published for Minecraft { $since }. Choose that release or a newer one, or choose a different loader.
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

pack-analysis-summary = { $name } - Minecraft { $version }, { $loader }, from { $provider }.
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
# Distinct from "Optional": the mod's author declared nothing for this side, so
# Ephervex has nothing to go on and does not guess on their behalf.
pack-requirement-unknown = Not stated

# Why a mod cannot be changed or removed. Each names a different situation, and
# the difference is what tells a user whether to expect this to change.
mod-restriction-pack-owned = The imported pack declares this mod. Enable independent mod changes for this instance to change it on its own.
mod-restriction-dependency = Another mod requires this one, so its release follows what needs it.
mod-restriction-hand-placed = This jar was added by hand, so Ephervex leaves it alone. Remove it from the mods folder yourself.
mod-restriction-identity-unresolved = Ephervex cannot tell which project this file is, so it has nothing to look up.
mod-restriction-version-unknown = Ephervex cannot tell which release is installed, so it will not guess what to replace.

# What carrying one mod decision to a linked server would do, and why it
# cannot. The exact release the client ends up with is the release every
# selected server is evaluated for, so "a newer release fits this server" is
# never one of these answers.
linked-target-will-install = Will install
linked-target-will-change-version = Will change version
linked-target-will-repair = Will restore its recorded release
linked-target-will-remove = Will remove
linked-target-already-current = Already at this release
linked-target-not-present = Does not have this mod

linked-target-server-unsupported = This mod does not support servers.
linked-target-incompatible-minecraft = This release does not support the server's Minecraft version.
linked-target-incompatible-loader = This release does not support the server's loader.
linked-target-exact-release-unavailable = This exact release cannot be installed on this server.
linked-target-server-running = Stop this server before changing its mods.
linked-target-server-not-installed = This server has no installation yet.
linked-target-no-longer-linked = This server is no longer linked to this instance.
linked-target-provider-unavailable = The mod catalog could not be reached, so this server could not be checked.
linked-target-state-unavailable = This server's recorded mods could not be read.
linked-target-pack-owned = This server's pack declares this mod.
linked-target-server-dependency = Another mod on this server requires this one.

error-artwork-unusable = A mod's icon could not be shown.
.action = The mod itself is unaffected; a generic icon is used instead.
error-loader-family-fixed = { $loader } instances have no loader build to change.
.action = Changing the loader itself is not supported; create an instance with the loader you want.
error-loader-change-blocked-by-mods = { $count ->
    [one] 1 installed mod does not fit the selected loader build.
   *[other] { $count } installed mods do not fit the selected loader build.
}
.action = Change those mods first, or confirm that you want to run the new build anyway.

error-mod-not-managed = That mod is no longer part of this instance.
.action = Reopen the instance to see its current mods.
error-mod-identity-incomplete = One of this instance's mods has no provider identity, so its dependencies cannot be worked out.
# $value is a URL with its query and fragment removed, because a confirmation
# prompt is not the place to print an identifier or a token.
error-single-instance-unavailable = Ephervex could not reach the launcher that is already running.
.action = The link was not handed over. Close the other Ephervex window and try again.
error-url-handler-unavailable = Ephervex could not change which application opens its links.
.action = Check that your home directory is writable.
error-launcher-url-not-one = That is not an Ephervex link.
error-launcher-url-too-long = That link is far longer than any address Ephervex reads.
error-launcher-url-unknown-action = That link asks for something this version of Ephervex does not do.
.action = Updating Ephervex may add it.
error-launcher-url-no-single-pack = That link does not name exactly one pack to import.
error-pack-url-invalid = That link does not name a pack Ephervex can download.
.action = A pack address has to be an absolute https address with no username or password in it.
error-pack-host-not-trusted = Ephervex has not been told it may download packs from that address.
.action = Open the link again and confirm the address, or import it once with `ephervex import <address> --trust always`.
error-pack-archive-download-failed = The pack could not be downloaded.
.action = Check that the address is still valid and that the machine serving it is reachable.
error-instance-already-persistent = That instance is already stored permanently.
.action = Only a temporary instance can be made persistent. Delete it when you no longer need it.
error-server-not-linked = That server does not belong to this instance.
error-mod-file-missing = That mod's file is not installed, so it cannot be enabled or disabled. Repair the instance to fetch it again.
error-mod-file-not-toggled = That mod's file could not be renamed. Check that the mods folder is writable.
.action = Mods with no identity have to be managed by hand.
error-mod-not-hand-placed = Ephervex installed that mod, so it is removed rather than deleted.
.action = Open the mod and use Remove, which also takes care of what it depended on.
error-mod-file-not-deleted = That mod's file could not be deleted. Check that the mods folder is writable.
error-mod-file-not-added = That file could not be copied into the mods folder. Check that there is room and that the folder is writable.

error-name-empty = A name cannot be blank.
error-name-too-long = That name is too long. Use at most { $limit } characters.
error-name-control-characters = A name cannot contain control characters.

error-mod-catalog-preference-unrecognized = "{ $catalog }" is not a mod catalog this version of Ephervex knows.
.action = Choose a catalog in Settings, or name one for this command. The stored value is left alone.

# Why one file the user chose cannot go into a mods folder. Each names
# something the user can act on rather than describing an internal check.
local-mod-not-a-file = Not a file.
local-mod-not-a-jar = Not a JAR. Minecraft mods are .jar files.
local-mod-unusable-name = Its name cannot be used for a file in the mods folder.
local-mod-not-an-archive = Not a readable JAR. It may be incomplete or corrupt.
local-mod-too-large = Too large to be a mod.

# Why a mod belongs to an instance. Kept distinct because they are different
# things to look at and, later, different things to change.
mod-origin-pack-declared = From pack
mod-origin-user-selected = Added
mod-origin-required-dependency = Dependency
mod-origin-selected-optional-dependency = Optional dependency
# A jar somebody copied into mods/ themselves. Ephervex lists it so the folder
# and the list agree, and does not manage it.
mod-origin-hand-placed = Added by hand

error-pack-archive-unreadable = The pack file could not be read.
.action = Check that { $path } exists and is readable.

error-pack-archive-invalid = That file is not a readable archive.
.action = A pack is a ZIP file. Check that { $path } downloaded completely.

error-pack-format-unrecognized = Ephervex does not recognize that file as a modpack.
.action = Choose a Modrinth `.mrpack` file or a CurseForge modpack export.

# A recognized but unsupported format is said out loud rather than reported as
# a broken file.
error-pack-format-unsupported = This build of Ephervex cannot import { $format } packs.
.action = Use a Modrinth `.mrpack` file or a CurseForge modpack export, which this build supports.

error-pack-manifest-missing = The pack is missing the file that describes it.
.action = Ephervex expected { $entry } inside the archive.

error-pack-manifest-invalid = The pack's description file could not be understood.
.action = Ephervex refuses a pack it cannot read completely rather than importing part of one.

error-pack-manifest-unsupported-version = That pack uses a newer pack format than this build of Ephervex understands.
.action = Update Ephervex. Guessing at a format revision would risk importing the wrong files.

error-pack-manifest-type-unsupported = That pack is not a Minecraft modpack.
.action = Its description file says it is { $found }. Ephervex imports Minecraft modpacks only.

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

error-pack-entry-id-invalid = The pack names a mod by an id that identifies nothing.
.action = { $field } is { $value }, which no project or file can have. Ephervex refused the whole pack rather than guessing which mod was meant.

error-pack-entry-inclusion-conflict = The pack lists the same file as both required and optional.
.action = File { $file_id } of project { $project } appears twice with different answers, so what the pack wants installed is not clear.

error-pack-provider-content-unsupported = This { $provider } modpack contains provider-managed content Ephervex cannot import.
.action = Project { $reference } has type "{ $project_type }".

error-pack-provider-files-unresolved = { $remaining } of the pack's files have not been looked up yet.
.action = Ephervex stopped rather than installing a pack with those files missing.

error-pack-provider-files-blocked = { $remaining } of the pack's files are not available for automatic download.
.action = Find each one in the import screen's blocked-downloads list, then download it manually or skip it, and try importing again.

error-pack-file-hashes-missing = One of the pack's files has no checksum Ephervex can verify.
.action = Ephervex will not download a file it cannot verify.

error-pack-file-downloads-missing = One of the pack's files has no download address.

error-pack-download-url-rejected = One of the pack's files is hosted somewhere Ephervex will not download from.
.action = Ephervex only downloads pack files over HTTPS, and only from the hosts the pack's own provider publishes them on.

# a hash mismatch is a hard failure, and the bad file is deleted.
error-pack-file-unmeasurable = An installed file could not be read back to be recorded.
.action = Check that { $path } is readable, then create the server again.

error-pack-download-failed = A file from the pack could not be downloaded or did not match its checksum.
.action = Ephervex deleted the bad file rather than installing it. Check the network connection and try again.

# The verified copy is no longer in the shared cache, so the file has to be
# fetched again from a provider that only serves it to an authenticated request.
error-pack-artifact-credential-unavailable = { $file } has to be downloaded again, and the API key its provider requires is not available.
.action = Add the provider's API key under Providers & APIs, then try again.

error-pack-write-failed = A file from the pack could not be written.
.action = Check permissions and free space at { $path }.

error-pack-recipe-unreadable = This instance's retained pack recipe could not be read.
.action = Check that { $path } is readable. Without it, a server cannot be created from this instance's pack.

error-pack-recipe-invalid = This instance's retained pack recipe is not valid.

error-pack-recipe-unsupported-version = This instance's pack recipe was written by a newer version of { app-name }.
.action = Update Ephervex rather than letting this build read part of it.

error-pack-canceled = The import was canceled.

## Mods installed through a provider
#
# Project ids, version ids, file names, and provider names are technical values
# and are passed in as arguments rather than translated.

# A jar Ephervex did not install is never overwritten. It is very often the
# local build the instance exists to reproduce a bug against.
error-mod-destination-occupied = A file Ephervex did not install is already in the way.
.action = { $path } was not installed by Ephervex, so it was left alone. Move or rename it if you want this mod installed there.

error-mod-destination-collision = Two of these mods install under the same file name.
.action = { $name } is claimed twice, so installing them would leave only one. Install them separately, or leave one out.

# Changing an installed mod's version is a mod update, which this build does not
# implement. Replacing the file would delete a jar the user did not ask about.
error-mod-already-installed = That mod is already installed at a different version.
.action = { $project } is installed from { $provider } at { $installed }, not { $requested }. Changing an installed mod's version is not supported yet.

error-mod-managed-identity-missing = That file cannot be recorded as an installed mod.
error-mod-hand-placed-not-mutable = That jar was added by hand, so Ephervex does not replace or remove it.
.action = Delete it from the mods list if you no longer want it.
.action = Ephervex only records mods it can look up again by project and version, so nothing was invented for { $path }.

## Browsing and resolving mods
#
# Almost every message here is a refusal. Each one is a case where guessing
# would produce an instance that reproduces something other than the bug being
# chased, so the launcher says what it will not do and why.

mod-search-empty = No mods matched.
.action = Try a different search, or turn on incompatible results to see mods that do not fit this instance.
mod-search-count = { $count ->
        [one] { $count } mod
       *[other] { $count } mods
    }
mod-list-empty = No mods have been installed into this instance through a provider.
mod-installed-count = { $count ->
        [one] { $count } installed mod
       *[other] { $count } installed mods
    }
mod-install-completed = Installed { $name }.

# What a crash report says about which mods to try without. "Confidence" is the
# report's own directness: a mod it named outranks one it merely showed frames
# from, which outranks one that was only inferred to be involved.
crash-none = This server has no crash reports.
running-nothing = Nothing is running.
gui-running = Running
gui-running-detail = Everything Ephervex is running right now. Client instances with a live launch, and servers this launcher is supervising.
gui-nothing-running = Nothing is running.
gui-nothing-running-detail = Launch an instance or start a server and it will appear here.
gui-uptime-seconds = { $count ->
        [one] { $count }s
       *[other] { $count }s
    }
gui-uptime-minutes = { $count ->
        [one] { $count }m
       *[other] { $count }m
    }
gui-uptime-hours = { $hours }h { $minutes }m
gui-process-usage = { $cpu }% CPU | { $memory }
gui-stop-game = Stop Game
gui-force-stop = Force Stop
gui-force-stop-signaled = Asked the launch to stop, and released the instance.
gui-force-stop-released = The launch was already gone. The instance is released.
gui-open-log = Open Log
gui-open-console = Console
crash-no-suspects = The crash report does not point at any mod.
crash-client-code-on-server = A missing class means client-only code ran on a server.
crash-reason-named = named as the mod that failed
crash-reason-in-failing-frames = appears in the failing stack
crash-reason-compat-trigger = { $mod } ran its compatibility code for this mod
crash-not-in-mod-list = Not in this server's mods list, so it cannot be switched off here.
gui-crash-title = This server failed to start.
gui-crash-review-mods = Review mods
gui-crash-disable-suggested = { $count ->
        [one] Disable { $count } mod
       *[other] Disable { $count } mods
    }
gui-crash-nothing-to-disable = The mods this crash points at are already switched off.
gui-crash-disabled-count = { $count ->
        [one] Disabled { $count } mod. Start the server again to see whether it gets further.
       *[other] Disabled { $count } mods. Start the server again to see whether it gets further.
    }

mod-install-nothing-to-do = Everything requested is already installed at the requested version.

# Shown next to an incompatible result rather than hiding it, so a user can see
# why the mod they are looking at is not offered.
mod-incompatible-minecraft-version = Does not support Minecraft { $required }.
mod-incompatible-loader = Does not support { $required }.

mod-warning-client-side-unsupported = { $project } says it does not run on the client.
.action = It was installed because you asked for it by name. Remove it if this instance is not meant to have it.
mod-warning-uncheckable-conflict = { $declared_by } says it conflicts with the file { $file_name }, which Ephervex cannot check automatically.
.action = Check your mods folder for that file.
mod-warning-optional-not-offered = { $project } was selected as an optional dependency, but nothing in this graph offers it.

error-mod-provider-unreachable = { $provider } could not be reached.
.action = Check the network connection and try again.

error-mod-provider-response-invalid = { $provider } returned something Ephervex could not read.
.action = This is usually temporary. If it persists, Ephervex may need updating for a changed API.

error-mod-provider-http-status = { $provider } answered with HTTP { $status }.
.action = This is usually temporary. Try again in a few minutes.

error-mod-credential-missing = { $provider } needs an API key, and none is configured.
.action = Add your { $provider } API key in Settings, under Providers & APIs.

error-mod-credential-unreadable = Ephervex could not read your { $provider } API key.
.action = The key is kept in the system keyring. Check that your keyring is unlocked, then try again.

error-mod-credential-rejected = { $provider } refused your API key (HTTP { $status }).
.action = The key may be invalid or no longer authorized. Check it on { $provider }, then set it again in Settings, under Providers & APIs.

error-mod-provider-not-browsable = Ephervex cannot browse { $provider }.
.action = This build records mods from { $provider } but has no browser for it.

error-mod-project-unknown = No such mod.
.action = Check the project id or slug. Ephervex looked for { $reference }.

error-mod-project-type-unsupported = That is not a mod.
.action = { $reference } has type "{ $project_type }". Modpacks are imported rather than installed into an existing instance.

error-mod-version-unknown = No such version.

# At most one file per version may be marked primary, so two is a malformed
# version rather than a choice to make.
error-mod-file-unknown = { $provider } has no such file for that mod.
.action = The pack asks for file { $file_id } of project { $project }, which { $provider } no longer lists. Ephervex will not install a different file in its place.

error-mod-file-unavailable = { $provider } is not serving that file.
.action = File { $file_id } of project { $project } exists but cannot be downloaded right now.

# The author's decision, not a fault. Ephervex has no other way to obtain the
# file and does not go looking for one.
error-mod-file-distribution-restricted = That mod's author does not allow other launchers to download it.
.action = Project { $project } has third-party downloads turned off, so Ephervex cannot fetch file { $file_id }. Download the pack through CurseForge's own app, or ask the pack author to replace the mod.

error-mod-file-identity-mismatch = { $provider } answered about a different file than the one requested.
.action = Ephervex asked for { $requested } and was told about { $returned }, so nothing was installed.

error-mod-primary-file-ambiguous = That version does not name exactly one file to install.
.action = Ephervex will not pick between { $found } candidate files. Choose a different version.

error-mod-hashes-missing = That version published no checksum Ephervex can verify.
.action = Every downloaded file is verified before it is installed, so this one is refused.

error-mod-loader-cannot-host-mods = { $loader } instances cannot install mods.
.action = Create a Forge or NeoForge instance to browse for mods.

# Never resolved by substituting a nearby release.
error-mod-version-incompatible = That exact version does not fit this instance.
.action = { $version_id } does not support Minecraft { $minecraft } with { $loader }. Choose a version that does; Ephervex will not substitute another.

error-mod-no-compatible-version = No version of that mod fits this instance.
.action = Nothing published for { $project } supports Minecraft { $minecraft } with { $loader }.

# Two mods each demanding a different exact version is a genuine conflict.
# Picking one would be the launcher inventing an answer.
error-mod-conflicting-pins = Two mods require different exact versions of the same dependency.
.action = { $project } is required at { $first } by { $first_source } and at { $second } by { $second_source }. Install them separately, or choose versions that agree.

error-mod-dependency-incompatible = These mods cannot be installed together.
.action = { $declared_by } declares that it is incompatible with { $conflicting }.

error-mod-dependency-unresolvable = A required dependency could not be identified.
.action = { $declared_by } requires the file { $file_name }, which names no project Ephervex can look up. Install that file yourself, then try again.

error-mod-dependency-inconsistent = A dependency contradicts itself.
.action = It names the project { $project_id } and a version belonging to { $version_project_id }.

error-mod-dependency-malformed = A dependency of { $declared_by } identifies nothing to install.

error-mod-resolution-did-not-converge = Ephervex could not settle this mod's dependencies.
.action = This is a defect in Ephervex rather than a problem with the mod. Please report it.

error-mod-canceled = The operation was canceled.

## Dedicated servers

error-server-property-invalid = That server property value is invalid.
.action = Correct the value and try again.
error-server-property-protected = That property has a dedicated server setting.
.action = Use the typed network or EULA command instead.
error-server-eula-not-accepted = Minecraft EULA consent has not been accepted.
.action = Review the Minecraft EULA, then accept it interactively with the server EULA command.
error-server-eula-consent-malformed = The recorded Minecraft EULA consent is incomplete or invalid.
.action = Revoke it, review the current EULA, and accept again interactively.
error-server-runtime-state = The server runtime evidence could not be read or written.
.action = Inspect the server's .launcher directory and its permissions.
error-server-file-io = A server file could not be read or written.
.action = Check the path, available disk space, and permissions.
error-server-runtime-may-be-alive = Ephervex cannot prove that the recorded server process is gone.
.action = Check the reported PID and executable with operating-system tools. Runtime evidence can be cleared only after the process is proven dead.
error-server-control-unavailable = The live server owner could not be reached.
.action = Check server status. An abrupt supervisor termination may leave an orphaned Java process that Ephervex will not adopt or signal.
error-server-process-start = The server Java process could not be started.
.action = Check the selected Java executable and the server installation.
error-server-already-running = This server is already running or may still be running.
.action = Check server recovery status before trying to start, delete, install, or change it.
error-server-console-invalid = A console command must be exactly one line.
error-server-process-io = Communication with the server process failed.
error-server-control-response-invalid = The server control channel returned an invalid response.
error-server-not-installed = This server's base installation is not complete.
.action = Install the exact selected Minecraft and loader versions before starting it.
error-server-readiness-timeout = The server did not accept connections at { $target } within { $timeout_seconds } seconds.
.action = Inspect the server output and network settings, then try again when the server can finish starting.
error-server-readiness-canceled = Waiting for the server was canceled.
.action = Start the operation again when you are ready.
error-server-persistence-unhealthy = This server's canonical metadata is unhealthy or conflicted.
.action = Inspect server status and resolve missing, corrupt, unsupported, or duplicate metadata before changing or starting it.

error-connect-server-not-linked = Server { $server } is not linked to a client.
.action = Choose a server created from the client you want to launch.
error-connect-relationship-mismatch = Server { $server } is linked to client { $linked_client }, not { $requested_client }.
.action = Launch the exact client recorded by the server link.
error-connect-target-mismatch = The client uses Minecraft { $client_minecraft } with { $client_loader }; the server uses Minecraft { $server_minecraft } with { $server_loader }.
.action = Restore matching canonical metadata. Ephervex will not substitute versions or loaders.
error-connect-runtime-uncertain = Ephervex cannot safely reuse or replace the recorded process for server { $server }.
.action = Inspect server status and resolve the unknown or orphaned runtime before connecting.
error-connect-server-stopped-before-ready = Server { $server } stopped before it accepted connections.
.action = Inspect the server output and correct the startup failure before launching the client.
error-connect-supervisor-lost = Supervision for server { $server } ended unexpectedly.
.action = Inspect server status before trying again.

## Desktop interface

gui-import-modpack = Import Modpack
gui-new-instance = + New Instance
gui-new-server = + New Server
gui-refresh = Refresh
gui-instances = Instances
gui-instances-detail = Exact client environments for focused reproduction work.
gui-servers = Servers
gui-servers-detail = Standalone and linked dedicated servers managed by Ephervex.
gui-logs = Logs
gui-activity = Activity
gui-settings = Settings
gui-settings-detail = Application, runtime, account, provider, and storage defaults.
gui-creation-already-running = This creation request is already in progress.
gui-creation-pending-detail = Validation and preparation are in progress. The finished environment will appear here automatically.
gui-reset = Reset
gui-state-creating = Creating
gui-state-ready = Ready
gui-state-starting = Starting
gui-state-starting-detail = Preparing the runtime process
gui-tagline = Spawn. Reproduce. Despawn.
gui-all = All
gui-temporary = Temporary
gui-persistent = Persistent
gui-filter-instances = Filter instances...
gui-loading-instances = Loading instances...
gui-no-client-instances = No client instances yet
gui-no-client-instances-detail = Create an exact Vanilla, Forge, or NeoForge environment, or import a supported modpack.
gui-client-instances-count = { $count ->
    [one] 1 client instance
   *[other] { $count } client instances
}
gui-attention = ATTENTION
gui-managed-mods-count = { $count ->
    [one] 1 managed mod
   *[other] { $count } managed mods
}
gui-linked-servers-count = { $count ->
    [one] 1 linked server
   *[other] { $count } linked servers
}
gui-source-instance = { $value } instance
gui-launch = Launch
gui-launch-as = Launch as
gui-add-account = Add account...
gui-launch-unavailable = Launch unavailable
gui-start = Start
gui-stop = Stop
gui-unavailable = Unavailable
gui-no-instance-selected = No instance selected
gui-select-instance = Select an instance to inspect it.
gui-open = Open
gui-overview = OVERVIEW
gui-status = Status
gui-minecraft = Minecraft
gui-loader = Loader
gui-source = Source
gui-mods = Mods
gui-managed-count = { $count } managed
gui-storage = Storage
gui-actions = ACTIONS
gui-needs-attention = Needs attention
gui-ready = Ready
gui-ready-to-launch = Ready to launch
gui-busy = Busy
gui-launch-connect = Launch & Connect to { $value }
gui-open-workspace = Open Workspace
gui-create-linked-server = Create Linked Server
gui-open-folder = Open Folder
gui-duplicate-instance = Duplicate
gui-make-persistent = Make Persistent
# Shown in place of the action while the game is running, and as the hover
# explanation on the button. Moving an instance underneath a running game would
# pull the directory out from under it.
gui-make-persistent-while-running = Make Persistent (stop the game first)
gui-takes-no-arguments = The Ephervex desktop application takes no arguments. Use the ephervex command for the command line.
gui-trusted-pack-hosts = Sites allowed to serve modpacks
gui-trusted-pack-hosts-detail = Ephervex may download a modpack archive from these without asking again. It does not change where a pack's own files may come from.
gui-no-trusted-pack-hosts = None yet. The first time a link points at a site, Ephervex asks, and your answer appears here.
gui-forget-pack-host = Forget
gui-open-pack-link = Open a modpack
# $value is the address with its query and fragment removed, so a link's
# identifiers and tokens are not printed in a prompt.
gui-open-pack-link-detail = A link asked Ephervex to download a modpack from:
gui-open-pack-link-remember = Do not ask again for this site
gui-open-pack-link-confirm = Download it
gui-downloading-pack = Downloading the modpack...
gui-make-persistent-confirm = Move it
# $value is the instance name, user-authored.
gui-make-persistent-detail = { $value } lives in temporary storage, which the operating system clears whenever it likes. Moving it keeps everything it has, including its mods, saves, logs and linked servers.
gui-make-persistent-servers = { $count ->
        [one] { $count } linked server is temporary too. Bring it along:
       *[other] { $count } linked servers are temporary too. Bring them along:
    }
# $value is a filesystem path.
gui-made-persistent = Moved to { $value }.
# $first is a server name, $second is the reason it did not move.
gui-server-not-made-persistent = { $first } could not be moved and is still temporary: { $second }
gui-delete-instance = Delete Instance...
gui-delete-server = Delete Server...
gui-delete-instance-detail = Remove "{ $value }" from Ephervex?
gui-delete-server-detail = Remove "{ $value }" from Ephervex?
gui-delete-choice-detail = Keeping files only removes the Ephervex record. Deleting files permanently removes the managed game or server directory.
# $count is how many servers were created from this instance.
gui-delete-instance-linked = { $count ->
    [one] Its linked server is deleted with it, the same way.
   *[other] Its { $count } linked servers are deleted with it, the same way.
}
gui-remove-from-ephervex = Keep Files
gui-delete-files = Delete Files
gui-instance-deleted = Instance removed.
gui-server-deleted = Server removed.
gui-notification-error = Action Needed
gui-notification-complete = Complete
gui-fov-normal = Normal
gui-fov-quake-pro = Quake Pro
gui-select-all = Select All
gui-diagnostics = DIAGNOSTICS
gui-view-logs-crash-reports = View Logs & Crash Reports
gui-back-instances = Back to Instances
gui-instance = Instance
gui-instance-tab-overview = Overview
gui-instance-tab-mods = Mods
gui-instance-tab-settings = Settings
gui-instance-tab-logs = Logs
gui-reproduction-environment = Reproduction Environment
gui-reproduction-environment-detail = Exact target, retained source details, and safe actions for this client instance.
gui-target-summary = Minecraft { $first } | { $second }
gui-open-diagnostics = View Diagnostics
gui-temporary-lifecycle = Temporary instances remain until the operating system clears its temporary storage. Make Persistent moves this one out of it, keeping its name, its mods and its linked servers.
gui-persistent-lifecycle = This instance is stored under the persistent instance root.
gui-managed-mods = Managed Mods
gui-managed-mods-detail = Every mod in this instance: the ones its pack brought, the ones you installed, and any jar you added yourself. Drop JARs here to add them.
# Filters the list of mods an instance or server already has, by name.
gui-filter-mods = Filter mods...
gui-no-mods-match-title = No matches
gui-no-mods-match = No mod in this list matches "{ $value }".
gui-refresh-mod-details = Refresh mod names and artwork
gui-no-managed-mods = No managed mods
gui-no-managed-mods-detail = Drop JAR files here to add them.
gui-mod-disabled = Disabled, so the loader skips it
# The jar was deleted here, and the pack still declares it, so it can come back.
gui-mod-file-deleted = Deleted. The pack still lists it, so it can be put back.
gui-restore-mod = Restore
gui-mod-restored = Put the mod back.
gui-mod-loads = Load { $value }
gui-server-mods = Server mods
gui-server-mods-detail = Everything in this server's mods folder, including jars you added yourself. Drop JARs here to add them. Turn off a mod to keep it installed while the loader skips it, which is how a client-only mod stops crashing a server.
gui-no-server-mods-detail = This server was not built from a pack, so nothing here is recorded as belonging to it.
# Shown when the mod's exact release cannot be proven from what Ephervex
# recorded. The file is installed; only its version is unknown.
gui-mod-version-unknown = Version not recorded
gui-mod-provider-unknown = No provider
gui-mod-metadata-offline = Ephervex could not load live mod metadata. Recorded mod information is still shown.
gui-modal-change-loader = Change Loader Build
gui-loader-change-open = Change Build
gui-loader-change-summary = Running { $value }
gui-loader-change-detail = Move this instance to a different build of the loader it already has. The loader itself does not change.
gui-loader-change-current = Current
gui-loader-change-target = Move to
gui-loader-change-unavailable = No builds are offered for this Minecraft release.
gui-loader-change-unverifiable = { $count ->
    [one] 1 installed mod could not be checked against the new build.
   *[other] { $count } installed mods could not be checked against the new build.
}
gui-loader-change-mods-untouched = Installed mods are left exactly as they are.
gui-loader-change-action = Install and Switch
gui-loader-changed = This instance now runs { $value }.
gui-modal-manage-mod = Manage Mod
gui-modal-remove-mod = Remove Mod
gui-mod-compatible-versions = Releases that fit this instance
gui-mod-versions-loading = Loading releases...
gui-mod-no-compatible-versions = No release of this mod fits this instance's exact Minecraft version and loader.
gui-mod-installed-version = Installed
gui-mod-newest-version = Newest
gui-mod-up-to-date = Up to date
gui-mod-change-version = Change Version
gui-mod-update = Update
gui-mod-open-project-page = View Project Page
# Shown on hover over the mod's name in Manage Mod, which opens the mod's
# project page in the browser.
gui-mod-open-project-page-hint = Open project page
gui-mod-reset-to-pack-version = Reset to Pack's Version
gui-mod-project-page-opened = Opened the project's page in your browser.
gui-mod-project-page-open-failed = Could not open a browser.
gui-mod-version-changed = { $value } now runs the release you selected.
gui-mod-already-current = { $value } is already the newest release that fits this instance.
gui-mod-repaired = { $value } was put back at its recorded release.
gui-mod-removed = { $count ->
    [one] Removed 1 mod.
   *[other] Removed { $count } mods.
}
gui-mod-remove-detail = { $value } will be deleted from this instance.
gui-mod-remove-dependencies = { $count ->
    [one] 1 mod was installed only for it and will go too:
   *[other] { $count } mods were installed only for it and will go too:
}
gui-mod-remove-no-dependencies = Nothing else was installed only for it.
gui-mod-remove-dependencies-unprovable = Ephervex could not confirm which mods are still needed, so none will be collected.
gui-mod-remove-action = Remove Mod
gui-add-mods = Add Mods
gui-catalog-compatibility = Search results are constrained to this instance's exact Minecraft version and loader.
gui-mod-catalog = Catalog
gui-provider-modrinth = Modrinth
gui-provider-curseforge = CurseForge
gui-search-compatible-mods = Search compatible mods...
gui-mod-search-running = Searching...
gui-no-mod-results = No matching mods
gui-no-mod-results-detail = Nothing in this catalog matches that search for this instance's Minecraft version and loader.
gui-load-more = Load More
gui-open-providers-settings = Open Providers & APIs
gui-search = Search
gui-mod-result-meta = by { $first } | { $second }
gui-mod-result-installed = Installed
gui-mod-results-per-page = Results
gui-mod-result-queued = Queued
gui-instance-runtime = Java and memory
gui-instance-runtime-detail = What this instance runs with. Anything left empty follows the global setting.
gui-instance-runtime-inherited = Global setting
gui-java-auto-selected = whichever runtime is selected
gui-instance-jvm-args = Extra JVM arguments
gui-instance-jvm-args-placeholder = Separated by spaces, for example -XX:+UseZGC
gui-instance-runtime-save = Save Runtime Settings
gui-instance-runtime-clear = Follow Global Settings
gui-instance-runtime-saved = Saved this instance's runtime settings.
# $first is the global heap range, $second the globally selected Java.
gui-instance-runtime-global = Globally: { $first }, { $second }.
gui-allow-pack-mod-changes = Independent mod changes
gui-allow-pack-mod-changes-detail = By default, a mod the imported pack declares can only be inspected here, not moved: the pack's declaration is treated as a historical record. Turning this on lets you update or change the version of a pack-declared mod for this instance only. "Reset to Pack's Version" always remains available to undo it.
gui-allow-pack-mod-changes-toggle = Allow changing pack-declared mods' versions
gui-instance-settings-unavailable = Instance-owned Minecraft settings
gui-instance-settings-unavailable-detail = The current core safely applies known Vanilla defaults when an instance is created, but does not expose a typed editor for an existing instance. Unknown and mod-added options remain untouched.
gui-open-global-defaults = Open Global Defaults
gui-filter-servers = Filter servers...
gui-no-servers = No servers yet
gui-no-servers-detail = Create a standalone exact server or create one linked to a client reproduction environment.
gui-no-standalone-servers-detail = No standalone servers are shown. Linked servers remain available from their client workspaces.
gui-linked-server = Linked Server
gui-no-linked-server-detail = Add a linked server when this reproduction environment needs a matching dedicated server.
gui-repair-server = Repair
gui-server-repaired = Cleared the stale runtime state. The server can be started again.
gui-server-needs-attention = This server needs attention before it can be started reliably.
gui-back-instance = Back to Instance
gui-no-server-selected = No server selected
gui-select-server = Select a server to inspect its runtime.
gui-state = State
gui-target = Target
gui-relation = Relation
gui-address = Address
gui-open-server-workspace = Open Server Workspace
gui-launch-client-connect = Launch Client & Connect
gui-force-kill = Force Kill...
gui-back-servers = Back to Servers
gui-server = Server
gui-restart = Restart
gui-server-tab-console = Console
gui-server-tab-properties = Properties
gui-server-run = Server run
gui-server-run-detail = Runtime state, exact target, network endpoint, and linked client relationship.
gui-dedicated-server = Dedicated Server
gui-exact-target = Exact target
gui-runtime = Runtime
gui-network = Network
gui-relationship = Relationship
gui-view-logs = View Logs
gui-bounded-output = { $value } | bounded retained output
gui-kill = Kill...
gui-server-console = Server Console
gui-server-console-detail = Output is streamed through the core supervisor and bounded to the retained capture window.
gui-enter-server-command = Enter server command...
gui-start-server-command = Start the server to send commands
gui-send = Send
gui-server-properties = server.properties
gui-server-properties-detail = Typed settings are saved explicitly. Unknown properties and comments are preserved by core.
gui-network-detail = Changes are validated and saved explicitly. A custom bind accepts an IP address, not a hostname.
gui-access = Access
gui-access-localhost = Localhost
gui-access-lan = LAN
gui-access-custom = Custom
gui-port = Port
gui-custom-bind-address = Custom bind address
gui-network-safety = Offline-mode servers exposed beyond localhost are flagged by core as a security risk.
gui-network-warning = Warning: Offline-mode access beyond localhost is unsafe.
gui-gameplay = Gameplay
gui-message-of-the-day = Message of the day
gui-default-motd = A Minecraft Server
gui-maximum-players = Maximum players
gui-allow-flight = Allow flight
gui-pvp = PvP
gui-allowlist = Allowlist
gui-online-mode = Online mode
gui-difficulty = Difficulty
gui-game-mode = Game mode
gui-save-changes = Save Changes
gui-server-eula = Minecraft Server EULA
gui-eula-review = Review
gui-eula-review-accept = Review & Accept
gui-logs-crash-reports = Logs & Crash Reports
gui-client = Client
gui-server-source = Server
gui-launcher = Launcher
gui-diagnostic-files-count = { $count ->
    [one] 1 diagnostic file
   *[other] { $count } diagnostic files
}
gui-no-diagnostics = No diagnostics
gui-no-diagnostics-detail = This source has not written any matching files.
gui-crash-name = Crash: { $value }
gui-log-name = Log: { $value }
gui-select-diagnostic = Select a diagnostic file
gui-search-loaded-content = Search loaded content...
gui-find = Find
gui-copy = Copy
gui-folder = Folder
gui-no-file-open = No file open
gui-no-file-open-detail = Choose a log or crash report. Ephervex reads a bounded, safe text window through diagnostics services.
gui-copy-path = Copy Path
gui-filter-all-levels = Show all log levels
gui-filter-info = Show INFO records
gui-filter-warn = Show WARN records
gui-filter-error = Show ERROR records
gui-filter-debug = Show DEBUG records
gui-filter-trace = Show TRACE records
gui-filter-fatal = Show FATAL records
gui-filter-other = Show unclassified records
gui-log-level-other = Other
gui-wrap-lines = Wrap Lines
gui-following = Following
gui-pause-following = Pause following new output
gui-jump-latest = Jump to Latest
gui-copy-retained = Copy Retained
gui-copy-selection = Copy Selection
gui-search-retained-logs = Search all retained logs...
gui-new-log-lines = { $count ->
    [one] 1 new line
   *[other] { $count } new lines
}
gui-retained-history-truncated = Older output is outside this retained view.
gui-no-log-output = No output yet
gui-no-log-output-detail = Ephervex will show new output here as it is written.
gui-log-output-block = Read-only log output
gui-log-bytes-omitted = [... { $count } bytes omitted]
gui-retained-log-records = { $count ->
    [one] 1 retained record
   *[other] { $count } retained records
}
gui-live-log-search-matches = { $count ->
    [one] 1 match in retained output
   *[other] { $count } matches in retained output
}
gui-live-latest-log = Live / Latest Log
gui-crash-reports = Crash Reports
gui-log-files = Log Files
gui-activity-detail = Current and recent work while Ephervex is running. Activity is not persisted across application restarts.
gui-no-recent-activity = No recent activity
gui-no-recent-activity-detail = Creation, imports, installations, launches, and server work will appear here.
gui-cancel = Cancel
gui-open-in-browser = Open in Browser
gui-skip = Skip
gui-settings-heading = SETTINGS
gui-settings-general = General
gui-settings-minecraft = Minecraft
gui-settings-java = Java
gui-settings-accounts = Accounts
gui-settings-providers = Providers & APIs
gui-settings-storage = Storage
gui-settings-about = About & Licenses
gui-application-behavior = Application behavior
gui-application-display = Application display
gui-application-display-detail = These settings affect Ephervex only. They do not change Minecraft's GUI scale.
gui-application-text-size = Interface text size
gui-application-text-size-detail = Adjust text across Ephervex while keeping the desktop layout and controls at their normal size.
gui-technical-text-size = Diagnostics and console text
gui-technical-text-size-detail = Set the monospace size used for selectable logs, crash reports, and server console output.
gui-text-scale-90 = 90%
gui-text-scale-100 = 100% (Default)
gui-text-scale-110 = 110%
gui-text-scale-125 = 125%
gui-technical-size-11 = Compact (11 px)
gui-technical-size-12 = Default (12 px)
gui-technical-size-14 = Comfortable (14 px)
gui-technical-size-16 = Large (16 px)
gui-save-general-settings = Save General Settings
gui-general-settings-saved = Saved general settings.
# $value is a Minecraft release id and is never translated.
gui-favorite-version = Offer Minecraft { $value } first
gui-unfavorite-version = Stop offering Minecraft { $value } first
gui-favorite-versions = Favorite Minecraft versions
gui-favorite-versions-detail = Releases you reproduce against often. They are offered first everywhere a Minecraft version is chosen.
gui-no-favorite-versions = No favorites yet. Star a release in the New Instance dialog to pin it here.
gui-linked-server-defaults = Linked server defaults
gui-linked-server-defaults-detail = Control where linked servers appear and how new development servers configure the selected account.
gui-show-linked-servers = Show linked servers in Servers
gui-show-linked-servers-detail = List a server built from an instance on the Servers page as well, rather than only under the instance it belongs to.
gui-default-account-operator = Make the default account an operator
gui-default-account-operator-detail = Applies to new servers, and only where the account's identity is compatible with the server.
gui-default-operator-level = Default operator permission level
gui-default-operator-level-detail = Applies to the individual ops.json entry on new servers.
gui-operator-level-1 = 1 - Basic commands
gui-operator-level-2 = 2 - Game management
gui-operator-level-3 = 3 - Player management
gui-operator-level-4 = 4 - Full server control
gui-operator-level-invalid = Choose an operator permission level from 1 through 4.
gui-port-automatic = Automatic port
gui-port-automatic-detail = Starts at 25565 and uses the next locally available port when needed.
gui-port-custom = Custom port
gui-display-settings-invalid = Select one of the available display sizes.
gui-minecraft-defaults-title = Minecraft defaults
gui-minecraft-defaults-detail = These settings are applied to new client instances and imports. Existing instances are not changed.
gui-minecraft-forget = Reset
gui-minecraft-advanced = Advanced
gui-minecraft-unsaved = Unsaved
gui-minecraft-unsaved-count = { $count ->
    [one] 1 unsaved change
   *[other] { $count } unsaved changes
}
gui-minecraft-total-count = { $count ->
    [one] 1 Minecraft setting
   *[other] { $count } Minecraft settings
}
gui-minecraft-filter = Filter settings...
gui-minecraft-show-advanced = Show advanced
gui-minecraft-only-overridden = Only overridden
gui-minecraft-preview = Compatibility preview
gui-minecraft-preview-detail = Pick a Minecraft release to see what it would do with these defaults. Nothing configured here changes.
gui-minecraft-no-matches = Nothing matches
gui-minecraft-no-matches-detail = No setting in this category matches the filter. Clear it, turn on advanced settings, or try another category.
gui-minecraft-category-gameplay = Gameplay
gui-minecraft-category-video = Video
gui-minecraft-category-sound = Sound & Music
gui-minecraft-category-controls = Controls
gui-minecraft-category-language = Language & Accessibility
gui-minecraft-section-general = General
gui-minecraft-section-volume = Volume
gui-minecraft-section-skin-parts = Skin parts
gui-minecraft-section-movement = Movement
gui-minecraft-section-gameplay = Gameplay
gui-minecraft-section-inventory = Inventory
gui-minecraft-section-multiplayer = Multiplayer
gui-minecraft-section-creative = Creative
gui-minecraft-section-spectator = Spectator
gui-minecraft-section-misc = Miscellaneous
gui-minecraft-section-debug = Debug
gui-minecraft-state-overridden = Override
gui-minecraft-state-not-overridden = Not overridden
gui-minecraft-compat-supported = Supported
gui-minecraft-compat-unavailable = Unavailable
gui-minecraft-compat-unverified = Unverified
gui-minecraft-compat-unrepresentable = Not representable
gui-minecraft-compat-unavailable-detail = Minecraft { $value } does not have this setting.
gui-minecraft-compat-unverified-detail = Nothing is established about how Minecraft { $value } stores this setting, so Ephervex will not write it.
gui-minecraft-compat-unrepresentable-detail = Minecraft { $value } cannot express the chosen value, so Ephervex will not write it there.
gui-minecraft-value-unreadable = That is not a value this setting can take.
gui-minecraft-value-out-of-domain = That value is outside the range this setting allows.
gui-minecraft-value-unset = Not set
gui-minecraft-scale-automatic = Auto
gui-minecraft-percent = { $value }%
gui-minecraft-degrees = { $value } deg
gui-minecraft-readout-named = { $first } ({ $second })
gui-minecraft-width = Width
gui-minecraft-height = Height
gui-minecraft-aspect = Aspect ratio
gui-minecraft-aspect-free = Free
gui-minecraft-aspect-ratio = { $first }:{ $second }
gui-minecraft-aspect-lock = Keep ratio
gui-minecraft-window-open = Show window
gui-minecraft-window-preview-title = Minecraft window size
gui-minecraft-window-preview-readout = { $width } x { $height }
gui-minecraft-window-preview-hint = Resize this window. Width and Height follow its exact size.
gui-minecraft-window-preview-hint-ratio = Resize this window from any edge or corner. It keeps the chosen ratio.
gui-minecraft-window-preview-close = Done
gui-minecraft-preference-fov = Field of view
gui-minecraft-binding-unset = Not set
gui-minecraft-binding-unbound = Unbound
gui-minecraft-mouse-left = Left mouse button
gui-minecraft-mouse-right = Right mouse button
gui-minecraft-mouse-middle = Middle mouse button
gui-minecraft-mouse-numbered = Mouse { $value }
gui-minecraft-binding-conflict = Also bound to { $value }. Minecraft allows this and shows the clash in game.
gui-minecraft-press-input = Press a key or mouse button...
gui-minecraft-capture-refused = Ephervex cannot tell exactly which key that was, so nothing was bound. Try a function key, a modifier, or a mouse button.
gui-minecraft-language-label = { $first } ({ $second })
gui-minecraft-language-label-region = { $first } - { $second } ({ $third })
gui-minecraft-language-absent = Minecraft { $second } does not list { $first }.
gui-minecraft-language-loading = Loading the language list...
gui-minecraft-language-unavailable = The language list could not be resolved. Every other setting stays editable.
gui-minecraft-language-retry = Retry
gui-minecraft-preserved-notice = { $count ->
    [one] 1 Minecraft default was created by a newer Ephervex version and is being preserved.
   *[other] { $count } Minecraft defaults were created by a newer Ephervex version and are being preserved.
}
gui-minecraft-unusable-notice = { $count ->
    [one] 1 configured Minecraft default could not be read and is not applied.
   *[other] { $count } configured Minecraft defaults could not be read and are not applied.
}
gui-minecraft-defaults-saved = Saved the Minecraft defaults.
gui-minecraft-defaults-discarded = Discarded the unsaved Minecraft defaults.
gui-discard-changes = Discard Changes
gui-clear = Clear
gui-java-detail = Runtime discovery and JVM defaults are owned by the application services.
gui-java-detect = Detect installed Java runtimes
gui-java-download = Download a managed runtime when needed
gui-java-memory-min = Minimum memory (MiB)
gui-java-memory-max = Maximum memory (MiB)
gui-java-preferred = Preferred Java executable (leave empty for automatic)
gui-java-refresh = Refresh Runtimes
gui-java-save = Save Java Settings
gui-accounts-detail = Access tokens and refresh credentials are never displayed. Microsoft credentials use the desktop keyring when available.
gui-offline-account-placeholder = Offline player name
gui-add-offline-account = Add Offline Account
gui-microsoft-sign-in = Sign in with Microsoft
gui-no-accounts = No accounts
gui-no-accounts-detail = Add an offline identity for local testing or sign in to launch authenticated Minecraft.
gui-default = DEFAULT
gui-make-default = Make Default
gui-rename = Rename
gui-modal-rename = Rename
gui-rename-detail = Only the name changes. Nothing on disk is named after it, so files, logs and links all stay where they are.
gui-renamed = Renamed to { $value }.
gui-delete = Delete
gui-delete-mod = Delete { $value }
gui-remove-mod = Remove { $value }
gui-modal-delete-mod = Delete Mod
gui-delete-mod-detail = Delete { $value } from the mods folder?
gui-delete-mod-note = You added this JAR yourself, so Ephervex cannot fetch it again. The file is removed from disk.
gui-mod-deleted = Deleted { $value }.
gui-modal-mod-clash = Name Already Taken
gui-mod-clash-detail = The mods folder already has a file called { $value }.
gui-mod-clash-disabled = The one already there is turned off. Replacing it turns the mod back on.
gui-mod-clash-progress = { $value } of { $other }
gui-overwrite = Overwrite
gui-overwrite-all = Overwrite All
gui-skip-all = Skip All
gui-add-local-mods = Add JARs...
gui-choose-mods = Choose mods to add
gui-mod-jars = Minecraft mods
gui-drop-to-add-mods = Drop to add mods
gui-drop-to-add-mods-detail = The JAR files land in this mods folder and the list reloads. Anything that is not a JAR is left alone.
gui-modal-absent-mods = Mods Without Files
gui-absent-mods-note = Keeping them means the copy can fetch them again. Leaving them out makes the copy describe only what it actually has.
gui-copy-with-them = Keep Them
gui-copy-without-them = Leave Them Out
gui-mods-added = { $count ->
    [0] No mods added.
    [one] 1 mod added.
   *[other] { $count } mods added.
}
gui-mods-skipped = { $count ->
    [one] 1 skipped.
   *[other] { $count } skipped.
}
gui-mods-refused = { $count ->
    [one] 1 could not be added.
   *[other] { $count } could not be added.
}
gui-absent-mods-detail = { $count ->
    [one] This instance records 1 mod whose JAR is not in its mods folder. Should the copy record it too?
   *[other] This instance records { $count } mods whose JARs are not in its mods folder. Should the copy record them too?
}
gui-remove = Remove...
gui-provider-detail = Provider configuration remains scoped. Stored secrets are reported only as present or absent.
gui-preferred-mod-catalog = Mod catalog
gui-preferred-mod-catalog-detail = Which catalog the mod browser opens on. Individual searches and installs can still name either one.
gui-preferred-mod-catalog-heading = Preferred catalog
gui-preferred-mod-catalog-note = Applies to browsing from here on. Mods already installed keep the catalog they came from.
gui-preferred-mod-catalog-unrecognized = "{ $value }" is not a catalog this version knows. It is kept as it is; choose one above to browse.
gui-preferred-mod-catalog-saved = Preferred mod catalog saved.
gui-provider-identifiers = Provider identity
gui-provider-identifiers-detail = Overrides are advanced settings. Empty fields use the effective defaults shown below.
gui-microsoft-client-id = Microsoft application Client ID
gui-user-agent = Custom HTTP User-Agent
gui-no-override = No override configured
gui-override = OVERRIDE
gui-effective-default = Effective default: { $value }
gui-effective-value = Effective value: { $value }
gui-reset-default = Reset to Default
gui-client-id-reset = Restored the Ephervex Client ID default.
gui-user-agent-reset = Restored the Ephervex User-Agent default.
gui-save-api-settings = Save API Settings
gui-curseforge = CurseForge
# Shown where this build carries a key of its own, so CurseForge works
# without the user doing anything.
gui-curseforge-detail-default = Set your own CurseForge API key to override the existing Ephervex key.
# Shown where it does not, and CurseForge therefore waits on the user.
gui-curseforge-detail-required = To use the CurseForge part of Ephervex, you need to set your own API key, none is provided by Ephervex itself.
gui-curseforge-api-key = CurseForge API key
gui-curseforge-key-configured = CONFIGURED
gui-curseforge-key-not-configured = NOT CONFIGURED
gui-curseforge-key-default-in-use = DEFAULT
# Shown where nothing is configured and the build ships nothing either.
gui-curseforge-key-missing-detail = Set a key to import CurseForge packs and to fetch CurseForge files. Everything else, Modrinth included, works without one.
gui-curseforge-key-placeholder = Paste your API key
gui-curseforge-key-save = Save Key
gui-curseforge-key-replace = Replace Key
gui-curseforge-key-storage = The key is stored in your system keyring, never in Ephervex's configuration file. A stored key cannot be shown again; replacing it means entering the new one.
gui-curseforge-key-help = Get a CurseForge API key
gui-curseforge-key-saved = Stored the CurseForge API key in the system keyring.
gui-curseforge-key-removed = Removed the CurseForge API key from the system keyring.
gui-curseforge-key-help-opened = Opened the CurseForge API key page in your browser.
gui-curseforge-key-help-failed = Could not open a browser. Visit the CurseForge support site to apply for an API key.
gui-storage-detail = Temporary instances use the operating system's temporary directory. Ephervex does not delete them when Minecraft exits; the operating system owns cleanup.
gui-storage-root = Persistent instance root (leave empty for the XDG default)
gui-save-storage-root = Save Storage Root
gui-persistent-storage = Persistent environments
gui-persistent-storage-detail = Choose where new persistent instances and servers are stored. Existing environments are not moved.
gui-choose-folder = Choose Folder
gui-choose-persistent-root = Choose the persistent instance root
gui-custom-location = CUSTOM
gui-default-location = DEFAULT
gui-effective-location = Current location: { $value }
gui-default-location-path = Default location: { $value }
gui-storage-change-future-only = Changing this location affects future persistent environments. Existing instance and server directories stay where they are.
gui-watched-download-folders = Watched download folders
gui-watched-download-folders-detail = When a mod author disables third-party downloads, Ephervex watches these folders for a manually downloaded copy and matches it by hash. Defaults to your Downloads folder until you add or remove one here.
gui-watched-download-folders-empty = No folders configured. Ephervex watches your Downloads folder by default.
gui-add-watched-folder = Add Folder
gui-choose-watched-folder = Choose a folder to watch
gui-artifact-cache = Reusable artifact cache
gui-artifact-cache-detail = Downloaded mods, pack files, loader installers, and runtime archives are kept so importing the same pack twice costs no network. Instances and servers own their own copies, so clearing this only costs a re-download.
gui-artifact-cache-usage = { $first } of { $second } used
gui-artifact-cache-usage-approximate = at least { $first } of { $second } used
gui-artifact-cache-limit = Limit
gui-artifact-cache-measuring = Measuring cache size...
gui-artifact-cache-unlimited = Unlimited
gui-artifact-cache-custom = Custom
gui-artifact-cache-custom-value = Size
gui-artifact-cache-over-budget = OVER BUDGET
gui-artifact-cache-cleanup-note = When usage passes the limit, the oldest downloads are removed until about four fifths of the limit is in use. Downloads in progress and artifacts an operation is still using are never removed.
gui-clear-artifact-cache = Clear Reusable Cache
gui-clearing-artifact-cache = Clearing the reusable artifact cache...
gui-artifact-cache-cleared = Freed { $value } from the reusable artifact cache.
gui-artifact-cache-cleared-partly = Freed { $value }; { $remaining } could not be removed and is still cached.
gui-artifact-cache-already-empty = The reusable artifact cache is already empty.
gui-artifact-cache-limit-saved = Saved the reusable artifact cache limit.
gui-save-artifact-cache-limit = Save Limit
gui-logo-accessible = Ephervex application logo
gui-version = Version { $value }
gui-apache-license = Licensed under the Apache License 2.0
gui-view-ephervex-license = View Ephervex License
gui-built-with-slint = Built with Slint
gui-third-party-software = Third-Party Software
gui-third-party-detail = Ephervex depends on open-source software with its own license terms.
gui-third-party-licenses = Third-Party Licenses
# How far through a stage that counts in whole things rather than bytes.
gui-operation-item-count = { $current } of { $total }
gui-active-operations = { $count ->
    [one] 1 active operation
   *[other] { $count } active operations
}
gui-no-active-tasks = No active tasks
gui-activity-minecraft-running = Minecraft is running
gui-kill-instance = Kill Instance
gui-kill-instance-requested = Stopping Minecraft.
gui-kill-instance-unavailable = Nothing this launcher started is running for that instance.
gui-dismiss-notification = Dismiss notification
gui-modal-new-instance = New Instance
gui-modal-import = Import Modpack
gui-modal-new-server = New Server
gui-modal-install-mod = Install Mod
gui-modal-force-kill = Force Kill Server
gui-modal-apache-license = Apache License 2.0
gui-create-instance-detail = Create an exact client environment. Temporary storage is the default.
gui-name = Name
gui-minecraft-release = Minecraft release
gui-loader-version-placeholder = Exact loader version
gui-store-persistently = Store persistently
gui-persistent-root = Persistent instance root
gui-temporary-root = Operating system temporary directory
gui-create-instance = Create Instance
gui-import-detail = Supported archives are inspected and imported through Ephervex application services. Imported commands and unsafe paths are never executed.
gui-modpack-archive = Modpack archive
gui-modpack-placeholder = Choose a .mrpack or supported .zip
gui-drop-to-import = Drop to import
gui-drop-to-import-detail = The archive is inspected by its contents, not by its name.
gui-browse = Browse...
gui-instance-name-optional = Instance name (optional)
gui-import-create-server = Also create a linked server
gui-import-create-server-detail = Built from the same pack once the instance is in place. It is not started.
gui-detected-pack-name = Use the detected pack name
gui-create-linked-server-detail = Create and materialize a server linked to the selected client's retained recipe.
gui-create-standalone-server-detail = Create and install a standalone exact server.
gui-eula-create-warning = EULA consent is not implied. Starting is blocked until you explicitly review and accept it.
gui-create-server = Create Server
gui-mod-plan-compatible = Ephervex core checked compatibility with the selected instance.
gui-mod-plan-install = { $count ->
    [one] 1 file will be installed
   *[other] { $count } files will be installed
}
gui-mod-plan-required = { $count ->
    [one] 1 required dependency
   *[other] { $count } required dependencies
}
gui-mod-plan-existing = { $count ->
    [one] 1 matching managed mod is already installed
   *[other] { $count } matching managed mods are already installed
}
gui-mod-plan-optional = { $count ->
    [one] 1 optional dependency is available but not selected
   *[other] { $count } optional dependencies are available but not selected
}
gui-mod-plan-warnings = { $count ->
    [one] Warning: the plan has 1 compatibility note.
   *[other] Warning: the plan has { $count } compatibility notes.
}
gui-install = Install
gui-mod-already-present = That mod and everything it requires are already installed.
gui-install-required = Install Required Set
gui-eula-detail = Minecraft's server EULA is a legal agreement. Ephervex records global consent only after this explicit action; starting a server never accepts it for you.
gui-open-minecraft-eula = Open Minecraft EULA
gui-eula-confirm = By choosing Accept, you confirm that you have reviewed and agree to the Minecraft EULA.
gui-revoke-consent = Revoke Consent...
gui-accept-eula = Accept EULA
gui-force-kill-detail = Force killing "{ $value }" may lose unsaved world data. Use normal Stop whenever the server responds.
gui-this-server = this server
gui-force-kill-action = Force Kill
gui-third-party-unavailable = An audited third-party license report is not bundled in this build yet. Ephervex does not invent dependency license information. The repository's Cargo metadata and lockfile remain the source inputs for a future generated license audit.
gui-slint-attribution = Slint attribution is available directly in About & Licenses through Slint's official AboutSlint widget.
gui-close = Close
gui-clipboard-copied = Copied to the clipboard.
gui-clipboard-unavailable = The desktop clipboard is unavailable.
gui-server-property-values-invalid = Correct the maximum-player, difficulty, or game-mode value before saving.
gui-digits-only = Use digits only.
gui-player-count-minimum = Use at least 1 player.
gui-java-memory-digits-only = Java memory values must contain digits only.
gui-java-memory-range-invalid = Java memory must be greater than zero, and the minimum cannot exceed the maximum.
gui-diagnostic-unavailable = Diagnostic unavailable
gui-diagnostic-disappeared = The selected diagnostic disappeared or is unavailable.
gui-available = Available
gui-diagnostic-search-matches = { $count ->
    [one] Search found 1 match in the loaded window.
   *[other] Search found { $count } matches in the loaded window.
}
gui-diagnostic-window-lossy = Loaded a bounded window; invalid bytes were replaced.
gui-diagnostic-window-bounded = Loaded a bounded window of a larger file.
gui-diagnostic-file-lossy = Loaded the file; invalid bytes were replaced.
gui-diagnostic-loaded = Diagnostic loaded.
gui-diagnostic-search-window-matches = { $count ->
    [one] Search found 1 match in this window.
   *[other] Search found { $count } matches in this window.
}
gui-client-already-running = The client is already running.
gui-port-required = Enter a port from 1 to 65535.
gui-port-digits-only = Ports contain digits only.
gui-port-range = Port must be from 1 to 65535.
gui-ip-required = Enter an IPv4 or IPv6 address.
gui-ip-invalid = Enter a valid IPv4 or IPv6 address.
gui-network-access-required = Choose a network access mode.
gui-background-service-failed = The background service could not start: { $value }
gui-background-runtime-failed = The background runtime could not start: { $value }
gui-choose-modpack-archive = Choose a modpack archive
gui-modpack-archives = Modpack archives
gui-choose-server-pack = Choose server files
gui-clear-server-pack = Clear the chosen server files
gui-server-pack-archives = Server file archives
gui-server-pack = Server files (optional)
# Shown under the file picker in the create-server dialog. Says what the archive
# decides and what Ephervex installs regardless, because both halves are
# surprising if you only know one of them.
gui-server-pack-detail = If the pack publishes its own server files, choose that archive and it becomes the server's content as it is. Ephervex still installs the loader and its libraries, and never runs the archive's installer or start script.
gui-import-pack-header = { $first } - { $second }
gui-import-author = by { $value }
gui-minecraft-version-label = Minecraft { $value }
gui-import-required-count = { $count ->
    [one] 1 required file
   *[other] { $count } required files
}
gui-import-optional-count = { $count ->
    [one] 1 optional file
   *[other] { $count } optional files
}
gui-import-analyzing = Reading the archive...
gui-import-resolving = Resolving files from the provider...
gui-import-resolution-unavailable = The import worker could not start to resolve provider files.
gui-import-open-providers = Open Providers & APIs
gui-import-blocked-detail = These files' authors do not allow automatic downloads. Download each one manually and place it in a watched folder, then Skip or wait for it to be found.
gui-import-open-watched-folders = Manage Watched Folders
gui-import-blocked-opened = Opened the file's download page in your browser.
gui-import-blocked-open-failed = Could not open a browser.
gui-import-duplicate-detail = This pack lists some mods more than once. Ephervex kept the file that fits and left the other one out automatically.
gui-import-duplicate-kept = Kept { $first } over { $second }
gui-import-duplicate-matches-version = Matches this pack's Minecraft version; the other file does not.
gui-import-duplicate-newer = Neither file names this pack's Minecraft version, so the more recently published one was kept.
gui-created-entity = Created "{ $value }".
gui-imported-pack = Imported "{ $value }".
gui-folder-unavailable = Folder unavailable.
gui-opened-path = Opened { $value }.
gui-unknown-author = Unknown author
gui-compatible = Compatible
gui-installed-managed-mods = { $count ->
    [one] Installed 1 managed mod with required dependencies.
   *[other] Installed { $count } managed mods with required dependencies.
}
gui-stop-requested = Stop requested.
gui-restart-requested = Restart requested.
gui-force-kill-requested = Force kill requested.
gui-command-sent = Command sent.
gui-server-properties-saved = Saved server properties.
gui-eula-consent-recorded = Minecraft Server EULA consent recorded.
gui-eula-consent-revoked = Minecraft Server EULA consent revoked.
gui-eula-browser-opened = Opened the Minecraft EULA in your browser.
gui-eula-browser-failed = The browser could not be opened. Visit https://aka.ms/MinecraftEULA.
gui-offline-account-added = Added offline account "{ $value }".
gui-default-account-set = "{ $value }" is now the default account.
gui-account-removed = Removed account.
gui-provider-settings-saved = Saved provider settings.
gui-storage-root-saved = Saved the persistent instance root.
gui-java-settings-saved = Saved Java settings.
gui-operation-canceled = Operation canceled.
gui-cancellation-requested = Cancellation requested.
gui-operation-not-cancellable = That operation has already finished or cannot be canceled.
gui-background-operation-failed = A background operation could not start: { $value }
gui-signin-browser-failed = Open the sign-in URL shown in launcher diagnostics.
gui-device-code-prompt = Open { $first } and enter code { $second }.
gui-signin-stage = Sign-in: { $value }
gui-signed-in = Signed in as "{ $value }".
gui-starting-minecraft = Starting Minecraft for "{ $value }".
gui-server-running = "{ $value }" is running.
gui-credential-stored = Credential stored securely
gui-signin-required = Sign-in required
gui-no-credential = No credential
gui-unknown-time = Unknown time
gui-just-now = Just now
gui-minutes-ago = { $count } min ago
gui-hours-ago = { $count } h ago
gui-eula-not-accepted = Not accepted. Server starts remain blocked.
gui-eula-accepted-at = Accepted explicitly on { $value }.
gui-eula-malformed = The recorded consent is invalid and must be reviewed again.
gui-warning-offline-exposed = This offline-mode server is exposed beyond localhost. Authentication is not enforced.
gui-warning-port-unavailable = The configured address { $first }:{ $second } is already in use or unavailable. Stop the conflicting service or choose another port.
gui-warning-server-generic = The server reported a warning. Review its console and logs before continuing.
gui-state-stopped = Stopped
gui-state-running = Running
gui-state-unknown = Unknown
gui-state-orphaned = Orphaned
gui-state-needs-recovery = Needs recovery
gui-state-queued = Queued
gui-state-canceled = Canceled
gui-state-completed = Completed
gui-state-failed = Failed
gui-state-ready-to-start = Ready to start
gui-state-controlled = Controlled by Ephervex
gui-state-process-may-run = A process may still be running
gui-state-orphaned-detail = Java is running without its supervisor
gui-state-needs-recovery-detail = Recorded processes have exited
gui-standalone = Standalone
gui-linked-client = Linked client
gui-log = Log
gui-crash-report = Crash report
gui-player-versus-player = Player versus player
gui-use-allowlist = Use allowlist
gui-activity-launch-minecraft = Launch Minecraft
gui-activity-launching-minecraft = Launching Minecraft
gui-activity-exited-cleanly = Exited cleanly
gui-activity-game-error = Game exited with an error
gui-activity-preparing-environment = Preparing exact environment
gui-activity-starting-server = Starting server
gui-activity-validating-server = Validating runtime, EULA, and network
gui-activity-server-run = Server run
gui-activity-server-exited = Server exited
gui-activity-connected-to = Connected to { $value }
gui-launch-and-connect = Launch & Connect

# Linked servers, shown inside the mod flows they belong to rather than on a
# page of their own.
gui-linked-servers = Linked servers
gui-linked-exact-release = Every selected server receives { $value }.
gui-linked-support-required = This mod requires a server-side install.
gui-linked-support-optional = Server support is optional for this mod.
# "Not stated" is not "not needed": the author declared nothing, and Ephervex
# says so instead of deciding for them.
gui-linked-support-unknown = This mod does not state whether it supports servers.
gui-linked-support-unsupported = This mod is client-only, so no server is offered it.
gui-linked-installs = { $count ->
    [one] 1 mod
   *[other] { $count } mods
}
gui-linked-removes = { $count ->
    [one] 1 mod removed
   *[other] { $count } mods removed
}
gui-linked-dependencies-unprovable = Dependencies could not be confirmed, so none will be collected.
# One word for every reason a server cannot take part. The reason itself is the
# sentence beside it, so the row reads as one state plus one explanation.
gui-linked-unavailable = Unavailable
gui-modal-apply-mod = Apply Mod Change
gui-linked-apply = Apply
gui-modal-mod-result = Result
gui-linked-result-client = This instance
gui-linked-applied = Done
gui-linked-skipped = Skipped
gui-linked-failed = Failed
gui-linked-partial = Some targets were not changed. Each one is listed with its own result.
gui-linked-all-applied = Every selected target was changed.
