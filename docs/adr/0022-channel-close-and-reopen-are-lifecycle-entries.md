# Channel close and reopen are lifecycle entries; closed means out of the working set

Status: accepted (Dan, 2026-06-12) · extends [`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md) · builds on [`0004`](0004-any-member-may-author-any-entry.md), [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md)

0016 anticipated *close* as the lifecycle family's next member. Decided: two new entry kinds, **`ChannelClosed { rationale }`** and **`ChannelReopened { rationale }`**, folding into a derived `closed` flag — last applicable wins in canonical order, members only, exactly the projection discipline everything else uses.

## What closed means

**The record outlives the inquiry.** Closing deletes nothing and hides nothing — it moves the channel out of the *working set*:

- It leaves the sidebar and the focus board (a closed channel demands no attention); its index card drops into a collapsed "closed channels" archive.
- Its page leads with a closed banner and offers *reopen*; its brief warns agents not to record new work without reopening.
- Its **name binding stays live**: a closed channel still resolves, and its name stays taken within the substrate (rename the closed channel if you want the name back). Releasing names on close was rejected — a later channel silently inheriting a dead channel's name would corrupt the human reading of old references.

## Decisions in the small

- **Any member may close or reopen** (0004's posture: authority lives at gates, not in authorship). If misuse appears, the fix is a gate on the act, not founder-gating the kind.
- **Reopen exists from day one.** A close without an inverse would make a misclick irreversible-by-design; the inverse is one more variant in the same fold. Last-applicable-wins keeps replicas convergent under sync, like standings.
- **No tombstone semantics.** Entries can still arrive by sync into a closed channel and project normally (convergence over strictness, as with membership in 0017); "closed" is a surface posture, not a write lock. The write surfaces may warn or refuse — that is their call, not the kernel's.

## Considered

- **Close as a Correction targeting the genesis** (the rename mechanism) — rejected: rename corrects a *binding*; close changes the channel's *state*. Overloading Correction with both would make the genesis's correction history unreadable.
- **Founder-only close** — rejected for symmetry with 0004; revisit alongside the Role/authority question (domain-model open question) if it bites.
- **Releasing the name on close** — rejected above.
