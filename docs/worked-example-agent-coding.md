# Worked example — agent-coding (code-PR) as a Playbook

> **Tracer bullet, NOT a spec.** Third of the rule-of-three set (with `worked-example-production-troubleshooting.md` and `worked-example-research.md`). With all three walked, the real generic-vs-playbook-specific seam can finally be extracted (see the synthesis at the end).

## Why this playbook: it's the baseline — and the *least* differentiated

This is Maggie's original domain and junto's "default" Playbook. Honesty up front: **code-PR is the most crowded space junto plays in.** Ace, Stripe Minions, Uber Code Inbox/uReview, Salesforce's PR stack, GitHub itself — everyone is building agent→PR tooling. So the worked example's job is **not** to show junto can do code-PR (table stakes); it's to locate the *small* slice where junto adds something the flood-tooling doesn't, and to be clear that **the non-coding playbooks are where junto actually differentiates.**

## The hard axis here: the attention economy (the AI-PR flood)

Each playbook stresses a different dimension:
- prod-troubleshooting → **tempo / reversibility**
- research → **epistemic rigor / false discovery**
- **code-PR → the attention economy** — one human can't review what a fleet of agents emits. (Evidence: the flat-queue bot with 105 open / 96 stale PRs; AI PRs accepted **32.7%** vs **84.4%** for human, and waiting **4.6× longer**.)

So the gate here routes on neither reversibility nor epistemics primarily — it routes on **review load and accountability**: *who looks, how much, and how do we not flood the reviewer into rubber-stamping.*

## Where junto adds the small differentiated slice

Three things, all from `architecture.md`, that the post-push forge tooling structurally *can't* do:

1. **Prevent > triage (alignment up-front).** The highest-leverage move is the same as Maggie's: agree the plan in-channel *before* the agent builds, so fewer wrong PRs ever exist. (The 32.7% acceptance stat means a third of review effort is spent on things that shouldn't have been built — prevention removes load rather than redistributing it.)
2. **Pre-remote, in-channel review.** Because the channel has its own conversation + git refs decoupled from `origin`, a change can be reviewed *before* it's pushed — no CI noise, no half-baked server branch, no premature CODEOWNERS ping. No forge offers this; it exists *because* junto sits beside git, not on the forge.
3. **Commissioner first-pass + provenance.** The human who commissioned the agent does a cheap first-pass (forced one-line rationale, not a checkbox) before externalizing review cost onto the team; the PR carries a provenance trailer ("channel-reviewed by X,Y").

## The scenario

A developer commissions an agent to implement feature Y. The agent works in a worktree and produces a change. Persona: commissioner (human) + coding agent; CODEOWNERS are the eventual remote reviewers; an assisting reviewer agent may *grade* but not *replace* humans.

## Walk-through (the shared spine, this playbook's shape)

1. **Intake / framing — the prevent lever.** Channel of playbook=`code-PR` opened on "implement Y." Before building, commissioner + agent align on a short plan in-channel (this playbook's deliberation step = *a plan*, vs research's *pre-registration*, vs incident's *triage checklist*). Cheap; catches "wrong thing" early.

2. **Agent-augmented build.** The agent implements in a worktree, running build/test in **headless PTYs**; **terminal-less** — the human sees **artifact cards** (the diff, test results, a risk summary), not scrollback. Change captured as a **git-patch artifact** on a `proposed/` ref + **provenance-bound** to the commands/commit that produced it.

3. **Risk-routing (the attention-economy gate).** Deterministic floor (sensitive paths — auth/migrations/CI/infra/public-API — always gated; reversibility as a *minor* input here, unlike incidents) + an LLM evaluator (reads diff + goal + policy → routing rec + risk summary + forced rationale). Outcomes: **auto-push / commissioner-only / full-channel review.** *Don't gate everything* — over-gating trains rubber-stamping (the uReview principle: noise is worse than nothing).

4. **Pre-remote in-channel review (the differentiated step).** If warranted, participants review the patch inline in-channel — *before* `origin`. Completing it **satisfies the commissioner first-pass**. The assisting reviewer agent grades/merges/drops low-confidence comments and is **usefulness-rated** (feeds the self-improving loop); it is an *input to* the human reviewer, never the verdict.

5. **Push + open PR with provenance.** On approval, the `ForgeAdapter` pushes and opens the PR (draft until first-pass done → draft suppresses the CODEOWNERS ping on GitHub; per-forge variations handled by the adapter). PR carries the "channel-reviewed by …" trailer.

6. **Remote review + merge.** CODEOWNERS notified *now* (not before) → standard forge review → merge. Always-human-gate on merge (the Stripe/everyone consensus); nothing auto-merges to protected branches.

7. **Ledger entry + self-improving loop — the EASY-eval playbook.** The PR + its plan + review form the durable record. **This is the one playbook where held-out evals are tractable: run the tests.** Outcome signals (tests pass, review-acceptance, post-merge stability via the bug-instability / SZZ signal) feed the loop: AAR/correction → proposed skill/workflow diff → **held-out eval (cheap here)** → human gate → promote. The role isolation (coder / reviewer / bug-finder, *independent* signal) applies cleanly. Reward *durable stabilization*, never PR-count (PR-count is the flood-amplifying Goodhart trap).

## Candidate Playbook declaration (🔵 conjecture)

```
code-PR:  (the built-in default playbook)
  lifecycle:   plan → building → [pre-remote review] → pushed(draft) → remote-review → merged
  gates:       ATTENTION-ECONOMY routing — deterministic sensitive-path floor + LLM risk
                 eval → auto-push | commissioner-only | full-channel; always-human merge gate
  roles/ACL:   commissioner (first-pass + accountable), party (channel reviewers),
                 CODEOWNERS (remote), assisting reviewer agent (grades, not verdict)
  agents+MCP:  coding agent; assisting reviewer agent; bug-finder agent (adversarial);
                 MCP caps = repo, tests, static analysis, CI status
  artifacts:   plan, git-patch (proposed/ ref), diff, test-results, risk-summary, review-comments,
                 PR-link (+ provenance trailer)
  renderers:   diff · test-results · risk-summary · review-thread
  views:       change review pane · risk routing · review-load / SLO-aging dashboard
  review:      pre-remote in-channel first-pass → forge CODEOWNERS; SLO/aging on the queue
```

## Candidate kernel primitives this exposed (🔵 now cross-checked against all 3 playbooks)

| Primitive | Generic or playbook-specific? | Cross-playbook verdict |
|---|---|---|
| Channel = unit of inquiry; party/ACL | **generic** | all 3 |
| git-refs durable plane (synced via hub) | **generic** | all 3 |
| **pty-exec → verifiable artifact** (terminal-less) | **generic** | all 3 — confirmed |
| **Provenance binding** | **generic** | all 3 — confirmed load-bearing everywhere |
| **Gate engine (state machine + approvals)** | **generic** | all 3 — **but the routing FUNCTION is playbook-specific** (reversibility / epistemic / attention) ← the real seam |
| Deliberation/framing step | **generic stage, playbook-specific content** | plan vs pre-register vs triage |
| Hypothesis ledger | **generic-but-weighted** | central (research) → useful (incident) → minor (code-PR) |
| AAR → proposal → eval → promote loop | **generic shape, eval difficulty varies wildly** | easy (code) → hard (incident) → poisoned-proxy (research) |
| Risk-routing inputs | **playbook-specific** | reversibility×blast (incident) / epistemic (research) / review-load (code) |
| Connectors (forge / tracker / knowledge) | **generic** | all 3 wire to SoRs |

## Synthesis — the seam, now that all three are walked

The rule of three pays off: across incident / research / code-PR, what's **truly generic (kernel)** vs **playbook-specific** separates cleanly.

- **Kernel (generic — the real product):** channel + party/ACL · git-refs durable plane (hub-synced) · **pty-exec→artifact** · **provenance binding** · **a gate-engine state machine** · a deliberation/framing stage · the **AAR→proposal→gate→promote** loop shape · Connectors. Plus the finding from the research doc: **the ledger entry is the spine the others hang off of.**
- **playbook-specific (the plug):** the **gate routing function** (reversibility×blast-radius vs epistemic-rigor vs attention/review-load) is the single biggest divergence — *this* is what a Playbook most distinctively supplies. Plus its framing content (plan/pre-register/triage), its domain MCP tools, its artifact renderers, and its eval strategy.
- **The eval problem is the hardest cross-cutting unknown**, and its difficulty is *itself* playbook-dependent: tractable for code, lagging-and-sparse for incidents, actively-Goodhart-prone for research. Any general self-improvement design must treat "how do we know better?" as per-playbook, not universal.

🔵 Strong implication for sequencing: **build the kernel against code-PR first** (easiest evals, most prior art, fastest dogfooding), but **design the gate-engine routing as a pluggable function from day one**, because the other two playbooks prove it's the part that varies. Do *not* hardcode code-PR's attention-economy routing into the kernel.
