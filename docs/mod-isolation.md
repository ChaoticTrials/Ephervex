# Mod isolation

Mod isolation narrows a reproducing problem to a smaller dependency-safe mod
configuration. It changes one client or local server in place while the session
is active, so Ephervex captures a safety snapshot before the first round.

## Desktop workflow

Open a client's or server's **Mods** tab and choose **Isolate mods**. Select any
mods that must stay enabled throughout the investigation. Ephervex also keeps
everything those mods require enabled.

The setup screen blocks when a required dependency is missing. If dependency
information can be refreshed but has not been recorded, refresh it first or
explicitly choose **Continue with reduced confidence**. Jars that cannot be
identified can still be tested, but the result is marked unproven.

For each round, launch the host using the exact enabled configuration shown in
the dialog. Then answer **Issue still present** or **Issue gone**. The dialog can
be closed while testing; the workspace banner keeps the session visible and
opens it again. **Undo answer** restores the preceding round when an answer was
wrong. A contradiction stops the search until the latest answer is undone or
the session is canceled.

When a result is ready, **Finish** leaves that reproducing configuration enabled
and keeps the pre-isolation snapshot in the Snapshots tab. **Cancel isolation**
restores the exact pre-isolation state instead. A client session never changes a
linked server; isolate that server separately when its mods are also relevant.

Ephervex must be stopped for the affected host before it starts, advances,
undoes, finishes, or cancels a round. Launching while a round waits for an
answer remains available.

## Command-line workflow

The same durable workflow is scriptable. Name exactly one host with
`--instance` or `--server` on every command:

```bash
ephervex mod isolate start --instance <id> --keep mods/always-on.jar
ephervex mod isolate status --instance <id>
ephervex mod isolate answer --instance <id> issue-still-present
ephervex mod isolate answer --instance <id> issue-gone
ephervex mod isolate undo --instance <id>
ephervex mod isolate resume --instance <id>
ephervex mod isolate finish --instance <id>
```

Use `cancel` instead of `finish` to restore the pre-isolation snapshot:

```bash
ephervex mod isolate cancel --instance <id>
```

`start` refuses refreshable unrecorded dependency information by default.
`--accept-unrecorded` explicitly continues with a reduced-confidence result.
Pass `--json` before `mod` for structured session, round, and result output.

If Ephervex or the machine stops during a session, run `status` to inspect the
persisted round and `resume` to reapply its exact configuration before testing.
The session lives at `.launcher/isolation.json` inside the host until finish or
cancel succeeds.
