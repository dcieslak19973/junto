# The Gate engine is event-sourced into the Ledger

Status: accepted (Dan, 2026-06-09) · builds on [`0002`](0002-ledger-entries-are-immutable.md), [`0003`](0003-ledger-entry-content-model.md)

A **Gate** (the checkpoint a *consequential action* passes before it happens) records itself as **ledger entries**; a proposal's gate status is **derived by folding** — exactly like an assertion's standing ([`0002`](0002-ledger-entries-are-immutable.md)), never stored. Three kinds join the closed set ([`0003`](0003-ledger-entry-content-model.md)) — the gate engine is the concrete thing that forces them:

- **`Proposal`** — a *subject* kind (targets nothing, like `Assertion`): `{ action, rationale, provenance, requirement }`. **`action` is a generic, repo-agnostic descriptor** (a string for now) so a research-persona gate — which may have *no git repo* — behaves identically to a code-PR gate. The `requirement` — what the gate needs before approval — is recorded **on the entry**, so the gate's outcome is auditable from the log alone. (How a requirement is *chosen* — the routing decision — is a separate concern, deliberately kept out of the kernel; see the ADR index in [`../domain-model.md`](../domain-model.md).)
- **`Approval`** / **`Rejection`** — acts referencing a `Proposal` by `target`. Deliberately **distinct from `Ratification`/`Park`**: *approve/reject* pass-or-block an action *before* it happens; *ratify* confirms a recorded claim *after*. The glossary's verbs differ, so the kinds differ.

## Derived `GateStatus { Pending, Approved, Rejected }`

- Approvals accumulate by **distinct author email** (the same member approving twice does not stack).
- **Rejection is *sticky*** — any one rejection ⇒ `Rejected` regardless of approvals, and order-independently (a later approval does not revive it).

This deliberately **diverges from assertion `Standing`'s last-applicable-wins** rule ([`0002`](0002-ledger-entries-are-immutable.md)); both rules are documented and test-pinned. Dangling acts (target is no known proposal) are ignored leniently.

## Considered: non-sticky (last-wins) instead

We could have made gate status last-applicable-wins, consistent with assertion `Standing` — then a later approval would overturn a rejection and **no override kind would ever be needed**. Rejected because "a block is a block": a rejection should not be silently undone by a subsequent approval. The cost of that choice is the deferred override below.

## Consequences

- **Approver eligibility is unenforced.** The kernel has no notion of *who may act on a gate* — `Count(2)` means "two *distinct* approvals," not "two *authorized* ones," and any Member's `Rejection` blocks, including one not on the channel's Party (which is not a type yet). This is a deliberate deferral to a future **Party/ACL** + the Rubric layer (the routing ADR); recorded here so no reader assumes a guarantee the code does not make.
- ⚠️ **Deferred — administrative override to undo a rejection.** Because reject is sticky, reversing one needs an explicit, append-only-consistent act — likely a new **authority-bound** kind (e.g. `Override`) that the fold lets flip a `Rejected` proposal back to `Pending`/`Approved`. Not built: it interacts with reject-stickiness and wants an **authority/Role** concept (still open — see `../domain-model.md` Open questions).
