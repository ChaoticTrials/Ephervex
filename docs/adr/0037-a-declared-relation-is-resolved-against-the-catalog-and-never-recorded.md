# ADR 0037 - A declared relation is resolved against the catalog and never recorded

**Status:** Accepted (builds on ADR 0034, scoped by ADR 0022)

## Context

A recipe entry records the relations its catalog declared about that file
(ADR 0034). Most of those relations name a project id and nothing else, because
that is all CurseForge states and usually all Modrinth states. Matching them
against the host's own files is offline and exact: a relation whose project id
belongs to a jar in `mods/` is answered from the directory, and the mod's page
names that jar.

A relation no jar matches has nothing to be answered from. The recorded id is
the whole of what is known, so the mod's page shows the id and says the mod is
not installed here. That reading is wrong more often than it is right. Published
mods do declare project ids their own catalog does not answer for, and there are
more ways for a declaration to be unactionable than there are ways for it to be
a mod the user should go and fetch:

- the catalog has no such project, so nothing can ever be installed for it;
- the catalog has the project and no release for this host's exact Minecraft
  version and loader, so nothing can be installed *here*;
- the relation named a jar and no project at all, so there is nothing to look
  up (ADR 0034 keeps such a relation rather than inventing an identity for it);
- nothing has asked yet.

All four render identically as "not installed here", and only the case that is
missing from that list - a project with a release that fits - is one a user can
act on. A launcher that says "not installed here" about a project that cannot be
installed sends the reader looking for a mod that does not exist.

## Decision

The identity and availability of a declared project are resolved against the
declaring mod's own catalog, on demand, and nothing resolved is recorded.

**The catalog is the declaring entry's own.** A relation carries no provider
(ADR 0034), because a dependency graph never crosses catalogs. The project id
is resolved against the catalog of the file that declared it and against no
other.

**Four answers, not a boolean.** `ProjectAvailability` distinguishes
`Installable`, `NoCompatibleRelease`, `UnknownToCatalog` and `NotChecked`.
Collapsing them would put the launcher back where it started: an install action
may be offered for `Installable` alone, and the other three each say what they
are.

**An unreachable catalog is not an answer.** A missing client, a missing
credential, and a transport failure all produce `NotChecked`, never
`UnknownToCatalog`. This is the same distinction ADR 0034 draws between an
unasked relation and one answered as empty, and it exists for the same reason: a
confident wrong answer is worse than an incomplete one.

**It is a separate call from the offline one.** `App::relations_of` stays
offline, pure, and as cheap as a directory read, so a frontend can ask as often
as it likes and paint immediately. `App::declared_projects_of` reaches the
network and is asked for afterwards. A frontend that never calls it shows every
unmatched declaration as not looked up, which is true.

**Nothing is recorded.** The display cache is read for a title already there and
is never written to from here. That cache's departure from ADR 0022 was decided
for mods the user has installed; a project merely named by somebody else's
declaration is not one, and widening the departure to cover it has no reason
behind it. The resolved answers live as long as the view that asked.

## Consequences

- A declaration that cannot be acted on says so, and the wording never implies
  an install would supply it.
- Opening a mod's page makes one batched project lookup and one release listing
  per unmatched declared project, for that catalog only. A page with no
  unmatched declarations makes no request at all.
- The answer is not durable. Closing and reopening the page asks again, which is
  what keeps a project the author has since published, retired or renamed from
  being described out of a stale record.
- CurseForge's batch endpoint fails a whole batch containing one id it does not
  know, so the batch is retried one project at a time. That retry is what turns
  its refusal into a per-project `UnknownToCatalog` instead of losing the
  answers for the projects it did know.
- An install action for a declared dependency has exactly one precondition to
  check, in one place, rather than a rule each frontend derives for itself.
