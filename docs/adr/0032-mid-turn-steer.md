# Mid-turn steer (interruptable agent turns)

Status: accepted (Dan, 2026-06-19) — **implemented** · builds on [`0023`](0023-agent-sessions-live-feed.md), [`0024`](0024-acp-is-the-harness-protocol.md) · ledger `mid-turn-steer` channel (`a907e37a`)

junto drives a coding-agent harness over ACP (`crates/junto/src/acp.rs`). A turn was one linear exchange: `initialize → session/new` (or `session/load`) `→ set_mode → session/prompt → pump` the `session/update` stream until the prompt resolved. **Nothing watched for human input while a turn ran** — the only early exit was the turn timeout. A between-turns `steer` existed (record a `SessionUpdated` note, then `session/load` + re-prompt in a fresh process), but it only redirects a session *after its current turn lands*. Steering a *running* turn was not just absent — it was racy: the web handler spawned a **second** concurrent turn against the same harness session and workspace.

This is the gap that made junto's agentic UX feel worse than the harnesses it drives: you couldn't interrupt and redirect mid-stream the way you can in Claude Code (ESC-then-type).

## Decision — a control channel + a steerable prompt loop

**One new primitive: a per-session control channel** (human → running turn), the missing reverse direction of the live feed. `LiveSessions` already held a per-session `broadcast` sender (host → human); it now also holds an `mpsc::Sender<TurnControl>` (capacity 1 — one human, one in-flight signal). `begin` returns the receiver to the running turn; `control(session, signal)` delivers, or returns `NotLive` when no turn is running. `TurnControl` is `Interrupt | Steer(String)`.

**The turn driver is interruptable and steerable.** The prompt step `select!`s the update-stream pump against the control channel. On a signal it sends a graceful ACP **`session/cancel`** for the live session, drains to `stopReason == "cancelled"`, and then:

- **`Interrupt`** → the turn ends `Interrupted` (a new `TurnEnd` distinct from `Failed`/`TimedOut`; an interrupt is a human choice, so the session lands `Done`, not `Error`).
- **`Steer(message)`** → a **new `session/prompt` is issued into the same live process and ACP session** and the pump continues — no reload, no second process, full context preserved. The turn driver is a loop; each prompt gets a fresh timeout budget (a steer is a new instruction).

**Why graceful `session/cancel`, not kill:** the adapter tears down its own child tree (adapter → Claude SDK → children). This **sidesteps the Windows process-tree-kill problem** (`kill_on_drop` reaps only the direct child, leaking grandchildren on Windows) — the rare case where the better UX is also the better cross-platform engineering. The cancel contract was verified empirically against the Claude adapter before building on it (a throwaway probe; `session/cancel` → `stopReason: cancelled` confirmed).

**One steer box, routed on liveness.** The human surface keeps a single steer input. The POST routes server-side: if a turn is live it is steered in place (`steer_live` → control channel); if the turn has landed it falls back to the existing between-turns resume (`steer` → `session/load`). Both paths record the `SessionUpdated` steer note (`0023` — the record keeps who steered and what). This also removes the prior race: a mid-turn steer is delivered to the running turn, never spawned as a second one.

## Scope / degradations

- **CLI fallback** (`claude -p`, a one-shot process) cannot be steered in place: an interrupt ends the turn, and a steer routes through the resume path. ACP is primary (`0024`); this degradation is acceptable.
- **The autonomous outcome loop** (worker/grader turns) is not an interrupt target — only the interactive launch/steer path is. Those callers use an inert control channel.
- The control channel is *shaped* to be the seam a future `AgentHarnessAdapter` converges on, but only the concrete ACP case is built (rule of three).
- The timeout/error paths still rely on `kill_on_drop` (the pre-existing grandchild-leak concern on Windows); graceful cancel covers the common interrupt case, not those.

## Consequences

A human watching the live feed can stop a runaway turn or redirect it without waiting for it to finish — the interactive feel junto was missing. The durable record gains an `Interrupted` end-state and a steer note per redirection; the transient redirection lives only in the feed. The alternative considered and parked (ledger `a907e37a`): **S1 — cancel + resume in a fresh process**, which reuses the existing resume machinery with less restructuring but pays reload latency and risks `session/load` fidelity when cancelled mid-tool-call, and doesn't fix the Windows kill-tree path.
