# Approved gates execute via an app-level reaction (the PR-open executor)

Status: accepted (Dan, 2026-06-19) · builds on [`0006`](0006-gate-engine-event-sourced.md), [`0007`](0007-routing-stays-out-of-the-kernel.md), [`0021`](0021-member-codes-guard-agent-surfaces-only.md), [`0025`](0025-align-terminology-on-anthropic-managed-agents.md), [`0026`](0026-routing-policy-resolves-to-auto-or-gate-the-autonomy-envelope.md) · completes the code-PR push-gate (the ForgeAdapter PR-open deferred in ledger `ba64074b`)

The code-PR push-gate records an **"Open the PR" Proposal** on a satisfied Outcome (slice 3), and the worker has committed onto a `junto/<session>` branch (slice 2). But junto has **no "approved gate → perform the action" loop**: approving a `Proposal` only folds its `GateStatus` to `Approved` during projection (`0006`) — a *recorded* approval, never an *executed* one. This ADR decides how an approved gate causes junto to actually push the branch and open the pull request.

## The reaction is app-level, not a kernel mechanism

The executor lives in the **app (the `junto` bin), in the approve handlers** (the web `verify` POST and the MCP `approve` tool): after recording an `Approval` and re-projecting, if the now-`Approved` proposal is a recognized actionable gate, the handler runs the action. It is **not** a kernel feature.

- **The kernel stays generic** (`0007`): "open a PR" is code-PR-specific; a kernel gate-executor would drag playbook logic into L0. The kernel keeps doing exactly one thing — folding approvals into `GateStatus`. The *meaning* of an approved gate (and what to do about it) belongs to the Playbook, executed at the app layer.
- This is the concrete, pre-autonomy-envelope form of `0026`: today a human approves and the app reacts; later, inside a ratified Routing-Policy region, an `Auto` requirement resolves the same gate without the human and the same executor runs. The executor is written once; what *flips the gate to Approved* (a human vs `Auto`) is the part `0026` governs.

## v1 fires on local approvals only

The reaction runs in the handler of **the host whose surface recorded the approval** — not on approvals that arrive later by sync. This is deliberate:

- It avoids **multi-host double-open**: if every host that ever syncs the approval tried to open the PR, two machines would race to `gh pr create`. Firing only on the local approve gives exactly one executor.
- The approver is the human at a junto surface; their host has the workspace and the forge auth. That is the right machine to act.

The cost: an approval made on machine A never triggers machine B's executor. For the single-machine dogfood that is a non-issue; the general form — a reconciliation **sweep** with explicit ownership/idempotency (like `0028`'s lineage reconciliation) — is **deferred** until multi-machine gate execution is real.

## Recognition and state recovery

- **Recognition** is by a **playbook-owned action-string prefix** — the code-PR Playbook owns proposals whose `action` begins with a known constant ("Open a pull request for this verified deliverable…"). The kernel's `Proposal.action` stays a free string (`0006`/`0019`); the app, not the kernel, knows the prefix. (A structured proposal *kind* is the eventual shape; the string prefix is the rule-of-three-honest v1 with one Playbook.)
- **State is recovered from the workspace + git, not from a proposal↔session key.** The executor takes the gate's **channel**, looks up its **workspace** (`workspace_for`, `0023`), reads the workspace's current `junto/<session>` **branch** and the **base** recorded in git config at branch creation (`branch.<b>.juntoBaseSha` plus a `juntoBaseRef` for the PR base). No new entry kind, no parsing of the display string, no proposal-to-session link beyond "this channel's workspace is on a junto branch with an approved open-PR gate." (v1 assumes one Outcome session per workspace at a time — concurrent sessions per workspace are out of scope.)

## Idempotency, recording, failure

- **Idempotent.** The executor no-ops if the session is already `Done`, and `gh pr create` itself refuses a second PR for the same branch — so a double-click or retry cannot open two PRs.
- **The PR URL is the deliverable.** On success the executor records an `ArtifactAttached` (kind `"pull-request"`, provenance = the PR URL) and a `SessionUpdated → Done` ("opened PR `<url>`"), so the durable record links to the outward result.
- **Capability-gated.** A PR-open gate is only worth offering when a forge can honor it; the satisfied arm checks `GithubForge::is_available()` (the `0021`-style capability probe, constraint #4) and falls back to `Done` (verified, no PR) when no forge is available — so the gate is never a dead end.
- **Failure surfaces, no auto-retry (v1).** If push or `gh pr create` fails (no `gh`, auth, network), the executor records a `SessionUpdated` note ("opening the PR failed: …") and leaves the gate approved; re-running the approve reaction retries. A first-class retry affordance is deferred.

## Considered and rejected

- **A kernel gate-executor.** Uniform "approved → act" in L0 — rejected: it puts playbook-specific action logic in the kernel, violating `0007`.
- **A background sweep on sync + startup** (like `0028`). More resilient to synced/remote approvals — rejected for v1: multi-host execution needs ownership + idempotency to avoid double-open; revisit when cross-machine gate execution is a real requirement.
- **Encoding session/branch/base in the proposal payload** and parsing it back. Self-contained — rejected: parsing a human-readable `action` string is fragile, and the workspace + git config already hold the truth.
