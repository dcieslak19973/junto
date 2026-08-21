# Live session plane — design

**Date:** 2026-08-20
**Status:** approved in brainstorming (Dan, 2026-08-20); spec for review
**Provenance:** [`competitive-landscape.md`](../../competitive-landscape.md) (Zed Delta/DeltaDB + Cursor Origin/Continuity assessment, 2026-08-20), the reopening ladder recorded there, and the brainstorming session of 2026-08-20.

## Summary

A **live plane** for junto: watchers join a running Session mid-flight across
machines — no commit/push — see its conversation and worktree evolve, see who
else is watching, and attach **span-anchored comments** that flow back to the
driving agent as steering context (the lavish-axi loop). This is **rungs 1+2**
of the graduated ladder from the Delta/DeltaDB assessment, built so that rungs
3–4 (shared documents, replicated worktrees) and a p2p transport remain
policy-and-plumbing changes rather than rewrites.

**What this is not:** a DeltaDB clone. No multi-writer co-editing, no
conversation-as-source-of-record, no new database. The durable record stays
append-only ratified entries under `refs/junto/*` (ADR 0011 untouched).

## Decisions (made in brainstorming, in order)

| # | Decision | Alternatives rejected |
|---|---|---|
| 1 | The itch is **liveness / join-mid-flight** (rung 2) | anchors-only (rung 1 alone), between-commit versioning, full CRDT worktree, positioning play |
| 2 | Rung 4 (multi-writer CRDT worktree) is **not foreclosed**: representation is multi-writer-capable, policy is single-writer | plain event log (would foreclose via rewrite cost) |
| 3 | Watchers get **view + anchored comments** — rung 1 becomes a prerequisite | view-only; direct steering injection; both |
| 4 | Comments reach the **driving agent mid-flight** (lavish-axi loop) | humans-only; turn-boundary-only |
| 5 | Transport v1: **driver-hosted server** (the driving machine's junto host serves watchers) | git-remote ref piggyback; self-hostable relay; piggyback-then-relay |
| 6 | Transport must admit **p2p evolution**: sync layer is transport-agnostic CRDT doc sync; star topology is deployment config | host-serialized feed protocol |
| 7 | Write topology: **multi-actor annotations** (watchers write annotation ops as CRDT actors); conversation/worktree streams stay single-actor (driver) | driver-sequenced all writes (bakes star topology into the write path); plain log |

## Dead-ends surfaced (per the dead-ends convention)

- **`1d9cf9b1`** (park ratified 2026-06-14): turn-taking unsolved; worktree
  isolation prerequisite. **Conditions changed:** isolation landed
  (`185fd301`); Delta is the use-case evidence; Dan reopened 2026-08-20. This
  design keeps single-writer session ownership, so the turn-taking problem is
  sidestepped, not solved-by-CRDT.
- **`b405a1cb`** (ratified 2026-06-13): collaborative space constrained to a
  turn-based, append-only versioned artifact. **Compatible:** annotations are
  append-only; the LiveDoc archives as a versioned session artifact; only
  ratified outcomes fold into entries.
- **Hard constraint #3** ("zero CRDT / presence / shared-buffer"): this design
  requires an ADR **scoping that constraint to the durable record** (which is
  what ADR 0011's argument actually covers). CRDT is confined to the ephemeral
  live plane; the record never changes. See Process obligations.

## Architecture

Three planes, strict separation:

```
  Live plane (ephemeral, CRDT)      Session plane (existing)      Record (untouched)
 ┌───────────────────────────┐     ┌───────────────────────┐     ┌────────────────────┐
 │  LiveDoc per session      │◄────│  ACP loop             │     │ append-only entries│
 │  (loro document)          │────►│  steer / interrupt    │     │ refs/junto/*       │
 │   • conversation (drv)    │     │  owned worktree       │     │ union-merge sync   │
 │   • worktree      (drv)   │     └───────────────────────┘     └────────────────────┘
 │   • annotations (multi)   │        session end: LiveDoc               ▲
 └───────────────────────────┘        archived as versioned              │
  + Presence (loro EphemeralStore,    session artifact ──────────────────┘
    own sync channel, never archived) (ratified outcomes only, via
                                       existing verification acts)
```

### LiveDoc

One **loro** document per live session (MIT — verify at adoption), three
containers with two write policies:

| Container | Writers | Content |
|---|---|---|
| `conversation` | driver's host only | the session stream junto already emits over SSE (ADR 0023): turns, tool events, thinking |
| `worktree` | driver's host only | edit/diff events derived from ACP tool-call events + periodic `git diff` snapshots. **No FS watcher in v1.** |
| `annotations` | any authenticated member | signed annotation ops (below), add-only |

**Presence** (who is watching) is **not** a `LiveDoc` container: it rides
loro's separate ephemeral store on its own sync channel — any authenticated
member may write into it, never persisted, and never archived with the
document snapshot at session end.

Single-writer on `conversation`/`worktree` is **enforced by session ownership
(policy), not by the data model** — lifting it later (rung 3: a shared plan
document; rung 4: the worktree) changes authorization, not format.

### Transport

- **v1:** one WebSocket endpoint on the driver's junto host speaking loro's
  sync protocol. Watchers need network reach to the driver (LAN / tailscale /
  tunnel) — honest local-first; no third-party infrastructure.
- **Evolution:** the sync layer is transport-agnostic by construction; relay
  or p2p mesh (e.g. iroh — verify license at adoption) swaps plumbing, not
  protocol. Star topology is deployment configuration.
- Reconnect: loro state vectors — missed ops backfill automatically.

### Auth

WebSocket handshake is challenge-response against the member's existing
ed25519 public key (`junto-kernel::sign`). Non-members rejected. Unsigned or
signature-invalid annotation ops are dropped and logged, never merged.
Presence requires auth (who is watching is itself information).

## Anchors & annotations

Two anchor kinds:

- **`CodeAnchor { commit, path, blob: ContentDigest, span }`** — pins "what it
  was" at comment time (reuses provenance's `ContentDigest` discipline).
  **Re-anchoring** ("where is it now") hunk-maps from the pinned commit to the
  worktree's current state via git diff/blame plumbing, shelled out in
  `junto-substrate-git`. Three-state degradation, surfaced honestly:
  1. **exact** — span unchanged;
  2. **moved** — re-anchored to the new location;
  3. **orphaned** — content gone; render against the pinned snapshot.
- **`StreamAnchor { session, op_id }`** — conversation turns, plan steps, tool
  calls. LiveDoc ops have stable identity by construction (CRDT op ids), so no
  re-anchoring machinery is needed.

**Annotation op:** `{ id, author, anchor, body, ts, signature }` — ed25519-
signed, add-only. No edit/delete: a revision is a new annotation superseding
the old by reference — the same append-only discipline as the record.

**Dual use:** `CodeAnchor` + re-anchoring is the substrate decision blame
(roadmap item 9) needs; the reverse provenance index (file/line → entries +
sessions) is a later projection over the same type. Built once here.

## Feedback loop (lavish-axi)

- The driver's host watches `annotations`; new ops enqueue per session in
  arrival order (no global sequencing — consistent with decision 7).
- **Default:** the accumulated batch injects at the agent's next turn boundary
  as one formatted steer message — each annotation rendered as `path:span` +
  quoted **pinned** text + body, so the agent sees what the human saw even if
  the code has moved.
- **Urgent flag:** triggers the existing interrupt → `TurnControl::Steer` →
  re-prompt path immediately (`acp.rs`; the 2026-06-19 mid-turn-steer design).
- Injection mechanics reuse the shipped steer machinery; no new harness
  capability is required. True mid-generation interruption remains
  harness-dependent; the floor is turn-boundary injection.

## Failure behavior

**Invariant: the live plane may never block or corrupt the session loop.**

- Publishing ops into the LiveDoc is fire-and-forget from the ACP loop.
- Driver host death: watchers receive a terminal `end` event (same pattern as
  the SSE stream). At most ephemeral presence is lost; annotations live in the
  doc and survive.
- Session end: LiveDoc snapshot archived as a versioned session artifact
  (`b405a1cb`'s shape); ratified outcomes fold into entries via existing
  verification acts.
- Queue policy: presence drops-oldest under pressure; annotations are never
  dropped.

## Component placement

| Component | Where | Notes |
|---|---|---|
| Anchor types (`CodeAnchor`, `StreamAnchor`), annotation op | `junto-kernel` | serde + canonical bytes, `serial.rs` round-trip pattern |
| Re-anchoring | `junto-substrate-git` | shell-out to git plumbing, existing posture |
| LiveDoc, sync, presence, annotation validation | `junto-live` (new crate) | owns the loro dependency |
| WS endpoint, auth handshake, annotation→steer bridge, archive-on-end | `junto` (host) | beside the existing SSE stream |
| Watcher surface | `junto-iced` | existing SSE subscription generalizes to remote WS; annotation composer + anchor rendering |

## Testing

- `junto-kernel`: anchor + annotation canonical-bytes round-trips.
- `junto-substrate-git`: re-anchoring against scripted repos (tempdir +
  `git init`); one scenario per degradation state.
- `junto-live`: convergence — two docs, concurrent annotation ops from
  distinct actors, sync both directions, assert identical state;
  reject-unsigned; presence excluded from persistence.
- Loop: stub ACP harness (acp-mock pattern) — annotation in → steer message at
  turn boundary; urgent → interrupt path.
- Smoke: two host processes on localhost; watcher connects over WS; annotation
  round-trips into the driver's steer queue; session end archives the doc.

## Non-goals (explicit)

- Multi-writer co-editing of conversation, plan, or worktree (rungs 3–4 —
  door open, not built).
- Any change to the record, ADR 0011, or entry kinds.
- FS-watcher-based worktree streaming (derived events + snapshots suffice in v1).
- Zero-install browser/wasm watcher client.
- Relay or p2p transport (v1 is driver-hosted; the seam is designed, not built).
- Hosted service of any kind.

## Process obligations (before/with implementation)

1. **Record the unpark:** new assertions in `junto-dev` citing `1d9cf9b1`
   (prerequisites landed; Delta as use-case evidence) and `b405a1cb` (this
   design as its rung-1 realization) — the record shows the resurrection.
   Drafted, exact wording in [ADR 0034](../../adr/0034-crdt-confined-to-the-live-plane.md)'s
   appendix, pending recording.
2. **ADR:** scope hard constraint #3's "zero CRDT / presence" to the durable
   record; CRDT confined to the ephemeral live plane / versioned artifacts.
   Done: [ADR 0034](../../adr/0034-crdt-confined-to-the-live-plane.md).
3. **License verification at adoption:** loro (MIT expected); iroh only if/when
   the p2p rung is climbed. Done for loro: MIT, confirmed at adoption
   (transitive MPL-2.0 deps named in ADR 0034's Consequences).

## Risks

- **Harness injection variance:** turn-boundary injection is the guaranteed
  floor; interrupt behavior differs per harness — branch on ACP capability
  flags, never vendor names (constraint #4).
- **Re-anchoring quality:** hunk-mapping degrades on heavy refactors; the
  orphaned state is the honest fallback, and the pinned snapshot keeps every
  comment legible forever.
- **loro API stability:** pre-1.0-adjacent ecosystem; confine it to
  `junto-live` so a swap (yrs/automerge) stays one crate wide.
- **Windows networking:** driver-hosted WS must be exercised on Windows early
  (junto's primary dev platform).
