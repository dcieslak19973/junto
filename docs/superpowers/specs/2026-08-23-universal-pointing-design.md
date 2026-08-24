# Universal pointing — design

> Status: ⚠️ **proposed, not decided.** Written after dogfooding the pointing
> gesture shipped in PR #72/#74. Ledger: `9d0ea0b6` (least surprise),
> `532826c2` (reaching the code), `02ff24be` (placement), `02bded62`
> (reachability). Three open questions for Dan are marked **DECIDE** below.

## The complaint

Pointing shipped working on diff rows and live feed blocks. Dogfooding it
produced three reports in one sitting, which look like three bugs and are one
design:

1. *"it violates the law of least surprise in that it doesn't work on anything
   else"* — and then, explicitly: **everything should be pointable.**
2. *"i'm also not sure what the selector box on the left for streaming stuff is
   for"* — the feed gutter is a 3px unlabelled rectangle.
3. *"it also doesnt seem like i can select multiple lines"* — multi-line exists,
   but only as "click a line, then click a *lower* line in the same file".

The single sentence underneath all three:

> Every line of every rendered thing should carry the same visible pointing
> affordance, a drag across it should select a range, and "pointable" should
> mean the record's content — not just diffs.

Answering these one patch at a time is what produced six patches in a day, each
revealing the next gap. Hence a design.

## What is true today

Verified in code, not recalled.

**The anchor vocabulary has exactly two kinds** (`crates/junto-kernel/src/anchor.rs:178-185`):

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    Code(CodeAnchor),     // { commit, path, blob, span }
    Stream(StreamAnchor), // { session, op_id }
}
```

So the surface's inconsistency is not arbitrary — it is the vocabulary showing
through. A diff row is the only rendered thing that yields a path plus a
new-file line; a feed block is the only thing that yields a conversation
container index. A memo, a log, a live-plane snapshot, the channel brief and an
entry card have nothing to build an anchor *from*.

**Two properties make the fix cheap.**

- `Anchor` is **internally tagged** by a `kind` field. Adding a third variant is
  purely additive: every existing `{"kind":"code",…}` and `{"kind":"stream",…}`
  value is byte-identical afterwards, so no canonical bytes move and no existing
  signature is invalidated. (`ADR 0033`; the same reasoning as the
  `skip_serializing_if` additions in `Assertion`.)
- An artifact is **already immutably addressable**. `ArtifactAttached`'s own
  entry id *is* the artifact's id (`entry.rs:374`), and its `provenance`
  carries a URI plus a `ContentDigest` so "drift is detectable". The ledger is
  append-only, so an artifact's content can never change under an anchor.

## DECIDE 1 — a third `Anchor` variant for record content

Proposed:

```rust
/// Pinned to a line span inside content already in the record.
RecordAnchor {
    /// The `ArtifactAttached` entry — which IS the artifact's id.
    entry: EntryId,
    /// The artifact's content digest, from its provenance.
    digest: ContentDigest,
    /// 1-indexed inclusive line span, exactly as `CodeAnchor` means it.
    span: Span,
}
```

**Why this rather than `DocAnchor` (spec §B4).** `DocAnchor { subject, version,
locator, quote }` is designed for **mutable external** documents — a Google Doc,
a Confluence page — which is why it carries a quote and needs the
Exact/Moved/Orphaned re-anchoring discipline (§318). Record content needs none
of that: it is immutable and digest-pinned, so a `RecordAnchor` is **Exact
forever, by construction**. The hard half of B4 is re-anchoring, and this
sidesteps it entirely.

The two are complementary, not competing: `RecordAnchor` covers what junto
already holds; `DocAnchor` still owes the off-repo document case. This proposal
does **not** close B4.

**What it makes pointable immediately:** every memo, log, diff and snapshot
artifact — i.e. the whole session record, which is most of what a reviewer looks
at.

## DECIDE 2 — are ledger *entries* pointable?

Anchoring into an artifact is unambiguous. Anchoring into an **entry** (an
assertion's rationale, a line of the brief) overlaps something that already
exists: commenting on a claim *is* `ratify` / `park` / `correct`, and ADR 0003
keeps the entry kinds a closed set.

Two readings, and this needs Dan:

- **Entries are pointable too.** "Point at this sentence in this assertion" is a
  real want, and an annotation is not a verification act — it is a remark. Cost:
  a `RecordAnchor` whose `entry` is any entry, not only an `ArtifactAttached`,
  and a digest question (an entry's canonical bytes are its digest).
- **Entries are not pointable.** Pointing is for *content*; entries are
  *claims*, and the instrument for a claim is a verification act with a
  rationale. Keeps the vocabulary honest and the surface simpler.

I lean the second, weakly. The brief is a projection rather than content, so
"point at the brief" is really "point at the entry it came from", which is the
verification act again.

## DECIDE 3 — sequencing against `532826c2`

`532826c2` asks for a **review-first arrangement** (the diff as the pane's
primary object, the record as a side panel). This proposal makes everything
pointable *within* the current arrangement. They are independent but both touch
the same rendering code. Before, after, or one plan?

## The gesture

Independent of the anchor decision, and where the other two complaints land.

**One affordance, everywhere.** Every pointable line gets the same gutter
marker, in the same place, whether it is a diff row, a memo line or a feed
block. Today a diff row is a full-width invisible button and a feed block is a
3px bar — two different mechanisms for one idea, and neither announces itself.

**Drag to select a range.** Press on a gutter, drag across rows, release. This
is buildable now: `mouse_area` in iced 0.14 exposes `on_press`, `on_enter`,
`on_move`, `on_release`, which is enough for press-drag-release across rows. The
current click-then-click-lower gesture exists only because I built the rows on
`button`, which reports no modifier or drag state in `on_press` — a tooling
constraint I let become a UX decision.

Consequences worth stating:

- Selection becomes **directionless** — dragging up works — which removes the
  "clicking above the start silently restarts" surprise.
- Click-then-click-lower should stay as a keyboard-free fallback.
- The gutter needs a tooltip. It is the one affordance a reviewer must
  understand, and right now it has no label at all.

## Slices

| # | Slice | Touches | Blocked on |
|---|---|---|---|
| 1 | `RecordAnchor` variant + validation | `junto-kernel`, `junto-live` | DECIDE 1 |
| 2 | Universal gutter — one affordance on every pointable line | `junto-iced` | — |
| 3 | Drag selection via `mouse_area` | `junto-iced` | 2 |
| 4 | Anchor memos/logs/snapshots to `RecordAnchor` | `junto-iced` | 1, 2 |
| 5 | Settle annotations into entries | `junto-kernel` | still owed by `anchor.rs:11-13` |

Slices 2 and 3 are pure surface work and can start before DECIDE 1 lands — they
improve the diff case on their own, and `pointing.rs` is already
renderer-neutral and anchor-agnostic below the `Anchor` construction itself.

## Non-goals

- **Character-level selection.** `iced#36` (Text Selection) is still open on
  0.14, verified 2026-08-23. Anchors are line- and block-granular by design
  (`anchor.rs:20-22`), so this is not a blocker — but copying prose by hand
  still is not possible, and the `copy` buttons remain the workaround.
- **Relaxing the anchor-sourcing rule.** A `CodeAnchor`'s commit still comes
  only from a `WorktreeDiff` that arrived on the wire. `RecordAnchor` needs no
  commit at all, which is precisely why it does not weaken that rule.
- **Pointing at historical *code*.** A stored diff artifact does not record the
  commit it was taken at (`b650dbf5`), so a `RecordAnchor` into a diff artifact
  anchors *the artifact's text*, not the file at a commit. Those are different
  claims and should not be conflated in the UI.

## Testing

- `RecordAnchor` round-trips and the golden canonical-bytes test is untouched —
  the additive-variant claim must be *proved*, not assumed.
- An existing `Anchor::Code` / `Anchor::Stream` value deserializes unchanged.
- Drag selection: press on row 5, enter row 2, release → span 2..=5, asserted
  headlessly with `iced_test` rather than by driving a window.
- Every artifact kind yields a gutter on every line; a diff's removed rows
  still yield none.
