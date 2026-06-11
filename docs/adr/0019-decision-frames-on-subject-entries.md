# Decision frames ride on subject entries — options recorded, unchosen included

Status: accepted (Dan, 2026-06-11) · builds on [`0003`](0003-ledger-entry-content-model.md), [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md) · design in [`../attention.md`](../attention.md) §Decision frames

The verification problem's first counterattack: **the proposer frames the
decision**. A subject entry (`Assertion`, `Proposal`) may carry an optional
**decision frame** — 2–4 options, each naming the verification act choosing
it performs and a **drafted rationale** the verifier adopts (and may edit).
The surfaces render the options as one-click acts with the draft editable in
place; free-text remains available always.

## The frame is durable, unchosen options included

Dan (2026-06-10): record the options presented. The frame lives **in the
entry payload** — an optional field, omitted from the canonical bytes when
absent (`0008`-style `skip_serializing_if`, so every pre-frame entry's bytes
are unchanged). The unchosen options are *alternatives considered* become
structural rather than prose — exactly the richer shape `0003` deferred
"until a second Playbook proves a richer shape"; this is the proof, arriving
from the verification side rather than a playbook.

## Shape

```
DecisionFrame { options: Vec<FrameOption> }
FrameOption   { label, act: Ratify|Park|Approve|Reject, rationale }
```

- The **kernel stays permissive** (it stores; `0004`'s spirit): which acts
  are *coherent* for a kind — ratify/park on assertions, approve/reject on
  proposals — is validated at the write surfaces (MCP refuses a mismatched
  frame at record time; the renderers only render applicable options).
- Choosing an option submits the mapped act with the (possibly edited)
  rationale through the **existing act routes** — a framed verification is an
  ordinary `Ratification`/`Approval`/… entry; nothing downstream changes.
- The frame is presentation-plus-record, **not** routing: gate requirements
  (`0006`) are untouched.

## Why this chips at rubber-stamping (`../attention.md` §Known tensions)

Blank-box friction makes "lgtm" the cheapest act. With a frame, the cheapest
act adopts a substantive, pre-articulated position — the floor rises — and
the **override/edit rate is measurable** from the record (chosen rationale
vs. drafted rationale), a standing signal of whether an agent's framing is
honest. A lazy verifier can still pick option one; this is a gradient fix,
not a solution, and is recorded as such.

## Considered

- **Frames outside the payload** (surface-side generic options per kind) —
  rejected: loses the agent-authored framing (the entire value) and the
  durable record of alternatives.
- **A new entry kind for frames** — rejected: a frame is not an act; it is
  part of what the subject entry asks. A field, not a kind (`0003`'s closed
  set is unchanged).
- **Required frames** — rejected: frameless entries stay legal and render
  with the plain form; frame quality becomes part of what makes an agent a
  good collaborator rather than a compliance gate.
