# Documentation website TODO

Open work for the website and the screenshot tool. Remove an item when it is
done. Remove this file when the list is empty.

Sources: the GUI in `crates/gui/ui`, the CLI in `crates/cli/src/main.rs`,
`docs/`, the ADRs, and the issue tracker. Code is the source of truth where
the README disagrees (see "Known stale sources").

## Site setup

- [ ] Configure GitHub Pages to deploy from Actions, set DNS for
      `https://ephervex.chaotictrials.de`, and verify the first deployment.
      The site configuration, `static/CNAME`, and Pages workflow use this domain.
- [ ] Verify the `/deb` and `/rpm` repository endpoints and signing metadata
      after the first release publishes them.

Docs use major versions: pre-v1 and v1 releases share v1 at `/docs/v1/`.
See `README.md` for archiving v1 when v2 work starts.

## 2. Fixture corrections (`tools/auto-screenshot-tool/src/fixtures.rs`)

- [x] `InstanceRow.source` must use the strings the app produces: `Manual`,
      `Modrinth`, `CurseForge`, `Prism`, `Server pack`. The current values
      render as "Modrinth pack 1.1.0 instance".
- [x] Set `HostView.browser_next_offset` to -1 in `client_mods`. It removes
      the stray "Load More" button.
- [x] Set `browser_provider`, browser results, `storage_root`, `mods_query`
      and `allow_pack_mod_changes` so the Mods surface looks populated.
- [x] Replace the account names `_jeb` and `Dinnerbone`, which are real
      developer handles, with synthetic names. Keep names to 3-16 characters
      from `[A-Za-z0-9_]`.
- [x] Check the capitalization of `AccountRow.kind` and `credential_state`
      against a live run (the app uses lower-case kinds and the credential
      strings "No credential", "Credential stored securely" and "Sign-in
      required").
- [x] Check the toggle knob rendering. In the light theme an enabled toggle
      has no visible knob at either size, so it is the app palette, not the
      capture.
- [x] Fixture `loader` is hardcoded to Forge. Derive it from the loader kind.

## 3. Fixture breadth

Keep values within what the product supports: Vanilla, Forge and NeoForge,
release versions 1.12.2 and newer. Never put an unreleased or unsupported
Minecraft version in a version field. Names and descriptions may use
Minecraft-flavored text. Do not use Mojang or Minecraft logos, textures, skins
or other artwork. Pack content beyond the documented CaveStone pack is
synthetic and recorded as such in `SOURCES.md`.

- [x] Instances (6-8): Ready, Starting, Running (with Kill), needs attention,
      creating a linked server, restarting. Sources: Manual, Modrinth,
      CurseForge, Server pack. Include a 1.12.2 and a 26.x instance.
- [x] Servers (4): stopped, starting, running, failed. Optionally one linked
      server with the linked-servers setting on.
- [x] Accounts (4): two offline, one Microsoft with a stored credential, one
      "Sign-in required".
- [x] Mods: mixed origin (pack, override, added, dependency, hand-placed), one
      disabled, one deleted, one with an unknown version, each with its logo.
- [x] Catalog browser results (4): one installed, one queued.
- [x] Java runtimes: 8, 17, 21 and 25, one of them managed.
- [x] Activity: running, queued, completed, failed and canceled rows.
- [x] Running page: one client, one server.
- [x] Instances, servers, mods, search results, snapshots, logs, crash reports
      and diagnosis results are recorded from real packs and servers, listed in
      `tools/auto-screenshot-tool/SOURCES.md`. Only the accounts other than
      `MelanX`, the CurseForge import states and the import review's optional
      files are made up.
- [x] Update the account test in `fixtures.rs` for the new rows.

## 4. New screenshot states (`catalog.rs`)

Fixture-only unless noted. Region crops of dialogs, pending cards, activity
cards and toasts need either an `accessible-label` on that element or a
geometry-based crop in the tool.

Getting started:
- [x] `instances-empty` (no instances, New Instance and Import Modpack)
- [x] `create-instance-dialog`
- [x] `import-modpack-ready` (pack card, optional files, blocked files,
      duplicates)
- [x] `import-modpack-manual-download` (Open in Browser for a blocked CurseForge file)
- [x] `import-modpack-credential-missing` (error plus Providers & APIs link)
- [x] `import-modpack-resolving` (frozen spinner)

In-progress work (the headless renderer uses mock time, so spinners and bars
are deterministic):
- [x] `import-modpack-running` (pending card, 57% bar, stage and item count)
- [x] `activity-import-in-progress`
- [x] `running-page` (one client and one server)
- [x] `instance-launching` (Starting state plus a Launching Minecraft row)
- [x] `server-starting`, `server-running`
- [x] `diagnostics-working`
- [x] `accounts-device-code` (the only place the sign-in code is shown is a
      toast)
- [x] `toasts-success-and-error`

Mods:
- [x] `client-mods` rebuilt per section 2
- [x] `mod-manager` (versions with Installed and Newest pills, Requires and
      Required by)
- [x] `mod-impact` (breaks, needs, cascade)
- [x] `mod-install-plan` (with linked servers)
- [x] `mod-isolation-setup`, `mod-isolation-round`

Hosts:
- [x] `client-overview-linked-server` (Paired pill, Start together)
- [x] `client-settings`, `snapshots`, `snapshot-restore-dialog`
- [x] `client-logs-live`, `client-crash-reports`
- [x] `client-diagnostics-result`, `client-diagnostics-fix-preview`
- [x] `client-run-failure-notice`
- [x] `server-console`, `server-properties` (EULA card), `server-crash-notice`
- [x] `split-workspace` (client logs beside server console)

Settings:
- [x] `settings-general` (theme, text sizes, trusted hosts)
- [x] `settings-minecraft-defaults`
- [x] `settings-java`
- [x] `settings-providers` (catalog, CurseForge key states)
- [x] `settings-storage`, `settings-disk-usage`

Menus, dropdowns, and the folded diagnostic bundle panel are captured by an
`interaction` on the state (a click or right-click on an accessible label before
the frame is taken). Not capturable yet: hover tooltips and disabled-button
reasons, the workspace "More" popup, `WindowSizePreview`. Do not add product UI
just to make a screenshot.

Limits of the captured states (the tool renders one frame and cannot scroll, and
interacts only to open a menu or dropdown):
- Tall dialogs and pages show only their top: `import-modpack-duplicates` and the lower part of
  `client-diagnostics-result` at 1080p (all of it fits at 1440p).
- `mod-impact` shows only the breaks group, and `mod-manager` shows Reset to
  pack version but no Update button, because the app's dialog layout cannot
  hold the other variants.
- `settings-general` lists one favorite version, and `settings-minecraft-defaults`
  shows four rows on the Video tab, so they fit at 1080p.
- The live log follows its newest line through a one-shot timer, so it holds
  for the first rendered size of a run only.
- In the light theme an enabled toggle has no visible knob (app palette).
- Add an `accessible-label` to dialogs, pending cards and toasts if crops of
  them are wanted.

## Screenshot coverage for task guides

Place existing captures beside the steps they illustrate before adding new
fixtures. Keep the images focused on the action or decision described there.

- [x] Getting Started and Accounts: use `accounts-settings/offline-profile` for
      adding an offline account, `accounts-device-code` for Microsoft sign-in,
      and `instance-launching` or `running-page` for the first launch.
- [x] Import Modpacks: use `file-drop-overlay` for dropping an archive,
      `import-modpack-running` and `activity-import-in-progress` for progress.
- [x] Diagnostics and Troubleshooting: use `client-run-failure-notice` and
      `server-crash-notice` to show the routes into logs and diagnosis.
- [x] Files and Privacy: use `settings-disk-usage` beside the storage guidance.

Add focused crops and numbered callouts where full-window captures make
controls too small. Keep the source captures clean and describe every marker
in nearby text and alt text.

- [x] Crop and mark the Minecraft release, loader build, and persistent-storage
      choice in `create-instance-dialog`.
- [x] Crop and mark Open in Browser in `import-modpack-manual-download` and the
      answer buttons in `mod-isolation-round`.
- [x] Crop and mark the result and action in `client-diagnostics-result`.
- [x] Crop and mark Access, Online mode, and the EULA action in
      `server-properties`; show the account and instance action menus beside
      the instructions that name them.
- [x] Add labeled dialog or control regions where an accessible-element crop
      is possible.

## 5. Documentation pages

Write only what ships. Each task page uses `Tabs` with `groupId="interface"`
and `queryString="interface"` (GUI first, then CLI). GUI-only and CLI-only
features show one tab or a short note.

- [x] Interface tour
- [x] Accounts: offline and Microsoft, device code, session-only, keyring
- [x] Instances: temporary vs persistent, create, launch, duplicate, make
      persistent, delete, per-instance Java and memory, change loader build
- [x] Import: Modrinth and CurseForge, optional files, `ephervex://` links,
      trusted hosts, blocked files and watched folders
- [x] Mods: browse and install, enable and disable, remove, change version,
      add your own JARs, dependencies, verify and repair
- [x] Mod isolation (source: `docs/mod-isolation.md`)
- [x] Servers: create (standalone and linked), EULA, properties and network,
      start, stop, console, Launch Client & Connect, recovery
- [x] Diagnostics, logs and crash reports, diagnostic bundles
- [x] Snapshots
- [x] Settings: General, Minecraft defaults, Java, Providers & APIs, Storage,
      Disk usage
- [x] Themes (source: `docs/themes.md`)
- [x] Files and locations, environment variables, working offline,
      credentials and privacy, upgrade and uninstall
- [x] Limitations and troubleshooting

## 6. CLI

- [x] Hand-written CLI tabs on the task pages, using real flags and noting
      confirmation prompts and exit behavior.
- [x] Generated CLI reference under `docs/reference/cli/`, one page per
      top-level command, produced from `ephervex --help` and
      `ephervex help <path>`. The build generates gitignored pages before
      Docusaurus reads them.
- [x] CLI-only features that need a mention on their own pages: `server
      recover`, `java select`, `instance recipe`, `url-handler`, `doctor`,
      `completions`.

## 7. Known stale sources

- The README says CurseForge mod browsing is not available. It is: the mod
  browser has a Modrinth/CurseForge switch, and `ephervex mod search
  --provider curseforge` exists. It also says updates are unavailable, but a
  per-mod version change and removal exist. Bulk update-all does not.
- Fabric and Quilt are not supported. Prism/MultiMC exports are recognized
  and refused.
- The desktop UI is `en-US` only.

## 8. Wait for planned changes before documenting

These change screens that exist today. Write the surrounding pages so they are
easy to update, and capture screenshots late.

- #9 instance search, filters, sort and favorites
- #11 Java runtime visibility and compatibility checks
- #12 server status and configuration shortcuts
- #13 navigation and active-state polish
- #28 installing server mods from Modrinth and CurseForge
- #378 sandboxed branding assets and theme packages, #379 the Haunted Pumpkin
  theme. Avoid fixed theme-list screenshots. When the launcher includes the
  Halloween theme, use its screenshots during the same date range as the site.
- #17 resource packs, shader packs and datapacks during import
- #10 cache and storage maintenance
- #341 clients and servers surviving an Ephervex restart
- #261 tooltips and disabled-button explanations, #257 keyboard reachability

## 9. Limitations the docs must state

- Linux only. Vanilla, Forge and NeoForge only. Release versions 1.12.2 and
  newer. No snapshots, Fabric, Quilt, Windows or macOS.
- Release builds include CurseForge API access. Files whose authors disable
  third-party downloads cannot be fetched automatically.
- Resource packs, shader packs and datapacks in imported packs are not yet
  preserved (#17).
- Quitting Ephervex stops running clients and servers (#341).
- Do not run concurrent mod changes on one host, or mix mod edits with an
  active isolation session (#232, #208).
- Session-only sign-in drops its credential. A Secret Service outage can show
  "not signed in".
- AppImage needs FUSE or `APPIMAGE_EXTRACT_AND_RUN=1`. glibc 2.35 baseline.
  Wayland sessions without XWayland are untested, and file drag and drop
  needs X11 or XWayland.
- Ephervex runs on ARM (aarch64), but ARM launches have not been tested.
  Minecraft also needs native libraries compatible with the selected version.

## 10. Verification before merging

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`
- [x] `tools/licenses/generate.sh --check`
- [x] `cd web && npm run screenshots:check && npm run typecheck && npm run build`
- [x] Regenerate screenshots and run `npm run screenshots:sync` locally to
      review captures before merging. Pages regenerates them for deployment.
