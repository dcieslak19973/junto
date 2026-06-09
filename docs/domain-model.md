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

## Still fuzzy / naming decisions to make ⚠️

1. ✅ **RESOLVED: the Ledger model** (Dan, 2026-06-08). One channel → one **Ledger** of many **entries** (decisions/findings/claims); the "verified record" = the ratified entries. Research's "hypothesis ledger" = the ledger of a research channel. ("Ledger" chosen over "intent record"; the old separate "Record" noun is folded in.)
2. ✅ **RESOLVED: "Agent Session"** (over "Run") — always qualified (overload: terminal/login session; Ace's "Session" = our Channel). One Channel → many Agent Sessions. *(Event names follow: `session.*`; the `run.*` examples in `self-improving-harness.md` should be aligned when convenient.)*
3. ⚠️ **Role** — first-class noun, or just per-playbook labels on Members? *(parked — Dan undecided, 2026-06-08)*
4. ⚠️ **frame / deliberate** — a kernel lifecycle stage with playbook-specific *content*, or fully per-playbook? *(parked — Dan undecided, 2026-06-08; also open in the spine)*
5. ✅ **RESOLVED (Dan, 2026-06-08):** keep **Gate**, **gate-routing function**, and **policy** as distinct terms; "policy" (a ruleset) must not collide with agent **Policy Version**.
6. ✅ **RESOLVED (Dan, 2026-06-08): Provenance is a property/relation** on Artifacts & ledger entries — not a standalone entity.
7. ✅ **RESOLVED: "Playbook"** (over "Channel Kind" — Dan, 2026-06-08). A playbook = a kind of work + its lifecycle/gates/tools/verifier. (Note: "skill" is no longer called a "playbook" to avoid collision.)
