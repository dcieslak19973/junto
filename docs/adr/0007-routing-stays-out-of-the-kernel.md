# Routing stays out of the kernel: it executes an ApprovalRequirement

Status: accepted (Dan, 2026-06-09) · builds on [`0006`](0006-gate-engine-event-sourced.md)

The kernel **executes** a gate's approval requirement; it does **not decide** which path a gate takes. *Choosing* the path is the single most playbook-specific thing (hard constraint #5 — no playbook logic in the kernel), so it stays out. The kernel only enforces a requirement it is handed:

```
ApprovalRequirement = Auto | Count(u32) | AllOf(Vec<Member>)
```

(`AllOf` is matched by `Member::email`.) The four route presets — auto / single-approver / full-review / hard-gated — are **rubric-level**; they compile down to an `ApprovalRequirement`, and the kernel never sees the preset names. (`hard-gated`'s extra "never auto-waivable / irreversible" semantics is a future refinement; the three requirements cover the common cases.)

## Considered: model the routing function in the kernel now

Rejected per the rule of three — there are **zero** playbooks yet, so a routing trait would be a socket built before any plug. The kernel taking a pre-decided requirement keeps the seam clean with no speculative abstraction.

## 🔮 Future seam — the Rubric layer (Dan's framing)

Routing rules should be **importable, addressable, reusable rubrics** resolved via a provider — shape mirrors `SubstrateProvider` and the architecture doc's anticipated *"policy service"* — and **not** bound to a directory/repo (a research channel has no working tree). "Import a rubric by id" is `provider.resolve(rubric_id)`, not "read a file from cwd."

- Candidate ubiquitous-language terms: **Rubric**, **ApprovalRequirement**, **GateStatus**. Keep **Rubric/policy** distinct from agent **Policy Version** (a settled naming call — see `../domain-model.md`).
- Build the `RubricProvider` only when **≥2 real playbooks** prove the shape (rule of three). Until then the kernel just takes a requirement.
