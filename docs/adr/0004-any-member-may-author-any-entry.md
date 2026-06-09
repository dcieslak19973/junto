# An author is any Member — human or agent; authority lives at the Gate layer

Status: accepted (Dan, 2026-06-09) · builds on [`0003`](0003-ledger-entry-content-model.md)

An **Author is any Member** — human *or* agent. Agents are first-class peers (the founding thesis); identity is the git author. So **an agent may author any LedgerEntry kind**. The kernel does **not** restrict authorship by entry kind.

## Why this is surprising enough to record

A reader sees "anyone, including a bot, can write a ratification" and assumes a missing guardrail. The guardrail exists — it just isn't *authorship*. The *"human-authored or human-ratified, never auto-captured truth"* principle (`../self-improving-harness.md`) lives at the **Gate / Verifier** layer, not in who may write:

- An entry enters in **provisional** standing and gains *ratified* standing only by passing the playbook's gate.
- **Authorship ≠ authority** (cf. a lab notebook: the researcher authors, a witness countersigns).
- *Who/what may ratify* is **per-playbook gate-routing** — human sign-off for consequential work, a held-out **eval** for the self-improvement playbook. So ratification is not even always human.

## Consequence

The kernel's append API takes any `Member` for any kind. Eligibility/authority constraints are a Gate/Verifier (and future Party/Role) concern — see the gate and routing ADRs (index in [`../domain-model.md`](../domain-model.md)).
