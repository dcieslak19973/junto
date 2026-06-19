# Channel lineage is diverge/converge edge entries (a cross-channel DAG)

Status: accepted (Dan, 2026-06-18) · builds on [`0002`](0002-ledger-entries-are-immutable.md), [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md), [`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md), [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md), [`0022`](0022-channel-close-and-reopen-are-lifecycle-entries.md) · far-side delivery in [`0028`](0028-eventually-consistent-lineage-reconciliation.md) · realizes the settled design in [`docs/attention.md`](../attention.md) §Side-quests / §Convergence

Channels form a **lineage DAG**: a child channel **diverges** from a parent at a point (the side-quest), and channels **converge** by a recorded act (a child back into its parent, or two into a new continuation). `0016` anticipated this family ("forking … closing … decided when concrete"); this ADR decides it. The verb pair is **diverge / converge** (settled in `attention.md`; never "fork", which implies copying history — exactly wrong, since entries are immutable and channel-scoped, `0002`).

## The shape: an edge is two entries, one in each ledger

An entry lives in exactly one channel's ledger and `0016`'s `target` only references *within* its channel. A lineage edge connects **two channels**, so each edge is recorded as **a pair of entries — one in each endpoint's ledger — referencing the other channel's id** ("both ledgers carry the relationship", `attention.md`). Reading either channel's ledger alone reveals its edges; lineage never requires scanning the whole substrate.

This extends `0003`'s closed `kind` set (as `0016`/`0020`/`0022` did) with **four distinct, self-documenting variants** — not one parametrized `LineageEdge` payload — to match the existing enum's grain (`ChannelOpened`, `MemberAdded`, `ChannelClosed`…) and keep the bytes legible:

- `DivergedFrom { parent: ChannelId, at: Option<EntryId> }` — child side. `at` is the entry **in the parent** the child split from.
- `ChildDiverged { child: ChannelId }` — parent side (announcement; the parent flows on).
- `ConvergedInto { target: ChannelId }` — source side, recorded at close.
- `ConvergenceReceived { source: ChannelId }` — target side. The **two-into-a-new-continuation** case is simply *two* of these in the continuation's ledger, naming both predecessors.

The host **normalizes all four into one generic `LineageEdge` projection view** (`{ relation, direction, other, point }`). So storage is concrete and self-documenting while the "same algebra" `attention.md` wants — recall and rendering "follow lineage edges" generically — is recovered *once*, in the projection, not baked into the bytes.

### The divergence point is optional and unvalidated

`at` is `Option<EntryId>`: a divergence is often "from the parent **as it stands now**" with no single anchor. It is **not validated** against the parent at write time — the kernel can't cheaply verify a cross-channel id (the parent may be in another substrate or arrive later by sync), consistent with the ledger already tolerating out-of-order delivery. A non-resolving `at` degrades to the divergence entry's own timestamp. `at` drives two things: the brief-inheritance cutoff (summarize the parent **as of** that point) and the x-coordinate where the child attaches on the lineage strip.

### Projection tolerates dangling edges

The two entries are **not** written atomically (separate refs, possibly separate substrates, sync races, and — per `0028` — deliberate one-sided creation). Projection therefore **renders the side it has and marks the other "unresolved"**, never errors. Dangling is a normal, transient (or eventually-bounded, `0028`) state, not a defect.

## Semantics

- **Diverge creates the child; converge lands into an existing channel.** A deliberate asymmetry matching reality: a side-quest is *born* by diverging (so the diverge op opens the child, genesis author = founder, in the **parent's home substrate**), but a convergence merges into a channel that already exists. `converge` never creates a channel.
- **Converge closes the source** and is gate-checked: it **refuses while the source has any open gate** — each pending proposal must be decided (reject *is* "abandon") or re-proposed into the target first (with provenance). Forces honest disposal of open questions at exactly the right moment (`attention.md`). Only *gates* block; provisional assertions park whole with the closed source.
- **No Party inheritance** (refines, doesn't bend, `0017`). The diverger founds the child and is already a member of the parent (the precondition for authoring the parent-side entry); the continuation is an ordinary opened channel. Rosters are **never auto-copied or auto-unioned** — side-quests are frequently someone else's inquiry, and auto-copy bakes in an often-wrong default. Membership stays explicit (`MemberAdded`).
- **Context inherits along edges, not membership.** A child's (or continuation's) brief carries a read-only **inherited-context block**: a one-hop summary of each incoming-edge ancestor's *standing decisions* (the parent as of `at`; each convergence predecessor as of convergence). Outgoing edges appear as lightweight references (a parent lists its open diverged children; a source shows "converged into …"). **One hop for v1** — transitive recall is pure projection logic with zero byte impact and is deferred.

## Surface

The lineage strip draws **real attachment points** on the existing stacked layout: a branch's diverge curve starts from its *actual parent's* row at the divergence x; a converged channel's curve lands in its *actual target's* row (two merge-ins for a two-predecessor continuation); roots (no resolved incoming diverge edge) and any dangling/unresolved edge **fall back to the baseline**. No new layout engine. Write surface: MCP `diverge_channel` / `converge_channel` (agents spin up side-quests — the `attention.md` vision) and thin `junto diverge` / `junto converge` CLI wrappers.

Cross-**machine** lineage is **sync-mediated** (a member with a synced clone authors the far side locally; it converges through the shared git remote, hard constraint #3) — **not** a remote MCP surface, which would turn `0017`/`0021`'s accident-proofing member codes into load-bearing network auth. The edge representation is identical either way, so remote-MCP federation stays open as a separate, deferred decision.

## Considered and rejected

- **A single parametrized `LineageEdge` payload.** Fewer variants and "same algebra" literally in the bytes — rejected: reads worse, fights the enum's grain; the unification is recovered cheaply in projection.
- **One entry + host-side discovery-by-scan for the reverse direction.** Less data — rejected: breaks cross-substrate (the parent's substrate can't scan a child it doesn't hold) and contradicts the "both ledgers" design.
- **Auto-inherit / auto-union the Party.** "The side-quest has the same people" — rejected (see Semantics): often wrong, hard to walk back.
- **Fields on `ChannelOpened`/`ChannelClosed` instead of edge entries.** Saves only the child/source variant (the far side needs its own entry regardless) and mixes lifecycle with lineage — rejected for a uniform edge family.
