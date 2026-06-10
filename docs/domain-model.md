# junto — domain model (nouns & verbs)

> The **ubiquitous language**: the words junto's design and (eventually) code should use consistently. **Extracted from the worked examples + the spine — not invented.** Tags: ✅ settled · 🔵 proposed/conjecture · ⚠️ fuzzy, needs a decision. The goal is a *lean shared vocabulary*, not a taxonomy — if a noun isn't earning its place, cut it.

## Shape (how the nouns relate)

```
  Channel  (one unit of inquiry; has a playbook)
   ├─ Party            — the Members (human + agent) on it
   ├─ Conversation     — append-only Messages
   ├─ Agent Session(s)— agent executions → Artifacts (+ Provenance + Events)
   ├─ Gate(s)          — checkpoints consequential actions must pass
   ├─ Ledger           — the durable, append-only record (synced via Substrate)
   │    └─ entries          decisions / findings / claims (provenance-bound, with a
   │                        verification state); entries reference Artifacts
   └─ Outcome          — what it produced (PR | memo | fix | promoted policy | parked)

  Playbook  supplies: Lifecycle (stages) · Gate-routing function · Verifier
                          · offered tools/agents · artifact kinds + renderers
```

## Core nouns

| Noun | One-line meaning | Layer |
|---|---|---|
| **Channel** | One workspace for one *unit of inquiry* (a question / piece of work); fuses conversation + work + party + gates + record. | ✅ kernel |
| **Playbook** | The *type* stamped on a channel; supplies its lifecycle, gate-routing, verifier, tools, renderers. (code-PR / research / prod-troubleshooting / self-improvement.) | ✅ kernel concept; playbooks are plugins |
| **Member** | A participant in a channel — **human or agent** (agents are first-class). | ✅ kernel |
| **Party** | The set of Members on a channel (its roster / ACL). | ✅ kernel |
| **Role** | A Member's function in a channel (commissioner, reviewer, approver…). | ⚠️ first-class noun, or just per-playbook labels? |
| **Message** | One append-only entry in the channel Conversation, from a Member. | ✅ kernel |
| **Artifact** | A verifiable output produced in-channel (diff, chart, log table, test result, memo, query result) — **not scrollback**; rendered on the surface. | ✅ kernel |
| **Provenance** | The binding of an Artifact/claim to the exact inputs that produced it (command, commit, data as-of, seed, env) → re-runnable. | ✅ kernel (a relation, not a free-standing thing) |
| **Agent Session** | One agent execution (a Harness invocation on an Execution Backend) → Artifacts + Events; has live state (working / blocked / awaiting-approval / done / error). **Always qualified "Agent Session"** — bare "session" is reserved (overloaded: terminal/login session; and **Ace calls its *channels* "Sessions"** ≈ junto's Channel, the opposite layer). | ✅ kernel |
| **Gate** | A checkpoint a *consequential action* must pass before it happens; routed (auto / one-approver / full-review / hard-gated); records approver + rationale. | ✅ kernel (engine) |
| **Ledger** | The channel's durable, append-only, provenance-bound record (synced via the Substrate). The research "hypothesis ledger" is just *the ledger of a research channel*. | ✅ kernel |
| **Ledger entry** | One decision / finding / claim in the ledger: question, options, rationale, outcome — provenance-bound, with a **verification state** (provisional → ratified \| parked/falsified). References Artifacts. The "why" that outlives the channel. | ✅ kernel (the load-bearing noun) |
| **Outcome** | What the channel produced — a PR, memo, fix, promoted policy, or *parked dead-end*. One of several per playbook. | ✅ kernel |
| **Lifecycle / Stage** | The playbook-specific sequence of states a channel moves through (this is "the workflow of a playbook" in the process sense). | ✅ playbook-specific |
| **Verifier** | What "verified" *means* for this playbook (tests pass · reproducible re-run / pre-registration · AAR ratification · eval). | ✅ playbook-specific |
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
| **Skill** | Agent-authorable instructions (markdown) — a reusable how-to. *(Not called a "playbook" — that's the work-type term above.)* |
| **Workflow (Definition)** | Agent-automation script — a conductor coordinating sub-agents (the *automation* sense of "workflow"). |
| **Agent Definition** | An agent's config (markdown). |
| **Policy Version** | A versioned snapshot of skills/workflows/agent-defs — for the self-improvement loop's provenance + rollback. |
| **Eval** | A held-out measure of "better" — the Verifier for the self-improvement playbook. |

## Verbs (operations & channel transitions)

- **open** a channel (of a playbook) — possibly **triggered** by an inbound Connector (alert/ticket → channel).
- **frame** — the deliberation step: *plan* (code) / *pre-register* (research) / *triage* (incident). ⚠️ kernel stage or per-playbook?
- **join / invite** — manage the Party.
- **run** (act) — execute work in an **Agent Session** → Artifacts (+ Provenance + Events).
- **propose** — surface a change/finding for a Gate.
- **route** — the Gate decides the path (auto / approve / review / hard-gate), per the playbook's routing function.
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

Settled **architectural** decisions live in [`adr/`](adr/), one file each — extracted from this doc's former in-line decision log and **back-filled together**, so ADR cross-references point *backward only* and the file numbers are stable identifiers, not a strict timeline (each ADR's `Status:` line carries the actual decision date).

| ADR | Decision |
|---|---|
| [0001](adr/0001-ledger-is-the-durable-record.md) | The Ledger is the channel's durable record (one Ledger of entries; the old separate "Record" noun folded in). |
| [0002](adr/0002-ledger-entries-are-immutable.md) | Entries are immutable; correct by appending — standing is **derived by folding**, never stored (event-sourcing). |
| [0003](adr/0003-ledger-entry-content-model.md) | One entry envelope + a **closed `kind` set**; verification is itself an entry; kernel `Assertion` is minimal; `Park`/`Falsify` collapsed. |
| [0004](adr/0004-any-member-may-author-any-entry.md) | **Any Member (human or agent) may author any entry**; authority lives at the Gate/Verifier layer, not in authorship. |
| [0005](adr/0005-provenance-ref-uri-plus-digest.md) | A `ProvenanceRef` = URI + optional self-describing **content digest** (drift-detectable). |
| [0006](adr/0006-gate-engine-event-sourced.md) | The **Gate engine** is event-sourced into the Ledger (`Proposal`/`Approval`/`Rejection`; derived `GateStatus`; reject is *sticky*). |
| [0007](adr/0007-routing-stays-out-of-the-kernel.md) | **Routing stays out of the kernel**: it executes an `ApprovalRequirement`; the importable **Rubric** layer (future) decides it. |
| [0008](adr/0008-canonical-entry-serialization-is-jcs-json.md) | A `LedgerEntry`'s **canonical byte form** is **JCS / RFC 8785 JSON** (deterministic by spec, readable/diffable); newtypes re-validate on deserialize. |
| [0009](adr/0009-git-refs-substrate-ndjson-per-author.md) | The **git-refs substrate** stores an append-only **NDJSON log per author** under `refs/junto/<channel>/<author>` (local durable record; forge sync deferred). |
| [0010](adr/0010-canonical-order-and-dedup-by-entry-id.md) | Canonical order is **`(timestamp, author email, entry id)`** — a deterministic *total* order — and projection **dedups by `EntryId`** (substrates may hold duplicates). |
| [0011](adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md) | **Sync** = push/fetch of author refs to any git remote; divergence (same author, two machines) reconciles by a **deterministic union-merge** — set union of immutable entries *is* the merge (no CRDT needed). |
| [0012](adr/0012-mcp-over-http-is-the-first-write-surface.md) | The first write surface is **MCP over streamable HTTP** (`junto serve`, localhost:1727); **named channels** derive their id via UUIDv5 (no registry). Identity is claimed, not verified — a recorded dogfood-era limit. |
| [0013](adr/0013-host-serves-the-read-surface-recall-via-hook.md) | The host serves the **read surface**: an HTML channel page (the first pixel of the one surface) + a markdown `/brief`. Agent **recall is a membership concern** (inject at join time, once modelled); a SessionStart hook is the bridge. |

**Settled naming** (low-stakes calls, already reflected in the tables above — recorded here only so the choice isn't re-litigated): **Agent Session** (over "Run"; always qualified) · **Playbook** (over "Channel Kind") · **Provenance** is a relation, not a standalone entity · keep **Gate** / **gate-routing** / **policy** distinct from agent **Policy Version**.

## Open questions ⚠️

- **Role** — a first-class noun, or just per-playbook labels on Members? *(parked — Dan undecided, 2026-06-08)*
- **frame / deliberate** — a kernel lifecycle stage with playbook-specific *content*, or fully per-playbook? *(parked — Dan undecided, 2026-06-08; also open in the spine)*
- **Event → LedgerEntry promotion** — turning a machine **Event** into a durable entry (e.g. an eval result becomes a recorded finding). Likely shape: a Member authors/ratifies an `Assertion` that references the Event as provenance. *(deferred — later problem)*
- **Administrative override to undo a rejection** — reject is *sticky* ([0006](adr/0006-gate-engine-event-sourced.md)), so reversing one needs an explicit, append-only-consistent **authority-bound** act (likely an `Override` kind). Depends on the **Role**/authority question above. *(deferred — Dan, 2026-06-09)*