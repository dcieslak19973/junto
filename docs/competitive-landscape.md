# Competitive & ecosystem landscape

> Snapshot: **2026-06-13.** This is a record of **bets and tradeoffs, not a
> claim of superiority** — where comparisons appear, they name where the other
> approach is *genuinely better* too. Covers two reference points junto keeps
> bumping into: **Ace** (the integrated rival from the "Zero Alignment" essay)
> and **Kun Chen's toolkit** (the composable agent-native-CLI school). Re-read
> [`junto.md`](junto.md) for the thesis these are measured against.

## TL;DR — where junto sits

Two schools are forming around "humans + agents do real work":

- **Integrated platforms** — one hosted product (Ace): sessions, shared docs,
  microVMs, governance, an MCP surface. Bet: own the whole surface.
- **Composable agent-native CLIs** — a toolkit of sharp single-purpose tools an
  agent invokes (Kun Chen: worktrees, gates, evals, HTML review, orchestration),
  unified only by a token-efficiency *design standard* (AXI). Bet: Unix, for
  agents.

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
**Agentic Context Engineering**). Product-side it is **synchronous multiplayer**
— shared workspaces, live cursors, co-editing, per-session microVMs.

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

## Licensing read

Checked against hard constraint #1 (`CLAUDE.md`): **MIT, no copyleft *source*;
reuse ideas clean-room; linking permissive/linking-exception libs is fine;
shelling out to a separate program is fine even if GPL (junto already shells out
to `git`, GPL-2.0).**

| Project | License | |
|---|---|---|
| treehouse · no-mistakes · lavish-axi · axi · gnhf · firstmate · acp-mock · gh-axi · chrome-devtools-axi | **MIT** | ✅ |
| **ACP** (Zed repo + `agent-client-protocol` Rust crate) | **Apache-2.0** | ✅ links into MIT |
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

## Sources

- Ace: [MCP docs](https://docs.aceagent.io/docs/developer-guides/mcp-integration/overview) · [docs home](https://docs.aceagent.io) · [Zero Alignment essay](https://maggieappleton.com/zero-alignment/)
- Kun Chen: [GitHub](https://github.com/kunchenguid) · [lavish-axi](https://github.com/kunchenguid/lavish-axi) · [no-mistakes](https://github.com/kunchenguid/no-mistakes) · [treehouse](https://github.com/kunchenguid/treehouse) · [firstmate](https://github.com/kunchenguid/firstmate) · [gnhf](https://github.com/kunchenguid/gnhf) · [axi](https://github.com/kunchenguid/axi) · [superpowers-bench](https://github.com/kunchenguid/superpowers-bench) · [acp-mock](https://github.com/kunchenguid/acp-mock)
- ACP: [Agent Client Protocol (Zed)](https://github.com/zed-industries/agent-client-protocol)
- t3code: [pingdotgg/t3code](https://github.com/pingdotgg/t3code)
