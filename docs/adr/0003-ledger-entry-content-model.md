# The ledger entry content model: one envelope, a closed kind set

Status: accepted (Dan, 2026-06-09) · builds on [`0002`](0002-ledger-entries-are-immutable.md)

All ledger appends share **one envelope** (id, channel, author, timestamp, payload) and differ by a **closed `kind`** set. Modeled on git-bug's single typed *Operation* stream and double-entry accounting's single journal. A channel's current state — including each entry's verification standing — is **derived by folding** the entry stream (the immutability/event-sourcing model of [`0002`](0002-ledger-entries-are-immutable.md)).

## The kinds

Starter set, deliberately small:

- **`Assertion`** — the decision / finding / claim. A *subject* (targets nothing).
- **`Ratification`** — accepts a prior entry → `Ratified`.
- **`Park`** — sets a prior entry aside as a negative/abandoned result → `Parked`.
- **`Correction`** — supersedes a prior entry with a restated claim → `Superseded`.

(The closed set has since grown — gate kinds were added later; see the ADR index in [`../domain-model.md`](../domain-model.md).)

**Ratify / park / falsify / correct are LedgerEntries, NOT Events.** They are durable, human-or-agent-authored, rationale-bearing *record* — which is what the Ledger holds. **`Event`** stays the separate *machine-observability* stream (`session.*`, `eval.*`); git-bug has no event stream at all, reinforcing that these are different layers.

## Two sub-decisions baked in

- **The kernel `Assertion` holds `statement` + `rationale` + `provenance` only** — the minimal universal content; everything playbook-flavored stays out of the kernel entry.
- **`Park` and `Falsify` are one kind, named `Park`.** It covers both *abandoned/inconclusive* and *disproven*; the distinction lives in `rationale`. Negative results are **kept, never deleted** (institutional memory).

## Consequences / guardrails

- Keep the `kind` enum **closed in the kernel**; resist sprawl — add a kind only when a concrete playbook (or engine) forces it (rule of three). The gate engine later forcing `Proposal`/`Approval`/`Rejection` is the worked example of this rule firing legitimately.
- 🔮 *"alternatives / options considered"* is deliberately **not** a field — record it in `rationale` prose. Promote it to a first-class field only when a **second** playbook proves the shape (likely trigger: deliberation-heavy playbooks — code-PR *plan*, research *pre-register*). The same defer-until-a-second-case rule applies to other playbook-flavored fields (e.g. a per-entry `outcome`, which also collides with the channel-level **Outcome** noun), and to splitting `Park` back into abandoned-vs-disproven if research's false-discovery tracking needs it structured.
