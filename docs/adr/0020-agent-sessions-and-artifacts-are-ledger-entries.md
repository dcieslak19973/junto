# Agent Sessions and Artifacts are ledger entries

Status: accepted (Dan, 2026-06-11) · builds on [`0003`](0003-ledger-entry-content-model.md), [`0005`](0005-provenance-ref-uri-plus-digest.md), [`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md)

The domain model names **Agent Session** (one agent execution → Artifacts, with live state) and **Artifact** (a verifiable output, not scrollback) as kernel nouns, but left their representation open. Decided: both are **ledger entry kinds**, reusing the subject/act/fold pattern everything else uses — not a separate live-state store beside the ledger.

- **`SessionStarted`** — a *subject* entry (like `Assertion`/`Proposal`): its id is the session's identity, the envelope's author is the agent, the payload is the session's `intent`. The session starts in the `Working` state implicitly.
- **`SessionUpdated`** — an *act* targeting a `SessionStarted` entry, carrying the new `SessionState` (`working / blocked / awaiting-approval / done / error` — the domain model's states) plus a note. Current state is **derived by folding**, last-applicable-wins, exactly like assertion standing (0002's projection discipline). `Done`/`Error` are terminal by convention, not enforcement.
- **`ArtifactAttached`** — an *act* targeting the producing session, carrying the artifact's `kind`, `description`, and **provenance refs (URI + optional digest, 0005)**. The entry's id is the artifact's id; the session's artifact list is derived during projection.

This extends 0003's closed `kind` set the same way 0016 did — closed means *enumerated and decided*, and this ADR is the deciding.

## The two constraints that shaped it

- **Artifact *content* never enters the ledger.** The record holds decisions/intent + provenance + digests, not blobs or transcripts (hard constraint #3). An Artifact in the ledger is a *claim about an output* — where it lives, what it is, its digest so drift is detectable — and the content stays wherever provenance points (a git object, a file, a URL). This keeps entries small, sync cheap, and the substrate honest.
- **Artifact `kind` is a string, not a kernel enum.** The kernel ↔ playbook seam (constraint #5) assigns *artifact kinds + renderers* to Playbooks. A closed kernel enum (`Diff | Log | Memo …`) would smuggle playbook vocabulary into the kernel; a free string lets a research playbook attach a `query-result` without a kernel change. Renderers can dispatch on the string; unknown kinds render generically.

## Why entries, not a live-session store

Same argument as 0016: one durable mechanism. Session entries sync, merge, order, dedup, and respect membership (0017) exactly like every other entry — a teammate's machine sees your agent's session and its artifacts after a `sync`, for free. A separate live store would re-solve sync and merge for a second shape, and the "live" state junto actually needs (is the agent working? blocked on what? what did it produce?) is exactly what a fold over a handful of entries yields. True high-frequency liveness (token streams, progress ticks) is **Event**-layer territory — explicitly out of the ledger (constraint #3), deferred with the Event noun itself.

## Considered

- **Sessions as host-memory state, only artifacts recorded** — rejected: the record loses *what ran and why* (provenance wants the session), and state would die with the host process.
- **A kernel `ArtifactKind` enum** — rejected for the seam violation above.
- **One `SessionClosed` act instead of a full state enum** — rejected: `blocked` and `awaiting-approval` are the states the attention surface (docs/attention.md) routes on; collapsing them loses the signal this slice exists to surface.
