# Routing Policy resolves to auto-or-gate: the autonomy envelope

Status: proposed (Dan, 2026-06-14) · **builds on** [`0007`](0007-routing-stays-out-of-the-kernel.md) (realizes its 🔮 future Routing Policy seam — does **not** reverse its kernel stance) and [`0006`](0006-gate-engine-event-sourced.md) · **extends** [`0025`](0025-align-terminology-on-anthropic-managed-agents.md) (gives the renamed *Routing Policy* + *Rubric/Grader* a concrete resolution shape) · informed by directional input ([PostHog "what if your product built itself"](https://youtu.be/zMiSRliEzv4), and the broader "product that improves itself" market — ACE)

A human authorizes **bounded unattended autonomy** by ratifying the Routing Policy that governs a region of work. Inside that region, a Grader-`satisfied` Deliverable **auto-resolves its Gate and emits a notification** ("send release notes on steps taken") instead of pausing for synchronous approval. Outside it, the Gate pauses for a human as today. This is the concrete mechanism the self-improvement Playbook has been hand-waving at ("raise autonomy for low-risk, eval-trusted changes — but only as evals earn trust", `../self-improving-harness.md`).

The "autonomy envelope" is **not a new noun**. It is the region of the Routing Policy's input space that resolves to the `Auto` `ApprovalRequirement`. The Routing Policy's output space — already "compiles down to an `ApprovalRequirement`" per 0007 — simply *uses* the `Auto` arm that 0007 already defined, paired with a mandatory notification.

## What this changes vs. what it reuses

**Reused, unchanged (0006/0007):**
- The kernel still only **executes** an `ApprovalRequirement` (`Auto | Count(u32) | AllOf(Vec<Member>)`); it never decides the path. Routing stays out of the kernel (constraint #5).
- `Auto` already exists. The kernel auto-resolving a gate when handed `Auto` is **not new** — 0007 shipped that arm.

**New (this ADR):**
1. **The Routing Policy's resolution is grade-aware.** Its inputs are `(target/region · grade · risk tier)`; `Auto` is reachable **only** when a Grader has returned `satisfied` against the region's ratified Rubric. The Routing Policy *reads* the grade; it does not compute it.
2. **`Auto` carries a notification obligation.** An `Auto` resolution is incomplete without a recorded notification entry (the "release notes": what was done, by which Agent/Session, under which ratified policy version, with provenance). Async accountability is the price of dropping synchronous approval — and the substrate for revocation.
3. **The autonomy-granting portion of a Routing Policy is human-ratified data**, addressable/importable via the future policy provider (0007's `RubricProvider`-shaped seam). **Ratifying it is the consent act** — the human "defines the rubric that defines the space."

## The flow

```
consequential action
  → Grader scores Deliverable vs the region's Rubric        → satisfied | needs_revision | failed   [playbook]
  → Routing Policy maps (region, grade, risk) → ApprovalRequirement                                  [playbook — out of kernel]
        satisfied ∧ inside ratified auto-region   → Auto
        otherwise                                 → Count(n) | AllOf(members)
  → kernel executes the ApprovalRequirement (0006/0007)
        Auto → auto-resolve the gate  +  record a notification entry (release notes)                 [kernel/host]
```

## The two safety invariants (non-negotiable)

1. **No self-widening.** The auto-region of a Routing Policy may **never** cover edits to a Routing Policy (its own or another's). Widening an envelope is itself a consequential action and **always** resolves to a human `ApprovalRequirement`. This is `../self-improving-harness.md`'s "immutable safety core the loop can't touch" expressed structurally — and the anti-runaway guard that lets autonomy escalate only as evals earn trust (the harness *proposes* a wider envelope; a human gates that one meta-decision).
2. **Grade and consent stay separate inputs.** "The Grader returned `satisfied`" is a *fact about the Deliverable*; "this region may proceed unattended" is *ratified policy*. The Routing Policy reads both; the grade never *grants* the autonomy. Collapsing them re-creates the broken gate where an agent self-authorizes by passing its own Grader (the self-preference failure `../self-improving-harness.md` is built to avoid).

## Why this is the right home (vs. a new primitive)

Considered: model the autonomy envelope as a standalone kernel noun (a "standing gate" / "mandate"). Rejected — it duplicates what 0007's `ApprovalRequirement.Auto` + a ratified Routing Policy already express, and it would put policy in the kernel (constraint #5). The Routing Policy is *the* playbook-specific routing layer by definition; "may this run unattended" is a routing question, so it belongs there. (Dan, 2026-06-14.)

## Consequences

- **It strengthens, not weakens, junto's governance thesis** (0025): the Gate is not removed, it is made **programmable** — the human pre-commits the decision for a region under audit, rather than clicking approve per event. Authority still originates with the human. junto delegates a bounded region under audit; it does not (like a pure autonomy harness) remove the human.
- **Kernel-general, not self-improvement-only.** The same mechanism gives a future `prod-troubleshooting` Playbook "auto-ship a fix behind a feature flag if it's not risky" for free — the flag rollback is the envelope's safety net.
- **The notification entry is the attention-economy payoff** (`../self-improving-harness.md` *Driving objectives*): it **moves** human attention from before-the-fact approval to after-the-fact audit + revocation, relieving the synchronous bottleneck without blinding the human.
- **No immediate kernel work.** `Auto` exists; the notification entry is a small host/kernel addition; the grade-aware Routing Policy and the policy provider remain **unbuilt** and gated behind the rule of three (0007: build the provider only when ≥2 real playbooks prove the shape).

## Open (deferred to first implementation)

- Exact shape of the notification entry (new ledger `kind` vs. a `SessionUpdated`/decision-frame payload) — settle when the first Playbook needs it.
- Whether `risk tier` is a first-class Routing Policy input or folds into the region definition.
- Revocation/rollback ergonomics (kill-switch on an envelope; what happens to in-flight auto-resolutions).
