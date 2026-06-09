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
8. ✅ **RESOLVED: LedgerEntries are immutable; corrections are new entries (Dan, 2026-06-09).** An entry is written once and never mutated. **Anchor: an accounting ledger** — you don't erase a posting, you record a **correcting/adjusting entry**. Consequences:
   - **`verification_state` is NOT a mutable field.** To ratify / park / falsify / correct, **append a new entry that references the prior one**; an entry's *current* verification state is **derived by folding the log** (event-sourcing projection).
   - This is the only model consistent with the **append-only, partition-by-author** git-refs substrate (`architecture.md`) — the ratifier is frequently a *different* author than the original recorder, so in-place mutation is impossible by construction.
   - The ratification/correction append carries its own author + ts + rationale, so *who* changed an entry's standing and *why* is itself recorded — not lost to a field overwrite.
   - **Prior art (clean-room inspiration only):** git-bug append-only *operations* folded into entity state (already junto's cited substrate prior art); event sourcing / compensating events; ADRs with *superseded-by* links; OSF/clinical-trial **pre-registration** (the research playbook's hypothesis ledger is this same shape — a provisional claim recorded *before* its evidence).
9. ✅ **RESOLVED: one LedgerEntry envelope + a closed set of kinds; verification is itself an entry (Dan, 2026-06-09).** Modeled on git-bug's single typed *Operation* stream (and double-entry accounting's single journal): **all** ledger appends share one envelope (id, author, ts, kind, optional reference(s) to prior entries, provenance) and differ by a **closed `kind`**. Starter kinds: **`Assertion`** (the decision/finding/claim) · **`Ratification`** · **`Park`/`Falsify`** · **`Correction`** (supersede). A channel's current state (incl. each entry's verification standing) is **derived by folding** the entry stream.
   - **Ratify/park/falsify/correct are LedgerEntries, NOT Events.** They are durable, human-authored, rationale-bearing *record* — which is what the Ledger holds. **`Event`** stays the separate *machine-observability* stream (`session.*`, `eval.*`) — git-bug has no event stream at all, reinforcing that these are different layers.
   - Keep the `kind` enum **closed in the kernel**; resist sprawl — add a kind only when a concrete playbook forces it (rule of three).
10. ⚠️ **PARKED: promoting an Event into a LedgerEntry (Dan, 2026-06-09).** May need to turn a machine **Event** into a durable **LedgerEntry** (e.g. an eval result becomes a recorded finding). Deferred — *later problem*. Likely shape when built: a Member authors/ratifies an `Assertion` that references the Event as **provenance**. Don't build yet.
11. ✅ **RESOLVED: an Author is any Member — human OR agent (Dan, 2026-06-09).** Agents are first-class peers (the founding thesis), identity = git author, so **an agent may author any LedgerEntry kind**. The *"human-authored or human-ratified, never auto-captured truth"* guardrail (`self-improving-harness.md`) is **not an authorship restriction** — it lives at the **Gate / Verifier** layer: an entry enters in **provisional** standing and gains *ratified* standing only by passing the playbook's gate. **Authorship ≠ authority** (cf. a lab notebook: researcher authors, witness countersigns). Who/what may *ratify* is **per-playbook gate-routing** — human sign-off for consequential work, a held-out **eval** for the self-improvement playbook (so ratification is not even always human). The kernel does **not** restrict authorship by entry kind.
12. ✅ **RESOLVED: the kernel `Assertion` holds `statement` + `rationale` + `provenance` only (Dan, 2026-06-09).** The minimal universal content; everything playbook-flavored stays out of the kernel entry.
    - 🔮 **NOTE FOR FUTURE US:** *"alternatives / options considered"* is deliberately **not** a field — record it inside `rationale` prose for now. Promote it to a first-class field **only when a second playbook proves the shape** (rule of three). Likely trigger: deliberation-heavy playbooks (code-PR *plan*, research *pre-register*). The same "defer until a second playbook proves it" rule applies to other playbook-flavored fields (e.g. a per-entry `outcome`, which also collides with the channel-level **Outcome** noun).
13. ✅ **RESOLVED: collapse `Park` + `Falsify` into one kind (Dan, 2026-06-09).** One negative-result kind, named **`Park`** (already junto's vocabulary: the **Outcome** noun and the *park/falsify* verb; "parked" = a kept dead-end). It covers both *abandoned / inconclusive* and *disproven*; the distinction lives in `rationale`. Negative results are **kept, never deleted** (institutional memory). Split back out only if the **research** playbook's false-discovery tracking needs the abandoned-vs-disproven distinction structured (same "defer until a second case proves it" pattern as #12).
14. ✅ **RESOLVED (working): a `ProvenanceRef` = URI + optional content digest (Dan, 2026-06-09).** An `Assertion`'s `provenance` is a list of refs; each is a **URI** (*where* the input is — git object / **Artifact** / external dataset; aligned with **W3C PROV** IRIs) **plus an optional content digest captured at record time** (`ProvenanceRef { uri, digest: Option<ContentDigest> }`).
    - **Why the digest:** it makes the ref **drift-detectable / tamper-evident** — re-hash the target later and compare to catch *stale as-of data*, delivering re-runnability even when the URI points at a **mutable** target. Prior art: Subresource Integrity, lockfile checksums, SLSA/in-toto, OCI `@sha256:` pinning, Nix fixed-output.
    - **Optional:** omit when the URI is already content-addressed (a git oid is its own integrity) or content isn't hashable.
    - **Self-describing / algorithm-agile:** store `sha256:…` (algo embedded, multihash-style), not a bare hash — records are long-lived (retention) and algos get deprecated.
    - Keep it its **own type**, not a bare `String`, so it can later become a typed enum (`Artifact(ArtifactId) | GitObject(Oid) | Session(SessionId) | External { uri, digest }`) without churning the entry API — the digest naturally lives on the mutable (`External`) variant.
    - ⚠️ Still the *pointer*, **not** the full re-runnable provenance (command+commit+data-as-of+seed+env, per #6) — deferred until Artifacts. Whether junto also **archives** the referenced bytes (content store) is a later Artifact-store question. Honest limit: re-fetch may be impossible for ephemeral sources, but the digest still records *what we saw*.
