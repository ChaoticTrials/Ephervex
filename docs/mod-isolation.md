# Mod isolation

Mod isolation narrows a reproducing problem to a smaller dependency-safe mod
configuration. It changes one client or local server in place while the session
is active, so Ephervex captures a safety snapshot before the first round.

## Desktop workflow

Open a client's or server's **Isolation** tab and choose **Set up isolation**.
Select any mods that must stay enabled throughout the investigation. Ephervex also keeps everything those mods require enabled.

The setup screen requires a separate acknowledgement when a required dependency
is missing. If dependency information can be refreshed but has not been
recorded, refresh it first or explicitly continue with reduced confidence.
Either acknowledgement marks the result as unproven. Jars that cannot be
identified can still be tested, with the same caveat.

For each round, launch the host using the exact enabled configuration shown in
the Isolation tab. Then answer **Still failing** or **Issue gone**. The active
workflow area keeps the current round and common answers available while
switching among Isolation, Logs, Mods, and the other host surfaces. **Detailed
view** returns to the complete Isolation surface without closing Logs or
changing the session. **Undo answer** restores the preceding round when an
answer was wrong. A contradiction stops the search until the latest answer is
undone or the session is canceled.

When a result is ready, **Finish** leaves that reproducing configuration enabled
and keeps the pre-isolation snapshot in the Snapshots tab. **Cancel isolation**
restores the exact pre-isolation state instead. A client session never changes a
linked server; isolate that server separately when its mods are also relevant.

Ephervex must be stopped for the affected host before it starts, advances,
undoes, finishes, or cancels a round. The workspace header continues to provide
the client's launch/stop action or the server's start/stop/restart actions while
the Isolation tab is open. Launching while a round waits for an answer remains
available.

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
