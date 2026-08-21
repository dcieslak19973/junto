# Competitive & ecosystem landscape

> Snapshot: **2026-06-13**; Zed Delta/DeltaDB section added **2026-08-20**.
> This is a record of **bets and tradeoffs, not a claim of superiority** —
> where comparisons appear, they name where the other approach is *genuinely
> better* too. Covers three reference points junto keeps bumping into:
> **Ace** (the integrated rival from the "Zero Alignment" essay), **Kun
> Chen's toolkit** (the composable agent-native-CLI school), and **Zed's
> Delta/DeltaDB** (the replicated-multiplayer school). Re-read
> [`junto.md`](junto.md) for the thesis these are measured against.

## TL;DR — where junto sits

Three schools are forming around "humans + agents do real work":

- **Integrated platforms** — one hosted product (Ace): sessions, shared docs,
  microVMs, governance, an MCP surface. Bet: own the whole surface.
- **Composable agent-native CLIs** — a toolkit of sharp single-purpose tools an
  agent invokes (Kun Chen: worktrees, gates, evals, HTML review, orchestration),
  unified only by a token-efficiency *design standard* (AXI). Bet: Unix, for
  agents.
- **Replicated multiplayer environments** — Delta/DeltaDB (Zed): the
  conversation and the worktree CRDT-replicated together in real time, review
  anchored in place as code evolves. Bet: liveness + anchoring beat ceremony.

**junto is an integrated *governed* surface with a verified record** — closer to
Ace in shape, but its differentiator is the **provenance-bound, append-only
record + gates**, and it is **vendor-neutral by adapters** rather than hosted.
The non-obvious result (see the Kun Chen section): junto and the CLI toolkit are
**complementary at the implementation layer** — junto can sit *on* that plumbing
(worktrees, harness protocol, forge ops) while keeping its product bet.

---

## Ace (aceagent.io) — the integrated rival

The product from Maggie Appleton's
["Zero Alignment"](https://maggieappleton.com/zero-alignment/), now shipping.

**What it exposes (API).** A **bidirectional MCP server** at `aceagent.io/mcp`
(read + write), eight tools centred on a closed **playbook-evolution loop**:

| Group | Tools |
|---|---|
| Discovery | `list_playbooks`, `find_playbook` (semantic), `get_playbook` (versioned) |
| Evolution | `create_playbook`, `create_version`, `trigger_evolution`, `get_evolution_status` |
| Outcomes | `record_outcome` (`success` \| `partial` \| `failure` + traces) |

Loop: *discover → load → apply → `record_outcome` → feeds evolution* ("ACE" =
**Agentic Context Engineering**).

### Update — re-assessed 2026-06-19

Three substantive changes since the baseline above (recorded in the `junto-dev`
side-quest channel *ACE - Update 20260619*, finding `0d93ed32`):

1. **Strategic pivot: synchronous-multiplayer SaaS → OSS core + hosted cloud.**
   Ace now splits into **ACE OSS** (open-source core, runs local with your own
   model keys / storage / backups — *local single-user capability must remain
   usable without ACE-operated services*) and **ACE Cloud** (Personal / Team /
   Enterprise tiers selling sync, backups, managed job execution, team
   governance, enterprise audit/compliance). The microVM / live-cursor
   multiplayer framing has receded from the public docs.
2. **The internal loop is now public and named:** **Generator** (produces
   outputs from the playbook) → **Reflector** (analyzes outcomes into *delta
   entries*) → **Curator** (periodically dedups, merges, removes contradictions).
3. **The mechanism is documented** (ICLR 2026 paper, [`2510.04618`](https://arxiv.org/abs/2510.04618)):
   context is a set of **itemized bullets carrying metadata**, updated by
   **localized delta — never full rewrite** — with a **grow-and-refine** dedup
   pass. The named failure modes it avoids are **brevity bias** (rewrites strip
   domain detail for conciseness) and **context collapse** (iterative rewriting
   erodes specificity until the context drifts generic). Reported +10.6% on
   agents / +8.6% on finance, without labeled supervision.

**Concepts to borrow (ranked).**

- **★ Itemized-delta brief + grow-and-refine — strongest fit.** This lands
  directly on junto's **recall bridge** (the scaled brief, [ADR 0013](adr/0013-recall-bridge-session-context-injection.md)).
  junto already does "state, not history" — folds verification into targets,
  decays resolved material — but has **no defense against its own context
  collapse** as a channel ages and standing decisions accumulate. The borrow:
  represent brief items as discrete units carrying metadata (last-touched,
  supersession, still-load-bearing?), make the fold a **localized delta**, and
  add an explicit periodic **curation pass** that dedups/merges standing
  decisions. Concrete, low-cost improvement to a surface junto already ships.
- **★ Strengthen junto's Curator step.** Generator/Reflector/Curator maps onto
  junto's **record → ratify/park/correct → fold-into-brief**. junto's Curator
  analog (the brief projection) is the weakest and least-deliberate of the
  three; make brief-curation an explicit, inspectable step rather than an
  emergent property of the projection.
- **✗ Do *not* borrow automated evolution on a coarse signal.** Ace
  auto-mutates context from `success | partial | failure`. junto's bet stays
  **governed** (gated, human-ratified) evolution — now corroborated by
  third-party assessment flagging Ace as *"production evidence thin… not for
  critical systems,"* which is exactly junto's consequential-work niche. Borrow
  the **representation** (itemized delta, grow-and-refine), keep the
  **governance**.
- **◆ Strategic validation.** Ace converging onto *local-core-must-work-standalone
  + hosted-sync/governance* validates junto's existing shape (MIT core + local
  host + sync over the user's own git remote) and gives a clean tiering template
  for junto's eventual commercial story.

**Three overlaps with junto — tradeoffs, not a scoreboard.**

1. **MCP surfaces are complementary.** Ace optimizes the agent's *input context*
   (find the right playbook); junto records the agent's *output decisions* (the
   ledger). junto could consume an Ace-shaped context layer behind an adapter.
2. **"Playbook" is a naming collision** (like "Session" was). Ace Playbook = an
   evolving instruction/context doc; junto Playbook = a **work-type** (lifecycle
   + gate-routing + verifier + tools, [`domain-model.md`](domain-model.md)).
   Disambiguate in the domain model before junto builds Playbooks.
3. **Ace ships the self-improvement loop junto only designed**
   ([`self-improving-harness.md`](self-improving-harness.md)). junto's
   differentiator is **governed** (auditable, gated) evolution, not a faster
   optimizer.

**The two rejected-from-Ace bets, stated honestly:**

- *Async versioned record vs synchronous shared buffer.* Ace is **genuinely
  better** for real-time pairing and immediacy. junto bets that for verified,
  across-time, agents-as-peers work, a durable provenance-bound record beats
  liveness — and it avoids CRDT entirely (`junto.md`: *"zero CRDT / presence /
  shared-buffer"*). Falsifiable, riding on the "alignment is the bottleneck; the
  fix is async deliberation, **not** co-presence" diagnosis.
- *Governed outcomes vs automated evolution.* Ace is **genuinely better** for
  frictionless self-improvement on low-stakes, high-volume tasks. junto bets
  that for consequential/accountable work, automated drift on a coarse signal is
  a liability and a verifiable record is the product. The cost junto accepts is
  **human-attention friction** — which [`attention.md`](attention.md) itself
  flags as the scarce resource, so junto's gates must keep earning their keep.

---

## Kun Chen's toolkit — the composable-CLI school

[Kun Chen](https://github.com/kunchenguid) (ex-Meta L8) ships single-purpose,
agent-native CLI tools. Strikingly, several **are primitives junto needs**, and
two **are junto concepts already built** (in CLI form).

| Tool | One line | Maps to junto |
|---|---|---|
| **lavish-axi** | agent writes HTML, human annotates inline (text + range anchors), feedback long-polls back to the waiting agent | the **parked collaborative-space** — and it's **turn-based**, junto's async-versioned side of the fork, not a shared buffer |
| **no-mistakes** | push here instead of `origin`; AI validation pipeline in a disposable worktree; **mechanical fixes auto-applied, intent-altering escalated (approve/fix/skip)**; forwards only when green | junto's **Gate**, concretely — with a graduated-escalation model to steal |
| **treehouse** | a **pool of reusable git worktrees** that persist across sessions (keep deps + build cache), with in-use detection | junto's eventual **worktree-per-session** isolation — the rule: *reuse, don't recreate* |
| **firstmate** | one "first mate" agent supervises a **crew** of workers (tmux + treehouse worktrees); the whole orchestrator is an `AGENTS.md` file any terminal agent can follow | multi-agent UX + **orchestration-as-a-document** (junto Playbooks could be markdown-driven) |
| **gnhf** | overnight **autonomous loop**: prompt (with `notes.md` context) → invoke agent → commit/repair → abort caps → loop | agent-session loop + **notes-as-cross-iteration-memory** |
| **superpowers-bench** | benchmarks whether an agent **picks the right skills** (precision/recall/F1, baseline vs hint-triggered) | concrete **eval methodology** for the self-improving Playbook |
| **axi** | design standard for agent-native CLIs; *"token budget as a first-class constraint"* | a benchmarked **challenge to junto's MCP surface** (below) |
| **gh-axi**, **chrome-devtools-axi** | forge / browser ops, AXI-style | `ForgeAdapter` / browser-tool ergonomics |
| **acp-mock** | deterministic **ACP** mock over stdio for CI | validates (and standardizes) junto's stub-harness testing |

### The strategic read

**Kun = a Unix-style toolkit of sharp, composable, agent-native CLIs; junto =
one integrated, governed surface with a verified record.** Different
philosophies — but **complementary at the implementation layer.** junto's
product bet (one surface, provenance-bound outcomes, governance) is untouched,
while junto could plausibly sit *on top of* his plumbing behind its adapters.

### Two finds that are decision-shaping

- **ACP (Agent Client Protocol)** — a standard for *a client driving an AI agent
  over stdio* (Zed's protocol; `acp-mock` is its test double). **That is exactly
  junto's job.** Today junto shells out to `claude -p` per-vendor; **ACP is a
  candidate unifying protocol for the `AgentHarnessAdapter`.** Evaluate it
  *before* writing a second bespoke harness integration (i.e. before OpenCode).
- **treehouse's pooled-persistent worktrees** — the design rule for junto's
  multi-session isolation: a reused pool that keeps caches, not throwaway
  worktrees (which make agents slow).

### AXI vs MCP — the challenge to junto's agent surface

Kun benchmarks **AXI CLI tools ~66% cheaper than MCP on GitHub ops, ~27% on
browser, at equal success**, via TOON output (~40% vs JSON), minimal schemas,
pre-computed aggregates, and contextual disclosure. junto chose **MCP** for its
agent write surface (ADR 0012). Honest read:

- AXI's savings are largest for **high-frequency tool loops**; junto's MCP
  surface is **lower-frequency authoring** (record / propose / gate / sessions),
  so less exposed to the token tax.
- But the **principles** (compact output, pre-computed aggregates, contextual
  disclosure) are worth adopting in junto's tool *and* brief outputs **regardless
  of MCP-vs-CLI** — the same philosophy as the `rtk` wrapper junto's contributors
  already use.
- Open question worth a real look: should junto's agent surface be MCP, an
  AXI-style CLI, or **both**?

---

## Zed — Delta & DeltaDB (assessed 2026-08-20) — the fluidity rival

The Zed team's second act: **DeltaDB**
([announced 2026-06-11](https://zed.dev/blog/introducing-deltadb)), "version
control built for the conversation," and **Delta**
([announced 2026-08-12](https://zed.dev/blog/introducing-delta)), a
multiplayer environment for coding with agents built on it. Private beta,
proprietary, their cloud. This is the closest thing to junto anyone has
shipped — closer than Ace — and it is also the strongest counter-evidence yet
against one junto bet (the reopening, below).

**What DeltaDB is (evidence from the posts).**

- Versions the work *between* commits: every operation is a **delta with a
  stable identity**; the worktree and the conversation driving it are
  replicated together, in real time, for everyone in a thread.
- **CRDT-replicated worktrees**: many people and agents edit the same files
  at once across machines; the files are real (agents work in them through a
  terminal; the worktree mounts to disk on demand).
- **References anchor to deltas, not line numbers**, so they survive as code
  moves: from a conversation line, jump to that code as it stands now or as
  it stood then; from a code line, find every conversation that touched it.
- **Coexists with git**: captured between commits; commit and push as
  always; teammates who never open Delta see a normal repo. (The same
  "don't replace the forge" posture as `refs/junto/*`.)

**What Delta adds.** The conversation is a document: cursor anywhere,
span-anchored comments on anything (a diff line, a plan step, a thinking
block), replicated in place for everyone as the code evolves. Join mid-work
without commit/push. Cloud runners; the same Rust app compiled to
wasm/WebGL for zero-install browser sharing; and third-party harness sync —
a Claude Code terminal session syncs live into a shareable Delta thread.
Notably absent from all the material: gates, verification standing, rubrics,
workflow-generality. Review is conversational, not governed.

**From the memory corpus (graphiti `agent_harness_research` /
`agent_eval_research` / `agent_infra_research`, episodes of 2026-08-20) —
hands-on and verified detail beyond the blogs:**

- An early-access hands-on (third-party demo video) reports Delta is
  **BYO-model and harness-pragmatic under the hood** — OpenCode as an agent
  backend, keys via Anthropic/OpenAI/OpenRouter/Baseten, Sonnet 5.6 —
  i.e. Delta itself is harness/model-neutral at the seam junto standardized
  with ACP (a Zed-originated protocol).
- The Zed team dogfoods it as **same-worktree, threads-instead-of-PRs**
  collaboration — the live shape of the workflow, not just marketing.
- The eval-research read: DeltaDB's delta-level identity enables precise
  attribution, SZZ-style analysis, and plan-adherence measurement, but
  **"adds granularity and offers no gating or scoring"** — independent
  corroboration that the verification layer is junto's unclaimed ground.

### Where it collides with junto's settled bets

| junto (settled) | Delta/DeltaDB |
|---|---|
| **No CRDT** (hard constraint #3); record = append-only entries, union-merge sync ([ADR 0011](adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md)) | CRDT-replicated worktree + conversation are the core abstraction |
| Record holds **ratified intent**, state-not-history; auto-captured rationale is "worse than no record" (`junto.md`) | The **full conversation is the source artifact** — auto-captured, exhaustively versioned |
| Pluggable substrate, forge-as-hub, MIT, local host | One proprietary DB + their cloud + their client |
| Workflow-general — coding is one Playbook | Code-centric |

The scope note that matters: ADR 0011's no-CRDT argument is about the
**durable record** — entries are immutable, so set union *is* the merge and
CRDT machinery has no job there. DeltaDB's CRDT lives in a plane junto
deliberately does not have: **live, in-flight working state**. The bets
collide only if junto builds that plane the same way; they do not collide
over the record.

### Decomposing the fluidity

The screencast fluidity has four separable causes — only one requires CRDT:

1. **Span-anchored, zero-ceremony comments everywhere** — needs anchors and
   fast rendering, not CRDT.
2. **Code ↔ conversation links that survive motion** — needs *stable
   reference identity*, not CRDT. Git-native approximations exist
   (commit + blob digest + hunk, re-anchored across commits the way blame
   already is).
3. **Join without commit/push** — needs replication of in-flight state.
   Zed's answer is CRDT; a **single-writer stream with live viewers** (one
   Session owns its worktree, everyone else watches and comments) delivers
   the felt experience for the dominant case — and most of what the Delta
   videos actually show *is* that case: an agent writes while humans comment
   and steer.
4. **True multi-writer co-editing** — the only part that genuinely requires
   CRDT, and the part junto's diagnosis (alignment is async/social, not
   co-presence) bets is not the bottleneck.

### Concepts to borrow (ranked)

- **★ Code→record back-links ("decision blame") — strongest fit.** From any
  file/line, surface the ledger entries and Sessions that touched it. This
  is junto's lead job — *your repo remembers why* — wearing Delta's best UI.
  Buildable today as a **projection over existing provenance** (entries →
  artifacts → paths, reverse-indexed); touches no constraint. Delta had to
  build a database to get this; junto already has the data.
- **★ Span-anchored, low-ceremony annotation.** Lands exactly on the parked
  collaborative-space (ledger `b405a1cb`), whose recorded load-bearing
  constraint — an append-only versioned artifact evolving through the loop,
  turn-based — remains compatible: comments are annotations/messages; only
  ratified outcomes fold into entries. Delta proves the UX ceiling worth
  aiming at.
- **★ Anchor durability.** Adopt the *idea* of delta-stable references via
  git-native content addressing: provenance today pins frozen digests
  ("what it was"); add re-anchoring ("where it is now").
- **◆ A live-viewer plane.** Presence + streaming a Session's conversation
  and worktree to watchers. The SSE session view exists locally; the gap is
  cross-machine cadence. `worked-example-production-troubleshooting.md`
  already licenses exactly this: *"shared real-time awareness … not shared
  real-time decision-making — a light presence layer, not a CRDT or
  co-edit."*
- **◆ Harness live-sync as validation.** Delta syncing Claude Code sessions
  into shareable threads is the same seam as junto's ACP session capture —
  it validates the session-artifact model and pressures its *cadence* (live
  stream vs post-hoc memo).
- **✗ Don't clone the CRDT worktree or conversation-as-source.** The
  engineering is enormous (Zed spent years on CRDTs before this), the record
  philosophy is opposed, and DeltaDB is proprietary — nothing to build on.

### The reopening (Dan, 2026-08-20) — rungs 1–2 built

Two parked/ratified decisions sat in this territory; per the dead-ends
convention they were surfaced, not silently retried:

- **`1d9cf9b1` (park ratified 2026-06-14):** *"Turn-taking is unsolved and
  worktree isolation is a prerequisite; the human-initiated sequential model
  works and avoids the coordination problem entirely. Park until both
  prerequisites and a real use case land."*
- **`b405a1cb` (ratified 2026-06-13):** the collaborative space stays on the
  roadmap, constrained to a turn-based, append-only versioned artifact.

Conditions had changed: **worktree isolation had landed** (the `185fd301`
ratification), **Delta is the real-use-case evidence**, and Dan signaled
willingness to reconsider the turn-based approach (2026-08-20). The
graduated ladder — each rung ships value alone, climb in order:

1. **Anchored comments** — async; no constraint touched. **Built:**
   span-anchored, signed `Annotation`s (`CodeAnchor`/`StreamAnchor`,
   re-anchored across code motion into `Exact | Moved | Orphaned`) —
   [`docs/superpowers/specs/2026-08-20-live-session-plane-design.md`](superpowers/specs/2026-08-20-live-session-plane-design.md).
   The **decision-blame back-link** this rung was paired with above (file/line
   → entries + Sessions) is *not* built — only the `CodeAnchor` type it will
   read is in place, by design, so that projection is free later.
2. **Presence + live session viewing** — ephemeral plane over the record;
   already licensed by the incident worked example. **Built:** a per-session
   `LiveDoc` (`junto-live`, loro), presence via `EphemeralStore`, and an
   authenticated WebSocket watcher surface in `junto-iced`.
3. **A live shared *conversation/plan document*** — CRDT scoped to one
   ephemeral document (loro adopted, MIT — verified at adoption; see
   [ADR 0034](adr/0034-crdt-confined-to-the-live-plane.md) for the transitive
   MPL-2.0 dependencies that came with it); durable outcomes still fold into
   entries. **Not built:** `conversation`/`worktree` are driver-only by
   policy in the shipped `LiveDoc` — multi-writer is representable, not
   authorized. Climbing this rung is an authorization change against the
   existing representation, not a rewrite; still only *if* rung 2 leaves the
   itch.
4. **Replicated worktrees** (DeltaDB territory) — only with evidence that
   rungs 1–3 can't deliver; a new substrate *plane*, never a change to the
   record. **Not built**, not attempted.

Rungs 1–2 shipped as designed: **the record stayed append-only ratified
entries** (ADR 0011 untouched throughout) and CRDT stayed confined to the
live plane, scoped by [ADR 0034](adr/0034-crdt-confined-to-the-live-plane.md)
— never the durable record. The unpark itself (drafted assertions citing
`1d9cf9b1` and `b405a1cb`) is recorded in ADR 0034's appendix, pending
Dan recording it in `junto-dev`.

### Tradeoffs, stated honestly

Delta is **genuinely better** at immediacy: joining work mid-flight,
anchored review fluidity, reference stability under motion, zero-install
web sharing. junto's surviving differentiators are exactly what the posts
never mention: **verification standing** (ratified/parked, gates, Rubrics),
**workflow-generality**, a **vendor-neutral pluggable substrate**, and
**MIT + local-first**. And Delta *validates* junto's spine from the largest
independent team yet: a conversation-centered, terminal-optional surface;
review where the work happened; agents as first-class thread participants;
comments on anything — the one-surface thesis, built by someone else.

The research corpus sharpens the split (`D:\git\research-reports\`
`pr-review-unbundling-2026-08.md`): the PR bundles four jobs — standards
enforcement, defect detection, verification of intent, knowledge transfer &
alignment — now separating by how cheaply each is verified. Delta rebuilds
the surface for the *alignment/knowledge-transfer* row (conversation
anchored to code, review where the work happened) and ships nothing for the
verification rows; junto's gates/rubrics/standing are the verification
rows with the alignment surface still thin. The unbundling frame says these
are complements — and that the team that mechanizes verification should
reinvest the freed attention into alignment, which is precisely the surface
Delta just raised the bar on.

**Openness (verified 2026-08-20, memory corpus):** Zed has **stated an
intention to open-source DeltaDB** "with optional paid services" — but no
license, no repo, no date (the zed-industries GitHub org contains no DeltaDB
repository; Delta is invite-only; sync runs on Zed's infrastructure). Today
it is hosted-only proprietary with a credible promise — credible because the
Zed editor and its collab server are already open source. Caveat for junto:
Zed's editor lineage is **GPL/AGPL**; if DeltaDB opens under copyleft, junto
can never vendor or link its source (hard constraint #1) — speaking its
*protocol* or shelling out would be the only integration paths. Until it
ships, DeltaDB-class conversation-linked version control is **a category of
one**. **Watch:** the license when it lands, and whether span-anchored
comment UX becomes table stakes for every agent surface.

### The opposite bet — Cursor's Origin & Continuity (assessed 2026-08-20)

[Git at any scale](https://cursor.com/blog/git-at-any-scale) (Vicent Martí,
2026-08-18) answers the same "version control in the agent era" pressure from
the opposite direction. Where Zed **replaces the versioning model**
(operation-level deltas, CRDT replication, a new DB), Cursor keeps git's
contract untouched and **rebuilds the hosting under it**: *Continuity* stores
every push as a WAL entry in S3 (the source of truth — on-disk repos are a
warm cache), linearizes all pushes via CAS on the WAL index, and scales
reads linearly with stateless replicas (rendezvous hashing + gossip, always
verified against S3). *Origin* is the hosted platform on top. Their stated
philosophy is junto's own: reuse git as-is, *"instead of doing weird stuff
with Git."*

Reads on junto:

- **Direct validation of the substrate bet.** Cursor's thesis is that
  git-the-contract stays the durable interface at any scale. junto's record
  rides exactly that contract (`refs/junto/*` over standard push/fetch), so
  it works against any host that honors it — Origin included.
- **The agent workload shape is confirmed:** *"vast numbers of small,
  throwaway repositories"* created by agents is a first-class design load —
  the same pressure behind junto's pooled worktrees and session isolation.
- **Hosting-level provenance:** Continuity retains every push ever made
  (*"we can look at every state a repository has ever been in"*) — a
  substrate-side complement to junto's entry-level provenance, and a
  reminder that re-anchoring (the Zed borrow above) can lean on history the
  host already keeps.
- **Layering, not rivalry (memory corpus):** DeltaDB is *single-repo-deep*
  (provenance inside one worktree); Continuity is *fleet-wide* (storage
  across millions of repos) — nothing prevents a DeltaDB-like layer running
  **on** a Continuity-like store. And Continuity's WAL is push-granularity
  time travel — a weaker cousin of delta identity (no conversation linkage,
  no survival across code motion) that **exists today under unmodified git
  tooling**.
- **Openness (verified 2026-08-20):** Origin is an early-beta **hosted SaaS
  gated by Cursor paid plans** — no self-hosting offered or promised; the
  blog's "deployed on any cloud" refers to Cursor's own portability across
  S3-compatible stores, not customer self-hosting; no license statement.
  The self-hostable analogue is GitLab's Gitaly (MIT; Spokes-pattern today,
  WAL-based rework in progress).
- **The week of 2026-08-18 in one line:** the three major agent-IDE vendors
  each attacked a different layer — Warp the SDLC orchestration envelope
  (Factories), Zed the version-control *data model* (Delta/DeltaDB), Cursor
  the version-control *hosting* (Origin/Continuity). The layer junto claims
  — the governed, verified record — is contested by none of them.
- **A new forge target with one capability question:** classify Origin in
  the custom-ref table when it's reachable — does it accept `refs/junto/*`?
  (Same verify-empirically posture as Bitbucket.)

Triangulating the three approaches: **Zed rebuilds the model, Cursor
rebuilds the hosting, junto adds a governed record beside the model.**
Cursor is orthogonal-complementary infrastructure junto could run on; Zed
is the workflow-layer competitor.

---

## Licensing read

Checked against hard constraint #1 (`CLAUDE.md`): **MIT, no copyleft *source*;
reuse ideas clean-room; linking permissive/linking-exception libs is fine;
shelling out to a separate program is fine even if GPL (junto already shells out
to `git`, GPL-2.0).**

| Project | License | |
|---|---|---|
| treehouse · no-mistakes · lavish-axi · axi · gnhf · firstmate · acp-mock · gh-axi · chrome-devtools-axi | **MIT** | ✅ |
| **ACP** (Zed repo + `agent-client-protocol` Rust crate) | **Apache-2.0** | ✅ links into MIT |
| **Delta / DeltaDB** (Zed) | **proprietary** today; open-sourcing *stated intent*, license TBD | ⚠️ ideas clean-room only; if it opens copyleft (Zed lineage is GPL/AGPL), protocol/shell-out only — never vendor/link |
| **Origin / Continuity** (Cursor) | **proprietary** hosted SaaS; no self-host promised | ⚠️ ideas clean-room only |
| CRDT crates, if rung 3 is ever built: **loro** · **yrs** · **automerge** | **MIT** (verify at adoption) | ✅ |
| **gsh** | **GPL-3.0** | ⚠️ don't vendor/link source |
| **superpowers-bench** | **no license** | ⚠️ all-rights-reserved |

**Three modes of reuse, three answers:**

1. **Reuse ideas clean-room** (graduated gates, pooled worktrees, skill-selection
   evals, HTML-review, orchestration-as-markdown): **always fine, for all of
   them** — patterns/methods aren't copyrightable. junto's default move anyway.
2. **Link a Rust library into junto's binary:** the **ACP Rust crate is
   Apache-2.0 → junto can depend on it directly** for the harness adapter. Zero
   friction. (Kun's tools are bash/node CLIs, not crates, so not linked anyway.)
3. **Shell out to a separate installed CLI** (treehouse, gh-axi): fine even for
   GPL — "mere aggregation", same posture as shelling out to `git`.

**The only two don'ts (both easy):** don't vendor/link **gsh** source (GPL-3.0;
tangential anyway), and take only the **methodology** from **superpowers-bench**
(unlicensed), never its code.

**Bottom line:** nothing here threatens junto's MIT/no-copyleft posture. The two
pieces junto might actually *depend on* rather than reimplement — **ACP**
(Apache-2.0) and **treehouse** (MIT) — are both clean. The licensing door for
the ACP-as-harness-protocol idea is wide open.

---

## Implications for junto's roadmap

1. **Domain model:** disambiguate **Playbook** (vs Ace) and reaffirm **Session**
   before building either.
2. **`AgentHarnessAdapter`:** evaluate **ACP** as the harness protocol before the
   second bespoke shell-out (OpenCode). License-clear (Apache-2.0).
3. **Multi-session isolation:** pooled-persistent worktrees (**treehouse** model),
   not throwaway.
4. **Gates:** adopt the **graduated** model (auto-apply mechanical, escalate
   intent — approve/fix/skip) from `no-mistakes`.
5. **Self-improving Playbook:** **skill-selection evals** (`superpowers-bench`
   methodology); the differentiator vs Ace stays **governed** evolution.
6. **Collaborative space (parked):** **lavish**'s turn-based annotate-HTML +
   long-poll is the reference design, on junto's async-versioned side.
7. **Agent surface:** weigh AXI principles (compact output) for junto's MCP
   tools and brief regardless of the MCP-vs-AXI question.
8. **Recall bridge:** borrow Ace's **itemized-delta + grow-and-refine** brief
   representation (ADR 0013) — discrete metadata-carrying items, localized-delta
   folds, an explicit curation pass — to defend the scaled brief against its own
   *context collapse* as channels age. Keep junto's governed evolution.
9. **Decision blame (code→record back-links):** build the reverse provenance
   index (file/line → entries + Sessions). The lead job's killer UI, proven
   wanted by Delta; a projection over data junto already has.
10. **Collaborative space, unparked path:** revive `b405a1cb` as rung 1 of
    the ladder (span-anchored annotation, turn-based, versioned artifact);
    reassess `1d9cf9b1` — its worktree-isolation prerequisite has landed.
11. **Live plane before CRDT:** presence + cross-machine session streaming
    first; any CRDT stays confined to an ephemeral document or versioned
    artifact — the record and ADR 0011 are untouched at every rung.

## Sources

- Ace: [MCP docs](https://docs.aceagent.io/docs/developer-guides/mcp-integration/overview) · [docs home](https://docs.aceagent.io) · [Zero Alignment essay](https://maggieappleton.com/zero-alignment/) · [ACE paper (ICLR 2026)](https://arxiv.org/abs/2510.04618) · [delta-update analysis (softmax)](https://softmaxdata.com/blog/the-biggest-lesson-from-ace-iclr-2026-the-power-of-agentic-engineering/) · [self-improvement-tools comparison (Ry Walker)](https://rywalker.com/research/agent-self-improvement)
- Kun Chen: [GitHub](https://github.com/kunchenguid) · [lavish-axi](https://github.com/kunchenguid/lavish-axi) · [no-mistakes](https://github.com/kunchenguid/no-mistakes) · [treehouse](https://github.com/kunchenguid/treehouse) · [firstmate](https://github.com/kunchenguid/firstmate) · [gnhf](https://github.com/kunchenguid/gnhf) · [axi](https://github.com/kunchenguid/axi) · [superpowers-bench](https://github.com/kunchenguid/superpowers-bench) · [acp-mock](https://github.com/kunchenguid/acp-mock)
- ACP: [Agent Client Protocol (Zed)](https://github.com/zed-industries/agent-client-protocol)
- Zed: [Introducing DeltaDB](https://zed.dev/blog/introducing-deltadb) ·
  [Introducing Delta](https://zed.dev/blog/introducing-delta) ·
  [delta.dev](https://delta.dev) · CRDT lineage: [zed.dev/blog/crdts](https://zed.dev/blog/crdts)
- Cursor: [Git at any scale — Origin & Continuity](https://cursor.com/blog/git-at-any-scale)
- Memory corpus: graphiti groups `agent_harness_research` · `agent_eval_research` · `agent_infra_research` (episodes `1f032055`, `cad5d93f`, `6ce279e0`, `88e6f546`; work done in `D:\git\graphiti`, 2026-08) and the syntheses in `D:\git\research-reports\`
- t3code: [pingdotgg/t3code](https://github.com/pingdotgg/t3code)
- "Harness engineering" (Ryan / OpenAI, AI Native DevCon): [talk](https://www.youtube.com/watch?v=c8bE0cj7vHY) — assessed against junto in [`self-improving-harness.md`](self-improving-harness.md) (the practitioner camp for the self-improvement loop; converges on the loop + observability afferent nerve, diverges on shift-right autonomy, eval rigor, and in-repo vs provenance-bound record)
