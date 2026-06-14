---
title: self-improving agent / harness — design framing
date: 2026-06-07
status: EXPLORATION (no implementation yet). Design framing + grounding only.
---

# The self-improvement Playbook (junto on itself)

> **Framing (Dan, 2026-06-08): self-improvement is NOT a separate pillar or sibling project — it's junto pointed at its own policies.** "Should we change our agents/skills/workflows?" is just another **unit of work run in a channel** — a specialized code-PR playbook whose *targets* are skills/workflows/agent-defs (not product code) and whose *verifier* is the eval harness. Same loop as every other playbook: deliberate → agent proposes a change → **gate** → **verified record (promote)**. So junto improves itself with its own mechanism. This doc specs that playbook's hard part — **the eval (THE CRUX)** — and its signal source (observability). Companion to `architecture.md` (the core loop) and `junto.md` (the spine).

## The ask

The self-improvement Playbook: a loop where outcomes feed back into improving the agents/skills/workflows (and eventually the harness) over time — run as a gated, recorded unit of work like any other channel.

## Key reframe: the substrate is the usual agent-policy surfaces

The "policy" that would be improved already exists as **agent-modifiable data**, and the authoring tools are standard. What's missing is the *loop*, the *evals*, and the *gating*.

What the substrate provides:
- **Skills are agent-authorable** — plain markdown with discovery roots (project-local / global / built-in) and precedence; an agent can create/edit its own skills.
- **Workflows: scratch→promote.** Agents generate one-off "scratch" workflows (reusable conductor scripts coordinating sub-agent tasks) and a human **promotes** them to reusable. *Manual* self-authoring of orchestration — the seed of an auto-promote loop.
- **Within-task feedback loops** are the closest existing precedent — e.g. a deep-review auto-fix loop, and a deep-research adversarial-verification lane. Study them first.
- **Agent definitions** are markdown too (builtIn + project/global, precedence) — another modifiable policy surface.
- **Macro self-modification is already the proven mode:** a reduced-privilege bot account builds the product via PRs under human review. So "propose → human gate → promote" is established.
- **Trust model**: project trust gates executable artifacts; workflow trust piggybacks on it. Relevant for "what may the agent modify."
- **Feature gating**: gate any new self-improvement feature behind an experiment flag.

What's MISSING (the actual work):
- No cross-session **learning / memory / reflection** service that turns outcomes into durable policy changes.
- No **eval harness** to judge "better" (the crux — see below).
- No **loop-closer** wiring outcome → proposal → validation → gate → promote.

## Design framing — two axes

**Axis 1 — what improves (low→high risk):** (a) within-run reasoning [exists] → (b) skills [tools exist] → (c) agent definitions → (d) workflows [scratch→promote exists] → (e) the harness/core code [via PRs today].

**Axis 2 — loop autonomy:** within-task reflect-and-retry [exists] → cross-task human-gated proposal [partial: promote is manual] → cross-task autonomous [doesn't exist; highest risk].

"Self-improving harness" = closing a cross-task loop that updates (b)/(c)/(d), human-gated first.

## THE CRUX (be adversarial here): evals are the whole ballgame

Self-improvement is only as good as its measure of "better." This is the load-bearing problem; if it's not solid, nothing else matters.
- **Goodhart / eval validity** — proxies (tests pass, PR merged, user didn't complain) are sparse, noisy, gameable; optimizing them degrades the real goal.
- **Overfitting / credit assignment** — trying many self-edits and keeping "wins" on noisy outcomes overfits. Need **held-out evals** + attribution of which policy version caused which outcome (provenance). (Same multiple-testing problem flagged for the quant use case in the sibling doc — see its "hypothesis ledger".)
- **Drift / runaway** — feedback loops oscillate or quietly degrade. Need **versioning, rollback, an immutable safety core** the loop can't touch.
- **Trust boundary** — data (skills/workflows) is far safer to self-modify than code (the harness). Escalate autonomy only as evals earn trust.

## Driving objectives: quality, human-attention, and cost — not just task success

The harness optimizes for "better," so **what "better" means is the whole design** (this is THE CRUX). "More/faster output" is the wrong objective — it **amplifies the AI-PR-flood** from the companion doc (the 105-open / 96-stale / single-reviewer case). So "better" must be **multi-dimensional**:

- **Output quality** — fewer defects/rework, higher review-acceptance, less churn. An improvement that raises volume but lowers acceptance is a *regression* (cf. the 32.7%-vs-84.4% AI-vs-human acceptance gap in the sibling doc).
- **Human-attention economy** — minimize the human review/grooming burden *per unit of value*, don't maximize throughput. Attention is the scarce shared resource (spent at **two phases** — below); flooding it is a cost, and worse, it **corrupts the learning signal**: overwhelmed reviewers rubber-stamp → the "merged/approved" proxy the loop learns from stops meaning quality (a Goodhart accelerant).
- **Cost** — tokens / $ / wall-clock per outcome. A real lever (industry AI spend is up ~6×): the harness can improve by reaching the same result with fewer iterations, smaller models where adequate, leaner context — and should be rewarded for it.
- (Task success — necessary but not sufficient; it's the floor, not the goal.)

**Attention is spent at BOTH phases, not just review:**
1. **Creation / grooming.** The loop's *own* proposals (skill/workflow diffs to review+promote) compete for the same attention as work PRs — a harness that proposes faster than the human can vet just **relocates the bottleneck onto itself**. So it must groom its own output: scope each proposal small, **dedup/batch** related ones, **self-filter** low-value ones before a human sees them, and attach a forced one-line rationale + risk summary so the gate is cheap.
2. **Review.** Higher throughput floods reviewers → rubber-stamping → the corrupted feedback above.

**Design implications:**
- **Encode all the dimensions in the eval objective** — optimize *quality per unit of human-attention and cost*, not raw task count. Without this, "more merged PRs" is gameable by flooding (the Goodhart failure in THE CRUX).
- **Theory of constraints — improve the bottleneck, not the producer.** If the harness gets better at *producing* faster than at *reviewing/grooming/triaging*, it widens the gap. Make the bottleneck capabilities (triage, grooming, high-signal review) **and** cost-efficiency first-class improvement targets — often ahead of raw production capability.
- **The human-gate is the attention sink.** Reduce what reaches it via auto-eval + auto-filter; raise autonomy (drop the gate for low-risk, eval-trusted changes) specifically to relieve attention — but only as evals earn trust (THE CRUX trust boundary).
- **Reuse the sibling doc's triage machinery.** Risk-routing + **SLO/aging** + uReview-style high-signal grading apply to *self-improvement proposals* exactly as to code PRs (same gate, same pre-receive backstop); an unreviewed proposal **ages out** rather than rotting.

Net: success is **higher-quality output at lower human-attention and lower cost** — not more of it. Reward the harness for *relieving* the attention/cost burden (better grooming, triage, efficiency), and treat any change that raises volume while degrading quality/acceptance as a regression.

## Candidate signal: bug-driven code instability (a lagging review-fatigue proxy)

Hard to measure review quality directly; an **indirect** proxy: track **code stability** per region over time, and isolate the **bug-fix subset** of churn. A region that keeps needing fixes means defects keep slipping through review → rising bug-driven instability ≈ degrading review quality (fatigue / rubber-stamping). Churn *alone* is noise; the bug-fix subset is the signal.

- **How to compute (prior art):** the **SZZ algorithm** (Śliwerski–Zimmermann–Zeller) — from a bug-fixing commit, `git blame` the changed lines back to the commit(s) that introduced the defect. Churn↔defect-density correlation is well-established. junto has the full git history; the companion doc's **provenance binding + hypothesis ledger** aid attribution. Metric = per-region (file/dir/owner) rate of *bug-introducing* changes over time, plus introduce→fix lag. Correlate spikes with reviewer load/throughput → ties directly to the **human-attention** objective above.
- **Adversarial caveats (per THE CRUX):**
  - **Churn ≠ fatigue.** Hot files (`agentSession`, `ChatPane`) churn from healthy active dev. Isolate the bug-fix subset, not raw churn.
  - **Attribution is the hard part.** SZZ is noisy (refactors / moved code / cosmetic fixes mislabel); "is this a bug fix?" needs a classifier (commit message / revert / linked-issue). This is the credit-assignment problem THE CRUX already flags.
  - **Lagging + sparse.** fatigue→bug→discovery→fix can be weeks → a slow **trend/health metric**, never a per-PR gate.
  - **Goodhart-able.** Optimizing "reduce bug-churn" is gameable (avoid touching fragile code, suppress/relabel fixes) → keep it a **held-out diagnostic, not a direct reward**.
- **Reward asymmetry (or the metric is perverse).** Penalizing bug-churn alone incentivizes *not fixing* and dodging fragile code. So the negative weight must attach to the **introducing** change + the review that passed it — **never the fixer**; bug-*fixing* is held harmless or positive. But don't reward raw **fix count** (invites introduce-then-fix farming / relabeling — the cobra effect): reward **durable stabilization** (a region's recurrence / escaped-defect rate trending *down*), not the fix event. **Net-zero (or negative) credit when the same policy version both introduced and fixed** — provenance/SZZ links the pair; reward fixing *pre-existing / older-version* defects.
- **Use:** a trend input that *raises review rigor / lowers throughput* (the attention/SLO knobs) when a region's bug-introduction rate climbs — feedback on review health, not a promotion gate.

## Role isolation: coder / reviewer / bug-finder (separation of duties)

Yes — isolate the roles. Briefly: **coder** writes the change; **reviewer** = the *judge* ("should this be accepted?" — holistic over correctness + design + intent, emits a verdict); **bug-finder** = the *adversary* ("can I break it?" — narrow to correctness, execution-grounded, emits reproducible defects, no verdict). The bug-finder is an **input to** the reviewer (evidence, alongside tests/static analysis) and a post-merge **auditor of** the reviewer (escaped defects grade it) — *not* a second reviewer. Three reasons to separate, all serving THE CRUX:

1. **Independent eval signal (anti-self-preference).** An agent grading its own output is the broken gate: LLM-as-judge has documented self-preference/sycophancy bias, and a coder's blind spot is the *same* blind spot in its self-review. A separate reviewer de-correlates the error.
2. **Opposed objectives (anti-collusion).** Coder wants to *ship*; reviewer wants to *catch defects*; bug-finder wants to *break it*. A shared reward → collusion toward "merged." Partially-opposed rewards create the tension the reward-asymmetry above needs.
3. **Clean credit assignment.** Separated roles make escaped defects attributable: bug-finder catches what the reviewer missed → improve the *reviewer* policy; neither caught it (escaped, via the instability signal) → improve *both*. Credit assignment is THE CRUX's hard problem; isolation makes it tractable.

**Reward shape — precision over volume, every role.** Each role is scored on *validated, material* outcomes, never raw activity — that symmetry is the anti-Goodhart spine:
- **Coder:** penalize bug *introduction*, reward *durable stabilization* (above) — not output count.
- **Reviewer:** reward catching real defects; the escaped-defect / instability signal penalizes misses (rubber-stamping).
- **Bug-finder:** reward *confirmed, material* defects — especially ones the reviewer missed — and **penalize false positives (spurious) and trivia (real but immaterial nitpicks)**. A finder scored on finding-*count* farms noise, which taxes the attention budget and trains reviewers to rubber-stamp (the uReview failure). Grade findings against ground truth (did it reproduce? did a fix stick? did it matter?) + usefulness-rate them; drop low-confidence before they reach a human.

(Introductions, fixes, findings, comments are all *activity*; reward the validated material outcome, not the activity.)

**Caveat — isolation is only real with *independent* signal.** Same base model writing + judging shares blind spots → separation is cosmetic. Get independence from **model diversity** (different family) and/or **execution grounding** (tests, static analysis, fuzz/property tests, the bug-instability signal) — not a second LLM opinion. (This is why uReview mixes dedicated bots + MCP/deterministic tools.) Give the reviewer the *artifacts* (diff, tests, provenance), not the coder's self-justification, which anchors it.

**Timing asymmetry:** coder (produce) → reviewer (pre-merge gate) → bug-finder (pre-merge adversarial testing **and** post-merge escaped-defect detection via the instability signal). The bug-finder is partly *what grades the reviewer over time*, closing the loop: escape → attribute → improve coder &/or reviewer policy.

**Cost (per objectives):** the full triad is expensive → **risk-route depth** — deterministic checks + small models on low-risk changes; full coder/reviewer/bug-finder only where risk warrants.

Grounding: a deep-review auto-fix loop + a deep-research adversarial-verification lane already separate *produce* vs *verify* within-task, and the commissioner first-pass separates human accountability from the coder. This generalizes that into explicit, independently-improvable roles.

## Recommended first rung (safe, tractable, reuses everything)

Do NOT start with autonomous harness/code self-modification. Start with **human-gated, data-level, outcome-driven refinement of skills/workflows**:

```
outcome signal (failed task / AAR / user correction / failed eval)
  → agent proposes a DIFF to a skill or workflow   [skill-write + scratch→promote]
  → validate against a HELD-OUT eval set           [the eval harness = the hard new part]
  → human gate                                     [reuse the review pipeline from sibling doc]
  → promote (git-versioned, reversible)            [institutional memory]
```

Properties: data-level (not code), human-gated, reversible, anti-overfit via held-out evals. It's the natural *output* of the AAR loop described in the sibling doc (AAR → proposed skill update → gate → promote). Raise autonomy (drop the gate for low-risk changes) only once evals are trustworthy.

## Connections to prior work (companion doc `architecture.md`)

- **AAR / incident-investigation** flows are the natural *source* of improvement proposals.
- The **graduated review pipeline** (commissioner first-pass → peer → risk) is the *gate*.
- The **hypothesis ledger / held-out evals / passive provenance** are the *anti-overfitting + credit-assignment* machinery.
- The **dynamic-workflows seams** — esp. "external/human-input steps" and "addressable run identity/event bus" — are what let a loop observe runs and insert gates.

## Open directions (user was asked to pick; not yet chosen)

1. **The eval harness** — *load-bearing*. What signals junto can trust, how to build held-out evals, anti-Goodhart design (one candidate signal sketched: *bug-driven code instability*, above). **Recommended starting point.**
2. **Loop mechanics** — outcome → proposal → gate → promote, concretely on skill/workflow primitives.
3. **Scope/safety model** — what the agent may modify per rung (skill→workflow→agent-def→code) + guardrails + rollback.
4. **Study existing within-task loops** — the deep-review auto-fix loop pattern, to see what loop machinery a within-task verify lane needs, and build from it.
5. **Objective design (quality / attention / cost)** — bake output-quality, human-attention burden, and cost into the eval objective + loop rate (see *Driving objectives*); decide the budget/SLO model and whether the harness's first improvement target should be the *reviewer/triage* bottleneck and *cost-efficiency* rather than the producer.

## Suggested concrete first action for the next agent

1. Study an existing within-task loop pattern (e.g. a deep-review auto-fix loop) to see the loop machinery.
2. Confirm the skill write/promote path and its trust gating.
3. Then draft the **eval-harness design** (direction #1) — it gates everything else.

Do NOT: start building a self-modification framework before evals exist; enable autonomous code self-modification; remove the human gate. Confirm direction with the user before writing implementation code — this is still exploration.

## Durable records via issue trackers (new direction)

The user asked to use an issue tracker as the first durable proposal/eval/review record. This can work well if we keep a provider abstraction and pair it with local canonical records.

### Why this is a good first backend

- Built-in audit trail and human review surface.
- Easy collaboration across sessions and teammates.
- Natural fit for human-gated promotion.
- Fastest path to dogfooding (especially with GitHub first).

### Required architecture

1. Provider abstraction
   - Common operations: create issue, update issue/body, add comment, list containers, search issues, read status/labels, produce URL.
   - Providers: GitHub, Jira, Linear.
   - Transport: MCP-backed where possible.

2. Two-mode target selection
   - Auto-discovery mode: probe available MCP servers and infer valid trackers/containers.
   - Explicit setup mode: prompt user for provider + container and persist config.

3. Hybrid persistence (recommended)
   - Local junto record remains canonical for runtime durability and offline resilience.
   - Tracker issue is the mirrored control-plane record for collaboration/review.
   - Store cross-links in both directions (local proposal id and external issue id/url).

### Discovery behavior

- Probe MCP servers for capability support:
  - create issue
  - list projects/repos/teams
  - search or fetch issue
  - update status metadata (labels/state/comments)
- If one valid target is found, ask for one-click confirmation.
- If multiple are found, show a short picker.
- If none are found, fall back to explicit setup.

### Provider notes

- GitHub
  - Container: owner/repo.
  - Best first implementation.

- Jira
  - Container: site plus project key (project is usually required).
  - Also requires issue type/state mapping.

- Linear
  - Container: team is required; project is commonly optional.
  - Workflow states are team-defined and must be discovered/mapped.

### Suggested issue record shape

- One proposal maps to one issue.
- Use labels for coarse state and target type.
- Put structured metadata in the body (or a normalized metadata block):
  - proposal id
  - target type and target name
  - base policy version/hash
  - trigger signals
  - eval suite ids and results summary
  - risk tier
- Use comments for lifecycle events:
  - generated
  - eval started
  - eval result
  - review decision
  - promoted or rolled back

### State model and mapping

Internal state should stay tracker-agnostic:

- draft
- validating
- needs_review
- approved
- rejected
- promoted

Each provider maps internal state to its native model (labels, state transitions, custom fields).

### Guardrails

- Do not block core junto flow on tracker outages or auth failures.
- Queue and retry mirror writes when remote is unavailable.
- Avoid storing sensitive raw payloads in issue bodies/comments.
- Keep promotion gated by eval plus explicit human approval.

### Recommended rollout

1. GitHub provider first (discovery + repo override).
2. Jira provider with guided project-key setup.
3. Linear provider with team picker and optional project picker.
4. Unified settings for provider, container, and status mapping.

## Observability is the loop's afferent nerve (not just enterprise export)

**The single most important framing for this whole doc:** observability is *how the loop senses outcomes.* The loop is `outcome signal → proposal → eval → gate → promote` — and **the left edge, the outcome signal, IS observability.** So observability is a **precondition** for self-improvement, not an add-on: THE CRUX is "what does *better* mean?", "better" is a *measurement*, and observability is the measurement layer. **No observability → no evals → no self-improvement.**

**One instrumentation, two consumers.** Instrument agent runs + lifecycle *once* (the Event model below), then fan the same stream to:
1. **observability backends** (OTEL / DataDog / Arize Phoenix) — humans watch dashboards;
2. **the self-improving loop** — the harness subscribes and learns.
Don't build two pipelines. These same events are *also* the provenance / verified-record capture — one event stream, three uses.

**What it measures = the multi-dimensional objective** (from *Driving objectives*): quality (defects, review-acceptance, post-merge churn via the SZZ/bug-instability signal) · attention (review load, time-to-review, rubber-stamp proxies) · cost (tokens/$/wall-clock per outcome).

**Two properties the per-playbook eval reality forces:**
- **Long-horizon, not just per-run.** Eval difficulty is per-playbook, so observability is too: code = fast per-run CI/test signals; incidents = MTTR/recurrence over *weeks*; research = calibration (stated-confidence vs realized-outcome) over *months*. The lagging signals need durable long-horizon outcome tracking, not just live traces.
- **Anti-Goodhart, restated.** The instant an observed metric becomes the loop's reward it's gameable — keep diagnostic signals (bug-instability) as held-out *diagnostics*, reward *validated material outcomes* not activity counts.

### Design intent

- Keep junto runtime logic independent of any single memory/observability backend (pluggable; see `pluggability.md` `MemoryProvider`).
- Support local-first durability with optional remote fan-out.
- Serve **both** consumers from one event stream: feed the self-improving loop **and** flow eval telemetry / proposal lineage into enterprise systems.
- **Regulated mode:** the feed must respect the substrate's trust regime — field-level redaction before fan-out, and the loop's signal source may have to stay on-prem (no PHI/trading-research telemetry into a vendor cloud).

### Layering

1. Memory core (tracker-agnostic, vendor-agnostic)
  - Canonical internal schema for run signals, proposals, eval results, decisions, and policy versions.
  - Stable internal ids and causality links.

2. Memory provider interface
  - appendEvent(event)
  - appendBatch(events)
  - query(filter)
  - getById(id)
  - upsertProjection(name, key, value)
  - health() / capabilities()
  - flush() for graceful shutdown

3. Provider adapters
  - local filesystem or sqlite adapter (default)
  - issue-tracker adapter (GitHub/Jira/Linear) for review workflows
  - observability adapters (DataDog, Arize Phoenix, others)

4. Router
  - one primary provider (authoritative record)
  - optional mirror providers (best-effort fan-out)
  - retry queue and dead-letter handling for mirrors

### Event model (minimum)

- session.started   (a Session — see domain-model.md)
- session.completed
- session.failed
- proposal.created
- proposal.evaluated
- proposal.approved
- proposal.rejected
- proposal.promoted
- policy.activated
- policy.rolled_back

Each event should include:

- event id
- timestamp
- workspace and project identity
- correlation ids (agent-session id, proposal id, policy version id)
- payload schema version

### Capability classes

Not every backend supports the same operations. Require explicit capability flags:

- append_only
- queryable
- mutable_records
- workflow_actions (create tickets, transitions, comments)
- tracing_native
- metrics_native

Runtime behavior should branch on capabilities, not provider name.

### Enterprise observability fit

- DataDog adapter
  - map lifecycle events to logs/events
  - map counters and rates to metrics
  - map run or proposal spans to traces when available

- Arize Phoenix adapter
  - map agent runs, prompts, outputs, and eval outcomes into Phoenix entities
  - preserve trace or span linkage for prompt-level and workflow-level analysis

- Generic OpenTelemetry bridge (recommended)
  - emit traces, metrics, and logs through OTEL where possible
  - keep vendor-specific adapters thin when OTEL is already deployed

### Privacy and governance

- Add field-level redaction before provider fan-out.
- Allow policy-based suppression of sensitive payload fields.
- Separate control metadata from content payloads.
- Support regional routing constraints for regulated environments.

### Configuration model

- provider list with priority and role (primary or mirror)
- per-provider auth reference (never raw secret in notes)
- capability overrides
- redaction policy profile
- retry and backoff policy

### Failure semantics

- Primary provider write failure should fail fast for critical operations.
- Mirror provider failure should not block execution; queue for retry.
- Expose provider health in diagnostics and workflow logs.

### Rollout sequence

1. Define canonical event schema and provider interface.
2. Implement local primary adapter.
3. Add issue-tracker mirror adapter.
4. Add OTEL bridge adapter.
5. Add direct DataDog and Phoenix adapters where needed.
6. Add policy-driven redaction and governance controls.


