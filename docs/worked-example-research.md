# Worked example — research / analysis as a Playbook

> **Tracer bullet, NOT a spec.** Second of the rule-of-three set (after `worked-example-production-troubleshooting.md`). Schema + primitives below are **candidates / conjecture** until all three playbooks are walked and the real seam is extracted.

## Why this playbook: it's the *opposite-tempo* adversarial case

Production troubleshooting stressed junto on **tempo** (you can't deliberate when MTTR is in minutes). Research stresses it on the **opposite** axis: you have *all the time in the world*, so honoring "deliberate before acting" is trivial — **the hard problem isn't tempo, it's epistemic rigor.**

The adversarial framing junto must confront honestly: **junto's own pitch — "make agent-augmented inquiry cheap" — is exactly what manufactures false discoveries.** Cheap experimentation → multiple-testing / data-snooping / p-hacking / HARKing. A system that makes search cheap **without tracking search breadth is an overfitting machine with a chat UI** (the architecture doc's own words). So the burden of proof is on junto to show it makes research *compound knowledge* rather than *manufacture spurious findings*. If the doc just shows an analyst happily finding an answer, it has confirmed priors and proven nothing.

## The resolution: the ledger + pre-registration + reproducibility-binding *are* the product

Three mechanisms — already named in `architecture.md` — turn the channel from an overfitting machine into a knowledge engine. They are not nice-to-haves; for this playbook **they are the reason to exist:**

1. **Pre-registration.** Declare the hypothesis + analysis plan **before** looking at outcomes. Antidote to HARKing (hypothesizing-after-results) and to silently reshaping the test until it's significant. The pre-registration is a first-class, timestamped, immutable channel artifact.
2. **Hypothesis ledger w/ search-breadth tracking.** *Every* test run is logged — not just the one that "worked." This is what enables **multiple-comparison correction** (FDR control in science; deflated-Sharpe / overfitting penalties in quant finance — López de Prado / Bailey). The ledger makes "we tried 200 variants and kept the best" *visible* instead of hidden.
3. **Reproducibility binding.** Every claim is bound to the exact inputs that produced it — data snapshot/as-of, code, seed, commands, environment — so it's **re-runnable**, not narrated. Trust the computation, not the prose.

## The centerpiece: the gate is *epistemic*, not operational

This is where research **diverges** from prod-troubleshooting. There the gate routed on *reversibility × blast-radius*. Here the gate asks **"is this finding real?"**:

| Gate criterion | What it checks |
|---|---|
| **Pre-registered?** | Was this hypothesis declared before the data was seen, or is it post-hoc? |
| **Search breadth corrected?** | Given N variants tried (from the ledger), does the result survive multiple-comparison correction (FDR / deflated-Sharpe)? |
| **Reproducible?** | Does re-running the bound artifact reproduce the claim? |
| **Robust?** | Out-of-sample / holdout / different period-or-cohort? Sensitivity to arbitrary choices? |
| **Material?** | Effect size meaningful, not just "significant"? |

Only after these does a finding earn **domain-expert sign-off**, which gates the *consequential action* — and note the action is usually **not code**: publish a memo, file a finding, **deploy capital, change a clinical protocol**. The graduated pipeline re-maps with *higher stakes than code* (capital, safety, liability).

## The scenario

A quant analyst notices a candidate signal: a backtest shows Sharpe ≈ 2.3. **Question: is this a real edge, or an artifact of having searched many variants (selection bias)?** Persona: analyst (human) + a research agent; a senior researcher / risk officer is the sign-off before any capital is allocated.

## Walk-through (the shared spine, this playbook's shape)

1. **Intake / framing.** Channel of playbook=`research` opened on the question. The agent and analyst write a **pre-registration**: the precise hypothesis ("signal S predicts next-day returns in universe U over period P"), the test, the success criterion, and the analysis plan — *timestamped before the backtest runs.*

2. **Agent-augmented investigation.** The agent pulls data and runs the backtest in **headless PTYs**; **terminal-less** — output renders as **artifact cards** (equity curve, drawdown table, the parameter set). Each result is **provenance-bound** to the data as-of date, code commit, and seed. Re-runnable.

3. **Hypothesis ledger — the load-bearing step.** Every variant the agent (or analyst) tried is logged: lookback windows, universes, thresholds. If 200 variants were swept and Sharpe 2.3 is the max, the ledger *knows that*. The agent computes a **deflated Sharpe** against the recorded breadth → the "edge" may evaporate. Resisting the temptation to report only the winner is enforced structurally, not by virtue.

4. **Ledger entry (provisional).** A finding is drafted: *"signal S shows in-sample Sharpe 2.3 but deflated-Sharpe 0.4 after correcting for 200-variant search → likely false discovery; evidence: [bound artifacts + ledger]"* — marked **un-ratified**.

5. **Epistemic gate (the centerpiece above).** Pre-registered? ledger-corrected? reproducible? out-of-sample? Here the finding *fails robustness* → routed to **"parked / falsified,"** not to sign-off.

6. **Outcome — including the negative result, kept first-class.** The parked channel is **institutional memory of a dead end**: "S looked promising, here's exactly why it isn't, here's the re-runnable proof." Next quarter, when someone re-proposes S, the answer already exists. *Negative results are an asset* — don't delete.

7. **(Counterfactual) the positive path.** Had it survived out-of-sample + correction, the ratified finding → senior sign-off → *then* a consequential action (allocate capital) behind its own gate. Provenance + ledger become the audit trail (relevant under SEC 17a-4 / MiFID II-style retention).

8. **Self-improving loop — the eval problem, different flavor.** What's a "good research channel"? **Beware the obvious proxy — "number of findings" / "number of significant results" is the single most dangerous metric here: optimizing it builds a false-discovery machine** (the Goodhart trap, sharpened). The honest signal is *durable, replicated* findings (did it hold out-of-sample / next-period?) and *calibration* (do channels' stated confidences match realized hit-rates?) — both **lagging**. The loop improves the *research skills* (better pre-registration templates, default robustness checks) — gated, held-out, never rewarding finding-count.

## Candidate Playbook declaration (🔵 conjecture)

```
research:
  lifecycle:   framing(pre-register) → investigating → analyzing → [ratifying | parked/falsified]
                 → [consequential-action behind its own gate]
  gates:       EPISTEMIC routing — pre-registered? search-breadth-corrected? reproducible?
                 out-of-sample? material?  →  sign-off only if it survives.
  roles/ACL:   analyst (investigate), domain expert / risk officer (sign-off),
                 info-barrier ACLs (regulated: need-to-know, private channels)
  agents+MCP:  research agent; MCP caps = data stores, analysis/sim engines, backtest/stats,
                 systems-of-record (read)  — domain-specific (~80%)
  artifacts:   pre-registration, hypothesis-ledger entry, dataset-snapshot ref, result (chart/
                 table), finding/memo, falsified-result
  views:       hypothesis ledger (w/ search breadth) · pre-registration · provenance graph
  review:      epistemic gate; negative results first-class; on-prem inference (regulated)
```

## Candidate kernel primitives this exposed (🔵 conjecture; generic vs playbook-specific)

| Primitive | Generic or playbook-specific? | Note |
|---|---|---|
| Pre-registration (timestamped immutable claim-before-evidence) | **likely generic** | prod-troubleshooting's runbook pre-auth is a cousin; code-PR's "plan before build" too |
| **Hypothesis ledger w/ search-breadth** | **generic but CENTRAL here** | was "generic-ish" for incidents; for research it's the whole game |
| Reproducibility / provenance binding | **generic** | all three playbooks; here it's load-bearing |
| Gate engine (state machine) | **generic** | the *routing function* is **epistemic** here vs reversibility-based for incidents |
| **Multiple-comparison / deflated-metric computation** | **playbook-specific (domain)** | FDR / deflated-Sharpe — needs domain stats tooling |
| Negative-results-first-class store | **generic, emphasized here** | parked channels as memory |
| AAR/loop → skill improvement | **generic shape**, finding-count is the poison proxy | eval = durable/replicated findings, lagging |
| Info-barrier ACLs / on-prem inference | **generic, mandatory here** | regulated reality pulls toward central self-hosted SoR (trust regime #2) |

## What this exposed about the core thesis
- **The "deliberate before acting" thesis is *easy* to honor here** (no tempo pressure) — so research validates the thesis from the calm end, exactly opposite to incidents. The interesting finding is that **junto's value in research is almost entirely in the durable-verified-record machinery** (ledger + pre-registration + provenance), *not* in the chat/coordination. That's a hint that the **ledger entry is the true kernel**, and "alignment/coordination" is one application of it.
- **Two trust regimes confirmed:** regulated research *wants* a central self-hosted system-of-record (retention, info barriers, supervision) — the decentralized/OSS substrate is the wrong default here. Share the channel abstraction; differ the substrate (`SubstrateProvider`).

## Open questions specific to this playbook
1. **Who declares "search breadth"** — automatic capture of every agent-run query is ideal but agents can search outside the ledger; how is breadth made tamper-evident?
2. **Domain stats tooling** (FDR, deflated-Sharpe) is per-domain — how much does the kernel know vs the Playbook's MCP tools?
3. **Pre-registration enforcement** — what stops post-hoc editing of the "pre"-registration? (Immutable signed timestamp; the same immutability the regulated record needs.)
4. **Calibration as the honest eval** — tracking stated-confidence vs realized-outcome is powerful but slow; is it worth building early?
