# Worked example — production troubleshooting as a Playbook

> **This is a tracer bullet, NOT a spec.** It walks *one* non-coding Playbook end-to-end to make junto's abstractions concrete and to stress-test the core thesis. Per junto's own **rule of three**, the Playbook schema and kernel-primitive list below are **candidates / conjecture** — we only learn the real seam after the *research* and *code-PR* playbooks are walked too. Don't crystallize a framework from this single example.

## Why this playbook first: it's the adversarial case

junto's central bet is **"deliberate before any consequential action."** Production incidents are the one domain where you *can't*: recovery is measured in minutes (MTTR), and **every gate adds latency to recovery.** This collides head-on with two of junto's load-bearing claims:

1. The thesis itself (async deliberation before acting).
2. The harness doc's anti-rubber-stamp principle — incident time-pressure is *exactly* when humans approve without reading.

So if junto's model survives here, it survives anywhere. A tidy walkthrough where the gate calmly fires would just confirm priors. This one confronts the tension — and in doing so it's secretly a test of **Open Decision (a)** (*is async deliberation sufficient, or do we need a live surface?*) run on the case most likely to break it.

## The resolution: deliberation splits in time (act-then-ratify)

The fix is **not** "deliberate faster." It's to **split deliberation across two loops:**

- **Fast loop (minutes) — stop the bleeding.** Take *reversible, pre-authorized* actions with a near-zero gate. Capture everything (commands, outputs, decisions) as provenance — but don't block recovery on human deliberation.
- **Slow loop (hours/days) — ratify + learn.** The *real* deliberation — root-cause confirmation, "was that the right call?", the durable verified record — moves into the **after-action review (AAR)**, when time pressure is gone.

This keeps junto's durable-verified-record value **without imposing deliberation latency on recovery.** It's a genuine adaptation of the Franklin/Maggie thesis for high-tempo work, not a betrayal: the Junto still deliberates and records — just at the tempo the work allows. **The provenance capture during the fast loop is what makes the slow-loop deliberation possible** (you can only ratify a decision you have the evidence for).

## The centerpiece: gate by reversibility × blast-radius × pre-authorization

Binary "needs sign-off?" is wrong for incidents. This playbook routes actions on **three axes** — and *this routing is where troubleshooting diverges from code-PR* (which barely needs reversibility classification):

| Action | Reversibility | Blast radius | Pre-authorized? | Gate during incident |
|---|---|---|---|---|
| Rollback to last-known-good | reversible | scoped to one service | yes (runbook blesses it) | **none** — auto-proceed, log it |
| Scale up replicas | reversible | low | yes | **none / notify** |
| Flip a feature flag off | reversible | medium | runbook-dependent | **lightweight** (one approver, async-OK) |
| Restart / drain a node | mostly reversible | medium | runbook-dependent | **lightweight** |
| Kill a long-running query | semi-reversible | medium-high | no | **commissioner/SRE approve** |
| Data mutation / schema change | **irreversible** | high | **never** | **hard-gated** even mid-incident |
| Failover to DR region | high-blast, hard to undo | very high | no | **hard-gated** |

Rules:
- **Pre-authorization is the latency-killer.** A runbook that pre-blesses "rollback-to-LKG for service X" turns a gate into a logged auto-action → zero recovery latency, full provenance. The *deliberation already happened* — when the runbook was written and reviewed (calmly, in advance). This is the fast-loop's escape valve, and it's itself a durable artifact the slow loop improves.
- **Irreversible/high-blast stays hard-gated even under MTTR pressure** — that's the floor that pressure must not erode (the harness anti-rubber-stamp principle, made literal).
- **The investigator agent is read-only by default.** It *proposes* remediations; it does not execute write/live-system actions. Execution is a human-approved (or pre-authorized-runbook) action. This bounds blast radius of an agent error and keeps a human accountable for live-system change.

## The scenario

`checkout-service` p99 latency spikes and 5xx errors climb, ~6 min after deploy `abc123`. On-call engineer (human) + an investigator agent; an SRE is the approver for any non-pre-authorized live action.

## Walk-through (the shared spine, this playbook's shape)

1. **Intake / trigger.** An alerting webhook (a **Connector**, "inbound trigger" mode) auto-spawns a channel of playbook=`prod-troubleshooting` and an investigator agent. Channel `party` = on-call + the service's owners. The triggering alert + recent deploys are attached as initial context.

2. **Frame the inquiry.** The question: *"why did checkout p99/5xx spike, and how do we safely restore service?"* This playbook's framing step = a triage checklist (what changed? scope? user impact? recent deploys? dependency health? rollback-safe?). The checklist seeds the hypothesis ledger.

3. **Agent-augmented investigation (read-only, fast).** The agent runs diagnostics in **headless PTYs** — metric queries, log searches, `kubectl` describes, the `abc123` diff. **Terminal-less:** the human never sees a shell; output is rendered as **artifact cards** (a latency chart, a 5xx-by-endpoint table, the suspect diff). Each finding is **provenance-bound** to the exact query + time window that produced it — re-runnable, not narrated.

4. **Hypothesis ledger.** Competing causes tracked with their evidence and *search breadth*: `bad deploy abc123` (N+1 query added) vs `DB connection-pool exhaustion` vs `upstream dependency timeout`. Tracking breadth resists first-plausible-cause and feeds the slow-loop's "did we look widely enough?".

5. **Act — fast loop.** Evidence points at `abc123`. **Rollback-to-LKG is reversible + pre-authorized → auto-proceeds, logged, party notified.** Bleeding stops. *No deliberation gate was imposed on recovery* — but every step is captured.

6. **Ledger entry (provisional).** A provisional root-cause claim is written — *"likely root cause: N+1 query in `abc123`; evidence: [bound artifacts]; action taken: rollback @ 14:32"* — explicitly marked **un-ratified** (fast loop only).

7. **Ratify + record — slow loop / AAR.** Post-recovery, with no time pressure: members confirm/refute the root cause against the bound evidence, ratify (or correct) the ledger entry, and judge *"was rollback the right first move? did we miss a faster signal?"*. The ratified AAR is the **durable verified record** — the thing that outlives the incident (Franklin's "crawling together").

8. **Self-improving loop — and why it's HARDER here.** The AAR proposes a runbook/skill update (e.g., "add an N+1 detector to the deploy check", or "pre-authorize flag-flip for this failure signature"). **But the held-out eval that works for code (run the tests) is weak here — you cannot cheaply replay a production incident against a candidate runbook.** THE CRUX gets *harder* in this playbook, not easier. Honest options, none clean: (a) replay against *recorded* incident telemetry (approximate); (b) shadow/game-day exercises (expensive); (c) longer-horizon outcome tracking (did MTTR for this signature drop over the next N incidents? — lagging + sparse, like the harness doc's bug-instability signal). Don't draw the AAR→eval→promote arrow as if it's solved. **Dead-end hypotheses are kept first-class** — institutional memory of what it *wasn't*.

## Candidate Playbook declaration (🔵 conjecture — pending the other two playbooks)

```
prod-troubleshooting:
  lifecycle:   triggered → investigating → [acting] → recovered → ratifying(AAR) → closed
  gates:       per-transition, routed by reversibility × blast-radius × pre-authorization
               (NOT binary). pre-authorized+reversible ⇒ no gate; irreversible/high-blast ⇒ hard.
  roles/ACL:   on-call (act), service-owners (party), SRE (approver for non-pre-auth live actions)
  agents+MCP:  investigator agent (READ-ONLY); MCP caps = metrics, logs, tracing, k8s-read,
               deploy-history, runbook-store  (NO write/live-mutation tools by default)
  artifacts:   metric-chart, log-table, deploy-diff, hypothesis-ledger entry, runbook-ref, AAR
  renderers:   chart / table / diff / ledger / AAR  (declarative over a fixed palette)
  views:       incident timeline · hypothesis ledger · action log (w/ reversibility tags)
  review:      fast-loop = pre-auth/reversibility routing; slow-loop = AAR ratification
```

## Candidate kernel primitives this exposed (🔵 conjecture — tag generic vs playbook-specific)

| Primitive | Generic (kernel) or playbook-specific? | Note |
|---|---|---|
| Inbound-trigger **Connector** (webhook → spawn channel+agent) | **generic** | research & code-PR also want event triggers |
| **pty-exec → verifiable-artifact** capture | **generic** | core terminal-less mechanic for all playbooks |
| **Provenance binding** (claim ↔ inputs) | **generic** | all playbooks |
| **Hypothesis ledger** w/ search-breadth | **generic-ish** | central to research too; maybe trivial for code-PR |
| **Gate engine** (state machine + approvals) | **generic** | but the *routing function* differs per playbook |
| **Reversibility × blast-radius × pre-auth routing** | **likely playbook-specific** | code-PR barely needs reversibility; *this is the divergence* |
| **Read-only-agent + propose-not-execute** boundary | **generic, sharper here** | live-system risk makes it non-negotiable for this playbook |
| **AAR → proposal → eval → gate → promote** loop | **generic shape**, weak eval here | replay is cheap for code, hard for incidents |
| Durable verified record (ratified intent) | **generic** | the point of all of it |

## What this exposed about the core thesis (feeds Open Decision a)

- **Async deliberation is *not* sufficient as a single mode** — but it doesn't need to be. The thesis survives by **splitting deliberation in time** (act-then-ratify) rather than demanding it up-front. The live element junto needs here is **shared real-time awareness during the fast loop** (who's doing what, the action log updating live), *not* shared real-time *decision-making*. That's a presence/awareness surface, not a CRDT or co-edit — a light presence layer (or the chat connector), not a peer mesh.
- **Pre-authorization is how junto buys back deliberation latency:** move the deliberation *earlier* (runbook authoring/review) so the incident-time action is a logged auto-step. The Franklin ritual happens in advance, not during the fire.

## Open questions specific to this playbook
1. **Who/what owns the reversibility×blast-radius classification** of an action, and how is it kept honest (the gate is only as good as the labels)?
2. **Agent auth to prod systems** — even read-only diagnostics touch sensitive infra; the read-only boundary must be *enforced* (scoped credentials), not just prompted.
3. **Pre-authorization as a durable, reviewed artifact** — runbooks become security-critical; who reviews/signs them, and how does the slow loop update them safely?
4. **The eval problem** (§8) is unsolved — likely the hardest part of making this playbook self-improving.
