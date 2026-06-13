# Ace — competitive note

> Snapshot: **2026-06-13.** Ace is the product from Maggie Appleton's
> ["Zero Alignment"](https://maggieappleton.com/zero-alignment/) essay, now
> shipping at [aceagent.io](https://aceagent.io). [`junto.md`](junto.md)
> already frames junto against it (the "ACE got the channel right; junto picks
> the other fork on co-presence" thread). This note captures what Ace's **API**
> shows as of today and — honestly — where junto's deliberately different bets
> are better *suited*, where **Ace's are genuinely better**, and what that
> means for junto's roadmap. It is a record of bets and tradeoffs, **not a
> claim of superiority.**

## What Ace exposes (2026-06-13)

A **bidirectional MCP server** at `https://aceagent.io/mcp` (read **and** write
— [docs](https://docs.aceagent.io/docs/developer-guides/mcp-integration/overview)),
eight tools:

| Group | Tools |
|---|---|
| Discovery | `list_playbooks`, `find_playbook` (semantic task→playbook match), `get_playbook` (version/section filtered) |
| Evolution | `create_playbook`, `create_version`, `trigger_evolution`, `get_evolution_status` |
| Outcomes | `record_outcome` (`success` \| `partial` \| `failure` + notes/reasoning traces) |

The prescribed agent loop: **discover playbook → load before execution → apply →
`record_outcome` → outcomes feed evolution.** A closed
**playbook-evolution loop** — fitting, since "ACE" stands for *Agentic Context
Engineering*. Product-side, Ace is **synchronous multiplayer**: shared
workspaces, live cursors, co-editing, and a per-session **microVM** on its own
git branch.

## Three overlaps with junto — each a tradeoff, not a scoreboard

### 1. Both expose an MCP agent surface — for different jobs

Ace's MCP optimizes the agent's **input context** (retrieve and apply the right
playbook). junto's `junto serve` records the agent's **output** — decisions,
findings, proposals, sessions, gates (the ledger). These are **complementary**,
not competing: junto could consume an Ace-shaped context layer behind an
adapter boundary. No better/worse here — different functions.

### 2. "Playbook" is a naming collision (like "Session" was)

- **Ace Playbook** = an evolving **instruction/context document** that improves
  from recorded outcomes.
- **junto Playbook** = a **work-type** stamped on a channel — it *supplies* the
  Lifecycle, the gate-routing function, the Verifier, the offered tools/agents,
  and artifact renderers ([`domain-model.md`](domain-model.md)).

Same word, different layer — exactly the trap junto already documents for
"Session" (Ace calls its *channels* "Sessions"). **Action:** note this in the
domain model before junto builds Playbooks, so the two don't get conflated.

### 3. Self-improvement: Ace ships it; junto designed it

Ace's `trigger_evolution` / `create_version` / `record_outcome→eval` is, almost
beat for beat, the loop in
[`self-improving-harness.md`](self-improving-harness.md) — and that doc's thesis
is **"evals are the crux."** Ace is **ahead on shipping** this loop. junto's
intended differentiation is not speed-to-ship but that its outcomes are
**governed and verified** (below), so evolution is auditable rather than an
opaque optimizer.

## Why junto takes the fork it rejects from Ace — stated as bets

This is the honest version of the claim I first overstated as "better."

### Async versioned record vs synchronous shared buffer

- **Where Ace is genuinely better:** real-time pairing and immediacy — live
  cursors, co-editing, the *felt experience* of collaborating when everyone is
  online together. Tight, fast loops.
- **junto's bet:** for work taken to a *verified, provenance-bound outcome*,
  often **across time** and with **agents as peers** (an agent needs no
  cursor), a durable, auditable record of *what changed and why* matters more
  than liveness — and it avoids CRDT/merge complexity entirely
  (`junto.md`: *"zero CRDT / presence / shared-buffer anywhere in the schema"*).
- **The diagnosis behind the bet:** the bottleneck is **alignment**, and the
  fix is *structured deliberation before a consequential action* — which
  `junto.md` argues is **"not synchronous co-presence."** If that diagnosis is
  right, co-presence is solving the wrong problem; if it's wrong, junto pays for
  this choice. **It is falsifiable, not proven.**
- **The cost junto accepts:** no live "working together" feel; collaboration is
  turn-based and recorded.

### Governed/verified outcomes vs automated eval-driven evolution

- **Where Ace is genuinely better:** frictionless, scalable self-improvement on
  low-stakes, high-volume tasks — the system learns from `success/partial/
  failure` with no human in the loop.
- **junto's bet:** for **consequential or accountable** work (the regulated /
  high-stakes end junto targets), automated drift on a coarse signal is a
  liability; a human-verifiable record (ratify / park, provenance, gates) is the
  product. "Why did the system change?" must be answerable.
- **The tradeoff, plainly:** junto trades **speed and automation** for
  **accountability and a brake on drift**. Ace trades the reverse. junto's own
  [`attention.md`](attention.md) concedes the scarce resource is **human
  attention** — so the gate that adds friction had better be worth it; that's a
  real ongoing risk for junto, not a free win.

## What this means for junto's roadmap

1. **Domain model:** disambiguate **Playbook** (and reaffirm **Session**) from
   Ace's senses before building either.
2. **Self-improvement Playbook:** the differentiator vs Ace is **governed
   evolution** — versions and outcomes that pass through gates and land in the
   record — not a faster optimizer. Lean into evals-as-the-crux.
3. **Parked collaborative-space idea:** Ace's synchronous shared workspace is
   the model junto rejected; junto's version (if built) is the **async
   versioned Artifact** in the single pane. (Parked 2026-06-13; design forks in
   the `junto-dev` ledger.)
4. **Complementarity, not just rivalry:** Ace's context/playbook layer and
   junto's verified-record layer could **compose** — an adapter boundary, not a
   head-to-head everywhere.

## Sources

- [Ace MCP integration docs](https://docs.aceagent.io/docs/developer-guides/mcp-integration/overview)
- [Ace docs home](https://docs.aceagent.io)
- [Maggie Appleton — "Zero Alignment"](https://maggieappleton.com/zero-alignment/)
- junto: [`junto.md`](junto.md) · [`domain-model.md`](domain-model.md) · [`self-improving-harness.md`](self-improving-harness.md) · [`attention.md`](attention.md)
