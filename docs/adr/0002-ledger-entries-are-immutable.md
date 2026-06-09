# Ledger entries are immutable; correct by appending

Status: accepted (Dan, 2026-06-09) · builds on [`0001`](0001-ledger-is-the-durable-record.md)

A ledger entry is written **once and never mutated**. To ratify, park, falsify, or correct one, you **append a new entry that references the prior one**; an entry's *current* verification standing is **derived by folding the log** (an event-sourcing projection), never stored on the entry. **Anchor: an accounting ledger** — you don't erase a posting, you record a correcting/adjusting entry.

## Why

- It is the only model consistent with the **append-only, partition-by-author** git-refs substrate (see `../architecture.md`): the ratifier is frequently a *different* author than the original recorder, so in-place mutation is impossible by construction.
- The ratification/correction append carries its own author + timestamp + rationale, so *who* changed an entry's standing and *why* is itself recorded — not lost to a field overwrite.

## Consequences

- `verification_state` is **not** a mutable field. Standing (`Provisional → Ratified | Parked | Superseded`, and gate status) is computed by [`Ledger::project`] folding the entry stream.
- The projection's conflict rule (last-applicable-wins for assertion standing) is a consequence of this model.

## Prior art (clean-room inspiration only)

git-bug's append-only *operations* folded into entity state (junto's cited substrate prior art); event sourcing / compensating events; ADRs with *superseded-by* links; OSF / clinical-trial **pre-registration** (the research playbook's hypothesis ledger is this same shape — a provisional claim recorded *before* its evidence).
