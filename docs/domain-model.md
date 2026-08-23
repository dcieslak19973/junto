# junto — domain model (nouns & verbs)

> The **ubiquitous language**: the words junto's design and (eventually) code should use consistently. **Extracted from the worked examples + the spine — not invented.** Tags: ✅ settled · 🔵 proposed/conjecture · ⚠️ fuzzy, needs a decision. The goal is a *lean shared vocabulary*, not a taxonomy — if a noun isn't earning its place, cut it.

## Shape (how the nouns relate)

```
  Channel  (one unit of inquiry; has a playbook)
   ├─ Party            — the Members (human + agent) on it
   ├─ Conversation     — append-only Messages
   ├─ Session(s)       — agent executions → Artifacts (+ Provenance + Events)
   ├─ Gate(s)          — checkpoints consequential actions must pass
   ├─ Ledger           — the durable, append-only record (synced via Substrate)
   │    └─ entries          decisions / findings / claims (provenance-bound, with a
   │                        verification state); entries reference Artifacts
   └─ Deliverable      — what it produced (PR | memo | fix | promoted policy | parked)

  Playbook  supplies: Lifecycle (stages) · Routing Policy · Outcome (+ Rubric, Grader)
                          · offered tools/agents · artifact kinds + renderers
```

> **Terminology aligned on Anthropic's Managed Agents (ADR 0025).** `Persona → Agent` (the config), `Outcome (produced) → Deliverable` so **Outcome** = the *target* (description + Rubric), `Agent Session → Session`, routing "Rubric" → **Routing Policy**; adopt **Rubric** (verification criteria) + **Grader**; the **Verifier** noun retires. The tables below are mid-migration — ADR 0025 is the source of truth until the big-bang rename PR lands.

## Core nouns

| Noun | One-line meaning | Layer |
|---|---|---|
| **Channel** | One workspace for one *unit of inquiry* (a question / piece of work); fuses conversation + work + party + gates + record. **Repo-agnostic:** no repo is part of a channel's identity or scope — a channel may work through one repo, several, or none. Identity is a **globally unique minted id**; the human **name is a label**, bound to the id when the channel is *opened*, **not required to be unique anywhere** (ADR 0014, amended by the collapse — spec §2). | ✅ kernel |
| **Channel lineage** | The DAG of relationships *between* channels, formed by **diverge** and **converge** edges (see verbs). Each edge is recorded as a pair of immutable entries — one in each endpoint's ledger, referencing the other channel's id — so histories never literally merge; lineage is a *recorded relation*, not a mutation. Recall and the human surface's lineage strip both follow these edges. | ✅ kernel |
| **Home substrate** | The one place a channel's durable record lives (today: one git repo's `refs/junto/*`). Exactly one per channel. A storage/admin fact, *not* the channel's scope — it may be a repo entirely unrelated to the repo(s) the inquiry works through. | ✅ kernel |
| **Channel binding** | Which channel(s) a working session consults and records into — a property of the **working checkout** (worktree), never derivable from the repo. Dogfood bridge: committed project default + uncommitted per-worktree override; destination: a membership concern (join at session start, ADR 0013). | 🔵 dogfood convention |
| **Workspace** | The machine-local repo(s) a channel's Agent Sessions execute in — a channel→repos mapping in machine config (`~/.junto/workspaces.toml`), set at first launch and remembered. A **machine fact, never ledger content** (paths don't sync); shaped as a list so one channel can span several repos later (v1 uses exactly one, and it must be a git repo). The inverse of Channel binding: binding says which channels a checkout consults; workspace says where a channel's agents execute. | 🔵 dogfood convention (Dan, 2026-06-12) |
| **Playbook** | The *type* stamped on a channel; supplies its lifecycle, **Routing Policy** (gate-routing), **Outcome + Rubric** (what verified means), tools, renderers. (code-PR / research / prod-troubleshooting / self-improvement.) | ✅ kernel concept; playbooks are plugins |
| **Member** | A participant in a channel — **human or agent** (agents are first-class). | ✅ kernel |
| **Party** | The set of Members on a channel (its roster / ACL). | ✅ kernel |
| **Role** | A Member's function in a channel (commissioner, reviewer, approver…). | ⚠️ first-class noun, or just per-playbook labels? |
| **Message** | One append-only entry in the channel Conversation, from a Member. | ✅ kernel |
| **Artifact** | A verifiable output produced in-channel (diff, chart, log table, test result, memo, query result) — **not scrollback**; rendered on the surface. | ✅ kernel |
| **Provenance** | The binding of an Artifact/claim to the exact inputs that produced it (command, commit, data as-of, seed, env) → re-runnable. | ✅ kernel (a relation, not a free-standing thing) |
| **Session** | One agent execution (an **Agent** running on an Execution Backend) → Artifacts + Events; has live state (working / blocked / awaiting-approval / done / error). Aligned with Anthropic's "Session" (ADR 0025); the old "Agent Session" qualifier is dropped. | ✅ kernel |
| **Agent** | A reusable, machine-local **config** (model · system · tools · MCP · skills) that a machine Member runs — Anthropic's "Agent" (was junto's "Persona", ledger `251c4bba`). Distinct from a Member: *an agent Member runs an Agent*. | ✅ kernel |
| **Gate** | A checkpoint a *consequential action* must pass before it happens; routed (auto / one-approver / full-review / hard-gated); records approver + rationale. | ✅ kernel (engine) |
| **Ledger** | The channel's durable, append-only, provenance-bound record (synced via the Substrate). The research "hypothesis ledger" is just *the ledger of a research channel*. | ✅ kernel |
| **Ledger entry** | One decision / finding / claim in the ledger: question, options, rationale, outcome — provenance-bound, with a **verification state** (provisional → ratified \| parked/falsified). References Artifacts. The "why" that outlives the channel. | ✅ kernel (the load-bearing noun) |
| **AssertionKind** | The epistemic state carried on an Assertion: `Finding` (an observation, worth recording and citing; asks nobody to decide anything) or `Decision` (a choice or claim wanting a verdict). Absent `kind` reads as `Decision`. Kernel state, not playbook vocabulary — but *whether* a Finding needs a human is app policy (`attention_for_view`), never kernel behavior (ADR 0039). | ✅ kernel |
| **answers** | The ids of open entries an Assertion claims to settle — authored by the recorder at record time, never inferred. Inert until the assertion itself is verified: a claim to have answered them, not a mutation of the targets. | ✅ kernel |
| **Deliverable** | What the channel produced — a PR, memo, fix, promoted policy, or *parked dead-end*. One of several per playbook. (Was junto's "Outcome"; renamed in ADR 0025 so "Outcome" can take Anthropic's meaning.) | ✅ kernel |
| **Outcome** | The *target* — "what done looks like" for a piece of work: a description + a **Rubric**. Anthropic's "Outcome" (ADR 0025). A Playbook supplies the Outcome shape; a **Grader** evaluates a Deliverable against it. | ✅ kernel |
| **Rubric** | The gradeable verification criteria (markdown) a **Grader** scores a Deliverable against — Anthropic's "Rubric". Supplies "what verified means" for a playbook. *(Not the routing layer — that is now **Routing Policy**, ADR 0007/0025.)* | ✅ playbook-specific |
| **Grader** | A clean-context evaluator that scores a Deliverable against a Rubric (separate context window — clean-room judgment) and returns per-criterion pass/fail. Anthropic's "Grader". | ✅ kernel |
| **Lifecycle / Stage** | The playbook-specific sequence of states a channel moves through (this is "the workflow of a playbook" in the process sense). | ✅ playbook-specific |
| **Event** | The observability/provenance atom (`session.*`, `proposal.*`, `eval.*`, `policy.*`); one stream → dashboards + the self-improvement playbook + the Record. | ✅ kernel (cross-cutting) |

## Boundary nouns (the pluggable edges — adapters)

| Noun | What it abstracts |
|---|---|
| **SubstrateProvider** | Where/how the Record is stored & authorized — *forge-as-hub* (OSS) / *central SoR* (regulated). |
| **ForgeAdapter** | Git host: GitHub / GitLab / Bitbucket. |
| **AgentHarnessAdapter** | *Which* agent: Claude Code / Codex / Goose / OpenCode / Copilot CLI. |
| **ExecutionBackend** | *Where* the harness runs: local / WSL / SSH / remote sandbox / managed platform. |
| **ChatConnector** | External chat ingested into the surface: Slack / Discord / Telegram / Teams. |
| **Connector** | Stateful external SoR bridge: `IssueTracker` (Jira/Linear) · `Knowledge` (Confluence). |
| **MemoryProvider** | Event sink + observability fan-out + the self-improvement loop's feed. |
| **InferenceEndpoint** | The LLM endpoint (hosted or on-prem). |
| **MCP tools** | Per-playbook domain capabilities. |

*All adapters declare **Capabilities**; junto branches on capability flags, not vendor name.*

## Agent-policy nouns (what the self-improvement playbook edits)

| Noun | Meaning |
|---|---|
| **Skill** | Agent-authorable instructions (markdown) — a reusable how-to; **= the Agent Skills standard ([agentskills.io](https://agentskills.io) / `SKILL.md`)** that Claude, OpenCode, Codex et al. load. junto authors and *evolves* these rather than inventing a rival concept. *(Not called a "playbook" — that's the work-type term above; and the broader self-improvement targets are separate nouns: **Workflow**, **Agent**.)* |
| **Workflow (Definition)** | Agent-automation script — a conductor coordinating sub-agents (the *automation* sense of "workflow"). |
| **Agent Definition** | An agent's config (markdown). |
| **Policy Version** | A versioned snapshot of skills/workflows/agent-defs — for the self-improvement loop's provenance + rollback. |
| **Eval** | A held-out measure of "better" — the Verifier for the self-improvement playbook. |

## Live plane nouns (the ephemeral CRDT plane — [ADR 0034](adr/0034-crdt-confined-to-the-live-plane.md))

A separate, ephemeral plane beside the Record, permitted narrowly by ADR 0034: it scopes hard constraint #3 ("no CRDT") to the durable record, so this plane is where the constraint does *not* apply. Never synced through `refs/junto/*`; never itself a Ledger entry. `junto-live` owns it.

| Noun | Meaning |
|---|---|
| **Live plane** | The ephemeral, per-Session CRDT plane: a watched conversation/worktree stream + Annotations + Presence, none of it durable on its own. Distinct from the Session plane (the ACP loop, steer/interrupt) and the Record (append-only entries) — see the spec's three-plane diagram ([`docs/superpowers/specs/2026-08-20-live-session-plane-design.md`](superpowers/specs/2026-08-20-live-session-plane-design.md) §Architecture). |
| **LiveDoc** | One [loro](https://github.com/loro-dev/loro) CRDT document per live Session, three containers: `conversation`/`worktree` (driver-only, seq-keyed replace-in-place — the Session's event stream and derived edit/diff events) and `annotations` (multi-actor, add-only, keyed by Annotation id). Archived as a versioned session Artifact (`turn-{n}-live.loro`) when the turn ends; the document itself is never a Ledger entry. **Presence is not a `LiveDoc` container** — it rides a separate `EphemeralStore` (below), deliberately kept out of what gets archived. |
| **Annotation** | A span-anchored, signed, add-only comment any authenticated Party member may attach to a running Session — `id`, `author`, `anchor`, `body`, an optional `excerpt`/`supersedes`, an `urgent` flag, a `timestamp`, and an optional `signature` over its own canonical bytes the way an entry is (ADR 0033). A revision is a new Annotation superseding the old by reference (`supersedes`), never an edit. Delivered to the driving agent as steering context at the next turn boundary, or immediately if flagged urgent. **Not** a **Message** (a Conversation entry is durable and unsigned; an Annotation is ephemeral-plane and signed) and not a **Ledger entry** (only a ratified *outcome* of a live session folds into one). |
| **Anchor** | Where an Annotation points. `CodeAnchor { commit, path, blob, span }` pins "what it was" at a commit; re-anchored across code motion by `junto-substrate-git` into `Exact \| Moved \| Orphaned` (three-state, degrading honestly rather than silently). `StreamAnchor { session, op_id }` pins a position in the live stream and needs no re-anchoring — LiveDoc op ids are stable by construction. Also the type the deferred "decision blame" projection (file/line → entries + Sessions — on the roadmap, not parked) will read later — built once, here. |
| **Presence** | Who is watching a live Session right now — a loro `EphemeralStore`, 30s timeout, never persisted, never an Artifact. |
| **watcher** | An authenticated Party member connected to a Session's live WebSocket to view it and attach Annotations, without owning the Session or writing its `conversation`/`worktree` — that stays the driving Member's alone (policy, not the data model; ADR 0034). |

## Device-key nouns (per-device signing, revocation, pairing surface — [ADR 0035](adr/0035-membership-is-set-based-except-after-revocation.md), [ADR 0036](adr/0036-device-pairing-is-a-surface-flow-over-a-multi-channel-invite.md))

A member's signing authority is no longer one key per email; it's a per-device projection layered beside the Party, amending [ADR 0017](adr/0017-party-is-a-projection-membership-is-founder-granted.md)'s set-based membership check for revoked members only. `junto-kernel` owns the projection; `crates/junto/src/{enroll,invites,keys,host,web}.rs` own the enrollment exchange, the CLI, and the native surface's identity endpoints.

| Noun | Meaning |
|---|---|
| **Key grant** | One key ever granted signing authority for an email (`KeyGrant`): the **signing** public key, an optional **transport** public key (`transport_key`, ADR 0036 — never a verification key; an entry signed with it never verifies), the entry that authorized it (`granted_by` — a `ChannelOpened` genesis or founder-authored `MemberAdded`), and when it was retired (`retired_at`, `None` until a founder parks it). A member has one Party row but can hold many grants, one per device. |
| **Keyring** | The per-email projection of every Key grant (`Keyring = HashMap<email, Vec<KeyGrant>>`), distinct from the **Party**: the Party answers "who is on the roster", the Keyring answers "which keys may sign for them right now". |
| **Transport key** | The second half of a device's identity (`Member.transport_public_key`, `KeyGrant.transport_key`), minted alongside the signing key at enrollment for a federated peer (iroh) to address this device by, never accepted as a signature over a ledger entry. `Member.transport_public_key` is additive to the pre-ADR-0036 record — omitted on serialize when absent — so a `Member` written before this change still deserializes and re-canonicalizes to identical bytes, the property that keeps its signature valid. `KeyGrant.transport_key` never serializes at all: `KeyGrant` is a projection folded at read time from `Member.transport_public_key`, not a wire type. |
| **Device enrollment** | The exchange that lets a member add a device's key pair without moving either private half across a wire: **`junto invite`** (founder-only; `--channel` repeatable, each resolved to its canonical id; mints one single-use code covering every named channel) → **`junto enroll`** (runs on the new device; mints its own signing *and* transport keypairs locally and echoes back only the two public halves) → **`junto add-member --enroll`** (founder-only; **takes no `--channel`** — the channel set is recovered from the invite by `invites::channels_for`, not retyped; burns the invite per channel and records one grant per channel). The native surface offers the same exchange as a Settings "this device" join screen and a channel-pane "invite a device" / "redeem" pair, over `POST /devices/enroll` (invite-token-authorized, no founder check — see **Identity surface** below), `POST /devices/preview` (unauthenticated read), and `POST /members` (per-channel founder check inside the redemption engine). |
| **Invite (v2)** | A single-use, 600-second-TTL code naming a **channel set** (`InvitePayload.channels: Vec<String>`, capped at 32, non-empty), not one channel. The set never travels in the code: the founder's `invites.toml` holds one `InviteRecord` per `(token_sha256, channel)`, recovered locally by `invites::channels_for`. A v1 code is refused outright (no shim — the TTL leaves no window one could still be live in). |
| **Redemption outcome** | Per-channel, not all-or-nothing: one redemption (`POST /members` or `junto add-member --enroll`) walks the invite's recovered channel set and reports `granted` / `already_a_member` / `invite_already_used` / `not_founder` / `failed` for each. The invite record is consumed before the append, for every channel that reaches that point, so a channel whose append then fails is already spent and simply drops out of `channels_for`'s next read (`redeeming_a_mixed_set_burns_only_what_it_granted`, `a_failing_channel_reports_failed_and_the_run_keeps_going`) — if it was the only channel still covered, redemption refuses the whole thing as exhausted rather than offering an empty set. A `failed` raised *before* the consume (an unresolvable channel, a projection or git-identity error, an unreadable invite store) burns nothing and stays retryable, exactly like `not_founder`/`already_a_member`. `invite_already_used` is reachable only from a stale duplicate invite record for the same `(token, channel)`, not from a normal re-paste after a partial failure. |
| **Retirement** | A founder-authored `Park` targeting the entry that granted a device's key (no new entry kind). Retires that one grant as of the park's own timestamp — inclusive at the boundary, so nothing signed at or before the park is affected. Two parks on the same grant: the **earliest** wins. Retirement is per channel — a device enrolled into N channels needs N retirements (or `revoke-member` acts) to fully offboard, a known limitation ADR 0036 records rather than fixes. |
| **Revocation cutoff** | An email-level, not grant-level, consequence: exists only once **every** grant for that email is retired, at the **latest** of their retirement timestamps — the moment the person held no valid key at all. While any grant stays active, there is no cutoff. Entries stamped strictly after the cutoff are *unrecognized*; the member is never removed from the Party, so their pre-cutoff history is untouched. |
| **Identity surface** | The native surface as an **authorization site**, not just a display (ADR 0036): six endpoints — `POST /invites`, `POST /devices/enroll`, `POST /devices/preview`, `POST /members`, `GET /channels/{c}/keys.json`, and a retire/revoke pair. No endpoint asks a human for a member code, honoring ADR 0021's principle — but the gate itself differs per endpoint, not one shared `WriteAuth::Human` check: `/invites` and retire/revoke call `identity::require_founder`; `/members` carries its per-channel founder check inside the redemption engine itself; `/devices/enroll` (the one endpoint that may mint secret material) has **no founder check and no member code at all** — the invite token is the authorization, and the caller being this machine's own localhost stands in for one; `/devices/preview` and `keys.json` are unauthenticated reads. `keys.json` publishes 16-hex fingerprints only, never a full public key. `unverified` and `unrecognized` badges now render natively as well as on the web surface. |

## Verbs (operations & channel transitions)

- **open** a channel (of a playbook) — an explicit, recorded act: mints the channel's id and writes a `ChannelOpened` genesis entry binding name → id in the home substrate (ADRs 0014/0016; never implicit on first write). Possibly **triggered** by an inbound Connector (alert/ticket → channel). Siblings, same recorded-act treatment: **close** (ADR 0016) · **diverge** / **converge** (lineage edges — see below).
- **diverge** — a *child* channel departs from a *point* in a parent (the common case: a side-quest). Recorded as a pair of entries, one in each ledger (the child's `DivergedFrom`, the parent's `ChildDiverged`); the parent flows on. The verb is settled as **diverge**, never "fork" (which implies copying history — exactly wrong; entries are channel-scoped and immutable). See `attention.md`.
- **converge** — two channels merge by *recorded act*, never a history mutation: either a child closes back into its parent, or both close into a new continuation channel whose genesis names its predecessors. Recorded as a pair of entries (`ConvergedInto` on the source, `ConvergenceReceived` on the target). Forces honest disposal of the converging channel's open gates.
- **frame** — the deliberation step: *plan* (code) / *pre-register* (research) / *triage* (incident). ⚠️ kernel stage or per-playbook?
- **join / invite** — manage the Party. Adding a device (not just a member) is **device enrollment**, above: `junto invite` → `junto enroll` → `junto add-member --enroll` (ADR 0035, extended by ADR 0036's multi-channel invite and `--channel` removal from `add-member --enroll`).
- **run** (act) — execute work in an **Agent Session** → Artifacts (+ Provenance + Events).
- **propose** — surface a change/finding for a Gate.
- **route** — the Gate decides the path (auto / approve / review / hard-gate), per the playbook's **Routing Policy**. The `auto` path is the **autonomy envelope** (ADR 0026): a human ratifies the Routing Policy for a region, and inside it a Grader-`satisfied` Deliverable auto-resolves the Gate **and emits a notification** (release notes) instead of pausing — outside it the Gate still waits for a human. Two invariants: editing a Routing Policy never routes to `auto` (no self-widening), and the grade is *read*, never *grants* autonomy (grade ≠ consent).
- **approve / reject** — pass or block a consequential action; record a rationale (not a checkbox).
- **promote** — (self-improvement) accept a Policy Version into use; versioned, reversible.
- **ratify** — confirm a ledger entry as *verified* (often the slow loop / AAR).
- **park / falsify** — close as a *negative result*, kept as institutional memory (never deleted).
- **record** — append a ledger entry (with Provenance) to the Substrate.
- **publish / push** — emit an Outcome to an external SoR (open PR via ForgeAdapter · publish memo via KnowledgeConnector · update ticket via IssueTrackerConnector).
- **sync** — push/fetch the Record via the Substrate (forge-as-hub).
- **observe / emit** — produce Events to observability + the loop.

## The lifecycle skeleton (generic) + per-playbook shapes

Generic: `open → frame → run(work) → [gate] → record(outcome) → closed | parked`

| Playbook | Lifecycle |
|---|---|
| code-PR | plan → build → [pre-remote review] → push(draft) → remote-review → merged |
| research | pre-register → investigate → analyze → [epistemic gate] → ratified \| parked |
| prod-troubleshooting | triggered → triage → investigate → [act: reversibility gate] → recovered → ratify(AAR) → closed |
| self-improvement | signal → propose → [eval gate] → promote \| reject |

## The kernel ↔ playbook seam (the one structural line)

- **Kernel (generic):** Channel · Member/Party · Message · Artifact · Provenance · Agent Session · **Gate engine** · **Ledger (entries)** · Outcome · Event.
- **A Playbook supplies:** the **Lifecycle** (stages), the **gate-routing function** (the single most playbook-specific thing), the **Verifier**, the offered tools/agents, and artifact kinds + renderers.

## Design decisions → ADRs

Settled **architectural** decisions live in [`adr/`](adr/), one file each; the index is [`adr/README.md`](adr/README.md).

**Settled naming** (low-stakes calls, already reflected in the tables above — recorded here only so the choice isn't re-litigated): **Agent Session** (over "Run"; always qualified) · **Playbook** (over "Channel Kind") · **Provenance** is a relation, not a standalone entity · keep **Gate** / **gate-routing** / **policy** distinct from agent **Policy Version**.

## Open questions ⚠️

- **Role** — a first-class noun, or just per-playbook labels on Members? *(parked — Dan undecided, 2026-06-08)*
- **frame / deliberate** — a kernel lifecycle stage with playbook-specific *content*, or fully per-playbook? *(parked — Dan undecided, 2026-06-08; also open in the spine)*
- **Event → LedgerEntry promotion** — turning a machine **Event** into a durable entry (e.g. an eval result becomes a recorded finding). Likely shape: a Member authors/ratifies an `Assertion` that references the Event as provenance. *(deferred — later problem)*
- **Administrative override to undo a rejection** — reject is *sticky* ([0006](adr/0006-gate-engine-event-sourced.md)), so reversing one needs an explicit, append-only-consistent **authority-bound** act (likely an `Override` kind). Depends on the **Role**/authority question above. *(deferred — Dan, 2026-06-09)*