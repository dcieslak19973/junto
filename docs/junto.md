# junto — vision spine

> **Status:** synthesis-in-progress, 2026-06-08. This is a *map*, not a manifesto — it pulls three sources together and points back at the primaries. **We will evolve it.**
>
> **Reading convention — three things kept separate on purpose:**
> - 📚 **Source says** — a claim from one of the inputs (cite it, don't absorb it).
> - ✅ **Already decided** — settled in `architecture.md` (the prior design doc).
> - 🔵 **Synthesis (this session)** — *proposed* framing, not yet endorsed by Dan. React to it.

---

## The name — why "junto"

**junto** /ˈhʊntoʊ/ — Benjamin Franklin's club, founded in Philadelphia, **1727**. ([Wikipedia](https://en.wikipedia.org/wiki/Junto_(club)))

📚 Franklin convened a **mutual-improvement society** of working tradespeople — a printer, a surveyor, a cabinetmaker, a clerk, a bartender — who met to get better at their work and improve their community. Members pledged **mutual respect across profession and background** (tolerance as the ground rule), and worked from a **standing set of ~24 questions** Franklin wrote as a repeatable meeting agenda — still borrowed by "mastermind" groups three centuries later. The club's habit of *"crawling together"* — pooling knowledge and effort in good faith — **outlasted the club itself**, seeding a subscription library, a fire company, and eventually a university.

That's the hook: *a junto is a small, diverse group that gets better at its work by thinking together, in good faith, on a repeatable cadence* — a fitting name for a system where some of those members are **agents**.

⚠️ Treat the history as the **naming hook + ethos, not a requirements doc** — no design decision derives from "because the 1727 club did X"; the mechanisms below stand on their own merit. With that caveat, the resonances (read as *theme*, not spec):

| Franklin's Junto (1727) | Resonance in junto |
|---|---|
| Diverse members (printer, surveyor, bartender…) | Heterogeneous participants **including agents** as peers |
| Mutual-respect pledge; tolerance as the base | Membership as a peer compact, not just an ACL |
| 24 standing questions — a repeatable format | Structured deliberation before a consequential action |
| "Not gentlemen performing for each other" | Self-improvement — get better at *the work* |
| "Crawling together" — projects outlast the club | Durable verified record / institutional memory |

🔵 So: junto = *a collaboration system where some members are agents*, named for a club that did something thematically similar. If the engineering ever pulls against the metaphor, **the engineering wins.**

---

## The thesis (🔵 proposed — this is the bet to react to)

**junto = Maggie's diagnosis + structured deliberation + one unified, vendor-neutral surface — vs Ace's GitHub-locked, synchronous, single-cloud prescription.**

1. **Diagnosis (Maggie):** the bottleneck is no longer writing code — it's **alignment**: shared understanding of *what to build and why*. Agents run on *unshared local plans*, collapsing the planning window so the PR becomes the only, too-late, alignment checkpoint.
2. **Mechanism (deliberation):** the fix is a **shared deliberative step members move through together** before a consequential action — not synchronous co-presence. Justified on its own (it's where alignment, pre-registration, and gating live); the Franklin "meeting" is just the evocative echo, not the reason.
3. **One surface (the point of junto):** so a user isn't juggling **Slack + Jira + GitHub** in five tabs. junto unifies the conversation, the work-unit, the tickets, and the code in **one channel surface**, pulling external systems *in* via **pluggable connectors** (chat / tracker / forge). Substrate underneath is pluggable too (forge-as-hub for OSS · central self-hosted SoR for regulated) — **no peer-mesh required.**

The wedge: **Ace also unifies — but locks you to GitHub + a central cloud + synchronous co-presence.** junto keeps ACE's good idea (the `channel`) and unifies **across vendors** (any forge / tracker / chat / agent-harness / execution backend), **terminal-less**, with a pluggable substrate. The honest cost: junto is therefore *"another app"* — the bet is **one unified pane beats five tabs.**

---

## junto is workflow-general — coding is just one Playbook (🔵 core framing)

Franklin's Junto never discussed code — it took up business, science, civics, self-improvement. junto is true to that: the unit is a **channel = a unit of inquiry (a question)**, and a code change is *one possible outcome* — alongside a memo, an analysis/result, a config change, an incident remediation, an alert, or a parked dead-end. **Maggie's "align before agents build" generalizes to "deliberate before any consequential action"** (deploy capital, change a protocol, publish, remediate prod).

The **same spine serves every kind of work** — *members deliberate → agent-augmented investigation/build → graduated review gate → durable verified record.* Only the **Playbook** differs — its **lifecycle** (stages + gates), its tools, its output.

> **Terminology:** a Playbook's process = its **lifecycle** (stages + gates). We reserve the noun "**a workflow / Workflow Definition**" for the *separate, agent-automation* sense — reusable scripts that drive sub-agents through a step (e.g. a "deep research" routine). A playbook's lifecycle may *call* such workflows to do the work, but the two are different layers. *(The adjective "workflow-general" just means junto handles many kinds of work — not the automation sense.)*

The "rule-of-three" set to build concretely *before* extracting the framework:

| Playbook | The inquiry | Outcome | Gate before acting |
|---|---|---|---|
| **code-PR** (built-in default) | "should we build/change X?" | a PR | commissioner first-pass → CODEOWNERS |
| **research / analysis** | "what's true about X?" | memo / result / decision | domain-expert sign-off; **hypothesis-ledger + provenance** guard against false-discovery |
| **production troubleshooting** (incident / AAR) | "why did X break — and what do we do?" | remediation + after-action record | sign-off before touching live systems; the **AAR feeds the next playbook** ↓ |
| **self-improvement** (🔵 junto on itself) | "should we change our own agents/skills/workflows?" | a promoted skill / workflow / agent-def | held-out **eval** is this playbook's verifier → human-gated promote |

🔵 **Self-improvement is not a separate pillar — it's the loop applied reflexively.** Improving junto's own agent policies is just another unit of work run *in a channel* (a specialized code-PR, targeting skills/workflows/agent-defs instead of product code). Its verifier is the eval harness; observability feeds its "what to improve" signal. So junto improves itself with its own mechanism — see `self-improving-harness.md` for that playbook's hard part (evals = THE CRUX).

Not scope creep: `architecture.md` already treats the channel as a unit of inquiry, and notes the research/inquiry case may matter more than code (higher stakes — capital, safety, liability) and is less served by existing tools. **Coding is the easy first playbook, not the point.**

→ **All three rule-of-three playbooks walked end-to-end** (tracer bullets), each stressing a *different* hard axis:
> - `worked-example-production-troubleshooting.md` — **tempo / reversibility** (MTTR vs deliberation latency)
> - `worked-example-research.md` — **epistemic rigor / false discovery** (cheap inquiry manufactures spurious findings)
> - `worked-example-agent-coding.md` — **attention economy / the AI-PR flood** (the least-differentiated, table-stakes playbook)
>
> **Synthesis (the payoff):** the **gate-engine routing function** is the single biggest thing a Playbook supplies (reversibility×blast vs epistemic vs review-load) — *design it pluggable from day one, don't hardcode code-PR's into the kernel.* The **ledger entry + provenance + gate state-machine** are the true generic kernel. The **eval problem is per-playbook** (tractable for code, lagging for incidents, Goodhart-prone for research), so self-improvement can't be one-size. See each doc's synthesis.

---

## The sources

### 1. Maggie Appleton — *"One Developer, Two Dozen Agents, Zero Alignment"*
📚 [maggieappleton.com/zero-alignment](https://maggieappleton.com/zero-alignment) · [GitHub Next talk](https://githubnext.com/talks/one-developer-two-dozen-agents-zero-alignment/)
- Rebuts "one person + a fleet of agents = a whole team" — *nine women don't make a baby in one month.* Individual throughput doesn't scale to good software.
- **Three failure modes of speed-without-alignment:** *wasted work* (built the wrong thing) · *coordination debt* (agents colliding, dup effort, context-free PR queues) · *timing collapse* (issue→PR in minutes kills the planning window).
- Her fix = **Ace**: multiplayer cloud sessions, shared editable plan docs, always-on dashboard. Reframe: cheap implementation *gifts time back* — spend it on alignment/craft, or accrue "a thousand crappy features."
- 🔵 We keep her **diagnosis**, not her **architecture** (central + synchronous + GitHub-only).

### 2. GitHub Next — **Ace** (the canonical competitor)
📚 OpenAPI snapshot: `ace-openapi-0.1.70.json` (**v0.1.70**, captured 2026-06-08, may disappear) · live: [api.ace.githubnext.com](https://api.ace.githubnext.com/)
- ✅ Unit of work = `channel` = git branch + forked microVM; "multiplayer" = `party: [githubUserId]`; **zero CRDT / presence / shared-buffer** anywhere in the schema. Still true at v0.1.70.
- Anti-misalignment weapon = **async LLM dashboards**: `/dashboard/{summary,team-summary,suggestions,greeting}` (all `POST context → {summary}`).
- 🔵 **New at v0.1.70 (postdates the design doc):** channel `migrate` lifecycle (`migrate` / `ready` / `cancel`) = staged gates; `lobby/rebuild` + `branches/sweep` = staleness sweeping. Ace is *growing* the lifecycle-gate + aging machinery the design doc proposed — convergent evidence, not a threat.
- ✅ **GitHub-locked** (OAuth + App installs; channels typed to GitHub issue/pr/ref) and **cloud-VM-central** (`/vm/{id}/{resume,forward,archive,log}`). These are the two things junto refuses. Strongest single argument for the forge-agnostic, no-central bet.

### 3. The prior design doc — `architecture.md`
✅ Carry the *ideas*, not the original mesh-centric implementation. ⚠️ **Reframed 2026-06-08** (see that doc's status banner): the peer-to-peer **git-mesh is deferred** — durable record syncs via **forge-as-hub** (OSS) or **central SoR** (regulated); realtime conversation comes from **chat connectors**, not a mesh. What carries forward:
- `channel = conversation fused to the work unit` — the core abstraction (ACE got this right).
- **Durable record as git refs** (`refs/junto/*`, partition-by-author, git-bug prior art) — kept, but synced through a **hub**, not meshed peer-to-peer.
- **No CRDT** — append-only log, interleave by `(ts, author)`; conversation realtime is delegated, not built.
- Identity = git author; authZ per substrate (roster pubkey for OSS; SSO/entitlements for regulated).
- Forge-agnostic (git worktrees, no forge-API coupling).
- 📚 Larger explorations (each stands alone): AI-PR-flood prevent/constrain/triage · commissioner first-pass · multi-forge `ForgeAdapter` · pre-remote in-channel review + risk-routing · **channels as research/inquiry spaces** · pluggable **Playbooks** · **Connector** abstraction (chat / tracker / knowledge) · provenance binding + hypothesis ledger.

### Companion thread — `self-improving-harness.md`
📚 The "members get better at the work" leg (Franklin's self-improvement, made mechanical). The *substrate* is the usual agent-policy surfaces — agent-authorable skills, workflows (scratch→promote), within-task verify loops. **Missing = the loop, the evals, the gating.**
- **THE CRUX:** evals are the whole ballgame — "better" must be **multi-dimensional** (quality / human-attention / cost), *not* throughput (throughput amplifies the AI-PR flood). Anti-Goodhart, held-out evals, role isolation (coder/reviewer/bug-finder with *independent* signal), reward validated material outcomes not activity.
- 🔵 First rung: human-gated, data-level, outcome-driven refinement of skills/workflows. Also specs an **issue-tracker durable-record backend** and a **pluggable memory/observability layer** (OTEL/DataDog/Phoenix) — both Connector-shaped.

### 🔵 Principle — the durable record holds *intent*, verified
The durable plane should hold more than chat: the **why** (problem, options considered, rationale, outcome) bound to the artifacts that produced it. This is the design doc's **provenance binding** + **hypothesis ledger**. Critical guardrail from the harness doc's CRUX: these ledger entries must be **human-authored or human-ratified, execution-grounded, and treated as claims-to-be-verified — never auto-captured "truth."** Auto-captured rationale is hallucination-prone and authoritative-looking, which is *worse* than no record.

---

## Core constraint: completely terminal-less (🔵 Dan, 2026-06-08)

junto has **no terminal as a human surface.** The terminal pane — the organizing metaphor of the agent-multiplexer tools junto departs from — is *gone*. Humans interact with **channels, plans, ledger entries, artifacts, and review surfaces**, never a shell.

- 🔵 **Why it's coherent:** the terminal was always *how an agent executes*, never the *alignment surface*. Maggie's bottleneck (deciding what to build) and structured deliberation live nowhere near a command line. Taking the thesis literally ⇒ terminal-less.
- 🔵 **What replaces it:** the channel/inquiry surface (deliberation + ledger entries), artifact renderers (diffs, charts, docs, browser preview), and review surfaces. Agent output is **verifiable artifacts, not scrollback** (provenance binding).
- ✅ **Scope DECIDED:** terminal-less for *humans*; agents keep the shell under the hood (Open Decision #4).

## Core constraint: MIT licensed (✅ Dan, 2026-06-08)

junto ships under **MIT** (permissive). Hard constraint with a copyleft consequence: **incorporate no AGPL (or other copyleft) source.**

- ✅ **What's free to carry forward:** **ideas, architecture, and design** are not copyrightable — only the *expression* (source) is. junto reuses every *concept* here (git-refs-as-record, forge-as-hub sync, pty-capture-to-artifact) via **clean-room reimplementation**, never by copying copyleft source.
- 🔵 **Net:** **greenfield MIT** — build the patterns from scratch. *(Not legal advice — the practical read.)*

## Architecture pillars (🔵 synthesis — how the sources compose)

```
  MEMBERS          humans + agents as peers (a peer compact)
     │
  DELIBERATION     structured framing before a consequential action
     │             (Maggie's missing alignment phase, async-native)
     │
  ┌──────────────────────── ONE SURFACE (the point) ───────────────────────┐
  │  CHANNEL = unit of inquiry                                              │
  │  conversation + work-unit + tickets + code + artifacts + gates,        │
  │  external systems pulled IN (not 5 tabs) — terminal-less               │
  └───────────────┬───────────────────────────────┬───────────────────────┘
   CONVERSATION (pluggable, optional)        DURABLE RECORD (the kernel's job)
   own surface | ChatConnector              verified intent + provenance,
   (Slack/Discord/Telegram/Teams)           append-only git refs
   | MCP for agent-posts                     │
     │                                       │
  GOVERNANCE       risk-routed graduated gates (routing fn is per-playbook),
                   SLO/aging, pre-receive-hook backstop

  CHANNEL KINDS (one loop, different work-types): code-PR · research · prod-troubleshooting
                   · self-improvement (junto on its own agents/skills — the reflexive playbook)

  cross-cutting PROPERTIES (not pillars):
   · vendor-neutral adapters — how the surface pulls your tools IN
   · observability — the sensing half (feeds the self-improvement playbook; one event
     stream → dashboards + that playbook + provenance)
   · substrate (pluggable): forge-as-hub (OSS default) | central self-hosted SoR (later)
```

🔵 Structural question worth settling early: **is the deliberation/framing step a kernel lifecycle stage, or per-playbook?** The "rule of three" warns against frameworkizing from one example — but a generic framing stage may be common enough to live in the kernel (with playbook-specific *content*: plan / pre-register / triage).

---

## Vendor-neutral integration — a property, not a pillar (✅ Dan, 2026-06-08)

Vendor-neutrality is **how the one-surface bet works**, not a co-thesis: connectors pull external systems **in** so the user doesn't juggle tabs. The rule: **junto wraps every external dependency in a swappable adapter; nothing vendor-specific reaches the kernel** — behavior branches on *capability flags, not vendor name*. (It's an implementation property of the surface, not an identity unto itself — junto is "one surface for verified work," which *happens to* integrate neutrally.) Boundaries: git forge (GitHub/GitLab/Bitbucket) · **agent harness — *what* agent (Claude/Codex/Goose/OpenCode/Copilot CLI)** · **agent execution backend — *where* it runs (local · WSL · SSH · remote sandbox · managed-agent platform)** · **chat (Slack/Discord/Telegram/Teams — `ChatConnector`, optional; MCP for agent-posts, Connector for the bidirectional bridge + inbound triggers)** · issue tracker (git/Jira/Linear) · knowledge SoR · memory/observability (local/OTEL/DataDog/Phoenix) · inference (on-prem capable) · **substrate (forge-as-hub OSS vs central self-hosted SoR; peer-mesh deferred)** · domain MCP tools. The non-trivial axes: the **harness** (differs in *capability* not just CLI; auth stays with it), the **execution backend** (composes with harness, except managed platforms *bundle* it — then junto is an API client), and **chat** (inbound aggregation primary; outbound bridge optional).

→ **Full design + interface sketches:** `pluggability.md`. ⚠️ North star, *not* MVP — rule of three: one adapter per boundary first, then extract the seam.

## junto vs Ace (🔵 how they differ, at a glance)

| Dimension | Ace (GitHub Next) | junto |
|---|---|---|
| Shared idea | the **`channel`** (work-unit + people + conversation) | **same** (ACE got this right) |
| Shared diagnosis | alignment is the bottleneck | **same** |
| Unification | one surface, **GitHub-shaped** | **one surface across vendors** (any forge/tracker/chat) |
| Alignment mechanism | **synchronous** co-presence (shared cloud VM) | **async structured deliberation** |
| Forge | **GitHub-only** (OAuth/Apps) | **forge-agnostic** (`ForgeAdapter`) |
| Deployment | central cloud + microVMs (fixed) | **pluggable substrate** (forge-as-hub OSS / central SoR regulated) |
| Human surface | cloud terminal/VM | **terminal-less** |
| Durable record | branch + async LLM dashboards | **ledger entries + provenance** (git refs) |

---

## Open decisions (settle before building)

1. 🔵 **Is the deliberation/framing step (plan / pre-registration / triage) a kernel lifecycle stage, or per-playbook?** (rule-of-three tension — each playbook frames differently: code-PR plans, research pre-registers, incidents triage.)
2. 🔵 **Conversation surface:** how much native chat does junto's one surface need vs. relying on `ChatConnector` (Slack/Discord/…)? Likely a minimal native surface + connectors for inbound aggregation. (The old mesh-specific items — fan-out, push cadence, DERP/NAT — are moot now the peer-mesh is deferred.)
3. ✅ **Build path — DECIDED: greenfield MIT.** Constrained by MIT (no copyleft source) + terminal-less. Clean-room reimplement the *patterns* (git-refs-as-record, forge-as-hub sync, pty-capture-to-artifact); incorporate no copyleft source. See the MIT constraint section.
4. ✅ **Terminal-less scope — DECIDED (Dan, 2026-06-08): human-surface only.** Humans never see/drive a terminal. **Agents keep the full shell** — they run commands in **headless PTYs under the hood, output captured as verifiable artifacts** (diffs, logs, charts), never rendered as scrollback. (Not the deeper "no shell anywhere / MCP-only" commitment.)
5. 🔵 **Substrate is a pluggability axis** (`SubstrateProvider`). OSS = **forge-as-hub** (push `refs/junto/*` to GitHub/GitLab/Bitbucket — leverage the hub teams already have). Regulated = **central self-hosted SoR** (WORM retention, SSO-tied info-barriers, supervision, revocation — which a mesh *cannot* provide by construction). One kernel, two substrate impls. ⚠️ **Identity tooth:** "no central service" / "decentralized" is at most the OSS default and probably rare in practice (forge-as-hub *is* a hub) — **not a core differentiator.** The differentiators that survive *every* mode: forge-/harness-/backend-agnostic · one unified surface · terminal-less · workflow-general · verified-reproducible record. Peer-mesh is a **deferred niche** (no-hub + native-realtime); don't build speculatively.
6. ✅ **Primary target segment — DECIDED (Dan, 2026-06-08): OSS / small-team first.** Substrate default = **forge-as-hub**; compliance minimal; regulated/central-SoR is a *later* add-on. Drives lean, adoptable positioning. (Don't build the regulated substrate or compliance machinery yet.)

---

## Next steps (pick)
- ✅ **(a) DONE — thesis pressure-tested** (incidents + research). Finding: async structured deliberation isn't sufficient as a single mode, but the thesis survives via **splitting deliberation in time** (act-then-ratify) + **pre-authorization**; the live element needed is **presence/awareness**, not real-time co-decision (chat connector / light presence layer covers it).
- ✅ **(c) DONE — all three playbooks walked end-to-end** (the worked-example docs); seam extracted (gate-routing function is the playbook-specific plug; verified-record + provenance + gate state-machine are the kernel).
- ✅ **(d) DONE — primary segment = OSS/small-team first** (forge-as-hub default, compliance later).
- ✅ **(b) DONE — ledger-entry content model specced and built** (`docs/adr/0003` and the surrounding ADR trail, 0001–0010; implemented in `crates/junto-kernel`). The human-ratified guardrail landed as derived verification standing (`docs/adr/0002`, `0004`); the per-playbook verifier remains a playbook slot.
- **(e)** Load the remaining background Dan mentioned, then revise this spine.
