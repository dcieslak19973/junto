# junto — architecture (design notes)

> Architecture notes for junto's substrate, sync, and transport. **Status: core reframed 2026-06-08.** The original peer-to-peer **git-mesh is deferred** (see Appendix) in favor of a **pluggable substrate** (forge-as-hub / central self-hosted SoR) and **pluggable chat connectors**; the **one unified surface** is the point. junto ships **MIT, greenfield** (no copyleft source).
>
> _Renamed from `decentralized-architecture.md` — "decentralized" is at most an OSS-mode default, not junto's identity (forge-as-hub is itself a hub). See `pluggability.md` for the adapter detail and `junto.md` for the spine._

**Contents.** _Core:_ Thesis · What ACE is · Key decisions · Substrate & sync · Conversation · Gotchas · Open decisions. — _Beyond the core:_ Prior art (AI-PR flood) · Commissioner first-pass · Self-hosted git providers · Pre-remote review · Research/inquiry spaces · Pluggable Playbooks · Jira/Confluence integrations · Complementary idea. — _Appendix:_ deferred peer-mesh sketch.

## Thesis

Agent platforms are single-player. They make individuals productive but ignore the multi-human coordination that real corporate work requires. Influenced by Maggie Appleton's *"One Developer, Two Dozen Agents, Zero Alignment"* and GitHub Next's **ACE** (OpenAPI spec: https://api.ace.githubnext.com/schema.json).

The honed insight: the misalignment that actually hurts is **async and social** (decisions detached from the work, context in people's heads), not a lack of synchronous co-editing. And the daily pain is **tab-juggling** — Slack for the conversation, Jira for the work, GitHub for the code, all in different apps. So the target is **one surface where the conversation is fused to the work unit**, pulling those systems *in* via connectors — not a Slack clone, not yet-another-silo.

## What ACE actually is (evidence from its API)

- Unit of work = a **`channel`** = one git branch + a forked microVM (`workspace: {ref, base, url}`).
- "Multiplayer" = **`party: [githubUserId]`** — a list of users on a branch-scoped channel. No CRDT, no shared buffer, no presence/cursor entity anywhere in the schema.
- Sits **on top of** GitHub (auth = GitHub OAuth + App installs; channels typed `issue|pr|ref`).
- Its anti-misalignment weapon is **async LLM dashboards**: `/dashboard/summary | team-summary | suggestions | greeting`, all `POST context → {summary}`.

Takeaway: the canonical product built for this exact problem uses **zero CRDTs**; it's git-native + async AI summaries. ACE got the **`channel`** right. junto keeps it — and unifies **across vendors** instead of locking to GitHub + one cloud.

## Key decisions

| Question | Decision |
|---|---|
| Product shape | **One unified surface**: the conversation fused to the work unit, with tickets/code/artifacts pulled *in* — so the user isn't juggling Slack + Jira + GitHub. Channel = unit of inquiry (a PR is one possible outcome, not the unit). |
| Human surface | **Terminal-less** (humans never drive a shell; agents run shells under the hood, output captured as artifacts). |
| Substrate / deployment | **Pluggable (`SubstrateProvider`)** — OSS = **forge-as-hub**; regulated = **central self-hosted SoR**. Not a fixed "no central service." |
| Conversation / realtime | **Delegated, pluggable** — `ChatConnector` (Slack/Discord/Telegram/Teams) ingested into the surface, and/or a light native surface. **No peer-mesh built.** |
| Durable record | **git refs** (`refs/junto/*`, partition-by-author, git-bug prior art) synced via the hub — holds verified intent + provenance, not raw chatter. |
| CRDT? | **No.** Append-only log; concurrent sends interleave by `(ts, author)`. |
| Vendor neutrality | Adapters at every boundary; behave on capability flags, not vendor name (see `pluggability.md`). |
| Licensing | **MIT, greenfield.** Build the patterns clean-room; incorporate no copyleft source. |

## Substrate & sync

The durable record (channel state, decisions/ledger entries, message log) lives in **git refs** — `refs/junto/*`, partitioned by author so git never conflicts (prior art: **git-bug**, a tracker stored in git refs). **Dedicated refs, not working-tree files** (no `git status` pollution). What differs by deployment is *how those refs are synced and authorized* — the `SubstrateProvider` boundary:

- **forge-as-hub (OSS / small teams).** Push/fetch `refs/junto/*` to the forge the team already uses (GitHub/GitLab/Bitbucket). The forge is the hub; no server to run, no VPN, no peer discovery. Simplest, and leverages existing infrastructure. Forge-agnostic.
- **central self-hosted SoR (regulated / enterprise).** A self-hosted server stores the record in a controlled store with **WORM retention, SSO-tied info-barrier ACLs, supervision, revocation, legal hold** — controls a mesh *cannot* provide by construction (no central authority). This is the higher-stakes mode; see §research-spaces.

> ⚠️ "No central service / decentralized" is therefore at most the OSS default, and even there forge-as-hub *is* a hub. It is **not** a core differentiator. What survives every mode: one unified surface · forge-/harness-/backend-agnostic · terminal-less · workflow-general · verified-reproducible record.

### Conversation
Realtime conversation is **delegated**, not built: a `ChatConnector` (Slack/Discord/Telegram/Teams) is ingested **into** the one surface (inbound aggregation primary; optional outbound bridge for non-adopters), and/or a light native conversation surface. Agents post via **MCP**; the **Connector** handles the bidirectional bridge + inbound triggers. junto's founding thesis (don't split conversation from work) is served by **unifying in one surface**, not by building chat transport.

### Identity & ordering
- **Identity** = git author (`user.email`/`user.name`) — stable, on every commit. AuthZ per substrate: pubkey-pinned roster (OSS) or SSO/entitlements (regulated).
- **Ordering** = wall-clock `(ts, author)`; HLC if clock skew bites.

## Important corrections / gotchas
- **Don't build chat transport.** The realtime requirement that justified a peer-mesh is met by the chat connector (or a light hub layer). The mesh is deferred (Appendix).
- In-window feature ⇒ build it natively (no plugin-API shortcut to lean on).
- Agent transcripts live **outside** the repo. The durable record holds **decisions/intent + provenance + digests**, not raw transcripts.

## Open decisions (settle before coding)
1. **Substrate default & primary segment.** forge-as-hub (OSS) vs central SoR (regulated) — and which segment is primary, since it sets the default and the positioning.
2. **Conversation surface.** How much *native* chat does the one surface need vs relying on `ChatConnector`? Likely minimal-native + connectors for inbound aggregation.
3. **Sync cadence** (forge-as-hub): push/fetch debounce vs poll vs webhook — affects freshness.
4. **Trust model per substrate:** pubkey-pinned roster (OSS) from day one; SSO/entitlements + supervision (regulated).

---

## Beyond the core — considerations & open explorations

_Larger, optional directions beyond the core above. Order: the strategic framing (AI-PR flood) first, then the governance mechanisms it motivates (commissioner first-pass → forges → pre-remote review), then the research use case, pluggability, and integrations. Each section stands alone._

## Prior art: the AI-PR flood — prevent / constrain / triage

The bottleneck has moved from *writing* code to *reviewing* it: one human can't keep pace with agent output. Concrete proof in the wild — a reduced-privilege agent bot account observed with **105 open PRs, ~96 stale (>2wk), fresh-first, 0 ever pruned, all merged by one person**; in the last 2 weeks **0 of the 96 old PRs were merged** — every merge was a brand-new PR. That's the failure mode the whole industry is now reacting to. Responses fall on a three-stance spectrum:

- **Prevent (upstream / alignment) — stop bad ideas before coding.** Maggie/ACE: agree on the plan in a shared, agent-present session *before* agents build, so fewer wrong/duplicate PRs ever exist.
- **Constrain (production) — make agents emit bounded, reviewable PRs.** Stripe **Minions** (https://stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents): ~**1,300 fully-AI PRs/week**, kicked off by an emoji reaction, built via **blueprints** (deterministic code + bounded agent loops) in isolated devboxes, emitting code+tests+docs; **every PR human-reviewed, nothing auto-merges.** Thesis: *"the walls matter more than the model."* (Both Stripe and Uber named it Minion(s) and forked Block's Goose; both keep a human gate on every PR.)
- **Triage (downstream) — help overwhelmed humans survive the flood.** Uber **Code Inbox** (risk-ranked routing + smart assignment by ownership/recency/timezone/calendar + **SLO escalation: respond-or-reassign-or-escalate**) + **uReview** (AI review comments **graded / merged / categorized**, low-confidence dropped, devs rate usefulness). uReview principle worth stealing: *outputting low-quality comments is the worst outcome* — noise trains rubber-stamping. Salesforce treats PR automation as a *stack* (merge queue, stacked PRs, smart assignment, AI review, analytics).

**The stat that sets priority:** AI PRs wait **4.6× longer** for review pickup and are accepted only **32.7%** of the time vs **84.4%** for human PRs (https://thenewstack.io/ai-generated-code-crisis/). Low acceptance ⇒ a third-plus of review effort is spent on things that shouldn't have been built ⇒ **prevention has the highest leverage**; triage only manages a flood that alignment would have shrunk. (Open-source maintainers are hitting this first.)

**Synthesis — where this design sits.** The three stances are complementary, and this design tries to span all three (across the Considerations below), where most tools pick one:
- *Prevent* = channels-as-alignment + the research/inquiry spaces (pre-PR alignment, pre-registration, hypothesis ledger) — see §research-spaces.
- *Constrain* = worktree/runtime sandboxing + **provenance binding** (agents produce verifiable artifacts, not prose) — see §research-spaces.
- *Triage* = commissioner first-pass + pre-remote risk-routing + graduated gates — see §commissioner-first-pass, §pre-remote-review.

The flat-queue bot above does **none** of the three well (no alignment gate, weak constraint, flat queue/no triage) — which is why it rots. The leverage argument says **weight upstream alignment**: the ACE-style channel isn't just a collab nicety, it's the highest-ROI lever because it removes review load rather than redistributing it. Two concrete refinements pulled from the prior art:

1. **Add an SLO/aging axis to the commissioner gate** (from Code Inbox). Risk-routing currently has no time dimension; without "respond-or-reassign-or-escalate / auto-age-out," the residual flood rots into a backlog. The pre-receive-hook backstop can also enforce staleness decisions.
2. **uReview-style review-quality loop** (high-signal only + usefulness rating). The assisting reviewer agent must grade / merge / drop low-confidence and be rated — feeding the self-improving-harness thread (rate comments → improve the reviewer). Hold the principle: a review layer that emits noise is worse than none.

Net posture: weight **prevention** (alignment channels), keep Stripe-style **constraint + always-human-gate**, and use Uber-style **triage with SLOs** for what remains.

## Consideration: commissioner first-pass before CODEOWNERS notified

Problem: in a corporate env, an agent PR auto-notifies CODEOWNERS — but the human who commissioned the bot should do a first-pass so they don't waste reviewers' time. Principle = **polluter-pays / accountability routing**: commissioner absorbs first-pass cost before externalizing review cost onto the team.

Key lever: **GitHub draft PRs do NOT auto-request CODEOWNERS**; the draft→ready conversion is the notification moment → use it as the gate. (Branch-protection code-owner rules gate *merge*, not the ping.)

Options (compose, nudge→hard): draft-by-default + reminder; soft gate = no "mark ready" until commissioner opened diff; first-pass-in-app before push; required status check on GitHub (`commissioner-reviewed` signal); **auto-promote draft→ready when first-pass completes**.

Recommended: **draft-by-default + first-pass-in-app + auto-promote**, using the in-app review surface (review pane, git-patch artifacts, assisted-review hunks) + an agent-generated risk summary to make the pass cheap; **force a one-line rationale** (not a checkbox) to avoid rubber-stamps. Layer a required status check if a client-independent hard guarantee is needed. A reviewing agent may *assist* but not *replace* the human accountability. Surface first-pass status in the collab work-unit thread for team awareness.

## Consideration: heterogeneous / self-hosted git providers

Many orgs run **self-hosted forges — Bitbucket Data Center, GitLab self-managed — not GitHub.** A collaboration/review layer that only works on GitHub excludes them, so the review/governance flow must be **provider-agnostic across GitHub, GitLab, and Bitbucket** (cloud and on-prem). Implications:

- **ACE is unusable off GitHub** — it's GitHub-native (GitHub OAuth/App installs, channels typed to GitHub issue/pr/ref). There's no buyable off-the-shelf answer for non-GitHub shops → strengthens building this.
- A good multi-forge base is **NOT forge-coupled:** no octokit / no GitHub API for PRs; forge names appear only where a git remote host is classified (a reusable adapter-selection seed). Core = git worktrees, forge-agnostic; PR creation is `gh` by *convention*, not hard code.
- Self-hosted forges = full APIs + webhooks you control; fits the on-prem/VPN ethos.

**The GitHub first-pass trick (draft suppresses CODEOWNERS ping) does NOT port.** Per-forge:
- GitHub: draft→ready is the gate (native CODEOWNERS).
- GitLab self-managed: draft blocks *merge* not pings; lever = **delay reviewer assignment** until first-pass; CODEOWNERS is **Premium/Ultimate only**; pipeline job can gate.
- Bitbucket DC: **no native CODEOWNERS** (has "Default Reviewers" auto-added at creation, or a marketplace Code Owners app); draft PRs **version-dependent**; gate via **merge checks / required build-status** API (notification-timing is hard since default reviewers attach at creation → lean on merge gate).

Portable abstraction = *control when broader reviewers are notified, gated on commissioner sign-off* — 3 implementations.

**Architecture: commissioner gate lives in junto (forge-agnostic); forge = thin `ForgeAdapter`** (`createChangeRequest({draft})`, `assignReviewers()`, `setCommitStatus()`, `promoteReady()`, `resolveCodeOwners()`), selected via remote-host detection. Normalize code-owners by keeping one `CODEOWNERS` file (GitHub+GitLab honor natively) and having the Bitbucket adapter translate it to Default-Reviewer API calls — junto gives a uniform code-owner concept across providers. Ship adapters per target deployment (GitHub / GitLab / Bitbucket); the gate logic is shared. Mesh-over-git chat is fully forge-agnostic; forge differences quarantined to the PR boundary. Per-deployment unknowns to verify: GitLab Code Owners tier; Bitbucket DC version for draft support.

## Consideration: pre-remote (pre-origin) in-channel review + risk-routing

Idea: within the channel, evaluate whether a change needs **local review (commissioner + maybe channel participants) BEFORE pushing to remote git** — a stage no forge offers natively (forges review post-push on the PR/MR). Possible because the channel has its own in-channel comms + git refs decoupled from `origin`. "Local" = channel-scoped, pre-origin.

Reusable infra: a **git-patch artifact service** turns agent changes into portable git patches (mbox/format-patch — subagent patches); a **policy service** (fetched-policy engine, precedent for a pre-push ruleset); a **workspace-goal service** for deviation-from-goal signal; a review pane / assisted-review hunks for the review surface.

Mechanism: agent change → patch artifact written to channel git-backed log (a `proposed/` ref) → participants review inline in channel (no origin push, no CI noise, no half-baked server branch) → on approval, `ForgeAdapter` pushes + opens MR.

**Risk-routing (don't gate everything — avoids friction/rubber-stamp):** deterministic rules (hard floor: sensitive paths via normalized CODEOWNERS — auth/migrations/CI/infra/public-API always gated; reversibility) + LLM evaluator (reads diff+goal+policy → routing rec + risk summary + forced one-line rationale). Outcomes: **auto-push / commissioner-only / full-channel review.** Channel `party` = eligible local reviewers.

**Unifies with the commissioner-first-pass gate:** the local channel review IS the first pass → completing it auto-satisfies that gate. Pipeline: change → risk-eval → [local review if warranted] → push → open MR with provenance ("channel-reviewed by X,Y" trailer/note) → CODEOWNERS notified.

Caveats: (1) keep local review LIGHT (cheaper-than-PR is the whole point; don't clone PR review). (2) Client gate is bypassable via raw `git push` → real enforcement = **pre-receive hooks on the self-hosted forges**: approval signal as signed commit trailer/git-note, hook rejects pushes to protected branches lacking it. Available *because* forges are on-prem (same property that killed ACE).

## Consideration: interactive PR-review workspace (concrete near-term feature)

The most actionable manifestation of "agent tooling over-serves production, under-serves review." There's no first-class way to pull a PR (esp. cross-fork) and review it interactively — workspaces are "local project + (usually new) branch to do work in"; the immersive review pane assumes a *workspace's own* diff-vs-base; and there's no path to submit a *human's* pane review back to the PR.

Missing primitive = a **"review a PR" workspace kind**: intake a PR (URL / `owner/repo#n`) → fetch `refs/pull/<n>/head` (works cross-fork) → worktree → diff **preloaded** in the immersive review pane (base = PR base) → **interactive human+agent session** over the checkout (navigate hunks, ask grounded questions, run/verify, co-author comments) → **push comments + verdict back** to the forge; persist a workspace↔PR link.

Human participation already *partly* exists — the immersive review pane is human-driven (navigate / mark-reviewed / pin notes) and the chat lets you interrogate the diff. So this is mostly an **entry point** + **review-shaped session** + **push-back**, not new review UI. Reuses worktrees, immersive review UI, agent chat + deep-review, run/verify; new bits = PR intake (cross-fork fetch), review-kind workspace, push-back, linkage. Maps to this doc's *PR-as-channel / unit-of-inquiry*, and is the smallest first step in rebalancing toward the review end.

## Consideration: channels as agent-augmented RESEARCH/inquiry spaces (pre-PR)

In **regulated / high-stakes knowledge work** (finance, healthcare, legal, scientific R&D, …), the same collab need spans non-engineering personas — analysts, scientists, clinicians, operators, reviewers — who investigate questions *before* any code change exists. Idea: use collab-channels for this agent-augmented inquiry, well before any code is on the table. **Possibly more important than the code case** — code collab is comparatively well-served elsewhere; cross-functional, agent-augmented, reproducible, auditable inquiry that crosses domain silos is less served, and carries real stakes (capital, safety, liability).

**Generalization:** channel = a **unit of inquiry** (a question), not a code change. A PR is just *one possible outcome* alongside a memo, an analysis/result, a config/param change, an alert, or a parked dead-end. The graduated review pipeline still applies when an inquiry crystallizes into action.

**Reproducibility binding:** agents run in worktrees capturing exact commands/commit/data/output, so every claim in a channel can be **bound to the inputs that produced it** (re-runnable). Antidote to unreproducible claims (stale/as-of data, lost seeds, one-off "findings"). Design rule: agents produce **verifiable artifacts, not just prose** — trust the computation, not the narrative.

**Domain-agnostic adversarial risks:**
- **Hallucination is dangerous when decisions carry real consequences** (capital, patient safety, legal exposure) → enforce verifiable/re-runnable artifacts.
- **Cheap experimentation → false-discovery explosion** (multiple-testing / data-snooping / p-hacking). The channel log should be a **hypothesis-testing ledger** tracking search breadth, enabling multiple-comparison corrections (e.g. FDR control in science; deflated-Sharpe / overfitting penalties in finance — López de Prado / Bailey). A system that makes search cheap MUST track search breadth or it manufactures spurious "findings."
- **Negative results are an asset** — make parked/falsified channels first-class (institutional memory of dead ends), don't delete.

**Regulated reality (pulls AGAINST the "no central service" ethos):**
- **On-prem / approved models non-negotiable** (no public LLM for sensitive data — trading research, PHI, privileged material). An OpenAI-compatible adapter can point at an internal inference endpoint. Hard requirement.
- **Access control / information barriers** — "trust anyone on VPN" is INADEQUATE here; need real per-channel ACLs tied to org SSO/entitlements; private channels = compliance control (cf. ACE `private:boolean`). (Info barriers in finance; need-to-know / PHI minimization in healthcare.)
- **Retention / audit / supervision** — the record may be regulated (SEC 17a-4 WORM / FINRA / MiFID II in finance; HIPAA in healthcare; legal hold / eDiscovery). Append-only signed git log is an asset IF immutable + retained + supervisable.
- **TWO TRUST REGIMES (→ `SubstrateProvider`):** the OSS/trustless mode fits **forge-as-hub** (or, niche, the deferred peer-mesh); regulated internal research WANTS a central (self-hosted) system-of-record (identity, retention, surveillance, revocation). Share the *channel abstraction*; let the substrate differ by mode. Don't force one across both.

**Scope check:** junto = substrate (~20%: channels, agents, provenance, sync, MCP). The ~80% is domain-specific: **MCP tools** for the domain's data stores / analysis or simulation engines / systems of record (e.g. market-data + backtest + P&L + blotter in finance; EHR + cohort/stats tooling in healthcare), plus the compliance layer (SSO entitlements, retention, supervision). Building a domain platform ON junto, not configuring junto.

**Review pipeline re-maps (same shape, higher stakes):** local channel review → domain-expert/commissioner first-pass; CODEOWNERS/remote review → **domain sign-off before a high-stakes or irreversible action** (deploy capital, change a clinical protocol, publish/file); pre-receive-hook enforcement → **production-deploy gate to live systems**.

Priorities to nail first: **(1) provenance binding** (every claim re-runnable) + **(2) hypothesis ledger / search-breadth tracking** — together they separate "research that compounds org knowledge" from "an overfitting machine with a chat UI."

Open next step: walk one persona end-to-end (e.g. an analyst investigating an anomalous result: question → agent data pull → analysis → finding → sign-off) to surface the exact tools + gates needed.

## Consideration: make Playbooks (e.g. the research flow) PLUGGABLE

Not everyone wants the research-channel flow → it (and trade-postmortem, code-PR, etc.) should be pluggable, not baked into core.

**Grounding:** **Workflow Definitions** (reusable JS conductor scripts coordinating sub-agent tasks; a definition store; scratch→**promotion**; **"discoverable like skills"** = the plugin model). Built-in deep research as the first showcase workflow — the research flow as a plugin is the planned direction.

**Behavior extensibility already fits:** skills (md), agent definitions (md), MCP tools, workflow defs (JS, discoverable). What's missing = **UI extensibility** + the **human-collaboration/channel/governance layer**.

**Layered pluggability map:**
- **Layer 0 — Kernel (generic, never plugged):** channels, party/ACL, git-refs record (hub-synced), passive provenance capture, presence, agent runtime, workflow runtime, MCP host, generic **gate engine** (state machine + approvals), artifact store, search.
- **Layer 1 — Already pluggable:** MCP tools (= market-data/backtest/P&L), agent defs (= research/ops agents), skills, workflow defs (= backtest-research orchestration).
- **Layer 2 — NEW: "Playbooks" (collaboration templates).** Package composing Layer 1 + declaring the human side: `{id, name, icon; lifecycle state-machine; gates per-transition (who/conditions/risk-routing); default roles/ACLs/info-barriers; offered workflow defs; required agents+MCP caps; artifact kinds+renderers; views (dashboard, hypothesis ledger); review policy}`. Research / trade-postmortem / code-PR each = a Playbook (code-PR "default" is just a built-in one). Discoverable like skills, scratch→promote.

**Adversarial caveats:** (1) **UI extensibility is the hard part** — no view-plugin model + XSS rules forbid arbitrary render code → make Layer-2 views **declarative** (schema over a fixed component palette), not arbitrary React; custom renderers are the one place core work may still be needed. (2) **Rule of three** — don't build the framework with one plugin; build research + trade-postmortem + code-PR concretely, then EXTRACT the Playbook seam. (3) **Plugin trust** — workflow defs run code (sandbox: conductor-only, no bash/file v1); plugins touching market data/capital need stronger permission + signed packages.

## Consideration: Jira/Linear + Confluence integrations — MCP covers only HALF

"Maybe solved with MCP" → only for the agent-action half. Five integration modes:
- **A. Agent action** (in-session, pull): agent files a Jira ticket → **MCP** ✅ (Atlassian/Linear MCP servers exist; Layer 1).
- **B. Bidirectional sync** (stateful): channel ↔ Jira issue; gate transition → ticket status → ❌ needs sync loop.
- **C. Inbound trigger** (event push): Jira "In Progress" → auto-spawn channel+agent → ❌ MCP is pull-only, no webhooks.
- **D. Publish/link** (outbound): channel concludes → memo published to Confluence as org record → ❌ publish action + provenance link.
- **E. Identity/permissions**: Atlassian groups define who-sees-what (info barriers) → ❌ SSO/entitlements.

MCP = agent-mediated, session-scoped, pull. B–E are persistent, event-driven, bidirectional. Using MCP for sync = agent polling = fragile.

**New plug type: Connector** (kernel-level, persistent; webhook receiver + reconciliation loop + creds/identity + entity mapping). **Same shape as ForgeAdapter** → unify as one **Connector abstraction**: `ForgeAdapter` (git hosts), `IssueTrackerConnector` (Jira/Linear), `KnowledgeConnector` (Confluence). A **Playbook (L2) declares** which MCP tools + connectors it uses (e.g., research kind: read Confluence via MCP, link Jira ticket via connector, publish memo to Confluence via connector).

**Principle (ACE/GitHub lesson again): integrate the SoR, don't replace it.** Channel = live agent-augmented *doing* surface; Jira = work-tracking SoR (channel↔issue, gate transitions↔issue status); Confluence = knowledge SoR (read-context via MCP + publish-target via connector). Resolves "where does institutional memory live": **rich re-runnable working record stays in junto** (provenance, hypothesis ledger); **curated summary publishes one-way to Confluence** for the org. Both, not one mega-store.

**Practical:** (1) Likely **Jira DC + Confluence DC** (consistent self-hosted theme; verify DC-vs-Cloud API like the forges). (2) **Minimize bidirectionality — where integrations die** (conflict/loops/mapping-drift/rate-limits). Prefer one-way: junto→Confluence publish (easy), Jira→junto trigger + junto→Jira status (tractable) over full field sync. Don't let the channel become a worse Jira — link/reflect status, don't mirror schema. Each connector = maintenance tax; add lazily, driven by a Playbook that needs it.

## Complementary idea (parked, not the core)
An async **"team dashboard"** (read everyone's append-only logs + git/PR state, emit per-person LLM summaries + resume-suggestions — mirroring ACE's `/dashboard/*`). Good optional add-on; the partition-by-author append-only pattern carried straight into the durable-record design.

---

## Appendix — deferred peer-to-peer mesh (sketch retained, not the plan)

> **Deferred 2026-06-08.** The pure peer-to-peer mesh solved "realtime without a central hub." But OSS/small teams have a hub (their forge → forge-as-hub), regulated teams want a central SoR, and realtime conversation is delegated to chat connectors — so the mesh's job evaporated. Retained here for the narrow niche (*no hub allowed AND native realtime required*); **don't build speculatively** (rule of three). The git-refs-as-record idea graduated into the main Substrate design above; only the *peer transport* is parked.

Two planes (the original design):

```
        ┌──────────────────── git (durable plane) ───────────────────────┐
        │  refs/junto/peers/<author>  → roster: vpn addr + pubkey         │
        │  refs/junto/chat/<author>   → that author's append-only msg log │
        └──────────▲────────────────────────────────────▲────────────────┘
           push/fetch own ref                     fetch peers' refs
                   │                                      │
   ┌───────────────┴────────┐                ┌────────────┴────────────────┐
   │  Local node            │   WSS direct   │  Teammate's node             │
   │  MeshTransport (WS srv)│◀══════════════▶│  MeshTransport (WS srv)      │
   │  MeshChatLog (git refs)│  over VPN/Tnet │  MeshChatLog (git refs)      │
   └────────────────────────┘                └──────────────────────────────┘
```

- **Durable plane = git refs** (graduated to the main design). **Live plane = direct WSS mesh over a VPN/Tailnet**; dial each roster peer; full mesh (team-sized N).
- **Lifecycle:** send → append local log → broadcast WS to live peers → debounced git push; receive → dedup by id; offline catch-up → `git fetch` peers' refs → union-merge → dedup. Fully functional offline, realtime when connected.
- **Components:** `MeshRoster` (roster from refs, replaces mDNS) · `MeshTransport` (WS + dialing + reconnect) · `MeshChatLog` (append-only, dedup/reconcile) · `PresenceTracker` (WS-only ephemeral).
- **authN** = VPN / Tailscale LocalAPI (peer IP → identity); **authZ** = roster pubkey pinning; harden with a signed handshake.
- **Niche-specific unknowns** (only if ever built): mesh fan-out, push cadence, DERP/NAT fallback.
- **Note:** mDNS is out over a routed VPN (link-local multicast not carried) — discovery would use the git roster.
