# The Ledger is the channel's durable record

Status: accepted (Dan, 2026-06-08)

One channel has **one Ledger** of many **entries** (decisions / findings / claims); the channel's "verified record" is its **ratified** entries. We chose the name **Ledger** over "intent record" and folded the old separate "Record" noun into it — there is one durable-record concept, not two.

The research playbook's "hypothesis ledger" is **not** a special structure — it is just *the ledger of a research channel*. Same for an incident channel's AAR record. This keeps one mechanism across playbooks.

This is the root decision the later ledger ADRs build on; see the ADR index in [`../domain-model.md`](../domain-model.md).
