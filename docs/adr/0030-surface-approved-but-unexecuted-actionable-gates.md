# Surface approved-but-unexecuted actionable gates

Status: accepted (Dan, 2026-06-19) — **designed, not yet implemented** · builds on [`0006`](0006-gate-engine-event-sourced.md), [`0029`](0029-approved-gates-execute-via-an-app-level-reaction.md) · motivated by the first push-gate dogfood (ledger `fad507c0`)

`0029` made an approved gate *execute* an action (open the PR) via an app-level reaction. The first dogfood exposed the failure mode of that decoupling: **approval and execution are separate, and nothing independent notices when an actionable gate was approved but never acted on.** A stale host binary meant the executor wiring wasn't in the running process — the approval recorded, `GateStatus` went `Approved`, and *nothing happened, silently*. The same silence covers every cause: the wrong host saw the approval, no executor handles that `kind`, or the executor crashed mid-run. The record looked resolved; the action never occurred.

The trap is structural: **you cannot rely on the executor to report that the executor didn't run.** The signal has to come from somewhere that doesn't depend on the action firing.

## Decision — make the gap a projected, surfaced state

Two parts, both independent of whether any executor runs:

1. **Projection surfaces the gap.** A gate that is (a) `Approved`, (b) carries an executable `kind` tag (`0029`), and (c) has **no recorded execution outcome referencing it** is a distinct attention state — *"approved, awaiting execution"* — shown on the focus board / attention projection, separate from a plain pending gate. Because it is derived from the *absence of an outcome*, it catches every silent case regardless of which host, binary, or executor was involved. It only **displays** — it never acts — so it is safe to compute on any host (no multi-host double-open risk, unlike auto-execution).

2. **The executor records its outcome against the gate.** On success *and* failure, the executor appends an entry that references the gate (the proposal id), not just the session. Success carries the result (e.g. the PR URL); failure carries the reason. This makes the "has an outcome?" check in (1) exact, and makes a failure as first-class and visible as a success (today a failure only leaves a loose session note).

Together: an actionable gate is never "done" merely by being approved — it is done when an outcome is recorded against it, and until then the gap is loud.

## Deferred — a sweep that re-attempts

`0029` already deferred a startup/sync **reconciliation sweep** that *re-executes* surfaced gates (which would have auto-healed the stale-binary case on the next host start). That remains deferred for the same reason: auto-execution needs **ownership** (only the host that owns the gate's workspace acts) plus **idempotency** (the recorded outcome from part 2 + `gh`'s own duplicate-PR guard) to avoid two hosts opening two PRs. Surfacing (this ADR) is the safe half and ships first; re-attempting is the riskier half and waits until cross-machine execution is a real requirement.

## Considered and rejected

- **Rely on the executor to report it didn't run.** Impossible by construction — the code that isn't running can't report its own absence. Hence detection by projection, not by the executor.
- **A host warning when its binary is older than source.** Addresses one *cause* (the stale binary that bit the dogfood) but not the *class* (wrong host, unhandled kind, crash). Worth doing as dev-ergonomics, but it is not the fix.
