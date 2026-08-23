# Answer-linking: `answers` as an authored, human-confirmed claim

Status: accepted (Dan, 2026-08-23) — **implemented** · realizes spec §4 of [`2026-08-23-verification-ceremony-design.md`](../superpowers/specs/2026-08-23-verification-ceremony-design.md) · precedent [`0034`](0034-crdt-confined-to-the-live-plane.md)'s `Annotation.supersedes` · terminology ruling per [`0025`](0025-align-terminology-on-anthropic-managed-agents.md) · companion to [`0039`](0039-findings-are-not-obligations.md)/[`0040`](0040-assertions-carry-their-session.md) (same `Assertion` change)

Reducers 1–3 shrink *what* becomes an attention item and *when* it demands a look. Reducer 4 attacks the remaining load a different way: the **count**. An open question and its answer are, today, two attention items a human reviews separately when they are really one review — the question was settled the moment the answer landed, but nothing says so. There is a second benefit, arguably larger: this mechanises the consult convention `CLAUDE.md` already asks agents to follow by hand. Ledger `9006f5f2` recorded the failure this prevents — two sessions deciding the same invariant differently — and 2026-08-23 produced another instance: two sessions working decision-blame territory without either seeing the other's open items.

## `answers` is authored, not inferred

`Assertion` gains `answers: Option<Vec<EntryId>>` — *"this bears on those open entries."* The precedent is `Annotation.supersedes` (`crates/junto-kernel/src/anchor.rs`): the same same-kind link one layer down, an entry pointing at a prior entry it stands in relation to. Same additive-field pattern as `frame`/`session`/`kind` — omitted from canonical bytes when absent, no effect on existing signatures.

The link is a **claim**, authored by whoever writes the new entry, never something the system infers from text similarity. `record`'s response (`crates/junto/src/mcp.rs`) parses `answers` as a list of `EntryId`s and refuses one that doesn't parse, exactly like `session`.

## Inert until verified

**The link is inert until the answering entry is itself verified.** A claim to have answered something is itself just a claim — recording `answers: [q]` on a still-provisional entry does not settle `q`; it stays exactly as open as it was. This is the same non-negotiable running through every reducer in this spec: **nothing is verified without a human act.**

What "inert" resolves into once a human *does* ratify the answering entry — offering the answered targets in the same confirm act, with ratify-or-park (an answer sometimes disproves the thing it answers) — is spec §3's confirm-and-ratify dialog. **That dialog is not built.** Nothing here batches ratifying an answer's targets into one act; a human notices an `answers` link and acts on the target by hand, the same way they act on anything else in the record today. A second piece is also not built and is named separately so it isn't assumed bundled with the field: **rendering `answers` on the entry it targets** (e.g. an open entry showing *"answer proposed by `<id>` (unverified)"*) does not exist in `render.rs`/`web.rs` today. The link is real, durable, and queryable by walking `answers`; it is not yet surfaced to a reviewer looking at the answered entry itself.

## Instrument boundaries

`answers` is deliberately a new instrument rather than an overload of either existing one:

| Relation | Instrument | Why not reuse it |
|---|---|---|
| Contradicts an open entry | `Correction` (exists) | Supersedes its target — asserts the target was wrong and replaces it. `answers` doesn't claim the target was wrong. |
| Kills it | `Park` (exists) | A human verdict that abandons the target. `answers` is authored by a worker, not a verdict. |
| Bears on / resolves it | `answers` (new) | Weaker than either: a claim of relevance, human-confirmed later, not an act performed on the target now. |

## Terminology: `answers`, never `outcome`

`0025` renamed junto's *old* `Outcome` — the produced thing (PR · memo · fix · parked) — to **`Deliverable`**, and reserved **`Outcome`** for *the target*: what done looks like, plus its Rubric. Naming this field `outcome` would reopen exactly the drift `CLAUDE.md` forbids when it requires that *"names carry the ubiquitous language"* — the word is already spoken for, on the opposite side of the loop from what this field means.

## Retrieval is a suggestion, never a link

`record`'s response gains a bounded tail: the channel's open entries (provisional assertions, pending-gate proposals) ranked against the new entry's statement + rationale, top `RELATED_OPEN_LIMIT` (3), one line each (`related_open_markdown`, `crates/junto/src/render.rs`). It fires at the one moment the writer already has the context loaded to judge relevance, at zero extra calls and zero obligation — nothing about recording is blocked or slowed by it, and nothing it names is written anywhere.

Lexical retrieval will suggest false candidates, and a false *"this answers that"* is worse than no link. Three gates stand between a bad suggestion and a consequence: **(1) retrieval only suggests** — the tail is prose in a tool response, not a write; **(2) the link is authored** — the agent must itself choose to put an id into its own `answers` field, on its own entry, with its own rationale; **(3) it is inert until verified** — a human ratifying the answering entry is what makes the link count, per "inert until verified" above.

### The ranker

`related_open_markdown` reuses `dead_ends_markdown`'s ranker verbatim in shape: `tokens()` (lowercased alphanumeric runs of 3+ characters) and `rank_by_overlap` — IDF-weighted token overlap normalized by `sqrt(document length)`, descending score, input-order ties, deterministic on every replica. The scoring was extracted into one shared function so `dead_ends_markdown` (the dead) and `related_open_markdown` (the living) are one ranker with one set of ranking tests, not two copies free to drift. It is deliberately crude next to embeddings — local, dependency-free, and already named its own upgrade path before this change: an embedding-based `MemoryProvider` (`docs/pluggability.md`) is the designed successor for when lexical overlap stops being enough.

## Considered

- **Auto-linking the top-ranked candidate** instead of only naming it in the response — rejected: turns a suggestion into an inferred link authored by nobody, exactly the failure mode the three-gate risk analysis exists to prevent.
- **Reusing `Correction` or `Park`** for "bears on" — rejected; see Instrument boundaries. Neither means "may resolve this" without also meaning "the target is wrong" or "the target is over."
- **Naming the field `outcome`** — rejected outright: `0025` already spends that word on the target/Rubric side of the loop.
- **Embedding-based retrieval now** — deferred, not rejected. The lexical ranker is judged sufficient today; `MemoryProvider` remains the named upgrade path, not built here.
