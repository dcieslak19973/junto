# junto reconciles against external forge state (it isn't the only actor)

Status: accepted (Dan, 2026-06-19) — **principle, build deferred** · builds on [`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md), [`0026`](0026-routing-policy-resolves-to-auto-or-gate-the-autonomy-envelope.md), [`0027`](0027-channel-lineage-is-diverge-converge-edge-entries.md), [`0028`](0028-eventually-consistent-lineage-reconciliation.md), [`0029`](0029-approved-gates-execute-via-an-app-level-reaction.md)/[`0030`](0030-surface-approved-but-unexecuted-actionable-gates.md) · surfaced by the first real diverge/converge dogfood

junto's value rests on a **verified, provenance-bound record**. But that premise has a crack the first lineage dogfood exposed: **consequential actions happen outside junto.** A side-quest of `junto-dev` produced PR #56; the operator **merged it in GitHub** — where the merge button is — and junto never saw it. junto's record drifted: the side-quest still showed *open and unconverged* while the work had shipped. The record was lying, and nothing inside junto could notice, because the truth-changing act bypassed it.

The wrong responses are the two extremes:
- **Accept the drift** (junto is merely advisory) — rejected: silent drift is the failure of the whole verified-record thesis.
- **Demand every action flow through junto** (no merging in GitHub) — rejected: unrealistic and user-hostile; you merge where the button is. Constraint-fighting the operator loses.

## Decision

**junto reconciles against external forge state.** For facts the forge *owns* (a PR merged / closed), the **forge is the source of truth**, and junto catches its own record up rather than assuming the action passed through it or tolerating drift. Concretely (when built): on **sync** (+ startup), junto polls the forge for the state of the PRs it opened — the `GateExecuted` PR URLs (`0030`) — and a **merged** PR auto-**converges** (or surfaces for convergence) its side-quest, so the lineage record matches reality. **Polling fits junto's posture** (localhost, no public endpoint — `0012`): it shells out to `gh`, the way the substrate shells out to `git`. Webhooks are rejected for now (they need an inbound endpoint junto deliberately doesn't run).

## This is the third reconciliation case — and it clarifies the family

We parked the "self-healing sweep" unification as premature at two cases (junto-dev ledger `4dd4ccb9`), with the trigger: *revisit at the third.* It's here, and it resolves the question by **direction**:

- **Egress** — junto pushes *its own* pending work outward: lineage far-side reconciliation (`0028`, built) and gate-execution retry (`0029`/`0030`, deferred).
- **Ingress** — junto pulls *external truth* in: forge-state reconciliation (this ADR).

So the three do **not** share a body — egress (idempotent ledger writes junto authors) and ingress (reading a forge it doesn't control) are different mechanisms. What they share is narrower and real: a **reconciliation pass that runs on sync + startup**, under the same invariants — **idempotent** and **ownership-bounded** (only the host that owns the work/workspace acts, so two hosts don't double-converge or double-open; `0029`). The measured extraction, *if and when convenient*, is that **thin shared trigger** (a registry of reconcilers run on sync/startup) — **not** a grand `Reconciliation` framework. This supersedes `4dd4ccb9`'s premature "family" framing: at three cases the taxonomy is clear, and the conclusion is "thin trigger, bespoke bodies."

## Deferred

The build itself. The principle is settled; the implementation waits until the drift bites more than once (the first instance — PR #56 — was reconciled by hand). Open when built: which forge facts to reconcile first (merged/closed PRs), how convergence is authored when a *human's* external merge triggers it (the workspace-owning host acts, but on whose authority?), and the `ForgeAdapter` capability for reading PR state (`gh pr view` for GitHub; capability-flagged per constraint #4).
