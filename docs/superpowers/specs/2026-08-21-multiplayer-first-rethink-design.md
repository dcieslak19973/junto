# Multiplayer-first rethink — design

**Date:** 2026-08-21
**Status:** approved in brainstorming (Dan, 2026-08-21); spec for review
**Ledger:** channel `Multiplayer-first rethink 20260821` (`3c38ead9-4907-4646-99b7-23b21933da35`), diverged from `junto-dev`. Four decisions recorded provisionally: `4256bd91` (topology), `f1cb3110` (collapse), `60810a3b` (subjects), `ff397d5f` (remote agents); `33949ae1` corrects the reopening survey.
**Prompted by:** Zed Delta (2026-08-12), Cursor Origin/Continuity (2026-08-18), and OpenClaw multiplayer (ClawCast ep. 8, 2026-08-20) — three vendors moving the shared unit of work off the individual machine inside nine days.

## Summary

junto already has a **multiplayer record** and a **single-player workshop**. The record is genuinely multi-actor: Party, founder-granted membership ([0017](../../adr/0017-party-is-a-projection-membership-is-founder-granted.md)/[0035](../../adr/0035-membership-is-set-based-except-after-revocation.md)), signed entries ([0033](../../adr/0033-entries-are-signed-over-their-canonical-bytes.md)), union-merge sync ([0011](../../adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md)), gates, lineage ([0027](../../adr/0027-channel-lineage-is-diverge-converge-edge-entries.md)). Two members share a channel fine — *afterward*. The work in flight is machine-resident at every layer: one local path per channel (`launch.rs:425-439`), a per-session worktree (`185fd301`), a localhost singleton host ([0015](../../adr/0015-one-host-per-machine-serving-many-substrates.md)), SSH-tunnel remoting ([0018](../../adr/0018-human-surface-is-a-desktop-shell-over-the-host.md)), and a live WebSocket served only by the driving host (`live_ws.rs:56`).

**The worktree is the visible symptom; machine-residency of the unit of work is the disease.** Four decisions treat the disease while keeping every settled architectural position:

1. **Topology: federation.** Each member keeps their own host; live sessions become peer-shareable. No shared gateway.
2. **Primitive: collapse.** A thread *is* a channel. Ceremony stripped, no required home repo, lineage carries the structure.
3. **Subjects: plural, typed, pluggable.** A thread is about 0..N subjects; a repo is one kind beside a Slack channel, a doc, a ticket.
4. **Agents: remote by default.** A remote agent is a session running under some junto host I can reach; local is the degenerate case.

Nothing here weakens the durable record. [ADR 0011](../../adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md)'s union-merge, hard constraint #3's no-CRDT-in-the-record, and [ADR 0034](../../adr/0034-crdt-confined-to-the-live-plane.md)'s confinement of loro to the ephemeral plane all survive untouched.

## What is settled territory, and what this reopens

Recorded in full at `33949ae1`. Three positions are genuinely reopened:

- **The reopening ladder** (`81ca1c38`, ratified 2026-08-20) gated rung 4 on "evidence rungs 1-3 cannot deliver". This design re-centres before rung 3 was attempted. Deliberate.
- **Session ownership as the turn-taking answer** (`9391191a`). Handoff reopens it — but decision 4 resolves it *without* concurrency control: ownership becomes who holds the control channel, still policy rather than data model.
- **Async deliberation over co-presence** (`junto.md:189`/`:209`), *partially*: presence and live viewing already shipped, handoff is co-presence-adjacent, but nothing here introduces real-time co-*decision*, so `:209` survives.

One position is **not** reopened, contrary to an earlier claim in this channel: the single-player wedge (`junto.md:46-56`) is an adoption-sequencing position. [`attention.md:40-44`](../../attention.md), tagged settled, already says *"the solo wedge is a Party of one human plus their agents, not the design ceiling. The attention design must generalize without rework."* This design cashes that cheque.

Dead-ends checked before proposing (all four in `junto-dev`): `779ad00e`, `4dd4ccb9`, `361e45ce`, `1b976fe8`. None covers this territory. The adjacent park `1d9cf9b1` (turn-taking) was already resurrected 2026-08-20.

---

## 1. Subject and Mount

### The split

`domain-model.md:32` holds a decision worth protecting: a Workspace is *"a machine fact, never ledger content (paths don't sync)."* Correct — `D:\git\junto` here is `/home/dan/junto` there. But a Jira ticket is not a machine fact: every party member must know a thread is about `PROJ-412`. Today's `Workspace` conflates the two because there is one kind and it is always a local path.

| | **Subject** | **Mount** |
|---|---|---|
| What | what the thread is *about* | how *this machine* resolves it |
| Example | `git+https://github.com/dcieslak19973/junto.git` · `jira:PROJ-412` · `slack:T123/C456` · `doc:sha256:ab3f…` | `D:\git\junto` · *(none)* · *(none)* · `D:\notes\spec.md` |
| Lives in | the ledger — durable, portable, synced | `~/.junto/mounts.toml` — machine-local, never synced |
| Cardinality | 0..N per thread | 0..1 per subject, per machine |

This preserves "paths don't sync" exactly as settled, and it explains the differing costs of the two halves of the ask: **multi-repo is easy** (the list already exists; `launch.rs:433` merely takes `.first()`), **non-repo needs the split** — a ticket is a Subject with no Mount, which the current model cannot express.

This is debt repayment, not new scope. `domain-model.md:28` already declares channels repo-agnostic (*"may work through one repo, several, or none"*), `:30` already separates home substrate from scope, and `:32` already shapes Workspace as a list — *"v1 uses exactly one, and it must be a git repo"* was the shortcut. All three worked-example playbooks were written end-to-end and then forced through a git checkout.

[ADR 0023](../../adr/0023-launching-agent-sessions-oneshot-first-pty-next.md) states both halves outright, including the reason for the shortcut: *"A channel's home substrate is **not** where work happens (the junto-dev record lives in a different repo than the code it describes)… The file stores a list of repos per channel so multi-repo inquiries are additive later; **v1 reads exactly one, and it must be a git repo (diff capture leans on git)**."* That parenthetical is the whole constraint — the `.git` requirement exists to serve `workspace_diff`, so a subject that cannot produce a diff was never actually blocked by anything but an unexamined default. Confirmed empirically: `~/.junto/substrates.toml` registers `D:\git\junto-ledger` and `D:\git\wmux\wmux`, and all fifteen channels — `junto-dev`'s 196 entries included — live in dedicated ledger repos, not in the project repos they describe.

### Capabilities are computed, never declared

Capabilities do not enter the ledger: they vary per machine, and recording them would smuggle machine facts into the record.

```
capabilities(subject, host) = kind_affordances(kind) ∩ provider_available(host) ∩ mount_present(host)
```

| Capability | Meaning | repo | Slack | Confluence | Jira |
|---|---|---|---|---|---|
| `read` | fetch current state | ✓ | ✓ | ✓ | ✓ |
| `watch` | change events push in | ✓ | ✓ | ~ | ✓ |
| `anchor` | stable spans survive motion | ✓ | ✓ (immutable) | ~ (versioned) | ✗ |
| `diff` | mechanical before/after | ✓ | ✗ | ~ | ✗ |
| `execute` | an agent can run *in* it | ✓ | ✗ | ✗ | ✗ |
| `mutate` | junto can write back (gated) | ✓ | ✓ | ✓ | ✓ |

A repo is simply the kind that lights up every flag — which states "less worktree-oriented" mechanically rather than aspirationally. Per §4, capabilities resolve against the **executing host**, not the viewing human.

### Kernel change

Two payloads shaped like the existing `ArtifactAttached`:

```rust
SubjectAttached  { kind: SubjectKind, uri: String, digest: Option<Digest> }
SubjectDetached  { target: EntryId }
```

Append-only, folded by projection into `ChannelView::subjects`. No mutation, no new sync semantics. New `crates/junto-kernel/src/subject.rs` for `SubjectKind` and `Capability`.

### Host change

| Today | After |
|---|---|
| `workspace_for(home, channel) -> PathBuf` (`launch.rs:425-439`), returns first entry | `mounts_for(channel) -> Vec<Mount>`, resolved against the ledger's subjects |
| `remember_workspace` (`:466-502`), hard-requires `.git` | `remember_mount`, keyed by subject uri, no `.git` requirement |
| `~/.junto/workspaces.toml` | `~/.junto/mounts.toml` — clean cutover, no shim (regenerable machine config) |
| session cwd = the one workspace | session declares a primary subject; no executable mount ⇒ scratch dir |

### Provenance degrades honestly

No repo means no diff. A Subject carries an optional `digest` captured at attach; Artifacts keep their own. A document subject therefore yields *content-digest* provenance — "this memo was produced against this exact version of that page" — not diff provenance. Weaker, and the UI must show it as weaker, in the spirit of `CodeAnchor`'s three-state degradation.

### Naming

The noun is **`Subject`**. The apparent collision with [ADR 0019](../../adr/0019-decision-frames-on-subject-entries.md)'s "subject entries" resolves in our favour: the kernel already calls the referent of a verification act a **`target`** (`GateExecuted { target, … }`; `correct`/`park`/`ratify` all take `target`). 0019's prose is already out of step with the code — fix the prose, take the noun.

---

## 2. The collapse

### The tension, and the seam it resolves on

[ADR 0014:15](../../adr/0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md) removed implicit creation deliberately: *"A channel is something a member opens, not a side effect of a stray write — implicit create-on-first-`record` goes away."* The thing it defends against is **a stray agent write minting a channel**, and that concern stands.

A human typing a question and pressing enter is an explicit act — it is simply not a *form*. The collapse keeps the act and deletes the paperwork, splitting on the human/agent seam [ADR 0021](../../adr/0021-member-codes-guard-agent-surfaces-only.md) already draws:

| Surface | Opening a thread |
|---|---|
| **Human** (native) | the first message opens it. Genesis entry still written, still author-attributed, still explicit. No name, substrate, or playbook prompt. |
| **Agent** (MCP) | unchanged — `open_channel` keeps its member code and explicit arguments. |

Defaults at birth: name derived-or-`Untitled`; substrate = the party's record substrate (empirically already `D:\git\junto-ledger`); playbook unset; subjects empty.

### Governance is acquired, not innate

`Playbook: Option<Playbook>`. A bare thread is conversation + record + sessions — the kernel spine, no lifecycle. Stamping a playbook later switches on its Lifecycle, Routing Policy, Outcome + Rubric, and gates.

### Derived thread standing

The answer to junk threads, and the load-bearing part of this section. Entries have standing today; channels do not. Hundreds of threads leave the ledger sound — unratified entries already do not outrank ratified ones — but **recall breaks**, and recall is what makes junto useful.

| Derived standing | Condition | Visible to |
|---|---|---|
| `scratch` | no ratified entry, no deliverable | its author only |
| `standing` | ≥1 ratified entry | the party; feeds recall |
| `settled` | closed or converged | recall, as history |

Derived by projection from state the ledger already holds — no new entry kind, no user action. **A scratch thread is epistemically free: invisible to the brief until something in it is ratified.** Existence and standing become different things, which is what makes cheapness safe.

### Names stop being unique

[ADR 0014:16](../../adr/0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md) makes a name unique per home substrate. With cheap threads you will have twelve called "auth stuff". 0014 already did the hard part — *"the name is a human-facing label, not identity"* — so we drop only the uniqueness rule layered on top: resolution returns most-recent-match and disambiguates in the surface.

### Lineage becomes the primary birth channel

Already built ([0027](../../adr/0027-channel-lineage-is-diverge-converge-edge-entries.md)/[0028](../../adr/0028-eventually-consistent-lineage-reconciliation.md), PR #51). Under the collapse, `diverge` stops being a special side-quest gesture and becomes the ordinary way one thread spawns another. No code change — promotion from feature to organizing relation.

### Touched

| Thing | Change |
|---|---|
| `Host::open_channel` (`host.rs:493-560`) | name/substrate/playbook optional; substrate defaults |
| `ChannelOpened` genesis | playbook optional |
| kernel projection | derive thread standing |
| ADR 0014 | amend: names not unique; record the human/agent asymmetry |
| recall / brief / `list_channels` | filter on derived standing |
| MCP `open_channel` | unchanged |

---

## 3. Federation: transport, ownership, takeover

### The live plane is not rewritten

Its protocol is already transport-agnostic: the ed25519 challenge handshake (`live_ws.rs:156-170`), frozen-fork frame validation (`validate.rs` — imports untrusted bytes into a throwaway fork, verifies driver containers unchanged and every annotation parses/matches/verifies, rejects the whole frame on any failure), presence as a separate `EphemeralStore`, archival to `turn-{n}-live.loro`. None of it cares whether bytes arrived over a localhost WebSocket or a QUIC stream. **The transport is replaced; nothing else is.** `validate.rs` was already written as if the peer were untrusted, which becomes literally true.

### Transport: iroh

`n0-computer/iroh`, MIT OR Apache-2.0, 1.0 since June 2026: peer-to-peer QUIC addressed by ed25519 public key, ~95% direct connections, relay fallback via self-hostable `iroh-relay`. It sits behind a capability-flagged adapter, so it is replaceable. Alternatives assessed and ranked below it: user-supplied overlay (Tailscale/WireGuard — zero code, assumes buy-in), automated SSH tunnel (vendor-free, clunky), store-and-forward over refs (30s+ convergence — a floor, not a strategy). Hole-punching with git as the signalling channel is **not viable**: candidate retry is millisecond-scale, a push/fetch cycle is 10-30s.

### The ledger is the address book — with one correction

iroh addresses peers by ed25519 public key; [ADR 0035](../../adr/0035-membership-is-set-based-except-after-revocation.md) publishes an ed25519 public key per device in `KeyGrant`s. That is nearly a complete signed, founder-authorized, revocable peer directory with no discovery service.

Nearly — because reusing one keypair for entry signing *and* the transport handshake violates key separation. **Mint two keys per device and publish both in the same `KeyGrant`.** The enrolment ceremony is unchanged — one paste, one confirmation — the joiner's host simply mints a second keypair in the same step. This has been relayed to the in-flight device-pairing work ([design](2026-08-21-device-pairing-surface-design.md)), which owns the envelope.

**Liveness is never recorded** — hard constraint #3 forbids it and push/fetch is too slow regardless. To find live sessions, dial each party member's node and ask: O(party size), no ephemeral state to garbage-collect.

### Ownership and takeover

Single-writer is policy, not data model (`domain-model.md:89`), so takeover needs no concurrency control — it needs an owner and a transfer. The transfer is **recorded**, because who drove a session is provenance:

```rust
SessionHandedOff { session: SessionId, from: Member, to: Member }
```

This is where junto should differ from the demo that prompted it: there, *"someone jumped into that same session and just took over and finished it"* leaves one session with a silently changed driver. Here the chain is in the record.

Under §4 (agents remote by default), two mechanisms with a crisp boundary:

| | When | Result |
|---|---|---|
| **handoff** | origin host is up | same session, new driver, `SessionHandedOff` recorded |
| **seeded continuation** | origin host asleep or gone | new session on the new owner's host, seeded from the archive, linked by lineage |

v1 scope: **offer → accept** only. Forced claim on an unresponsive owner needs a policy decision (who may seize, after how long) and is deferred rather than guessed.

### `DocAnchor`

`CodeAnchor{commit, path, blob, span}` re-anchors to `Exact | Moved | Orphaned`. Documents get the same three-state honesty:

```rust
DocAnchor { subject: SubjectUri, version: Version, locator: Locator, quote: String }
```

Re-anchoring is fuzzy-match of `quote` against the current version, yielding the same enum. This is exactly what the `anchor` capability means: can this provider supply a stable-enough locator plus a version? A Slack message is immutable, so always `Exact`; a Confluence page has version numbers, so usable; a Jira body has neither, so no `anchor`, and the UI says so rather than pretending.

---

## 4. Agents are remote by default

### Smaller than it sounds

`acp.rs:4`: *"junto speaks ACP (newline-delimited JSON-RPC over the adapter's stdio)"*; `:117-134` spawns the adapter as a child and talks JSON-RPC over its pipes. junto already addresses agents **by protocol, never by in-process API** — [ADR 0024](../../adr/0024-acp-is-the-harness-protocol.md) settled that, and says so in as many words: *"ACP is **transport**."* Serialization is already paid; only the pipe is local.

[ADR 0023](../../adr/0023-launching-agent-sessions-oneshot-first-pty-next.md) already took the first step for a different reason. Its `oneshot-exec` v1 keeps session state *"in the harness's own session storage, **not** in a host child process — the host restarts constantly (every rebuild), and `--resume` makes that harmless."* The agent is already not owned by the host process; this design finishes the thought by making the *transport* location-independent too.

> A remote agent is a session running under some junto host I can reach.

No new component: the far side is another per-machine singleton host doing its existing [ADR 0015](../../adr/0015-one-host-per-machine-serving-many-substrates.md) job, reached over the §3 transport. Local is the degenerate case — dialling your own host.

| Today | After |
|---|---|
| local session — in-process child, LiveDoc owned by my host | session under **my** host |
| teammate's session — the federation special case | session under **their** host |
| cloud / beefy-box session — unbuilt | session under a host **there** |

Three code paths become one, and the remote path cannot rot because every local session exercises it. `ExecutionBackend` (specified in [`pluggability.md`](../../pluggability.md), used by ADRs 0023/0024) collapses from *spawn strategy* — local / WSL / SSH / sandbox — to *endpoint selection*.

### It settles a gap in §1

The machine that needs the checkout is the **executing host**, not the watching human. Capabilities therefore resolve per executing host. A human with no clone can drive a session on a host that has one — the piece that makes "less worktree-oriented" true for humans specifically: your laptop stops needing to be where the work is.

### The bright line

> **A junto host runs sessions on behalf of exactly one human identity — its owner.** Reaching a host means asking *its owner's* junto to run something as *them*.

The moment one host drives sessions under several humans' identities, junto has rebuilt a shared gateway by accident and inherited the permissioning problem OpenClaw named on the ClawCast (*"it definitely needs a different permissioning scheme… a little out of scope for this release"*). **Multi-tenancy, not remoteness, separates federation from a gateway.** This is a named invariant because it is the kind of line that erodes by convenience.

### Costs

- **Auth on the local path.** Dialling your own host still needs the handshake; loopback keeps [ADR 0012](../../adr/0012-mcp-over-http-is-the-first-write-surface.md)'s posture, where the OS remains the trust boundary.
- **Lifecycle across the wire.** `kill_on_drop(true)` (`acp.rs:123`) reaps the child today; a session on another host must be supervised and reaped *there*, and CLAUDE.md already flags Windows process-tree kills as fiddly.
- **Failure modes multiply.** "The UI lost the feed" becomes "is the agent alive, and whose problem is it" — to be designed, not discovered.

---

## 5. The surface

The guardrail is already ratified. `attention.md:337`: *"Be the place attention goes, not a predictor of when to interrupt."* The board is explicitly **not a queue** (`:85`) and not a room list. Multiplayer must not smuggle in a channel sidebar or an all-activity feed; ordering stays attention-ranked, never chronological.

The focus board (shipped, slice 15 / `04c94274`) already has *needs-you* and *waiting-on-others*. Multiplayer adds one lane and two filters:

| | Change |
|---|---|
| **New lane: `live now`** | sessions running across every reachable host. Ranked by attention, not presence for its own sake. |
| **Standing filter** | scratch threads never reach the board; author-only. This is what stops cheap threads becoming a wall. |
| **Subject chips** | a thread card shows its subjects; chips grey out where *your* host lacks the capability. |
| **Blocking-by-name, extended** | `attention.md:52-60` already designs it; it now reads across machines. |

**Session view:** stream, span-anchored annotations, presence avatars, owner named, and the acts — **watch · annotate · steer · offer**, with claim appearing on an offered session. Everything but offer/claim exists today.

---

## 6. Build sequence

Two tracks, independent until they meet.

**Track A — the primitive** (no network work)

| # | Slice | Proves |
|---|---|---|
| A1 | `Subject` + `Mount`; `mounts_for` replaces `workspace_for`; kinds `repo` + `document` | a thread with no repo exists and runs a session |
| A2 | collapse — optional name/substrate/playbook, derived standing, zero-ceremony human open, ADR 0014 amendment | opening a thread costs one keystroke and does not pollute recall |
| A3 | surface — standing filter, subject chips, thread view | the board survives hundreds of threads |

**Track B — remoteness** (B2 depends on the device-pairing work landing)

| # | Slice | Proves |
|---|---|---|
| B1 | ACP transport becomes pluggable; **the local path goes through it, over loopback** | one code path, zero distributed-systems bill |
| B2 | iroh transport; party-dial liveness; hostile-connection hardening (rate limits, connection caps, non-party rejection before handshake) | a second machine watches a live session |
| B3 | `Session.owner`; offer/accept; `SessionHandedOff` | overnight handoff, both directions |
| B4 | `DocAnchor`; annotations on document subjects | Delta's best borrow works off-repo |

Then **third subject kind → extract `SubjectProvider`**. Rule of three (`CLAUDE.md:160`), not before: only `repo` is built today, and extracting a provider trait from four hypotheticals is the move that convention forbids.

**B1 is the slice to defend hardest.** It buys the whole one-path simplification while everything still runs on one machine, and it de-risks B2 completely — by the time iroh lands the protocol will have been exercised by every session for weeks. It is also the answer to the strongest objection recorded against decision 4 (*"distributed-systems tax on a path that is local 95% of the time"*): B1 pays no tax, and B2 ships only when a second machine exists.

## Relationship to the in-flight device-pairing work

[`2026-08-21-device-pairing-surface-design.md`](2026-08-21-device-pairing-surface-design.md) — **currently on branch `dcieslak19973/initial-ux-delta-ish`, so this link dangles until it merges** — is a **prerequisite, not a competitor**. It moves identity from CLI-only into the native surface and is what makes distributed device keys usable — without it, a watcher hand-copies `keys.toml` (`keys.rs:37`). Two coupling points, neither requiring a redesign:

1. It must publish a **second, transport-scoped key** per device (above). Relayed; cheap now, a second enrolment migration later.
2. Its Decision 1 rejected device-level trust because grants are per-channel, and its consequences note *"a lost laptop is N retirements."* Fine at 14 channels; under the collapse N reaches hundreds, and the 32-entry cap on `InvitePayload.channels` is stressed. `channels_for` and the per-channel outcome walk should stay shaped so a later device-level grant layers on without reworking the envelope.

## Non-goals

- **A shared gateway**, in any form. Rejected explicitly (`4256bd91`); the §4 bright line is its guard.
- **CRDT in the durable record.** Hard constraint #3 and [ADR 0011](../../adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md) stand; loro stays confined to the ephemeral plane per [0034](../../adr/0034-crdt-confined-to-the-live-plane.md).
- **Multi-writer co-editing** (ladder rung 4). Not attempted; nothing here requires it.
- **Real-time co-decision.** `junto.md:209` survives; deliberation stays async.
- **A chat surface.** No rooms, no channel sidebar, no chronological feed.
- **Forced session claim** on an unresponsive owner. Needs a policy decision; deferred.
- **Extracting `SubjectProvider`**, until a third concrete kind exists.
- **Record migration** between substrates. Still unbuilt, still preserved as possible by [ADR 0014:10](../../adr/0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md); not needed, because the party record substrate is already distinct from subject repos.

## Testing

- **Subject/Mount:** a thread with zero subjects opens and runs a session in a scratch dir; a thread with three repo subjects resolves three mounts; a `jira:` subject attaches with no mount and reports `read`-only capabilities; `mounts_for` returns nothing for an unmounted subject without erroring; a subject digest captured at attach is stable across sync.
- **Capabilities:** the same thread yields different capability sets on two hosts, one with a mount and one without — proving computation is per executing host, not recorded.
- **Collapse:** a human open with no arguments writes a genesis entry with derived name, default substrate, and `playbook: None`; the MCP path still refuses a missing member code; two threads may share a name and resolve by id; a thread with no ratified entry is `scratch` and absent from the brief; ratifying one entry moves it to `standing` and it appears.
- **Remote agents:** an identical session transcript results whether the ACP transport is a child pipe or a loopback stream; killing the far host mid-turn surfaces a distinguishable error rather than a hang.
- **Transport:** a non-party node id is refused before the handshake; a party node with a retired grant is refused; a valid peer receives the same LiveDoc stream a local watcher sees; annotations from a remote watcher pass `validate.rs` and reach the driving agent through the existing steer path.
- **Handoff:** offer/accept transfers ownership and records `SessionHandedOff`; the previous owner's writes to driver containers are refused afterwards; with the origin host stopped, the fallback produces a new session seeded from the archive with a lineage edge to the old one.
- **`DocAnchor`:** an annotation on an unchanged document re-anchors `Exact`; after edited surrounding text, `Moved`; after the quote is deleted, `Orphaned` — the same three-state discipline `CodeAnchor` is pinned to.
- **End-to-end, two junto homes on one machine** (the pattern the device-pairing plan uses): open a thread with a `document` subject on home A, run a session, watch and annotate it from home B over the real transport, hand it off, and assert the record on both sides carries the subject, the annotations' ratified outcomes, and the handoff entry.
- **The surface is exercised, not assumed:** drive the native board through a scratch thread staying hidden, a ratified thread appearing, a live session from another home showing in `live now`, and an offer/claim round trip. A GUI claim with no GUI run is not evidence.

## ADRs owed

Written with the implementation, citing this spec: **Subject/Mount** · **the collapse + derived standing** (amends 0014) · **agents remote by default + the one-identity-per-host invariant** (extends 0023/0024) · **iroh as the live-plane transport** (extends 0034) · **session ownership and handoff**.
