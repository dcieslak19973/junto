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
| **Channel** | One workspace for one *unit of inquiry* (a question / piece of work); fuses conversation + work + party + gates + record. **Repo-agnostic:** no repo is part of a channel's identity or scope — a channel may work through one repo, several, or none. Identity is a **globally unique minted id**; the human **name is a label**, unique only within the home substrate, bound to the id when the channel is *opened* (ADR 0014). | ✅ kernel |
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

## Verbs (operations & channel transitions)

- **open** a channel (of a playbook) — an explicit, recorded act: mints the channel's id and writes a `ChannelOpened` genesis entry binding name → id in the home substrate (ADRs 0014/0016; never implicit on first write). Possibly **triggered** by an inbound Connector (alert/ticket → channel). Anticipated siblings, same recorded-act treatment when designed: **fork** (from a point in time) · **close** (ADR 0016).
- **frame** — the deliberation step: *plan* (code) / *pre-register* (research) / *triage* (incident). ⚠️ kernel stage or per-playbook?
- **join / invite** — manage the Party.
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