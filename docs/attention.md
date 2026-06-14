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
- **Show who you are blocking — by name** (Dan, 2026-06-10). The reciprocal
  view: every gate knows its proposer, so an attention item can say
  *blocking: Claude Code for 3h; Sarah's side-quest queued behind it*. An
  item framed as a person (or agent) kept waiting is psychologically a
  different object than a row in a list — social accountability is one of
  the few forces that reliably beats inbox apathy, and it gives the
  "waiting on others" view its teeth: what I see as *waiting quietly* on my
  board is, symmetrically, *me on someone else's blocking line*. Derivable
  today from the proposer on the entry; gets sharper when routing can name
  the awaited member.
- **Agents are members with attention too.** An agent blocked on a pending
  gate is the canonical highest-urgency item on the *human's* board — and
  symmetrically, the surface can show what each agent is currently waiting
  on, so a stalled fleet is diagnosable at a glance.
- **The Party's load is eventually visible** (⚠️ open: how much, to whom):
  if every gate routes to the one architect, the surface is where that
  bottleneck becomes obvious. Side-quests distribute across the Party — a
  diverged channel can be *someone else's* inquiry from birth.
- The personal-optimum measurement (below) is per-member by construction —
  it must never become a comparative leaderboard (⚠️ note: a measurement
  that affects people changes behavior; this stays a private mirror, not a
  management report).

> 🔎 **External corroboration** (reviewed 2026-06-14, [`junto-dev` ledger
> entry `58902102`](https://www.youtube.com/watch?v=pmoDeA3RBZY)): Vincent's
> "doc factories" talk frames the engineer running agent swarms as a
> **factory manager** whose bottleneck is no longer tokens but *taste* and
> *brain space to keep an eye on all the sessions* — and answers "how do you
> manage 10+ agents?" with "how do you manage 10+ staff? It's the soft
> skills." That is the attention-router thesis from lived experience: at
> scale the scarce resource is the human's attention across many in-flight
> inquiries, which is exactly what this surface is built to route.

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

⚠️ **Honest scope note on the citations** (from the 2026-06-10 adversarial
review): Leroy's experiments use short lab tasks with forced mid-task
switches, and the ~25-minute figure concerns resuming interrupted *deep
work* — neither directly measures scanning a triage list, so group-by-
inquiry is a design judgment these findings *support*, not one they prove.
The ~20%-per-project figure is Weinberg's practitioner observation (1992),
not peer-reviewed. The supertasker/WMC work concerns *simultaneous*
dual-tasking on a seconds timescale, stretched here to project-level WIP.
And there is counter-literature (task variety aiding motivation,
interleaving aiding incubation) this doc does not survey. Suggestive, not
dispositive — held with appropriate looseness.

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

## 🔵 Decision frames — making verification worth doing

The attack on the rubber-stamping tension (below), inspired by the
multiple-choice-plus-your-own pattern agent harnesses use well (Dan,
2026-06-10): **the proposer frames the verification**. What makes the
pattern work is not the buttons — it is that the party requesting the
decision does the work of articulating the decision space.

- A `propose`/`record` may carry an optional **decision frame**: 2–4
  options, each mapping to an act (approve / reject / ratify / park) with a
  **drafted rationale** the verifier adopts by choosing (and may edit), plus
  **what to check first** — links to the diff, the CI run, the evidence —
  rendered inside the attention item. Free-text remains available always.
- ✅ **The full frame is recorded durably** (Dan, 2026-06-10) — including
  the options *not* chosen. The unchosen options are "alternatives
  considered" become structural rather than prose: exactly the richer shape
  `adr/0003` deferred until something proved the need. Lives as an optional
  field on the entry payload (a field, not a new kind — short ADR when
  built).
- Why it chips at rubber-stamping: blank-box friction makes "lgtm" the
  cheapest act; choosing between substantively different pre-drafted
  positions is low-friction *and* high-signal — the cheapest action now
  leaves a meaningful record. A lazy verifier can still always pick option
  one, but the floor rises, and the **override/edit rate is measurable** —
  a standing signal of whether an agent's framing is honest (self-
  improvement playbook food).
- Items without a frame fall back to the plain form; frame quality becomes
  part of what makes an agent a good collaborator.

**What makes a frame meaningful** (Dan's verdict on the first live frame,
2026-06-11: "the choices are not meaningful — maybe because they were
previously generated"):

- Options must be **substantively different positions about the claim** —
  tied to checkable evidence ("park if the canonical-bytes test wasn't
  extended"), never restatements of yes/no or claims about the mechanism
  itself.
- The decline option must be the **steelman** — the strongest concrete
  reason a reasonable verifier would park or reject. The proposer wants
  approval, so the natural failure mode is a strong accept option beside a
  strawman decline; a frame whose decline option the verifier would be
  embarrassed to choose is a biased frame.
- **Frames are priors, structurally.** Written at record time, they encode
  what the proposer anticipated; the verifier's real concern often emerges
  from what checking *finds*, which no pre-generated option contains. That
  is inherent — it is why the draft is editable and free-text always
  remains, and why persistent chosen-vs-drafted divergence (or persistent
  decline-option avoidance) indicts the framing, not the verifier.

## 🔵 Side-quests (divergence) — the supply side of attention

> Naming (Dan, 2026-06-10): the lifecycle pair is **diverge / converge** —
> symmetric, DAG-native, and free of loaded baggage: git's "fork" implies a
> *copy of history* (exactly wrong here), and "split" implies the parent
> halves or stops — when in fact the parent flows on while a new inquiry
> departs from a point. "Side-quest" is the friendly name for the common
> diverge-then-converge-back-to-parent pattern. (`adr/0016` said "fork" when
> anticipating the family; the verb is settled as diverge when designed.)

The focus board shows the load; **side-quests are how load gets honest**.
A tangent mid-inquiry ("the UI looks barebones") becomes a *diverged child
channel*: its own ledger and gates, pursued, then **closed with its outcome
recorded back into the parent** (an entry citing the child — record-style,
not a merge). The parent ledger stays the spine; tangents stop polluting it
and become *retirable as units* — a dead side-quest parks whole, instead of
leaving orphan provisional entries scattered in the main channel.

Mechanics anticipated by `adr/0016` (lifecycle entry kinds, decided when
concrete):

- the child's genesis-adjacent entry records *diverged from parent, at this
  point*; the parent gets a matching entry — both ledgers carry the
  relationship with provenance both ways;
- the child's **Deliverable** (`domain-model.md`) is concrete: converged-back or
  parked;
- the focus board nests side-quests under their parent (shallow tree).

### ⚠️ Convergence — channels form a DAG (Dan, 2026-06-10)

Divergence has a dual: **two channels may converge**. Entries are immutable
canonical bytes *including their channel id* (`adr/0002`/`adr/0008`), so
histories can never literally merge — convergence is therefore a **recorded
act, not a mutation** (a merge commit, never a rebase): either channel B
closes *into* A (a closing entry in B, a receiving entry in A, provenance
both ways), or both close into a new channel whose genesis names two
predecessors. Nothing moves on the substrate.

- Diverge and converge are edges of one **channel lineage DAG**; side-quests
  (diverge then converge back to parent) and independent convergence are the
  same algebra. The brief-inheritance question is thereby *one* mechanism —
  recall follows lineage edges with summarization — not two.
- Convergence **retires WIP** (two boards become one) but forces a ceremony
  with real content: the converging channel's pending gates cannot dangle in
  a closed channel — each must be resolved, explicitly re-proposed into the
  continuation (with provenance), or explicitly abandoned. Honest disposal
  of open questions, forced at exactly the right moment.
- **Naming**: "join" is taken — `domain-model.md` uses join/invite for Party
  membership — and "merge" drags git baggage about histories combining,
  precisely what does not happen. The pair is **diverge / converge**.
- When designed, `adr/0016`'s lifecycle family likely grows as one algebra:
  diverged / closed / converged.

⚠️ Open when divergence is designed: does the child inherit the parent's
Party at divergence time (likely: yes, the diverger = founder)? Does a
checkout bound to the parent get briefs for open side-quests? Are closed
side-quests shown anywhere by default? Who founds a convergence's
continuation channel, and is its Party the union of its predecessors'?

## Prior art (assessed 2026-06-10)

Five lineages, each validating a piece of the design — and one gap none of
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
5. **Practitioner evidence — the hand-rolled version (Zack Proser / WorkOS,
   2025–26).** Proser's "untethered" workflow is this document lived out,
   built from duct tape: a morning desk session setting up *verification
   gates* (lint/build checks, screenshot verification, CLAUDE.md rule
   enforcement), voice-dispatching tasks, then walking away while agents
   work; phone- and watch-sized check-ins through the day; an afternoon
   pass of review and merge approvals. His thesis is the personal-optimum
   section in a practitioner's words — *"the agents scale infinitely, but
   your nervous system doesn't"* — with a burnout warning attached
   (his estimate: ~18 months for fleet-running devs without intentional
   workflow design), and he reports a ~90% drop in perceived
   context-switching from *classifying and routing* signals instead of
   reading streams.
   <https://www.youtube.com/watch?v=so9l_MwS2yg>,
   <https://zackproser.com/blog/aie-london-untethered-productivity>.
   Validates the demand and the persona (the agent-heavy developer the
   wedge targets) from lived experience rather than a product pitch.

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
   with Sessions subordinate. Deeper: agents produce verifiable work
   far faster than humans can verify it, so an attention surface that
   assumes every item deserves human eyes becomes a slop inbox. The escape
   is already in the design — gate routing (`adr/0004`/`0007`) lets
   playbooks route routine verification to evals and agent verifiers,
   reserving human attention for consequential gates. The board must make
   that routing visible rather than pretend humans scale.
7. **The walk-away workflow already exists; what's missing is the record**
   (the practitioner lesson). Proser proves one human can keep a fleet
   shipping from a phone — by hand-building gates, signal filters, and
   review passes for himself. Every piece is ephemeral and single-player:
   the verification act leaves nothing durable behind, so a teammate (or a
   future agent, or Proser-in-six-months) inherits none of the rationale or
   the parked dead-ends. The focus board's job is to be that same
   glanceable check-in surface with the act *writing the record* — and
   decision frames are what make a phone-sized glance actionable: one
   meaningful choice instead of reconstructing context on a small screen.

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
- Practitioner corroboration: prior-art lineage 5 (Proser) is this section's
  thesis from the field — *"the agents scale infinitely, but your nervous
  system doesn't"* — including the cost of ignoring it (his burnout
  estimate), and a working example of the daily rhythm the board should
  serve: gates set up at the desk, glanced at from anywhere, cleared in
  batches at natural boundaries.
- Privacy posture: the analysis is machine-local (the same trust domain as
  the record itself); nothing leaves the substrate the person already owns.
- Not designed further until the focus board has produced real usage data to
  analyze.

## ⚠️ Known tensions (adversarial review, 2026-06-10)

Held openly so the next design round inherits the sharpened version:

1. **Verification asymmetry vs. the board's own content.** Lesson 6 says
   most verification must eventually route to evals/agent verifiers — yet
   the board's bulk is provisional assertions awaiting human ratify. The doc
   does not yet resolve whether verification debt is real debt (the board
   drowns at agent speed) or tolerable (then it should not render as
   urgent). Decision frames raise the floor; they do not resolve the
   asymmetry. **This is junto's instance of the industry's central unsolved
   problem** — representable here (verification is a first-class, measurable
   act) in a way it is nowhere else, which is the position to work it from.
2. **Cleared states cut both ways.** Closure reduces residue *and* imports
   inbox-zero psychology — processing-for-clearance is the rubber-stamp
   gradient. Mitigation: decision frames + measuring override rates; watch
   for ratify-latency collapsing to seconds.
3. **"Gates block agents" is currently aspirational.** Dogfood agents build
   optimistically and gates ratify retroactively (every slice 10–14 was
   built before approval). Optimistic execution is a *legitimate* policy
   (merge-then-validate is real engineering), but the urgency hierarchy
   must say which policy is in force — blocking vs. optimistic is a
   per-playbook knob to make explicit, and stale gates need an aging model.
4. **Member-scoped routing awaits the Rubric layer** (`adr/0007`). Today's
   requirements (`Count(N)`) address everyone — and an item addressed to
   everyone invites diffusion of responsibility in a multihuman Party.
   Until routing can say *whom* a gate awaits, multihuman boards are
   under-specified.
5. **The "private mirror" is posture, not enforcement.** Gate-open→act
   latency per member is computable from the shared record by anyone with
   the repo — that is the substrate working as designed, git-blame-like.
   The leaderboard risk is structural; naming it honestly beats pretending
   locality of analysis makes the data private.
6. **Our own lesson 3 indicts the current bridges.** The recall hook and
   convention file are Claude-Code-shaped (SessionStart, CLAUDE.md) —
   dogfood-era pragmatism that must stay quarantined behind the
   AgentHarnessAdapter seam, or junto dies with a host platform exactly the
   way Mylyn did.
7. **Pure pull may not survive multihuman SLAs.** The surviving triage
   successes are pull+push hybrids; a gate that sits unseen for days
   because nobody opened the app is a real failure mode. Notification
   affordances will eventually be needed — designed against lesson 1, not
   in ignorance of it.
8. **Side-quest recall sharding is load-bearing** (also flagged above):
   per-channel briefs + many small channels could fragment the decision
   memory the wedge exists to provide. Brief-inheritance across the
   lineage DAG must be designed *with* divergence, not after it.
