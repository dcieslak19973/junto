# junto — pluggability & vendor neutrality

> Design sketch for junto's swappable boundaries. **Interfaces below are candidates / conjecture** (🔵) — shapes to validate against real implementations, not settled APIs. Companion to `junto.md` (principle) and `architecture.md` (forge/connector origin).

## Principle

**Wrap every external dependency in a swappable adapter; nothing vendor-specific reaches the kernel.** One consistent shape across boundaries: each adapter exposes `capabilities()` + a set of normalized operations, and **junto branches on capability flags, not on vendor name.** Use the richest signal an implementation offers; degrade gracefully when it's absent.

Two corollaries that fall out repeatedly:
- **Auth stays with the adapter's vendor** wherever possible (esp. agent harnesses & inference) — junto holds as few secrets as it can. Critical for the on-prem/regulated posture.
- **Capability-driven, not name-driven** behavior is what keeps the kernel honest: the day a new vendor appears, you write an adapter, not a kernel `if`.

## Layered model

```
 L2  CHANNEL KINDS        collaboration templates (code-PR · research · prod-troubleshooting · …)
     │                    — the richest plug: declares lifecycle, GATE-ROUTING fn, roles,
     │                      offered tools, artifact renderers, views, review policy
 L1  ALREADY PLUGGABLE    MCP tools · agent definitions · skills · workflow defs
     │
 L0  KERNEL (never plugged) channel + ACL · git-refs durable record (hub-synced) · pty-exec→artifact ·
                          provenance · gate-ENGINE (state machine) · agent runtime ·
                          workflow runtime · MCP host · artifact store · search
        ▲
        │  adapters bridge L0 ↔ the outside world:
   ForgeAdapter · AgentHarnessAdapter · ExecutionBackend · ChatConnector
   · IssueTracker/Knowledge Connector · MemoryProvider · InferenceEndpoint · SubstrateProvider
```

The kernel provides the **gate-engine** (a state machine + approvals); a **Playbook supplies the gate-routing *function*** — the single most playbook-specific thing (per the rule-of-three synthesis: reversibility×blast-radius for incidents vs epistemic-rigor for research vs review-load for code-PR). **Design the routing as a pluggable function from day one; never hardcode code-PR's into L0.**

Likewise the **verifier is a per-playbook slot, not one model** (research pressure-test): "ledger entry" means *pre-registration / reproducible re-run* for research (verify **before** evidence), *AAR ratification* for incidents (verify **after** action), *tests pass* for code (mechanical). The kernel stores the ledger entry; the **Playbook supplies what "verified" means.**

A Playbook's **Rubric** (what a Grader scores a Deliverable against) need not be opaque test code — it can be a **human-readable *and* executable** artifact. The clearest worked instance is **BDD/Gherkin (Cucumber)** for the code-PR Playbook: scenarios a human reviews in plain language *and* a Grader runs, closing the gap that prose specs leave open ("how do we know the code actually adheres to the spec?"). It also keeps the verification artifact reviewable — one less opaque "AI test" for a human to audit. (External corroboration: Michal (Safe Intelligence), conference talk *Capturing decisions for humans and AI alike*, 2026.) This is a Playbook-supplied Rubric *format*, not a kernel concern — the kernel only stores the entry and its verification state.

> **What those terms mean** (for readers new to them):
> - **BDD (Behavior-Driven Development)** — writing a spec as concrete behavior examples ("given this situation, when X happens, then Y results") so the *same* artifact is spec, acceptance test, and documentation.
> - **Gherkin** — the plain-language syntax those examples are written in: structured English with keywords `Feature` / `Scenario` / `Given` / `When` / `Then`. Readable by a non-coder.
> - **Cucumber** — the tool that *runs* Gherkin. You write small "step definitions" binding each `Given/When/Then` line to real code, so the readable scenario executes as a test and reports pass/fail. It exists across languages (Ruby originally; JVM, JS, and a Rust `cucumber` crate).
>
> The payoff: one artifact a human can **read and sign off on** and a Grader can **run mechanically**. junto would borrow this as *one* candidate Rubric format — a Playbook's choice, not a kernel dependency.

## The boundaries

| Boundary | Adapter | Targets | Status |
|---|---|---|---|
| Git forge | `ForgeAdapter` | GitHub · GitLab · Bitbucket (cloud + self-hosted DC) | designed |
| **Agent harness** (*what* agent) | **`AgentHarnessAdapter`** | **Claude Code · Codex · Goose · OpenCode · gh Copilot CLI · …** | 🆕 sketched below |
| **Agent execution backend** (*where* it runs) | **`ExecutionBackend`** | **local · WSL · SSH · remote sandbox (E2B/Northflank/Coder) · managed-agent platform (Claude Managed Agents/Devin/open-swe/Temporal)** | 🆕 sketched below |
| **Chat** (optional) | **`ChatConnector`** | **Slack · Discord · Telegram · Teams** | 🆕 sketched below |
| Issue tracker | `IssueTrackerConnector` | git-native · Jira · Linear | designed |
| Knowledge SoR | `KnowledgeConnector` | Confluence (read via MCP + publish) | designed |
| Memory / observability | `MemoryProvider` | local/sqlite · OTEL · DataDog · Phoenix | designed |
| Inference | `InferenceEndpoint` | hosted · on-prem/internal (regulated) | noted |
| **Substrate / deployment** | **`SubstrateProvider`** | forge-as-hub (OSS) · central self-hosted SoR (regulated) · [peer-mesh: deferred] | 🆕 from research pressure-test |
| Domain tools | **MCP** | per-playbook data stores / engines | L1 |

---

## 🆕 `AgentHarnessAdapter` (the new, non-trivial one)

junto must not be wedded to one agent runtime. Harnesses differ in **capability, not just CLI** — that's what makes this more than a shell-out.

### Capability surface (the part that matters)

```
🔵 capabilities() → {
  invocation:    "interactive-multiturn" | "oneshot-exec",   // codex exec / aider --message vs a session
  state_source:  "hooks" | "stream-parse" | "poll" | "exit-only",  // how authoritative is lifecycle state?
  mcp_client:    bool,            // can it consume junto-provided MCP tools?
  streaming:     bool,            // incremental output, or only final?
  resumable:     bool,            // resume a prior session id?
  permission_model: "ask" | "auto" | "modes" | "none",  // does it gate its own tool use?
  artifact_hooks: bool,           // can we capture tool-calls/edits structurally (not scrollback)?
  auth:          "self" | "delegated",   // does it own its credentials? (prefer self)
}
```

### Normalized operations (🔵)

```
start(task, {cwd, model?, mcp_endpoints?, permission_policy?}) → sessionId
send(sessionId, message) → void            // multi-turn; no-op-ish for oneshot
onEvent(sessionId, cb)                      // tool-call, file-edit, command-run, output-chunk,
                                            //   state-change, needs-input, needs-approval
state(sessionId) → AgentState              // working | blocked | awaiting-approval | done | error
cancel(sessionId)
```

### Why the capability model is load-bearing

- **State authority varies wildly.** Claude Code exposes **lifecycle hooks** → sub-second authoritative state. `codex exec` / `aider --message` are **one-shot** → state is "running → exit". Others need **stream-parsing** heuristics. junto must render accurate live state (it's terminal-less — the human can't watch a shell), so it uses hooks when `state_source=="hooks"`, falls back to stream-parse/poll otherwise. **Same UI, different fidelity.**
- **Terminal-less capture depends on `artifact_hooks`.** When a harness emits structured tool-call/edit events, junto renders **artifact cards** directly. When it only emits a text stream, junto must parse/segment it into artifacts — lossier, but still no raw scrollback shown to humans.
- **Auth = self, preferably.** Each harness applies *its own* model credentials (`claude`, `codex`, `gemini`, …); **junto holds no model API keys.** This is what makes on-prem/regulated deployments tractable and keeps junto out of the secrets-custody business.
- **One-shot vs multi-turn changes the channel UX.** A multi-turn harness supports in-channel back-and-forth; a one-shot harness is "fire a task → get a result artifact." The adapter normalizes both behind `start`/`send`, but the Playbook may prefer one.

### Reality notes per target (🔵, verify when implementing)
- **Claude Code** — interactive multi-turn, **lifecycle hooks** (authoritative state), MCP client, permission modes, resumable. The richest adapter.
- **Codex** — `codex exec` one-shot + interactive; MCP support evolving.
- **Goose** — session-based agent framework with extensions (MCP); programmatic.
- **OpenCode** — TUI agent with a server/API mode (drive programmatically).
- **gh Copilot CLI** — narrower/agentic; treat as lower-capability (likely `oneshot-exec`, `state_source: exit-only`) and degrade.
- **Aider** — `--message` one-shot, git-native.

---

## 🆕 `ExecutionBackend` (*where* the harness runs — orthogonal to *which* harness)

The harness (Claude/Codex/…) is the **what**; the backend is the **where + how**. They **compose** — Claude Code can run local, in WSL, over SSH, or on a remote platform — *except at the managed end, where they collapse* (the platform bundles the harness).

### The spectrum (location → managed)

| Backend | What junto does | Notes |
|---|---|---|
| **local** | spawn a process + pty on this machine | Windows: native (ConPTY-equivalent) |
| **WSL** (Windows, if installed) | route into a distro (per-distro shell flavor) | detect WSL presence; pick distro |
| **SSH** | spawn on a remote host, reverse-tunnel junto's API back | the host runs the harness; junto drives it remotely |
| **remote sandbox** | provision an isolated env (E2B / Northflank / Coder workspace / devbox), run junto's harness *there* | junto still owns the agent loop; the sandbox is just compute |
| **managed-agent platform** | submit a task via API, ingest status + artifacts (Claude Managed Agents / Devin / open-swe / Temporal-orchestrated) | the **platform owns the loop**; junto is a client, not a driver |

### Capability surface (🔵)

```
capabilities() → {
  interactive:     bool,           // can junto drive multi-turn, or submit-and-await?
  filesystem:      "direct" | "synced" | "remote-only",
  bundles_harness: bool,           // TRUE for managed platforms → harness adapter is a thin API client
  state_observ:    "pty" | "stream" | "poll" | "webhook",
  artifact_return: "live" | "on-complete",   // how do results/provenance come back?
  data_residency:  "local" | "tenant" | "vendor-cloud",  // gates regulated/PHI use
  auth:            "ssh-key" | "platform-token" | "none",
}
```

### Why it's its own axis (and where it collapses)

- **`bundles_harness` is the collapse flag.** local/WSL/SSH/sandbox = junto runs *its* `AgentHarnessAdapter` on that compute (harness × backend fully compose). A **managed-agent platform** *is* the harness+runtime → junto's harness adapter degenerates to an API client (`invocation: oneshot-exec`, `state_source: poll/webhook`). One model spans both because both answer `capabilities()`.
- **Terminal-less + provenance must hold across all backends.** Wherever it ran, output comes back as **verifiable artifacts**, never scrollback. `artifact_return` tells junto whether it streams or arrives on completion.
- **`data_residency` ties execution to the trust regime.** You can't run a PHI/trading-research agent on a public vendor sandbox — regulated mode constrains backends to `local`/`tenant`. This axis and `SubstrateProvider` (below) are linked: regulated deployments pin *both*.
- **Auth per backend** — SSH keys, platform tokens, sandbox provisioning creds. Same "junto holds as few secrets as possible" posture; managed platforms own their model auth too.

### 🆕 Concurrency budget (resource governance — separate axis from worktree pooling)

**The lesson (field report, `junto-dev` entry `86273858`, from the "doc factories" talk):** a maintainer running agent swarms had every PR spawn a new git worktree — 70–80 active in a day — and *heavy concurrent test runs across them "nuked his machine."* junto's planned answer to the *proliferation* half is **treehouse-style pooled worktrees** (`competitive-landscape.md`: a bounded, cache-warm, reusable pool — *reuse, don't recreate* — strictly better than his own "I should've cloned 10×" workaround). But pooling bounds the **number of isolated worktrees**, not the **amount of compute running at once** — ten pooled trees all running `cargo test` still thrash a laptop. **These are two different limits and junto must not conflate them:**

| Limit | Bounds | Owned by | Lever |
|---|---|---|---|
| **Isolation-pool size** | how many worktrees exist / persist (disk, checkout, warm cache) | the worktree pool (treehouse model) | pool capacity |
| **Concurrency budget** | how much work executes *simultaneously* (CPU / RAM / IO) | a kernel **execution governor** in front of `ExecutionBackend` | execution slots |

🔵 **Design (rule-of-three: build the simple local version first, extract later):**

- **A session holds a slot only while it executes a heavy command, not for its whole life.** A session mostly *thinking* (waiting on the model) costs ~nothing; a session running a build/test pegs the box. So the governor is a **semaphore acquired around command execution**, released on completion — not a cap on live sessions. v1: any non-interactive shell exec takes one slot; later, **weighted slots** (a build = N, a lint = 1) or **live load-sampling** (back off when system load crosses a threshold — the principled form of the "self-heal when overloaded" tooling he hand-rolled).
- **The budget is a `capabilities()` value of the backend, because only the backend knows its capacity.** `local` derives a small budget (e.g. from core count); a remote-sandbox fleet or managed platform is effectively unbounded (provision more). So *the same code* that bursts to provisioned compute (below) is *how you raise the budget* — the local cap and the elastic cap are one mechanism with different ceilings.
- **Saturation is an attention/observability fact, not silent thrash.** A session blocked on an execution slot is exactly the "*what is each agent waiting on*" signal `attention.md` wants surfaced — a stalled or queued fleet stays diagnosable at a glance instead of melting the machine quietly.
- **Cross-platform caveat (CLAUDE.md):** "overload" and load-sampling APIs differ Win/Mac, and worktree-cleanup races with live processes (Windows file locks). v1's fixed core-derived slot count is portable and safe; defer live-sampling until it earns its keep.

### 🆕 Elastic / on-demand provisioning (deferred concept — the high end of the spectrum)

Today's spectrum table *selects* among fixed backends. A natural extension — **deferred, not built** (rule of three: capture the seam so the concurrency-budget design isn't baked local-only) — is a backend that **provisions compute on demand**: a fresh isolated environment *per session*, run there, torn down after. This turns the concurrency budget from a *fixed local cap* into an *elastic* one — **burst to provisioned compute instead of queueing** when the local budget saturates.

- **Candidate provisioners** (all map onto `ExecutionBackend` as *provisioners*, not fixed *locations*): **k8s** (pod-per-session — enterprise-native, in-tenant), **Firecracker** (microVMs — strong isolation + fast boot; what several serverless platforms use underneath), **Daytona** (dev-environment / AI-sandbox provisioning), **Modal** (serverless containers, pay-per-second). These are the same idea at different isolation/cost/latency points.
- **New capability flags (🔵):** `provisioning: "static" | "on-demand"`, plus a cost/elasticity hint (always-on pod vs pay-per-second function) and lifecycle (`ephemeral` per-session vs `pooled`). The existing `data_residency` flag already gates them — regulated work provisions in-tenant k8s, never a vendor cloud like Modal.
- **Why defer but capture now:** junto isn't running fleets at that scale dogfood-era, and a provisioning adapter is a maintenance tax (the "don't boil the ocean" discipline below). But the concurrency-budget governor must treat its ceiling as a **backend capability**, not a constant, so that adding an elastic backend later *raises the budget* without reworking the governor.

## `SubstrateProvider` (🆕 from the research pressure-test)

junto's *channel abstraction* is shared; the **substrate underneath is swappable.** The durable record always lives in **git refs** (`refs/junto/*`); what differs is how they're synced/authorized:
- **forge-as-hub** (OSS / small-team default) — push/fetch `refs/junto/*` to the forge the team already uses (GitHub/GitLab/Bitbucket). The forge *is* the hub — no server, no VPN, no peer discovery. Simplest, leverages existing infra, forge-agnostic.
- **central self-hosted SoR** (regulated) — WORM retention (SEC 17a-4), SSO-tied **info-barrier ACLs** (need-to-know, not "anyone on VPN"), supervision, revocation, legal hold — controls a mesh/forge-hub can't enforce on their own.
- **peer-to-peer mesh** — *deferred niche* (no-hub allowed **and** native realtime). Sketch retained in `architecture.md` appendix; don't build speculatively.

⚠️ **Identity consequence:** "no central service" / "decentralized" is at most the OSS default — and even forge-as-hub *is* a hub — so it's **not** a core differentiator. In regulated mode junto **is** a central self-hosted service (Ace-like, minus GitHub-lock). The differentiators that survive *every* mode: one unified surface · forge-agnostic · harness-agnostic · **execution-backend-agnostic** · terminal-less · workflow-general · verified-reproducible record. (One kernel; the substrate is an adapter like the rest.)

## `ForgeAdapter` (designed — `architecture.md`)

```
🔵 createChangeRequest({draft}) · assignReviewers() · setCommitStatus() ·
   promoteReady() · resolveCodeOwners()        capabilities(): draft?, codeowners-native?, ...
```
Selected via git-remote-host detection. Normalizes a single `CODEOWNERS` concept across providers (GitHub/GitLab honor natively; the Bitbucket adapter translates to Default-Reviewer API calls). The forge differences are quarantined to the PR boundary — the channel/conversation layer is fully forge-agnostic. Per-forge gotchas (draft→ready ping suppression, CODEOWNERS tier, DC version) live in the adapter.

## 🆕 `ChatConnector` (optional — Slack / Discord / Telegram / Teams)

junto does **not** build realtime chat. The one surface ingests an existing chat tool via a connector — this is *how the one-surface bet works without forcing people off their tools*.
- **Primary direction = inbound aggregation.** Pull the chat conversation *into* the junto channel surface (alongside tickets, code, artifacts), so the user isn't tab-juggling. Optional **outbound bridge** (post junto activity back to a Slack channel) is an *adoption aid* for non-adopters, not the goal.
- **MCP vs Connector split:** **MCP** = agent *action* (an agent posts a message) — pull/one-shot, L1. **`ChatConnector`** = the persistent bidirectional bridge + **inbound triggers** (human types in Slack → appears in the channel → can spawn an agent) — MCP can't do that (pull-only, no webhooks).
- **Capability flags (🔵):** `threads?` · `reactions?` · `inbound_webhook?` · `edit/delete?` · `identity_mapping` (chat-user ↔ junto identity). Behavior branches on these.
- **Separation of concerns:** ephemeral chat ≠ the durable record. junto captures **decisions / ledger entries** into the substrate, *not* every message. In regulated mode external chat isn't a supervised WORM record → archive it or keep conversation in-house.

## `Connector` (issue tracker / knowledge — designed)

Persistent, kernel-level (unlike MCP, which is agent-mediated/pull): **webhook receiver + reconciliation loop + creds/identity + entity mapping.** Same shape as `ChatConnector` above. Uses:
- `IssueTrackerConnector` — git-native / **Jira** / **Linear**: create/update/comment, status mapping. Keep internal state tracker-agnostic (`draft|validating|needs_review|approved|rejected|promoted`), map per-provider.
- `KnowledgeConnector` — **Confluence**: read context (via MCP) + publish memo (outbound).
- **Discipline:** minimize bidirectionality (where integrations die — loops/drift/rate-limits). Prefer one-way: junto→Confluence publish; Jira→junto trigger + junto→Jira status. Don't mirror schema; link/reflect status.

## `MemoryProvider` / observability (designed — `self-improving-harness.md`)

The event/observability layer is **dual-consumer**: one instrumented event stream feeds **(a) observability backends** (humans/dashboards) **and (b) the self-improving loop** (the harness's *sensing* half — observability is the loop's afferent nerve; no observability → no evals → no self-improvement). The same events are also the provenance/verified-record capture — instrument once, three uses.

`appendEvent · appendBatch · query · getById · upsertProjection · health · capabilities · flush`. One **primary** (authoritative) + optional **mirrors** (best-effort fan-out, retry/dead-letter). Capability flags: `append_only · queryable · mutable_records · workflow_actions · tracing_native · metrics_native`. Targets: local/sqlite (default), issue-tracker (review), **OTEL bridge** (recommended thin path), DataDog, Arize Phoenix. **Long-horizon** outcome tracking (lagging signals: incident MTTR over weeks, research calibration over months — not just per-run traces). Field-level redaction before fan-out; regional routing / on-prem-only for regulated envs.

## `InferenceEndpoint`

OpenAI-compatible by default → can point at hosted or **on-prem/internal** inference. Hard requirement for regulated data (no public LLM for PHI / trading research / privileged material).

## MCP (L1 — already pluggable)

Domain tools per Playbook (market-data/backtest/P&L for quant; EHR/cohort stats for healthcare; repo/tests/CI for code). Agent-mediated, session-scoped, pull. A Playbook *declares* which MCP caps it needs.

---

## Cross-cutting

- **Capability negotiation, not feature assumption.** Every adapter answers `capabilities()`; junto adapts. New vendor ⇒ new adapter, no kernel change.
- **Trust & signing.** Plugins/Playbooks run code (workflow defs especially). Sandbox (conductor-only, no bash/file v1); plugins touching capital/PHI need stronger permission + **signed packages**.
- **Declarative views.** Playbook views are **schema over a fixed component palette**, not arbitrary render code (XSS/UI-safety). Custom renderers are the one place core work may still be needed.

## Open questions

1. 🔵 **Harness `permission_model` vs junto's gate engine — overlap to resolve.** Some harnesses (e.g. Claude Code) gate their *own* tool use (ask/auto/modes); junto *also* gates consequential actions via the L0 gate-engine + playbook routing. Unresolved: does junto's gate **replace** the harness's permission prompts, **layer on top** of them, or **delegate** to them? Naive composition risks **double-gating** (human approves twice) or **gaps** (an action junto would gate slips through because the harness auto-approved it). Likely answer: junto sets the harness to non-interactive/auto for tool use it can observe, and is itself the single accountable gate — but that depends on `artifact_hooks` fidelity (junto must *see* every action to gate it). Resolve before building the harness adapter.
   ✅ **Position recorded (Dan, 2026-06-09 — `junto-dev` ledger, grill session): they LAYER.** The harness gates *mechanics* (synchronous, ephemeral — "may I run this command"); junto gates *outcomes* (asynchronous, durable, rationale-bearing — "should this merge"). *Replace* was rejected as unbuildable today (needs full `artifact_hooks` observability). Full resolution — whether junto additionally absorbs the harness prompts when observability allows — still waits on the `AgentHarnessAdapter`.

## Discipline (don't boil the ocean)

"As vendor-neutral as possible" is the **north star, not the MVP.** Per the **rule of three**: build **one adapter per boundary first** (suggested: **GitHub + Claude Code + git-native issues + local memory**), *then* extract each interface from what two real implementations actually share. Building a 3-vendor abstraction speculatively is the tar pit the rule warns against. **Each adapter is a maintenance tax — add lazily, driven by a deployment that needs it.**
