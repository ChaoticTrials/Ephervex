# Mod isolation

Mod isolation narrows a reproducing problem to a smaller dependency-safe mod
configuration. It changes one client or local server in place while the session
is active, so Ephervex captures a safety snapshot before the first round.

## Desktop workflow

Open a client's or server's **Isolation** tab and choose **Set up isolation**.
Use the switches beside the candidate mods to pin any that must stay enabled
throughout the investigation. Ephervex also keeps everything those mods require
enabled. Canceling the setup returns
to the empty Isolation tab. A setup is only valid for the mods it was planned
from: enabling, disabling, adding, deleting, updating, or restoring mods,
restoring a snapshot of the host, or recording new dependency information,
discards a setup that has not started. Set up
isolation again to plan from the current mods. A change that fails or leaves the
mods as they were keeps the setup.

The setup screen requires a separate acknowledgement when a required dependency
is missing. If dependency information can be refreshed but has not been
recorded, refresh it first or explicitly continue with reduced confidence.
The Mods tab's refresh button records dependency information for every mod the
catalogs can identify, in one pass. Mods a catalog could not answer for stay
listed as incomplete.
Either acknowledgement marks the result as unproven. Both choices sit directly
above **Start isolation**. While Start waits on the user, the setup names what
it needs beside it: stopping the host or giving an acknowledgement. While a
requested start is in progress, Start stays disabled and the workspace's
progress banner names the current stage. Jars that cannot be identified can
still be tested, with the same caveat.

For each round, launch the host using the exact enabled configuration shown in
the Isolation tab. Then answer **Still failing** or **Issue gone**. The active
workflow area keeps the current round and common answers available while
switching among Isolation, Logs, Mods, and the other host surfaces. **Detailed
view** returns to the complete Isolation surface without closing Logs or
changing the session. **Undo answer** restores the preceding round when an
answer was wrong. A contradiction stops the search until the latest answer is
undone or the session is canceled.

Every enabled mod that is not pinned, and not required by a pinned mod, can be
isolated, including one another mod requires. Mods that require each other are
tested as one candidate. A
round never enables a mod without everything it requires, so a dependency can
be tested without the mods requiring it, but not the other way around. The
result lists the remaining candidates that no other remaining candidate
requires. Mods they require are listed separately, as are mods that were
pinned throughout and the mods those pinned mods require. Pinned mods and their
requirements were never tested for removal, so a remaining candidate is not
presented as the sole cause. **Finish** leaves the smallest tested configuration enabled
and keeps the pre-isolation snapshot in the Snapshots tab. Until the finished
result is closed, the host's banner says the tested configuration is still
enabled, whichever tab is shown. **Cancel isolation**
restores the exact pre-isolation state instead.

A client session lists its linked servers and leaves them alone unless told
otherwise. For a problem that needs both sides, turn on **Keep linked servers in
step with each round** on the setup screen. Each installed linked server then
enables a mod exactly when the round enables the same project on the client, so
every round tests the same mods on both sides, and the Isolation tab shows what
each server was set to. Launch the client and the server for each round. A
server mod follows a client mod only when that client mod takes part in the
search and the server had the mod enabled. Every other server mod, such as one
only the server has, one whose client mod is disabled or ignored, or one the
server had disabled, stays as it was and is not part of the search; the result
names those mods as untested. Each server gets its own safety snapshot, and
cancel restores every server from it. While the session is active the server
refuses mod changes, snapshot restore, linked mod sync, and a session of its
own, it cannot be deleted, and its Isolation tab says which client's session
owns it. Servers whose
dependency information is incomplete need the same acknowledgements as the
client. A server whose folder cannot be reached when the session ends, such as
on an unmounted drive, is skipped by cancel and finish: the session ends and
names the server as not restored or not set to the result, and its safety
snapshot keeps its starting state. Isolate a server that is not kept in step separately when its mods are
also relevant.

The affected host must be stopped before a session starts or resumes, or a round
is answered, undone, finished, or canceled. When linked servers are kept in
step, they must be stopped too. The workspace header continues to provide
the client's launch/stop action or the server's start/stop/restart actions while
the Isolation tab is open. Launching while a round waits for an answer remains
available. While Ephervex is starting the session, applying or restoring a round,
finishing, or canceling, the Isolation tab says which step is running, and the
answer, undo, finish, cancel, and launch controls for that host wait until it
completes.

The saved session, not the mods folder, decides what a round contains. Whenever
the desktop shows an active session, it first checks that the host's mods match
the round and reapplies them if they do not, for example after Ephervex closed
in the middle of applying a round. If the host is running, or reapplying fails,
the round cannot be answered, undone, or finished until **Try again** succeeds;
stop the host or correct the problem first. The session and its safety snapshot
are kept either way.

The safety snapshot cannot be deleted while its session is active. If it is
already missing, finishing still completes, and canceling re-enables the mods
that were enabled when the session started before ending the session.

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
`start` also refuses when a required dependency is missing. The CLI has no way to
continue past that; install the dependency first, or use the desktop setup,
which can acknowledge it.
`--linked-server <id>` (repeatable, client sessions only) keeps that linked
server's mods in step with each round, as described above; every command that
prints a round then also prints what each server was set to, and `status --json`
includes the same in `server_configurations`.
Pass `--json` before `mod` for structured session, round, and result output.

If Ephervex or the machine stops during a session, run `status` to inspect the
persisted round and `resume` to reapply its exact configuration before testing.
`answer` refuses while the host's mods differ from the round, since the answer
would describe a configuration that was not tested.
While a session is active, every other change to the host's mods is refused
(install, update, remove, add, delete, enable, disable, repair, snapshot
restore, and loader changes), because the session decides which of them are
enabled. Finish or cancel the session first.
The session lives at `.launcher/isolation.json` inside the host until finish or
cancel succeeds.
