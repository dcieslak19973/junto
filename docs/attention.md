# Attention — the human surface's organizing principle

> Status tags as in the rest of the corpus: **✅ settled · 🔵 proposed · ⚠️ open.**
> Decided in the junto-dev ledger (entry `bfbdb8ed` and the 2026-06-10 design
> session); this doc folds the principle, its research grounding, and the
> anticipated mechanics into one place.

## ✅ The principle: the surface is an attention router

The human's role in junto's loop is to **deliberate and verify** — everything
else on the surface exists to serve that act. So the surface's job is to
*focus the human's attention on the important things*, not to display a
chronological log. History, filters, and full-ledger views are secondary
views behind the attention-first one.

Two layers, matching the kernel ↔ playbook seam (CLAUDE.md constraint #5):

1. **Kernel-generic attention** (buildable now) — acts awaiting a member, in
   urgency order:
   - **pending gates** first: an agent proposed an action and is *blocked*
     on a human's approve/reject;
   - **provisional assertions**: unverified record accumulating —
     verification debt;
   - **anomalies**: unrecognized authors (`adr/0017`), sync failures.
2. **Playbook-contributed attention** (anticipated) — each workflow type
   creates its own things-needing-attention: a code-PR playbook says *a PR
   needs review*; a research playbook says *a hypothesis has new evidence to
   weigh*; a troubleshooting playbook says *an incident needs a decision*.
   These feed the **same surface** when Playbooks exist.

The **attention item shape is stable across both layers**: what it is, whom
it awaits, why it matters, where to act — and the act available inline. Build
the surface once for the kernel's items; playbooks plug into it later (rule
of three: the kernel is contributor #1, so no framework yet).

🔵 Attention is ultimately **member-scoped** — "the important things *for
them*". Which gates await *you* is gate-routing/roles territory
(`adr/0007`); dogfood-era, the board is simply the machine user's.

## ✅ Multihuman + multiagent from the start

junto is a **multihuman-multiagent collaborative environment** — the solo
wedge is a Party of one human plus their agents, not the design ceiling.
The attention design must generalize without rework:

- **Each member has their own board.** Attention routing follows gate
  routing: when a playbook says *this gate needs a human approver from set
  X*, the item lands on those members' boards — and on no one else's.
- **"Waiting on you" vs "waiting on others"** are different facts and render
  differently: my board leads with what blocks *on me*; what I'm waiting
  *for* (a teammate's approval, an agent mid-task) is visible but quiet.
- **Agents are members with attention too.** An agent blocked on a pending
  gate is the canonical highest-urgency item on the *human's* board — and
  symmetrically, the surface can show what each agent is currently waiting
  on, so a stalled fleet is diagnosable at a glance.
- **The Party's load is eventually visible** (⚠️ open: how much, to whom):
  if every gate routes to the one architect, the surface is where that
  bottleneck becomes obvious. Side-quests distribute across the Party — a
  fork can be *someone else's* inquiry from birth.
- The personal-optimum measurement (below) is per-member by construction —
  it must never become a comparative leaderboard (⚠️ note: a measurement
  that affects people changes behavior; this stays a private mirror, not a
  management report).

## ✅ Not a queue — a focus board (what the research says)

A flat queue implies a linear list you drain to zero. Real work is several
inquiries in flight at once, and the research is specific about what that
costs and how a surface should respect it:

- **Switching is the cost, not the items.** "Attention residue" (Leroy 2009,
  *Organizational Behavior and Human Decision Processes*): switching tasks
  leaves residual cognition on the prior task that degrades performance on
  the next — worst when the prior task is unfinished. Interruption studies
  add that knowledge workers take roughly 25 minutes to fully re-engage
  after a switch.
  ⮕ *Design:* group attention items **by inquiry (channel)**, everything for
  one inquiry handleable in one pass; never interleave inquiries in a flat
  list. Closure reduces residue, so "this inquiry is now clear" is a visible
  moment, not a silent state change.
- **Switching between *similar* tasks produces less residue than between
  dissimilar ones** (same literature).
  ⮕ *Design:* a side-quest groups **under its parent** — parent ↔ side-quest
  switches are the cheap kind, and the board nesting them is cognitively
  honest, not just tidy.
- **Work-in-progress is load-bearing.** Little's-law reasoning and the
  classic ~20%-productivity-cost-per-additional-project observation
  (Weinberg): more in-flight inquiries means everything finishes slower.
  ⮕ *Design:* show the **count and weight of in-flight inquiries**, not just
  items; offer ways to retire WIP (park/close a channel — the lifecycle acts
  `adr/0016` anticipates).
- **Capacity genuinely varies per person — and self-assessment is
  unreliable.** "Supertaskers" exist but are ~2.5% of people (Watson &
  Strayer); working-memory capacity predicts multitasking performance;
  notably, *preferring* to multitask (polychronicity) does **not** predict
  being good at it.
  ⮕ *Design:* junto does **not** enforce a WIP limit; it makes load visible
  and lets ordering do the nudging. Limits, if any, are personal and
  discovered (below), never hardcoded.

Citations:
- Leroy, S. (2009). *Why is it so hard to do my work? The challenge of
  attention residue when switching between work tasks.* OBHDP 109(2).
  <https://www.sciencedirect.com/science/article/abs/pii/S0749597809000399>
  (author's summary: <https://www.uwb.edu/business/faculty/sophie-leroy/attention-residue>)
- Watson, J. M., & Strayer, D. L. (2010). *Supertaskers: Profiles in
  extraordinary multitasking ability.* Psychonomic Bulletin & Review 17(4).
  <https://link.springer.com/article/10.3758/PBR.17.4.479>
- Redick et al. — working memory capacity predicts effective multitasking.
  <https://www.sciencedirect.com/science/article/abs/pii/S0747563217304752>
- Individual differences in everyday multitasking and its relation to
  cognition and personality (review, 2022).
  <https://link.springer.com/article/10.1007/s00426-022-01700-z>
- WIP limits / Little's law applied to knowledge work (practitioner
  syntheses): <https://6sigma.com/multi-tasking-leads-to-lower-productivity/>,
  <https://roadmap.one/blog/posts/blog21-limit-work-in-progress/>

## 🔵 The focus board (the buildable kernel slice)

The index leads with a **needs-you board** above the channel cards:

- **Grouped by inquiry**, ordered by urgency: channels with blocking gates
  first, then channels with only verification debt; within a group, gates
  before provisional assertions, then anomalies.
- **Inline acts**: approve/reject and ratify/park forms on each item — act
  from the board without entering the channel. The member code is remembered
  after first successful use (machine-local; the code stays accident-
  proofing, `adr/0017`).
- **A load line**: "N inquiries in flight need you" — WIP made visible.
- **A cleared state per inquiry** — the closure moment the residue research
  asks for.
- Channel pages get the same items as an attention strip above the ledger.

## 🔵 Side-quests (forking) — the supply side of attention

The focus board shows the load; **side-quests are how load gets honest**.
A tangent mid-inquiry ("the UI looks barebones") becomes a *forked child
channel*: its own ledger and gates, pursued, then **closed with its outcome
recorded back into the parent** (an entry citing the child — record-style,
not a merge). The parent ledger stays the spine; tangents stop polluting it
and become *retirable as units* — a dead side-quest parks whole, instead of
leaving orphan provisional entries scattered in the main channel.

Mechanics anticipated by `adr/0016` (fork/close as future lifecycle entry
kinds, decided when concrete):

- the child's genesis-adjacent entry records *forked from parent, at this
  point*; the parent gets a matching entry — both ledgers carry the
  relationship with provenance both ways;
- the child's **Outcome** (`domain-model.md`) is concrete: merged-back or
  parked;
- the focus board nests side-quests under their parent (shallow tree).

⚠️ Open when forking is designed: does the child inherit the parent's Party
at fork time (likely: yes, forker = founder)? Does a checkout bound to the
parent get briefs for open side-quests? Are closed side-quests shown
anywhere by default?

## Prior art (assessed 2026-06-10)

Four lineages, each validating a piece of the design — and one gap none of
them fill:

1. **Academic attention management — Microsoft Research's AUI project
   (1998–2003).** Horvitz et al. treated human attention as the central
   organizing construct of computing: *Priorities* ranked email by inferred
   urgency; the *Notification Platform* modeled the cost of interrupting you
   right now (Bayesian, sensor-driven).
   <http://erichorvitz.com/cacm-attention.htm>,
   <https://www.microsoft.com/en-us/research/publication/attention-sensitive-alerting/>.
   The intellectual ancestor of the personal-optimum idea — but they
   *inferred* attention from sensors, top-down; junto can *measure* decision
   latency from its own ledger, bottom-up.
2. **Task-focused interfaces — Eclipse Mylyn (Kersten, 2005; later
   Tasktop).** Gave every artifact a degree-of-interest relative to the
   *active task* and filtered the whole IDE around it — proof that
   organizing the surface around the unit of work measurably helps.
   <https://en.wikipedia.org/wiki/Task-focused_interface>. The ancestor of
   group-by-inquiry and side-quests; it never escaped the IDE.
3. **Dev-work triage inboxes.** GitHub notifications and the repair
   ecosystem they spawned (Octobox, Graphite's PR inbox with its
   needs-your-review / waiting-on-others split, Linear Triage, Gmail
   Priority Inbox before them all). They validate the demand — and they are
   all *notification streams*: items vanish when dismissed, nothing durable
   remains, each covers one tool.
4. **Agent command centers (the contemporary lane).** Devin Desktop's
   kanban-style Agent Command Center ("plan, delegate, review, ship from one
   surface") and its PR-review inbox; OpenAI Codex's and Cursor's dashboards
   of agent runs awaiting review.
   <https://www.fixedlabs.ai/blog/devin-desktop-review>,
   <https://cognition.ai/blog/devin-101-automatic-pr-reviews-with-the-devin-api>.
   They have the agents-in-flight framing — for one vendor's agents, on one
   vendor's servers, for code only.

**The gap junto fills:** in every lineage above, the attention item is
ephemeral — a notification to dismiss, a session to close. In junto, acting
on an attention item **writes a provenance-bound entry into a durable record
the Party owns** (rationale, identity, gate semantics, on a vendor-neutral
substrate) — and the items come from *any* playbook, addressed to *specific
members*, human or agent. The dashboards above are panes of glass; the focus
board is a pane of glass **over a record**.

### Lessons from their failures and successes

1. **Be the place attention goes, not a predictor of when to interrupt**
   (AUI's fate). Horvitz's systems inferred attention from sensors and tried
   to time interruptions — invasive, brittle, never shipped broadly. A
   pull surface the human visits at natural boundaries needs no inference,
   and matches the interruption research besides.
2. **The unit-of-work signal must be zero-effort** (Mylyn's fate). Mylyn's
   measured productivity gains depended on users manually activating "the
   task I'm on" — that friction capped adoption. In junto the bookkeeping is
   done by the *agents* (channel binding, recorded entries); the human never
   declares context.
3. **Platform-neutral or die with the platform** (Mylyn/Tasktop again). The
   task context lived inside Eclipse; when developers moved editors, it
   couldn't follow. junto's record is git refs, its surface is HTTP — no
   host platform to die with.
4. **Items must be act-shaped, and acting must *resolve* them in the system
   of record** (GitHub notifications' fate). A notification whose only verb
   is *dismiss* refills forever; volume without semantics buries the user;
   and third-party repairs (Octobox) are stuck mirroring a record they
   cannot change. The focus board's items each carry their act inline, and
   the act *is* a state change in the record itself.
5. **Semantics beat inference.** Gmail's Priority Inbox guesses importance
   from an adversarial stream and gets gamed. junto's urgency is derived
   from gate/standing *semantics* — a pending gate blocks an agent, by
   definition, no model required.
6. **The unit is the inquiry, not the agent** — and plan for verification
   asymmetry (the agent-command-center risk). A kanban of agent sessions
   makes the fleet the subject; junto already chose the Channel as the unit
   with Agent Sessions subordinate. Deeper: agents produce verifiable work
   far faster than humans can verify it, so an attention surface that
   assumes every item deserves human eyes becomes a slop inbox. The escape
   is already in the design — gate routing (`adr/0004`/`0007`) lets
   playbooks route routine verification to evals and agent verifiers,
   reserving human attention for consequential gates. The board must make
   that routing visible rather than pretend humans scale.

## ⚠️ Finding a person's optimum (anticipated, not designed)

junto's ledger records what no productivity tool can see from the outside:
when a gate opened and when *you* acted on it, how many inquiries were in
flight at that moment, and how long inquiries take end-to-end. Over time
that is enough to **estimate a person's own optimal concurrency
empirically** — e.g., correlate in-flight count against verification latency
and inquiry cycle time (Little's law, per person) and surface it gently:
*"you clear gates ~2× faster when ≤3 inquiries are in flight."*

Connections and cautions:

- This is the **self-improvement playbook's** territory
  (`self-improving-harness.md`): a measured claim about the process, entering
  the ledger as an assertion with the data as provenance — never an
  auto-enforced limit.
- Per the individual-differences research above, the point is *personal*
  measurement precisely because population averages and self-assessment both
  mislead.
- Privacy posture: the analysis is machine-local (the same trust domain as
  the record itself); nothing leaves the substrate the person already owns.
- Not designed further until the focus board has produced real usage data to
  analyze.
