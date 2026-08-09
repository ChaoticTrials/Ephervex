# Ephervex contributor instructions

These instructions apply to the entire repository unless a more specific `AGENTS.md` in a subdirectory overrides them.

## Product and architecture

- Ephervex is a Linux-first Minecraft launcher and developer tool for disposable bug-reproduction environments.
- Keep application logic independent of the frontend. GUI and CLI code should call `ephervex-core` instead of reaching through it into lower-level crates.
- Keep normalized pack and instance models provider-neutral. Provider-specific identifiers and metadata belong only in explicit provider/source fields.
- Preserve exact user-selected Minecraft, loader, and pack versions. Never silently upgrade, downgrade, substitute, or approximate an unsupported selection.
- Shared artifacts such as Minecraft assets, libraries, game JARs, managed runtimes, and verified downloads belong in shared stores rather than being duplicated into disposable instances unless the design explicitly requires an instance-local copy.
- Treat imported archives, metadata, URLs, filenames, paths, and launcher exports as untrusted input.

## Durable naming and documentation

- Name files, modules, types, functions, tests, fixtures, and commands after the behavior, domain concept, invariant, or feature they represent.
- Do not encode implementation chronology, milestones, sequencing, temporary work ownership, or development-session provenance into tracked source code or documentation.
- Do not use numbered implementation-stage names in filenames, test names, symbols, comments, doc comments, fixture names, environment variables, operation labels, or user-visible text.
- Do not mention coding agents, chat sessions, temporary checkpoints, handoffs, or similar development-process details in tracked project content.
- Comments and doc comments must explain durable intent, non-obvious behavior, invariants, compatibility constraints, or security reasoning. Delete comments that only narrate when or why a developer happened to add the code.
- When a durable project decision needs a citation, reference the relevant ADR. When behavior follows an external format or service, reference the upstream specification or documented contract.
- ADRs describe lasting decisions and rationale. Keep their status and wording independent of implementation chronology.
- Documentation should describe the current architecture, supported behavior, known limitations, and verification procedures rather than the order in which features were built.

## User-facing text

- Never expose internal engineering references, implementation chronology, source-code locations, or planning terminology in CLI help, GUI copy, errors, logs intended for users, or localization strings.
- Remember that Rust doc comments on `clap` commands and fields can become public `--help` output. Write them as product documentation for users.
- User-facing errors should be actionable and should not leak secrets, credentials, access tokens, authorization headers, or sensitive query parameters.
- Technical values such as provider names, paths, versions, hashes, and upstream identifiers should remain exact when displayed.

## Language and spelling

- Use US English consistently in source code, identifiers, filenames, tests, comments, documentation, commit messages, and English localization text.
- Prefer spellings such as `behavior`, `color`, `favor`, `initialize`, `optimize`, `normalize`, `serialize`, `canceled`, `canceling`, `labeled`, `license`, and `center`.
- Do not mechanically alter third-party API names, dependency methods, protocol fields, quoted upstream text, fixture payloads, URLs, hashes, or identifiers whose spelling is defined externally. Keep exact upstream spellings at integration boundaries.

### Writing style

- Use US English in all Ephervex-owned content.
- Prefer plain technical-software punctuation and concise, natural wording.
- Use straight ASCII quotes (`"` and `'`), not typographic/smart quotes.
- Use three ASCII periods (`...`), not the Unicode ellipsis (`…`).
- Avoid em dashes (`—`) and similarly editorial punctuation when a comma,
  colon, parentheses, or separate sentence reads naturally.
- Avoid marketing-like, overly polished, or AI-characteristic prose. Write like
  a concise developer tool.
- Preserve exact upstream, protocol, API, filesystem, and user-provided text
  even when it does not follow these conventions.

## Rust and API quality

- Prefer typed domain models over loosely related strings and booleans when invalid combinations can otherwise be represented.
- Use `Result` and typed errors for recoverable failures. Do not add production `unwrap`, `expect`, `panic!`, `todo!`, or `dbg!` calls without a compelling invariant and an explicit justification.
- Preserve error context without exposing secrets. Keep user-facing localization separate from low-level technical diagnostics.
- Keep public APIs minimal. Do not make an item public solely to make a test easier.
- Avoid duplicating security, path-validation, version-selection, or compatibility logic across crates. Reuse the owning crate's implementation.
- Do not weaken a production safety rule to make a test pass. If tests require a special environment such as loopback HTTP, use an explicit test-only or narrowly scoped escape hatch whose production default remains strict.

## Import and filesystem safety

- Archive extraction must reject path traversal, absolute paths, unsafe prefixes, escapes through links, and writes into launcher-owned metadata areas.
- Enforce archive entry-count and uncompressed-size limits before untrusted data can exhaust disk or memory.
- Never execute imported pre-launch commands, post-exit commands, wrapper commands, arbitrary Java paths, or other executable customization from untrusted packs.
- Verify downloadable artifacts with the strongest hashes supplied by the source. A hash mismatch is a hard failure; never install the mismatching file.
- Keep production downloads on approved secure transports and hosts. Do not bypass third-party distribution restrictions.
- Write incomplete downloads and installations to staging locations and publish them atomically where practical.

## Tests and fixtures

- Test names must state the behavior or invariant under test. A reader should understand the purpose without consulting project history.
- Integration-test filenames must describe the covered subsystem or workflow, not when the test was introduced.
- Ordinary test runs must not depend on live Mojang, Microsoft, Modrinth, CurseForge, Forge, NeoForge, or other production services. Use fixtures and loopback servers.
- Keep real-world/manual smoke tests opt-in when they download or execute production artifacts.
- Fixtures should be minimal, deterministic, reviewable, and documented with durable provenance in `SOURCES.md` where appropriate.
- Do not derive general format behavior from one real-world sample. Implement documented formats and use samples only as validation.
- When fixing a bug, add or strengthen a regression test when practical.

## Validation

Before declaring repository changes complete, run the checks relevant to the touched code. For normal Rust changes, the expected baseline is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If CLI definitions or help text changed, also inspect the affected help output, including the top-level command when relevant:

```bash
cargo run -p ephervex-cli -- --help
```

If a required manual acceptance step cannot run because of an external environment limitation, report the exact blocker. Do not reinterpret an implementation failure as an environment limitation.

## Git hygiene

- Keep the working tree understandable and intentional. Do not commit scratch files, temporary checkpoints, generated debugging notes, or agent handoff documents.
- Do not create, rename, or delete branches, rewrite existing commits, force-push, or otherwise alter published history unless the user explicitly asks for that operation.
- Preserve unrelated user changes.
- Commit messages must describe the delivered feature or fix in durable product terms, not implementation chronology or agent activity.
- Before finishing a change, check `git status` and report any intentionally uncommitted files.
- Use Conventional Commit-style subjects where they fit naturally, such as `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, or `ci:`.
- Do not use implementation-stage numbers, planning references, WIP/checkpoint wording, session provenance, or temporary milestone names in commit subjects or bodies.
- Preserve meaningful feature boundaries in history. Do not squash unrelated changes together merely to make the history shorter.
