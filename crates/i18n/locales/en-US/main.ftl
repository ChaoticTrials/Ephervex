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

reconcile-index-repaired = { $count ->
        [one] Corrected { $count } index entry from the metadata stored with its files.
       *[other] Corrected { $count } index entries from the metadata stored with their files.
    }

reconcile-restores-rolled-back = { $count ->
        [one] Rolled back { $count } interrupted snapshot restore to the state it had before the restore.
       *[other] Rolled back { $count } interrupted snapshot restores to the state they had before the restores.
    }

reconcile-interrupted-hosts = { $count ->
        [one] { $count } instance or server has unfinished changes left in its directory.
       *[other] { $count } instances or servers have unfinished changes left in their directories.
    }
.action = Ephervex left those files untouched. Check what they hold and remove them once you are sure you no longer need them.

reconcile-persistence-unhealthy = { $count ->
        [one] { $count } instance or server has metadata that cannot be read.
       *[other] { $count } instances or servers have metadata that cannot be read.
    }
.action = Ephervex kept the record from its index, but will not launch or change it until its metadata is readable again. Restore it from a backup, or delete the instance or server if it is gone for good.

reconcile-client-conflicts = { $count ->
        [one] { $count } instance is claimed by more than one directory.
       *[other] { $count } instances are claimed by more than one directory.
    }
.action = Ephervex did not choose between the directories. Move or delete the copies you do not want, then run reconcile again.

reconcile-server-conflicts = { $count ->
        [one] { $count } server is claimed by more than one directory.
       *[other] { $count } servers are claimed by more than one directory.
    }
.action = Ephervex did not choose between the directories. Move or delete the copies you do not want, then run reconcile again.

reconcile-nothing-to-do = Everything in the index matches what is on disk.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-persistent-root-missing-detail = { $name } expected its files at { $path }.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-unfinished-instance-detail = { $name } was never finished at { $path }.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-attention-detail = { $name } at { $path }

# $id is an instance or server identifier, $paths is a comma-separated list of
# filesystem paths. Untranslated.
reconcile-conflict-detail = { $id } is claimed by { $paths }.

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
error-pack-override-shadows-declared-file = The pack's override files replace { $path } on only some sides.
.action = The pack ships different bytes for one mod path depending on the side. Ephervex will not pick one; ask the pack author to fix the pack.
error-pack-recipe-destination-occupied = A selected recipe file would overwrite part of the installed server.
.action = Fix the retained recipe path collision; Ephervex will not overwrite the base server silently.

## Configuration errors 

error-config-unreadable = The configuration file could not be read.
.action = Check that { $path } exists and is readable, or delete it to start from defaults.

error-config-invalid = The configuration file is not valid TOML.
.action = Fix the syntax at { $path }, or delete the file to start from defaults.

error-config-unwritable = The configuration file could not be saved.
.action = Check that { $path } is writable and that the filesystem is not full.

error-config-busy = Another Ephervex window or command is saving settings right now.
.action = Try again in a moment. If it keeps happening, check which process holds { $path }.

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

error-registry-host-busy = That instance or server is being started or changed by another operation, such as a snapshot restore, a move to another storage location, or a deletion.
.action = Wait for the other operation to finish, then try again.

error-registry-host-not-claimed = That change was not reserved for this instance or server.

error-registry-recipe-busy = Another { app-name } window or command is still changing the mods of that instance or server.
.action = Wait for it to finish, then try again.

error-registry-restore-unfinished = That instance has an interrupted snapshot restore that has not been recovered yet.
.action = Restart { app-name } so it can recover the restore, then try again.

error-registry-host-guard-unusable = The lock that keeps changes to one instance or server apart could not be used.
.action = Check that { $path } can be written, then try again.

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

error-minecraft-cached-version-metadata-damaged = The copy of that Minecraft version's metadata saved on this computer is damaged.
.action = Nothing was downloaded to cause this. Launch the version or create an instance with it again so Ephervex fetches a fresh copy, or delete the file from the metadata cache.

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
error-java-managed-canceled = Installing the Java runtime was canceled.

error-java-managed-runtime-unusable = The downloaded Java runtime could not be started.
.action = Ephervex did not use it. Try again, or install a Java runtime through your distribution's package manager.

error-java-managed-install-blocked = Something Ephervex cannot safely replace is in the way of this Java runtime.
    .action = Delete { $path } and try again.

error-java-managed-cache-key-invalid = The Java runtime could not be given a safe location in the cache.

error-java-not-managed = That Java runtime is not one Ephervex installed, so it cannot be removed here.
    .action = Remove it the way it was installed: through your distribution's package manager, SDKMAN, or whatever placed it.

error-java-io = A file needed for the Java runtime could not be written.
    .action = Check free space and permissions on { $path }.

error-java-settings-invalid = Java memory settings are invalid: minimum { $minimum } MiB ({ $minimum_layer ->
        [override] set by the override
       *[global] from the global settings
    }), maximum { $maximum } MiB ({ $maximum_layer ->
        [override] set by the override
       *[global] from the global settings
    }).
    .action = Use positive values and make the maximum at least the minimum. When an override is involved, change it or the global settings it combines with.

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
.action = Start the sign-in again and finish it before the time limit runs out.

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

# No Java Edition profile was found and the entitlement store did not answer, so
# nothing is known about ownership. This must not advise buying the game.
error-auth-game-ownership-unconfirmed = Ephervex could not confirm whether this account owns Minecraft: Java Edition.
.action = The Minecraft entitlement service did not respond. Try again in a few minutes.

error-auth-profile-unavailable = Your Minecraft profile could not be read.
.action = Check your network connection and try again.

error-auth-profile-malformed = Minecraft returned a profile Ephervex could not read.

error-auth-secret-storage-unavailable = Ephervex cannot reach secure storage for your sign-in on this computer.
.action = Install and unlock a keyring such as GNOME Keyring or KWallet and try again, or sign in for this session only. Ephervex will never write your credentials to a file.

error-auth-credential-change-busy = Another Ephervex window or command is changing sign-in credentials right now.
.action = Try again in a moment. If it keeps happening, check which process holds { $path }.

error-auth-credential-change-unavailable = Ephervex cannot safely change your sign-in right now.
.action = Check that { $path } can be created and written, then try again.

error-auth-credential-store-failed = The system keyring could not be used.
.action = Check that your keyring is unlocked, then try again.

error-auth-credential-blank = No value was entered.
.action = Paste the credential and try again. Nothing was changed.

error-auth-not-signed-in = No Microsoft account is signed in.
.action = Sign in with your Microsoft account before launching Minecraft.

error-auth-account-mismatch = The saved Microsoft sign-in belongs to a different account.
.action = Sign in again to replace it.

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
auth-logged-out-credential-deleted = Signed out and deleted the saved credential.
# Shown when the keyring could not be reached: the credential may still be there.
auth-logged-out-credential-pending = Signed out. The keyring could not be reached, so any saved credential will be deleted once it is available.
auth-logged-out-no-credential = Signed out. There was no saved credential to delete.
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
# Shown when the keyring could not be reached: the credential may still be there.
account-removed-credential-pending = Removed the account { $name }. The keyring could not be reached, so any saved credential will be deleted once it is available.

# An offline account has no credential at all - this is not a missing or
# degraded one. Offline accounts have no credential material to store.
auth-storage-none-needed = No credential needed
auth-storage-not-saved = Not saved
auth-storage-unknown = Unknown, secret storage is unavailable

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

# $lines is how many of Minecraft's output lines were not shown because they
# arrived faster than they could be read.
launch-output-dropped = { $lines ->
        [one] 1 line of Minecraft output was skipped.
       *[other] { $lines } lines of Minecraft output were skipped.
    }

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
operation-stage-preparing-environment = Preparing exact environment
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

error-launch-lock-busy = That instance is already being started or stopped.
.action = Try again in a moment. If it keeps happening, check which process holds { $path }.

error-launch-lock-lost = This launch no longer holds its instance.
.action = Another launch or a force stop took over the instance while this one was preparing, so the game was not started. Launch again.

error-launch-lock-unusable = Ephervex could not claim the instance for launching.
.action = Check permissions on { $path }.

error-launch-id-invalid = That launch handle cannot be used.
.action = A handle must be one or more letters, digits, "-" or "_".

error-launch-invalid-version-metadata = That version's metadata does not describe something Ephervex can launch.
.action = The loader metadata may not match its declared Minecraft parent. Ephervex refuses rather than constructing an approximate launch.

# $loader is a product name and stays untranslated.
error-launch-loader-not-supported = This build does not support the { $loader } loader.
.action = Ephervex refuses to substitute Vanilla or another loader because that would not reproduce the selected instance.

error-launch-incompatible-native-library = This version selects native libraries that do not support your Linux CPU architecture.
.action = Run this instance on a compatible machine. Ephervex keeps the selected versions and does not substitute native downloads.

error-launch-missing-native-artifact = A required native library is missing from this version's download information.
.action = Report the incomplete download information to the version's provider.

error-launch-download-failed = A file Minecraft needs could not be downloaded.
.action = Check your network connection and try again. Nothing partial or unverified is kept, so a retry starts clean.

error-launch-artifact-mismatched = A file Minecraft needs arrived with the wrong contents.
.action = Ephervex refused it rather than launching with a file it cannot vouch for. Nothing unverified was kept. If it happens again the source is serving the wrong file, and retrying will not change that.

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
error-loader-installer-library-conflict = The loader installer declares two different files for library { $library }.
.action = Ephervex refused the installer instead of choosing one. Choose a different loader build.
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
linked-target-server-not-installed = This server has no installation yet.
linked-target-no-longer-linked = This server is no longer linked to this instance.
linked-target-provider-unavailable = The mod catalog could not be reached, so this server could not be checked.
linked-target-state-unavailable = This server's recorded mods could not be read.
linked-target-isolation-active = This server has an isolation session in progress, so its mods cannot change.
linked-target-pack-owned = This server's pack declares this mod.
linked-target-server-dependency = Another mod on this server requires this one.
linked-target-moves-another-mod = This release would also move another mod on this server that its pack declares, that was chosen directly, or that another mod needs at its current release.

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
error-mod-dependency-not-movable = That change would also move a mod that the pack declared, that you chose, or that another mod needs at its current release. Change that mod's version first.
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
# $name is the linked server's name.
error-linked-server-running = The linked server { $name } is running.
.action = Nothing was removed. Stop it first, or delete with the option to stop linked servers.
# $name is the linked server's name.
error-linked-server-unstoppable = The linked server { $name } may still be running, and Ephervex cannot stop it.
.action = Nothing was removed. Check it on its server page, then try again.
# $name is the linked server's name.
error-linked-server-not-stopped = Ephervex could not stop the linked server { $name }.
.action = Nothing was removed. Check the server on its server page, then try again.
# $pid is the process that holds the instance.
error-instance-running = That instance is launching or running (process { $pid }).
.action = Wait for the launch to finish and close the game, then try again.
error-instance-already-persistent = That instance is already stored permanently.
.action = Only a temporary instance can be made persistent. Delete it when you no longer need it.
error-server-not-linked = That server does not belong to this instance.
error-mod-file-missing = That mod's file is not installed, so it cannot be enabled or disabled. Repair the instance to fetch it again.
error-mod-file-not-toggled = That mod's file could not be renamed. Check that the mods folder is writable.
.action = Mods with no identity have to be managed by hand.
error-mod-spelling-occupied = That mod has both an enabled and a disabled file, so switching it would replace one of them. Neither file was changed.
.action = Delete or rename the file you do not want in the mods folder, then try again.
error-mod-not-hand-placed = Ephervex installed that mod, so it is removed rather than deleted.
.action = Open the mod and use Remove, which also takes care of what it depended on.
error-mod-file-not-deleted = That mod's file could not be deleted. Check that the mods folder is writable.
error-diagnostic-bundle-not-written = The diagnostic bundle could not be written.
    .action = Check that there is room and that you can write to { $path }.
error-diagnostic-bundle-overwrites-source = The bundle cannot be written over one of the diagnostic files it collects.
    .action = Choose a different file for the bundle than { $path }.
error-mod-file-not-added = That file could not be copied into the mods folder. Check that there is room and that the folder is writable.

error-repair-host-running = This host cannot be repaired while it is running.
.action = Stop it first. A repair moves jars the game may have open.
error-repair-isolation-active = This host cannot be repaired while an isolation session is active.
.action = Finish or cancel the isolation session first. A repair would put back mods the current round turned off.
error-change-host-running = This host cannot be changed while it is running.
.action = Stop it first, then apply the change again.
error-change-isolation-active = This host's mods cannot be changed while an isolation session is active.
.action = Finish or cancel the isolation session first.
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
local-mod-recorded-by-pack = The imported pack declares a mod with this file name, so a JAR cannot replace it. Enable independent mod changes for this instance to move the mod to another release.

# Why a mod belongs to an instance. Kept distinct because they are different
# things to look at and, later, different things to change.
mod-origin-pack-declared = From pack
# A jar the pack ships inside itself rather than pointing at a catalog for.
# Named apart from a declared one because the two behave differently: this one
# has no published release behind it and reappears when the pack is reapplied.
mod-origin-pack-override = Override
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

error-pack-limit-exceeded = The pack is larger than Ephervex is willing to unpack, or lists more files or download addresses than it will fetch.
.action = Ephervex stopped before the pack could fill the disk or memory, or trigger an unreasonable number of downloads. The limits are deliberately generous.

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

error-pack-file-hashes-missing = One of the pack's files is missing a checksum Ephervex requires.
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

error-storage-full = The storage device is full.
.action = Free space on the device that holds the instance or Ephervex data, then try again.

error-snapshot-host-running = { $host } is running, so its state cannot be captured or restored safely.
.action = Stop the client or server, then try again.
error-snapshot-host-busy = That instance or server is being started or changed by another operation, such as a snapshot restore, a move to another storage location, or a deletion.
.action = Wait for the other change to finish, then try again.
error-snapshot-isolation-active = A snapshot cannot be restored while an isolation session is active.
.action = Finish or cancel the isolation session first. A restore would replace the mods the session is testing.
error-snapshot-not-found = Snapshot { $snapshot } no longer exists.
error-snapshot-host-mismatch = Snapshot { $snapshot } belongs to { $found }, not { $expected }.
.action = Restore it only from the host that created it.
error-snapshot-label-invalid = The snapshot label is empty, too long, or contains a control character.
error-snapshot-unreadable = Snapshot state could not be read.
.action = Check permissions at { $path }.
error-snapshot-unwritable = Snapshot state could not be written.
.action = Check permissions and free space at { $path }.
error-snapshot-invalid = Snapshot metadata is invalid.
.action = Review { $path }. Ephervex left the snapshot untouched.
error-snapshot-version-newer = This snapshot was written by a newer Ephervex format ({ $found }, supported { $supported }).
.action = Update Ephervex before using { $path }.
error-snapshot-canceled = The snapshot operation was canceled.
gui-snapshots = Snapshots
gui-snapshots-detail = Local safety copies of the complete development state. Snapshots are not pack exports.
gui-snapshot-create = Create snapshot
gui-snapshot-restore = Restore snapshot
gui-snapshot-rename = Rename snapshot
gui-snapshot-delete = Delete snapshot
gui-snapshot-label = Label
gui-snapshot-create-detail = Capture the game directory, retained source, and exact host metadata as they are now.
gui-snapshot-local-note = The snapshot stays inside this client or server and is retained until you delete it.
gui-snapshot-restore-detail = Restore "{ $value }"?
gui-snapshot-replacement-note = Files created since this snapshot will be removed. Restore replaces the captured state; it does not merge files.
gui-snapshot-safety-copy = Capture the current state first
gui-snapshot-rename-detail = Change the label shown for this snapshot. Its contents and identity do not change.
gui-snapshot-delete-detail = Permanently delete "{ $value }"?
gui-snapshot-delete-note = This removes the retained safety copy and cannot be undone.
gui-snapshot-none = No snapshots yet
gui-snapshot-none-detail = Capture this host before changing a reproduction or testing a risky fix.
gui-snapshot-total = { $value } apparent total
gui-snapshot-copy-note = At least one snapshot used a full byte copy because this filesystem did not support reflinks.
gui-snapshot-copy = Full copy
gui-snapshot-reflink = Reflink
gui-snapshot-origin-manual = Manual
gui-snapshot-origin-before-restore = Before restore
gui-snapshot-origin-before-isolation = Before isolation

# Dependency-aware mod isolation.
error-isolation-host-running = { $host } is running. Stop it before changing an isolation round.
error-isolation-already-active = { $host } already has an isolation session. Resume or cancel it first.
error-isolation-not-active = { $host } has no isolation session to resume.
error-isolation-unrecorded = { $count } mods have refreshable dependency information that has not been recorded. Refresh dependencies or explicitly accept reduced confidence.
error-isolation-missing-dependencies = { $count } required dependencies are missing. Repair or install them before isolation.
error-isolation-not-awaiting-answer = This isolation session is not waiting for a round answer.
error-isolation-not-applied = The mods of { $host } no longer match the current isolation round. Resume the session to reapply the round, then test it again.
error-isolation-not-finished = The isolation search has not reached a result yet.
error-isolation-snapshot-in-use = Snapshot { $snapshot } is the safety snapshot of an active isolation session. Finish or cancel the session before deleting it.
error-isolation-host-mismatch = This isolation session belongs to { $found }, not { $expected }.
error-isolation-unreadable = Isolation state could not be read.
error-isolation-unwritable = Isolation state could not be written.
error-isolation-invalid = Isolation state is invalid.
error-isolation-version-newer = This isolation session uses format { $found }, but this Ephervex supports through { $supported }.
operation-starting-isolation = Starting mod isolation
operation-stage-planning-isolation = Calculating the first test configuration
operation-stage-snapshot-isolation = Creating the safety snapshot
operation-stage-applying-isolation = Applying the first mod setup
operation-canceling-isolation = Restoring the pre-isolation state
gui-isolation-title = Isolate Mods
gui-isolation-tab = Isolation
gui-isolation-active = Mod isolation is active
gui-isolation-detailed-view = Detailed view
gui-isolation-start = Start isolation
gui-isolation-start-setup = Set up isolation
gui-isolation-finish = Finish
gui-isolation-cancel = Cancel isolation
gui-isolation-undo = Undo answer
gui-isolation-empty-detail = Set up a dependency-aware isolation session for this host.
gui-isolation-setup = Isolation setup
gui-isolation-candidates = Candidate mods
gui-isolation-search = Search candidate mods
gui-isolation-no-candidates = No candidates match
gui-isolation-no-candidates-detail = Clear the search to see the complete candidate list.
gui-isolation-setup-summary = { $count } candidate roots can be tested.
gui-isolation-pinned-detail = Pin mods that must remain enabled in every test. Their required dependencies stay enabled too.
gui-isolation-missing = Required dependencies are missing. Continuing will make the result unproven.
gui-isolation-accept-missing = Ignore missing dependencies and continue
gui-isolation-unrecorded = { $count } mods have incomplete dependency information that Ephervex can refresh:
gui-isolation-unrecorded-detail = Ephervex can still run the search, but it cannot account for relationships it does not know about. The result may leave related mods out.
gui-isolation-accept-unrecorded = Run anyway with the dependency information available now
gui-isolation-start-blocked-running = Start is unavailable while this host is running. Stop it first.
gui-isolation-start-blocked-missing = Start is unavailable until you choose to ignore the missing dependencies, or install them.
gui-isolation-start-blocked-unrecorded = Start is unavailable until you choose to run with the dependency information available now, or refresh it from the Mods tab.
gui-isolation-unidentifiable = { $count } mods cannot be identified. They will be tested as independent candidates, so unknown relationships may be left out of the result:
gui-isolation-linked-servers = { $count } linked servers will not be changed by this client session.
gui-isolation-linked-unchanged = Linked servers are not changed. Run a separate isolation session on a server when needed.
gui-isolation-round-summary = Waiting for an answer to round { $round }.
gui-isolation-round = Round { $value }
gui-isolation-granularity = Testing { $value } groups in this pass
gui-isolation-test-instruction = This is the exact configuration to launch and test. Launching is allowed while the session waits for your answer.
gui-isolation-enabled = { $count } enabled
gui-isolation-disabled = { $count } disabled
gui-isolation-issue-present = Still failing
gui-isolation-issue-gone = Issue gone
gui-isolation-history = Answer history
gui-isolation-history-entry = Round { $round }, { $answer }, { $kind }, { $count } enabled
gui-isolation-reduced-confidence = Some dependency relationships were unknown during this search. The remaining set is useful for testing, but it does not prove that other related mods are uninvolved.
gui-isolation-contradiction = An earlier answer and this one cannot both be right.
gui-isolation-contradiction-detail = Undo the latest answer, test the restored configuration again, and then continue.
gui-isolation-ready = Result ready
gui-isolation-ready-summary = The smallest tested candidate set is ready to review and finish.
gui-isolation-one-minimal = Removing any one remaining candidate stopped the issue. This result is one-minimal, but another set with fewer mods may also reproduce it.
gui-isolation-smallest = The search reached the smallest candidate set it could test.
gui-isolation-remaining-candidates = Remaining testable candidates
gui-isolation-remaining-candidates-detail = These are the candidate roots left in the smallest configuration Ephervex tested while the issue was still present.
gui-isolation-no-remaining-candidates = No removable candidate roots remain in this tested configuration.
gui-isolation-pinned-result = Pinned throughout
gui-isolation-pinned-result-detail = These mods, and the mods they require, stayed enabled in every round and were never tested for removal. They may still be involved.
gui-isolation-pinned-dependency = { $path }, required by a pinned mod
gui-isolation-result-dependencies = Required by the remaining candidates
gui-isolation-result-dependencies-detail = These stay enabled only because a remaining candidate requires them. Where they could be tested without it, the issue did not reproduce.
gui-isolation-finish-snapshot = Finishing keeps this configuration enabled and retains the pre-isolation safety snapshot.
gui-isolation-finished = Isolation finished
gui-isolation-result-summary = { $count } testable candidate roots remain in the smallest tested set.
gui-isolation-finished-detail = The tested configuration remains enabled. The retained safety snapshot is available from the Snapshots tab.
gui-isolation-canceled = Isolation canceled. The pre-isolation state was restored.
gui-isolation-busy-answer = Recording the answer and applying the next mod setup
gui-isolation-busy-undo = Restoring the previous round's mod setup
gui-isolation-busy-finish = Finishing isolation and keeping the tested setup
gui-isolation-busy = Isolation is still changing this host's mods. Try again once that finishes.
gui-snapshot-busy = A snapshot of this host is still being taken or restored. Try again once that finishes.
gui-repair-busy = A repair is still replacing this host's files. Try again once it finishes.
gui-fix-busy = A fix is still changing this host. Try again once it finishes.
gui-mod-change-busy = Another change to this host's mods is still running. Try again once it finishes.
gui-loader-change-busy = This instance's loader is still being changed. Try again once that finishes.
gui-isolation-busy-recover = Reapplying the current round's mod setup
gui-isolation-reapplied = This host's mods did not match the current isolation round, so Ephervex reapplied the round's mod setup. Test the round before answering.
gui-isolation-recovery-summary = The current round's mod setup must be reapplied before it can be answered.
gui-isolation-recovery-running = This host is running, so Ephervex cannot reapply the current round's mod setup. Stop it, then try again. Answers are unavailable until the setup is reapplied.
gui-isolation-recovery-failed = Ephervex could not reapply the current round's mod setup. Answers are unavailable until it is reapplied. Correct the problem, then try again.
gui-isolation-recovery-retry = Try again
gui-snapshot-mod-counts = { $enabled } enabled, { $disabled } disabled mods
gui-snapshot-created = Snapshot created.
gui-snapshot-restored = Snapshot restored.
gui-snapshot-renamed = Snapshot renamed.
gui-snapshot-deleted = Snapshot deleted.
## Diagnostics: file integrity and bundles

gui-diagnostics-tab = Diagnostics
operation-verifying-host = Verifying files
operation-stage-checking-shared-files = Checking shared launch files
operation-stage-diagnosing-again = Diagnosing again
operation-stage-checking-java = Checking Java
operation-stage-checking-mods = Checking mod requirements
operation-stage-asking-catalogs = Asking mod catalogs about requirements
gui-integrity-open-logs = Open logs
gui-integrity-nothing-recorded = Nothing is recorded for this host, so there is nothing to check.
gui-integrity-absent = Missing
gui-integrity-corrupt = Damaged
gui-integrity-unreadable = Unreadable
gui-integrity-unverifiable = No checksum
gui-integrity-repaired = { $count ->
    [0] Nothing needed replacing.
    [one] Repaired one file.
   *[other] Repaired { $count } files.
}
gui-integrity-repair-stopped = { $count ->
    [0] Repair stopped before replacing anything.
    [one] Repair stopped after replacing one file.
   *[other] Repair stopped after replacing { $count } files.
}
gui-integrity-repair-failed = { $count ->
    [one] One file could not be replaced. Check the network connection and try again.
   *[other] { $count } files could not be replaced. Check the network connection and try again.
}
gui-integrity-recovery = A broken file could not be put back after its repair failed. What was moved aside is kept here for review, and this host needs attention before it is changed again:
gui-integrity-shared-title = Shared launch files
gui-integrity-shared-detail = The Minecraft client JAR and libraries every instance of this release shares. Repair does not replace these.
gui-integrity-shared-not-cached = Not checked: this Minecraft release has not been launched yet, so there is nothing cached to check against.
gui-integrity-shared-sound = { $count ->
    [one] One shared file checked, and it is intact.
   *[other] { $count } shared files checked, none damaged.
}
gui-integrity-shared-faults = { $count ->
    [one] One shared file is damaged or unreadable. Removing it from the shared library store makes the next launch download it again.
   *[other] { $count } shared files are damaged or unreadable. Removing them from the shared library store makes the next launch download them again.
}
gui-integrity-shared-waiting = { $count ->
    [one] One is not downloaded yet. Launching downloads it, so this is not damage.
   *[other] { $count } are not downloaded yet. Launching downloads them, so this is not damage.
}
gui-integrity-shared-damaged = Damaged: { $error }
gui-integrity-shared-loader = The loader's own libraries are not part of this check.
operation-exporting-diagnostic-bundle = Writing the diagnostic bundle
gui-bundle-detail = Collects the files you choose into one zip file saved on this computer, to attach to a bug report yourself. Nothing is uploaded.
gui-bundle-privacy = Files are copied exactly as they are. Logs and crash reports contain whatever the game and its mods wrote, which can include server addresses, player names, or file paths, so choose only what you are willing to share. Ephervex never offers its configuration file, credentials, or tokens.
gui-bundle-empty = This host has no logs, crash reports, or recorded state to bundle yet.
gui-bundle-selection = { $count ->
    [0] Nothing chosen
    [one] One file chosen, { $size }
   *[other] { $count } files chosen, { $size }
}
gui-bundle-select-all = Choose all
gui-bundle-select-none = Choose none
gui-bundle-choose-one = Choose at least one file to export.
gui-bundle-export = Export bundle
gui-bundle-save-title = Save diagnostic bundle
gui-bundle-archives = Zip archives
gui-bundle-show-folder = Show in folder
gui-bundle-exported = { $count ->
    [one] Saved one file, { $size }, to { $path }.
   *[other] Saved { $count } files, { $size }, to { $path }.
}
gui-bundle-unreadable = These were chosen and could not be read, and the bundle's manifest says so: { $value }
gui-bundle-kind-game-log = Game log
gui-bundle-kind-crash-report = Crash report
gui-bundle-kind-launcher-log = Ephervex log
gui-bundle-kind-recipe = Recorded contents
gui-bundle-kind-record = Host record
gui-host-change-wait = Another host's files are still being changed. Try again once that finishes.
## Diagnostics: configuration and failure analysis

operation-diagnosing-host = Diagnosing
operation-applying-fix = Applying a fix
gui-diagnosis-title = Diagnostics
gui-diagnosis-detail = Checks whether this host is configured correctly for what it runs. Nothing changes unless you apply a fix.
gui-diagnosis-diagnose = Diagnose
gui-diagnosis-diagnose-again = Diagnose again
gui-diagnosis-stop = Stop
gui-diagnosis-unchecked-title = Not diagnosed yet
gui-diagnosis-unchecked = Diagnose checks Java, JVM settings, mod requirements, the last start's log and crash reports, and every recorded file. Hashing files can take a while on a large pack.
gui-diagnosis-working = Diagnosing...
gui-diagnosis-healthy = No problems found
gui-diagnosis-problems = { $count ->
    [one] 1 problem found
   *[other] { $count } problems found
}
gui-diagnosis-incomplete = Java could not be checked
gui-diagnosis-summary = Checked at { $time }. { $count ->
    [0] No customizations.
    [one] 1 valid customization.
   *[other] { $count } valid customizations.
}
gui-diagnosis-summary-quick = Checked at { $time } after the last run failed. Recorded files were not hashed.
gui-diagnosis-failed = Diagnosis could not finish
gui-diagnosis-canceled = Diagnosis stopped before it finished. Nothing was changed.
gui-diagnosis-stale = The host changed. The last result stays on screen until it is checked again.
gui-diagnosis-likely-cause = Likely cause
gui-diagnosis-customized-detail = Differs from what Ephervex would choose and still works. Not a problem.
gui-diagnosis-other-findings = Other findings ({ $count })
gui-diagnosis-show = Show
gui-diagnosis-requires = requires { $value }
gui-diagnosis-problems-heading = { $count ->
    [one] Problem
   *[other] Problems ({ $count })
}
gui-diagnosis-customized-heading = Valid, customized ({ $count })
gui-diagnosis-copy-report-hint = Diagnose this host first. The report summarizes what a diagnosis found.
gui-diagnosis-hide = Hide
gui-diagnosis-badge-problem = Problem
gui-diagnosis-badge-customized = Customized
gui-diagnosis-checked-title = What was checked
gui-diagnosis-check-ok = OK
gui-diagnosis-check-problem = Problem
gui-diagnosis-check-customized = Customized
gui-diagnosis-check-skipped = Not checked
gui-diagnosis-failure-context = Opened after a failed run: { $value }
gui-diagnosis-support-title = Report a problem
gui-diagnosis-support-detail = A readable summary to paste into a bug report or support thread. Paths in your home folder are shortened and JVM argument values are left out.
gui-diagnosis-copy-report = Copy technical report
gui-diagnosis-bundle-show = Prepare diagnostic bundle
gui-diagnosis-bundle-hide = Hide diagnostic bundle
gui-diagnosis-more-files = { $count } more
gui-diagnosis-wait = Another diagnosis or fix is still running. Stop it or wait for it to finish.
gui-diagnosis-open-mods = Open Mods
gui-diagnosis-open-settings = Open settings
gui-diagnosis-open-java-settings = Open Java settings
gui-diagnosis-open-log = Open log
gui-diagnosis-open-crash-report = Open crash report
gui-diagnosis-review-fix = Review fix
gui-diagnosis-use-recommended-java = Use recommended Java
gui-diagnosis-java-pinned-host = Chosen in this host's settings.
gui-diagnosis-java-pinned-launcher = Chosen by the preferred Java path in Settings.
gui-diagnosis-java-major = This runtime is Java { $found }.
gui-diagnosis-java-too-old = It is { $found }, older than the { $minimum } the loader needs.
gui-diagnosis-java-too-new = It is { $found }, newer than the { $maximum } the loader accepts.
gui-diagnosis-java-architecture = It is built for { $found }, and this host needs { $expected }.
gui-diagnosis-java-requires = Minecraft { $minecraft } requires Java { $major }.
gui-diagnosis-java-incompatible = The selected Java cannot run this host
gui-diagnosis-java-unusable = The selected Java path is not a usable runtime
gui-diagnosis-java-detection-off = No Java runtime to use
gui-diagnosis-java-none-installed = No compatible Java is installed
gui-diagnosis-java-detection-off-detail = Java detection is off in Settings and no Java path is set.
gui-diagnosis-java-download-off-detail = Automatic Java download is off in Settings.
gui-diagnosis-java-custom = Custom Java { $version }
gui-diagnosis-java-satisfies = It satisfies what Minecraft { $minecraft } requires (Java { $major }). Ephervex would otherwise choose a runtime automatically.
gui-diagnosis-required-by = Required by { $name }.
gui-diagnosis-required-by-others = Required by { $name } and { $count ->
    [one] 1 other mod.
   *[other] { $count } other mods.
}
gui-diagnosis-dependency-disabled = { $name } is turned off
gui-diagnosis-dependency-missing = { $name } is not installed
gui-diagnosis-dependency-missing-detail = Install it from the Mods page. Diagnostics does not choose a version for you.
gui-diagnosis-dependency-restore-detail = This host records it, so restoring puts back the exact release it recorded.
gui-diagnosis-dependency-install-detail = Installing takes the newest release for this host's Minecraft version and loader, and shows it before anything changes.
gui-diagnosis-restore-mod = Restore { $name }
gui-diagnosis-install-latest = Install latest compatible version
gui-diagnosis-conflict = { $name } does not work with { $other }
gui-diagnosis-conflict-detail = Its recorded relations say so. Turn one of them off to test without it.
gui-diagnosis-loader-missing = The loader requires { $dependency }
gui-diagnosis-loader-version = The loader requires another version of { $dependency } (found { $actual })
gui-diagnosis-loader-range = Accepted versions: { $range }
gui-diagnosis-loader-asked-by = Asked for by { $mods }
gui-diagnosis-loader-ranges = The mods asking for it accept different versions:
gui-diagnosis-loader-range-by = { $range } for { $mods }
gui-diagnosis-loader-detail = Reported by the loader when this host last started.
gui-diagnosis-hand-placed = { $count ->
    [one] 1 hand-placed mod
   *[other] { $count } hand-placed mods
}
gui-diagnosis-hand-placed-detail = Jars in the mods folder that Ephervex did not install.
gui-diagnosis-crash = The last run crashed
gui-diagnosis-crash-names = The crash report names: { $names }
gui-diagnosis-files-damaged = { $count ->
    [one] 1 recorded file is missing or damaged
   *[other] { $count } recorded files are missing or damaged
}
gui-diagnosis-files-restorable = { $count ->
    [one] 1 can be restored from its recorded source.
   *[other] { $count } can be restored from their recorded sources.
}
gui-diagnosis-files-unrestorable = { $count ->
    [one] 1 has no recorded source. Import the pack again to restore it.
   *[other] { $count } have no recorded source. Import the pack again to restore them.
}
gui-diagnosis-shared-damaged = { $count ->
    [one] 1 shared launch file is damaged
   *[other] { $count } shared launch files are damaged
}
gui-diagnosis-jvm-args-host = Custom JVM arguments
gui-diagnosis-jvm-args-launcher = Launcher-wide JVM arguments
gui-diagnosis-jvm-args-host-detail = { $count ->
    [one] This host sets 1 JVM argument of its own.
   *[other] This host sets { $count } JVM arguments of its own.
}
gui-diagnosis-jvm-args-launcher-detail = { $count ->
    [one] 1 JVM argument from Settings applies to every host.
   *[other] { $count } JVM arguments from Settings apply to every host.
}
gui-diagnosis-memory = Custom memory settings
gui-diagnosis-memory-default = default
gui-diagnosis-memory-detail = Minimum { $min }, maximum { $max }.
gui-diagnosis-check-java = Java
gui-diagnosis-check-java-pinned = { $version }, chosen by a setting. Requires Java { $major }.
gui-diagnosis-check-java-managed = { $version }, managed by Ephervex. Requires Java { $major }.
gui-diagnosis-check-java-automatic = { $version }, chosen automatically. Requires Java { $major }.
gui-diagnosis-check-java-download = Java { $major } is downloaded at the next launch.
gui-diagnosis-check-java-refused = No usable runtime.
gui-diagnosis-check-failed = Could not be checked: { $error }
gui-diagnosis-check-mods = Mod requirements
gui-diagnosis-check-mods-value = { $enabled } enabled, { $disabled } turned off. Requirements recorded for { $answered }, unknown for { $unanswered }.
gui-diagnosis-check-mods-unconfirmed = { $count ->
    [one] 1 required project could not be confirmed missing, because a jar here may be it under another catalog. Diagnose asks the catalogs.
   *[other] { $count } required projects could not be confirmed missing, because a jar here may be them under another catalog. Diagnose asks the catalogs.
}
gui-diagnosis-check-log = Last start's log
gui-diagnosis-check-log-none = No log from a previous start.
gui-diagnosis-check-log-refused = The loader refused to start.
gui-diagnosis-check-log-clean = No loader refusal.
gui-diagnosis-check-crash = Crash reports
gui-diagnosis-check-crash-none = No crash since the last start.
gui-diagnosis-check-crash-found = Crashed after the last start: { $report }
gui-diagnosis-check-files = Recorded files
gui-diagnosis-check-files-skipped = Not checked in this pass. Diagnose hashes them.
gui-diagnosis-check-files-value = { $count } checked: { $intact } intact, { $problems } missing or damaged, { $unverifiable } without a checksum.
gui-fix-title = Proposed changes
gui-fix-loading = Working out what would change...
gui-fix-cancel = Cancel
gui-fix-apply = Apply
gui-fix-applying = Applying...
gui-fix-preview-failed = The change could not be previewed: { $error }
gui-fix-enable = Enable
gui-fix-restore = Restore
gui-fix-current = Current
gui-fix-recommended = Recommended
gui-fix-java-unknown-version = Version could not be read
gui-fix-java-version = Java { $version }
gui-fix-java-managed = Ephervex managed Java { $version }
gui-fix-java-download = Ephervex managed Java { $major }, downloaded at the next launch
gui-fix-java-requires = This host requires Java { $major }.
gui-fix-enable-note = No files will be removed.
gui-fix-java-note = This changes only this host's Java selection. Its memory and JVM arguments stay as they are.
gui-fix-restore-note = Each file is fetched from its recorded source and checked before it replaces the broken copy. No other files change.
gui-fix-nothing-left = Nothing needs to change anymore. Diagnose again to see the current state.
gui-fix-blocked-running = Stop this host to apply this change.
gui-fix-blocked-isolation = Finish or cancel mod isolation to apply this change.
gui-fix-enabled = { $count ->
    [one] Enabled 1 mod.
   *[other] Enabled { $count } mods.
}
gui-fix-enabled-partly = Enabled { $count } of { $total } mods. These did not change:
gui-fix-enable-stopped = Stopped after enabling { $count } of { $total } mods. These did not change:
gui-fix-java-applied = This host now uses automatic Java selection.
gui-fix-java-not-applied = The Java selection did not change:
gui-run-failed-client = The last launch failed
gui-run-crashed = Minecraft crashed
gui-run-failed-server = The server stopped with an error
gui-run-exited-with-error = Minecraft exited with an error.
gui-run-server-exited-with-error = The server exited with an error.
gui-run-server-stopped-before-ready = The server stopped before it finished starting.
gui-run-likely-cause = Likely cause: { $cause }
gui-run-no-cause = Diagnostics found no configuration problem. The crash report or log has the details.
gui-run-open-diagnostics = Open Diagnostics
gui-run-dismiss = Dismiss

operation-creating-snapshot = Creating snapshot
operation-restoring-snapshot = Restoring snapshot
operation-renaming-snapshot = Renaming snapshot
operation-deleting-snapshot = Deleting snapshot

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

error-mod-path-unsafe = That mod would be written outside its place in the game directory.
.action = { $path } is not a valid location for a mod file, so nothing was written.

# A link at or above a mod's destination would send the write wherever the link
# points, which can be outside the instance.
error-mod-destination-linked = A link is in the way of that mod's file.
.action = { $path } is a link or inside one, so nothing was written there. Replace the link with a regular file or folder if you want this mod installed there.

error-mod-destination-collision = Two of these mods install under the same file name.
.action = { $name } is claimed twice, so installing them would leave only one. Install them separately, or leave one out.

error-mod-duplicate-project = A mod is listed more than once.
.action = { $project } from { $provider } appears more than once in this change or in the instance's mod list, so it cannot be changed safely. Nothing was changed.

error-mod-rollback-incomplete = The mod change failed and the previous files could not be fully restored.
.action = Review the recovery files at { $path } before changing this instance again.

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
gui-force-stop-signaled = Asked the launch to stop. The instance stays locked until its game has exited.
gui-force-stop-released = The launch was already gone. The instance is released.
gui-force-stop-owner-running = The launcher run that owns this launch is still running. The instance stays locked until it has exited.
gui-force-stop-owner-unproven = Nothing was stopped. The launcher run that owns this launch could not be confirmed, so it was not signaled. The instance stays locked.
gui-force-stop-nothing = Nothing was stopped. This launch no longer exists.
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
mod-install-kept-modified = Kept { $path }: its contents differ from the recorded file, so it was left as it is.
mod-install-record-updated = Updated the record for { $path }. No file was written.

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

error-mod-acquisition-restricted = That mod's author does not allow other launchers to download this release.
.action = Open { $file } on CurseForge and save it to a watched download folder. Ephervex verifies the copy and finishes the change for you.

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
error-server-properties-escape-malformed = Line { $line } of { $path } has a malformed \u escape, so Minecraft cannot read the file.
.action = Give the escape exactly four hexadecimal digits or remove it, then try again.
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
error-server-start-canceled = Starting the server was canceled before it began.
.action = Start the server again when you are ready.
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
# The badge an instance carries while a linked server is being built for it.
gui-state-creating-server = Creating server
# The row's own line before the work reports anything more specific. Kept
# to one short line, because it stands where the instance's source line does.
gui-creating-linked-server = Preparing the linked server
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
# Shown in place of the action while the game is running. A copy taken then
# would mix files the game is still writing.
gui-duplicate-instance-while-running = Duplicate (stop the game first)
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
# The download of an agreed link was redirected to another site. The address
# shown has its query and fragment removed.
gui-open-pack-link-redirected-detail = The download was redirected to another site. Ephervex would download the modpack from:
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
# Shown in place of the action while the game is launching or running.
gui-delete-instance-while-running = Delete Instance (stop the game first)
gui-delete-server = Delete Server...
gui-delete-instance-detail = Remove "{ $value }" from Ephervex?
gui-delete-server-detail = Remove "{ $value }" from Ephervex?
gui-delete-choice-detail = Keeping files only removes the Ephervex record. Deleting files permanently removes the managed game or server directory.
gui-delete-host-snapshots =
    { $count ->
        [one] Deleting files also deletes 1 retained snapshot.
       *[other] Deleting files also deletes { $count } retained snapshots.
    }
gui-delete-host-snapshots-unreadable = Deleting files also deletes all retained snapshots. Their count could not be read.
# $count is how many servers were created from this instance.
gui-delete-instance-linked = { $count ->
    [one] Its linked server is deleted with it, the same way.
   *[other] Its { $count } linked servers are deleted with it, the same way.
}
# Shown in the delete dialog while linked servers are running.
gui-delete-instance-stops-servers = { $count ->
    [one] Its linked server is running. It is stopped first: shut down cleanly if you keep files, or killed if you delete them.
   *[other] { $count } of its linked servers are running. They are stopped first: shut down cleanly if you keep files, or killed if you delete them.
}
# Shown in the delete dialog. Deleting is refused while this applies.
gui-delete-instance-unstoppable-servers = { $count ->
    [one] Its linked server may still be running, and Ephervex cannot stop it. Check it on its server page, then try again.
   *[other] { $count } of its linked servers may still be running, and Ephervex cannot stop them. Check them on their server pages, then try again.
}
gui-remove-from-ephervex = Keep Files
gui-delete-files = Delete Files
gui-stop-and-keep-files = Stop and Keep Files
gui-stop-and-delete-files = Stop and Delete Files
gui-instance-stopping-servers = Stopping linked servers before removing the instance...
gui-instance-deleting = This instance and its linked servers are being removed. Wait until that finishes.
gui-linked-server-deleting = This server is being removed with its instance. Wait until that finishes.
# $first is the linked server's name, $second is why it is busy.
gui-delete-instance-linked-server-busy = Its linked server { $first } is busy: { $second }
gui-instance-deleted = Instance removed.
gui-server-deleted = Server removed.
gui-notification-error = Action Needed
gui-notification-complete = Complete
# How many times the same notification arrived, shown on it as x2, x3 and so on.
gui-notification-repeat = x{ $count }
gui-fov-normal = Normal
gui-fov-quake-pro = Quake Pro
gui-select-all = Select All
gui-diagnostics = DIAGNOSTICS
gui-view-logs-crash-reports = View Logs & Crash Reports
gui-back-instances = Back to Instances
# A workspace pane whose client or server was deleted while it was shown.
gui-workspace-host-gone = No longer installed
gui-workspace-host-gone-detail = This client or server was removed. Choose another host for this pane or close it.
# Splitting the host workspace into panes that each show one host and one view.
gui-workspace-split = Split view
gui-workspace-split-title = Split view
gui-modal-pane-host = Show in this pane
gui-pane-host-no-clients = No client instances yet.
gui-pane-host-no-servers = No servers to list here. A linked server is listed under its client.
gui-host-choice-client = Client: { $value }
gui-host-choice-server = Server: { $value }
# What assistive technology calls a pane: where it is, its host, and its view.
gui-pane-label = { $position } pane: { $host }, { $surface }
gui-pane-label-single = { $host }, { $surface }
gui-pane-left = Left
gui-pane-right = Right
gui-pane-top-left = Top left
gui-pane-bottom-left = Bottom left
gui-pane-top-right = Top right
gui-pane-bottom-right = Bottom right
gui-pane-host = Host shown in { $value }
gui-workspace-more-surfaces = More
gui-pane-split-below = Add a pane below { $value }
gui-pane-close = Close { $value }
# A host action inside one pane, named with the pane: (action, pane).
gui-pane-action = { $first } ({ $second })
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
gui-refresh-mod-details = Refresh mod names, artwork, and dependencies
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
gui-modal-mod-acquire = Download This Release Yourself
gui-mod-compatible-versions = Releases that fit this instance
gui-mod-versions-loading = Loading releases...
gui-mod-no-compatible-versions = No release of this mod fits this instance's exact Minecraft version and loader.
gui-mod-installed-version = Installed
gui-mod-newest-version = Newest
gui-mod-browser-download = Browser download
gui-mod-acquire-detail = The author of { $value } does not allow other launchers to download this release. Open it in your browser and save it to a watched folder.
gui-mod-acquire-watching = Ephervex is watching those folders. As soon as the file's contents match, it verifies the copy and continues the operation for you.
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
gui-instance-jvm-args-placeholder = Separated by spaces, for example -XX:+UseZGC "-Dname=a b"
gui-instance-jvm-args-unclosed-quote = The extra JVM arguments have a double quote that is never closed. To use a quote inside an argument, put the whole argument in double quotes and write \" for the quote.
gui-instance-jvm-args-unsupported-escape = In the extra JVM arguments, a backslash inside double quotes must be followed by a double quote or another backslash. Write \\ for a single backslash.
gui-instance-runtime-save = Save Runtime Settings
gui-instance-runtime-clear = Follow Global Settings
gui-instance-runtime-saved = Saved this instance's runtime settings.
# $first is the global heap range, $second the globally selected Java.
gui-instance-runtime-global = Globally: { $first }, { $second }.
gui-allow-pack-mod-changes = Independent mod changes
gui-allow-pack-mod-changes-detail = An instance you created here starts with this on, because it declared nothing to preserve. An imported one starts with it off: the pack's declaration is treated as a historical record, so a mod it declares can only be inspected, and the bin puts a jar aside so you can put it back. Turning this on lets you update or change the version of a pack-declared mod for this instance only, and makes the bin take a mod out of the instance for good, along with the dependencies nothing else needs. "Reset to Pack's Version" always remains available to undo a version change, and turning a mod off is still how you put one aside to find which one crashes.
gui-allow-pack-mod-changes-toggle = Let me change and remove this instance's mods
gui-instance-settings-unavailable = Instance-owned Minecraft settings
gui-instance-settings-unavailable-detail = The current core safely applies known Vanilla defaults when an instance is created, but does not expose a typed editor for an existing instance. Unknown and mod-added options remain untouched.
gui-open-global-defaults = Open Global Defaults
gui-filter-servers = Filter servers...
gui-no-servers = No servers yet
gui-no-servers-detail = Create a standalone exact server or create one linked to a client reproduction environment.
gui-no-standalone-servers-detail = No standalone servers are shown. Linked servers remain available from their client workspaces.
gui-linked-server = Linked Server
gui-paired = Paired
gui-paired-with = Paired with
gui-paired-with-host = Paired with { $value }
gui-paired-server-state = Paired server: { $value }
gui-paired-client-state = Paired client: { $value }
gui-not-paired = Not paired
gui-pair-this-server = Pair with client
gui-pair-with-client = Pair with its client
gui-unpair = Unpair
gui-pair-start-together = Start together
gui-pair-starting = Starting the pair...
gui-pair-start-busy = This pair is already starting.
gui-pair-start-server-failed = Server { $server } did not start, so { $client } was not launched: { $error }
gui-pair-start-client-failed = Server { $server } is running, but { $client } did not launch: { $error }
gui-pair-show-beside = Show beside
gui-pair-no-room = The workspace has no room for another pane. Close a pane to show both sides.
gui-pair-already-shown = { $value } is already shown.
gui-pair-no-room-for = No room to show { $value }. Close a pane first; no open pane is replaced.
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
gui-property-unset = Not set
gui-property-toggles-unset-detail = A switch for a setting that server.properties does not have shows the server's default. It is written only once you change it.
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
gui-search-open-report = Search the open report...
gui-search-open-log = Search the open log...
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
gui-application-color-scheme = Interface theme
gui-application-color-scheme-detail = The colors Ephervex paints itself in. Add your own themes as TOML files in { $value }. Minecraft is unaffected.
gui-theme-problem = { $file }: { $value }
gui-theme-problem-folder = The themes folder could not be read.
gui-theme-problem-too-many = Only the first { $count } theme files are read. { $value } more were skipped.
gui-theme-problem-file-name = Skipped. Name theme files with lowercase letters, digits, "-" and "_" only.
gui-theme-problem-unreadable = Skipped. The file could not be read as UTF-8 text.
gui-theme-problem-too-large = Skipped. The file is larger than a theme can be.
gui-theme-problem-syntax = Skipped. Line { $line }: { $value }
gui-theme-problem-newer-format = Skipped. The file uses theme format { $count }, which this version of Ephervex cannot read.
gui-theme-problem-invalid-field = Skipped. "{ $value }" has the wrong type.
gui-theme-problem-unknown-parent = Skipped. "{ $value }" is not a built-in theme. Use "dark" or "light".
gui-theme-problem-invalid-color = Skipped. The color for "{ $value }" is not written as "#rrggbb" or "#rrggbbaa".
gui-theme-problem-unknown-color = "{ $value }" is not a theme color and was ignored.
gui-theme-problem-unknown-key = "{ $value }" is not a theme setting and was ignored.
gui-color-scheme-dark = Dark
gui-color-scheme-light = Light
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
gui-java-detect-detail = Look for runtimes on PATH, under JAVA_HOME, in the system JVM directories, and in SDKMAN.
gui-java-download = Download a managed runtime when needed
gui-java-download-detail = Fetch a Temurin build when an instance needs a Java version this machine does not have.
gui-java-memory = Memory per instance
gui-java-memory-detail = What each instance's JVM is given, unless that instance sets its own.
gui-java-memory-min = Minimum memory (MiB)
gui-java-memory-max = Maximum memory (MiB)
gui-java-preferred = Preferred Java executable
gui-java-preferred-detail = Used wherever an instance has no runtime of its own. Leave it empty to let Ephervex choose one.
gui-java-runtimes = Installed runtimes
gui-java-runtimes-detail = Every runtime this machine offers, and where each one was found.
gui-java-no-runtimes = No runtimes found yet.
gui-java-no-runtimes-detail = Refresh Runtimes scans PATH, JAVA_HOME, the system JVM directories, and SDKMAN again.
gui-java-refresh = Refresh Runtimes
gui-java-save = Save Java Settings
gui-java-remove-runtime = Remove { $value }
gui-java-remove-runtime-title = Remove Java Runtime
gui-java-remove-runtime-detail = Delete the downloaded Java runtime { $value }?
gui-java-remove-runtime-note = Ephervex downloads a runtime again when an instance needs one, so this is not permanent. A game already running keeps the files it opened.
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
gui-mod-impact-disable-title = { $value } is needed by other mods
gui-mod-impact-enable-title = { $value } needs other mods
gui-mod-impact-breaks = { $count ->
    [one] One mod requires it and will stop loading:
   *[other] { $count } mods require it and will stop loading:
}
gui-mod-impact-needs = { $count ->
    [one] It needs one mod that is turned off:
   *[other] It needs { $count } mods that are turned off:
}
gui-mod-impact-absent = { $count ->
    [one] One mod it needs is not installed here. Add it from the mod browser:
   *[other] { $count } mods it needs are not installed here. Add them from the mod browser:
}
gui-mod-impact-incompatible = { $count ->
    [one] It says it does not work alongside one mod that is loading:
   *[other] It says it does not work alongside { $count } mods that are loading:
}
gui-mod-impact-degrades = { $count ->
    [one] One mod lists it as optional and will keep loading with less:
   *[other] { $count } mods list it as optional and will keep loading with less:
}
gui-mod-impact-unknown = { $count ->
    [one] Ephervex has not recorded what one other mod requires, so it cannot tell:
   *[other] Ephervex has not recorded what { $count } other mods require, so it cannot tell:
}
gui-mod-impact-unidentifiable = { $count ->
    [one] Ephervex cannot tell what one other jar is:
   *[other] Ephervex cannot tell what { $count } other jars are:
}
gui-mod-impact-through = { $first } (through { $second } mods)
gui-mod-impact-refresh = Refresh Dependencies
gui-mod-impact-cascade-off = Turn those off too
gui-mod-impact-cascade-on = Turn those on too
gui-mod-impact-remember = Do not ask again
gui-mod-impact-turn-off = Turn Off
gui-mod-impact-turn-on = Turn On
gui-mod-disabled-with-dependents = { $count ->
    [one] Turned off one mod that required it as well.
   *[other] Turned off { $count } mods that required it as well.
}
gui-mod-enabled-with-dependencies = { $count ->
    [one] Turned on one mod it needs as well.
   *[other] Turned on { $count } mods it needs as well.
}
gui-mod-requires = Requires
gui-mod-required-by = Required by
gui-mod-requires-nothing = Its catalog says it needs nothing else.
gui-mod-requires-unrecorded = Nothing has asked its catalog what it needs.
gui-mod-requires-unanswerable = Ephervex cannot tell what this JAR is, so no catalog can be asked what it needs.
gui-mod-required-by-nothing = No other mod here names it.
gui-mod-relation-required = Required
gui-mod-relation-optional = Optional
gui-mod-relation-incompatible = Incompatible
gui-mod-relation-embedded = Bundled
gui-mod-relation-disabled = turned off
gui-mod-relation-absent = not installed here
gui-mod-relation-no-release = no release fits here
gui-mod-relation-unknown-project = not in this catalog
gui-mod-relation-file-only = names a file, not a project
gui-mod-relation-unchecked = not here, not looked up
gui-mod-dependencies-refreshed = { $count ->
    [one] Recorded what one mod requires.
   *[other] Recorded what { $count } mods require.
}
gui-modal-delete-mod = Delete Mod
gui-delete-mod-detail = Delete { $value } from the mods folder?
gui-delete-mod-note = You added this JAR yourself, so Ephervex cannot fetch it again. The file is removed from disk.
gui-delete-restorable-mod-detail = Put { $value } aside?
gui-delete-restorable-mod-note = The JAR is removed from the mods folder, but the mod stays in this list so you can restore it.
gui-mod-deleted = Deleted { $value }.
gui-mod-deleted-with-dependents = { $count ->
    [one] Deleted { $value } and one mod that required it.
   *[other] Deleted { $value } and { $count } mods that required it.
}
gui-modal-mod-clash = Name Already Taken
gui-mod-clash-detail = The mods folder already has a file called { $value }.
gui-mod-clash-disabled = The one already there is turned off. Replacing it turns the mod back on.
gui-mod-clash-recorded = The instance keeps a record of the one already there. Replacing it drops that record, so repair and new servers no longer restore the old release.
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
gui-mod-changes = Mod changes
gui-mod-changes-detail = What a mod's switch does to the mods that need it.
gui-follow-dependencies = Change the mods that need it too
gui-follow-dependencies-detail = Turning a mod off takes the mods that require it with it, and turning one on brings back what it needs. Off, every switch changes only the mod it belongs to. What each mod requires stays recorded either way.
gui-dependency-warning-unused = Nothing else is being changed, so there is nothing to confirm.
gui-dependency-warning = Ask before changing other mods
gui-dependency-warning-detail = Say what else a change affects and wait for an answer. Off, the change is made and reported in a message afterwards.
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
gui-mod-install-not-shown = { $name } was not installed because another dialog was open. Press Install to try again.
gui-dialog-not-shown = That dialog was not opened because a download is waiting for you. Finish or cancel the download, then try again.
gui-dialog-not-shown-open-dialog = That dialog was not opened because another dialog was open. Close it, then try again.
gui-install-required = Install Required Set
gui-eula-detail = Minecraft's server EULA is a legal agreement. Ephervex records global consent only after this explicit action; starting a server never accepts it for you.
gui-open-minecraft-eula = Open Minecraft EULA
gui-eula-confirm = By choosing Accept, you confirm that you have reviewed and agree to the Minecraft EULA.
gui-revoke-consent = Revoke Consent...
gui-accept-eula = Accept EULA
gui-force-kill-detail = Force killing "{ $value }" may lose unsaved world data. Use normal Stop whenever the server responds.
gui-this-server = this server
gui-force-kill-action = Force Kill
# The list is generated from this build's own dependency graph, so it describes
# exactly what this copy contains.
gui-third-party-summary = { $crates ->
    [one] 1 open-source package, under { $licenses } license texts
   *[other] { $crates } open-source packages, under { $licenses } license texts
}
gui-third-party-note = Every license here also ships beside Ephervex in full, as THIRD-PARTY-LICENSES.md.
gui-third-party-select = Select a package to read the license it is under.
# Shown only when this build's own bundled list cannot be read, which means a
# damaged build rather than a missing report.
gui-third-party-broken = This build's third-party license list could not be read. Reinstall Ephervex, or download the release again.
gui-third-party-back = Back to the list
gui-slint-attribution = Slint attribution is available directly in About & Licenses through Slint's official AboutSlint widget.
gui-close = Close
gui-clipboard-copied = Copied to the clipboard.
gui-clipboard-unavailable = The desktop clipboard is unavailable.
gui-server-property-values-invalid = Correct the maximum-player value before saving.
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
gui-background-unavailable = The background service is not running. Nothing in this window will take effect until Ephervex is restarted.
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
gui-mod-records-updated = { $count ->
    [one] Updated the record of 1 mod to match the jar already in place.
   *[other] Updated the records of { $count } mods to match the jars already in place.
}
gui-mod-kept-modified = { $count ->
    [one] Kept { $names }: its contents differ from the recorded file, so it was left as it is.
   *[other] Kept { $names }: their contents differ from the recorded files, so they were left as they are.
}
gui-stop-requested = Stop requested.
gui-state-restarting = Restarting
gui-settings-disk-usage = Disk usage
gui-disk-usage-page-detail = How much space Ephervex's own directories hold, and what for.
gui-disk-usage-title = Storage
gui-disk-usage-detail = Measures Ephervex's instances, servers, caches, and records. Nothing is deleted or changed.
gui-disk-usage-metric = Sizes are the files' own lengths. Copy-on-write snapshots and sparse files can take up less actual disk space than shown. A file with several hard links is counted once, and linked folders are not followed.
gui-disk-usage-measure = Measure
gui-disk-usage-refresh = Measure again
gui-disk-usage-stop = Stop
operation-measuring-disk-usage = Measuring disk usage
gui-disk-usage-measuring = Measuring...
gui-disk-usage-progress = { $count ->
    [one] One file counted so far
   *[other] { $count } files counted so far
}
gui-disk-usage-idle = Not measured yet. Measuring reads every file Ephervex keeps, which takes a while on a large installation.
gui-disk-usage-total = { $value } in total
gui-disk-usage-measured-at = Measured at { $value }
gui-disk-usage-incomplete = { $count ->
    [one] One place could not be read, so its size is missing from every figure:
   *[other] { $count } places could not be read, so their size is missing from every figure:
}
gui-disk-usage-retention-bar = Storage by whether it can be recreated
gui-disk-usage-open = Open { $value }
gui-disk-usage-retention-persistent = Kept until you remove it
gui-disk-usage-retention-persistent-detail = Persistent instances and servers, their snapshots and logs, and Ephervex's own records.
gui-disk-usage-retention-temporary = Temporary hosts
gui-disk-usage-retention-temporary-detail = Temporary instances and servers, in the system's temporary directory. They go when it is cleared.
gui-disk-usage-retention-regenerable = Can be downloaded or rebuilt
gui-disk-usage-retention-regenerable-detail = Caches and Java runtimes. A cached file can still be the only copy of a download its author has since withdrawn.
gui-disk-usage-retention-recovery = Recovery state
gui-disk-usage-retention-recovery-detail = Files an interrupted change left behind. They are what lets that change be reviewed or undone, so they are not disposable.
gui-disk-usage-client-instances = Client instances
gui-disk-usage-client-instances-detail = Game directories and their records, apart from the mods, logs, and snapshots listed on their own.
gui-disk-usage-local-servers = Local servers
gui-disk-usage-local-servers-detail = Server directories and their records, apart from the mods, logs, and snapshots listed on their own.
gui-disk-usage-mods = Mods
gui-disk-usage-mods-detail = The mods folder of every instance and server. Each one is inside its host, so there is no single folder to open.
gui-disk-usage-snapshots = Snapshots
gui-disk-usage-snapshots-detail = Safety snapshots inside each instance and server. A copy-on-write snapshot shares unchanged data with its host, so it usually takes up much less space than this.
gui-disk-usage-logs = Logs and crash reports
gui-disk-usage-logs-detail = Game logs and crash reports inside each host, Ephervex's own log, and the output it captured from servers.
gui-disk-usage-download-cache = Download cache
gui-disk-usage-download-cache-detail = Verified downloads, loader installers, provider metadata, and artwork.
gui-disk-usage-shared-game-files = Shared game files
gui-disk-usage-shared-game-files-detail = Minecraft assets, libraries, and natives, shared by every host on the same release.
gui-disk-usage-java-runtimes = Java runtimes
gui-disk-usage-java-runtimes-detail = Java runtimes Ephervex downloaded.
gui-disk-usage-launcher-state = Ephervex settings and records
gui-disk-usage-launcher-state-detail = Configuration, the instance index, and icons.
gui-disk-usage-recovery = Recovery state
gui-disk-usage-recovery-detail = Files moved aside, files still arriving, and snapshot-restore journals that interrupted changes left inside hosts. Kept for review.
gui-disk-usage-other = Other
gui-disk-usage-other-detail = Anything else in Ephervex's directories.
gui-activity-restarting = Restarting
gui-restart-stage-stopping = Stopping, to start it again once it has stopped
gui-restart-stage-starting = Stopped. Starting it again
gui-restart-done = { $value } restarted.
gui-restart-not-stopped = { $value } did not stop, so Ephervex did not start another copy.
# $detail is the reason the new start failed, already localized.
gui-restart-start-failed = { $value } stopped, but starting it again failed: { $detail }
gui-restart-interrupted = { $value } was not restarted: { $detail }
gui-restart-stopped-instead = it was stopped while the restart waited for it.
gui-restart-unexpected-answer = the server answered in a way a restart does not expect.
gui-restart-launch-lost = the new launch ended without saying whether it started.
gui-restart-exited-at-once = the new game exited right after it started.
gui-restart-game-failed = the new game exited with an error.
gui-restart-client-unavailable = Ephervex can restart a client it launched in this session, but not one launched alongside another. Stop this one and launch it again.
gui-restart-server-unavailable = That server is not running under Ephervex, so there is nothing to restart. Start it instead.
gui-restart-busy = This host is restarting. Try again once that finishes.
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
gui-account-removed-credential-deleted = Removed account and deleted its saved credential.
gui-account-removed-credential-pending = Removed account. The keyring could not be reached, so any saved credential will be deleted once it is available.
gui-provider-settings-saved = Saved provider settings.
gui-storage-root-saved = Saved the persistent instance root.
gui-java-settings-saved = Saved Java settings.
gui-java-runtime-removed = Removed the Java runtime.
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
gui-credential-unknown = Unknown, secret storage is unavailable
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
gui-activity-server-running = Server running
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
gui-linked-recorded = { $count ->
    [one] 1 record updated
   *[other] { $count } records updated
}
gui-linked-kept-modified = { $count ->
    [one] 1 changed jar kept
   *[other] { $count } changed jars kept
}
gui-linked-nothing-changed = Nothing needed changing.
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

# Local development server shortcuts and control feedback.
gui-open-server-folder = Open Server Folder
gui-server-properties-shortcut = Server Properties
gui-server-folder-location = Server folder location
gui-server-config-folder = Mod configuration
gui-server-game-logs = Minecraft logs
gui-server-capture-folder = Ephervex captured output
gui-server-managed-folder = Server root and Ephervex metadata
gui-server-files-detail = Edit server.properties and arbitrary server files with your usual tools. Mod configuration is in config when the mod uses that folder.
gui-server-log-locations-detail = Minecraft logs and crash reports are available after the server writes them. Ephervex captured output is stored separately.
gui-server-captured-output = Captured output
gui-server-console-busy = Console input is busy. Wait for the current server operation to finish.
gui-server-console-unavailable = Console input requires a running server with a live Ephervex control channel. Ephervex cannot take control of an orphaned process. Retained output is still available.
gui-state-stopping = Stopping
gui-server-stopping-detail = Waiting for this server to stop. This affects every view of the server.
gui-server-restarting-detail = Restarting this server. This affects every view of the server.
gui-server-failed-detail = The server failed to start or exited unexpectedly. Open Console for captured output or Logs for server files and crash reports.
gui-server-control-refused = Server control was refused: { $value }
gui-server-control-unconfirmed = The server did not confirm the request. Check its status and output before retrying.
gui-server-control-result = { $first }: { $second }
gui-server-console-channel-lost = Server input is unavailable. Check the output and server status. Stop the server, or use Kill if it cannot stop, then start it again.
