# Design — Mid-Turn Steer (interruptable agent turns)

> Status: design, awaiting review. Topic: let a human **interrupt a running agent turn**
> and **redirect it in place** — the Claude-Code "ESC-then-type" experience — over ACP.
> Builds on ADR 0023 (Agent Sessions + live feed) and ADR 0024 (ACP is the harness protocol).
> Extends both; will land its own ADR.

## 1. The problem, precisely

junto drives a coding-agent harness over ACP (`crates/junto/src/acp.rs`). A turn is one linear
`async` block under a single `TURN_TIMEOUT`: `initialize → session/new` (or `session/load`) `→
set_mode → session/prompt → pump_until` blocks reading the `session/update` stream until the prompt
resolves. **Nothing concurrently listens for human input while a turn runs** — the only early exit is
the timeout, which drops the future and `kill_on_drop` reaps the adapter.

A `steer()` already exists (`launch.rs:1298`), but it is **between-turns** only: it records the
human's instruction as a `SessionUpdated` note, flips the session to `Working`, and `spawn_turn`s a
fresh turn with `resume = Some(harness_session)` → `session/load`. It is designed to redirect a
session *after its current turn has landed*.

The gap — and the thing that makes junto's agentic UX feel worse than Claude Code — is that there is
**no way to interrupt a turn mid-flight**. Worse, doing so today is *racy, not merely absent*:
`steer_session` (`web.rs:688`) has no guard against a running turn, so steering mid-turn
`tokio::spawn`s a **second** turn that calls `live.begin()` (replacing the live feed) and
`session/load`s the same harness session in a second adapter process against the same workspace —
two concurrent turns editing the same tree.

**Goal:** a human watching the live feed can interrupt the running turn and type a redirection; the
turn stops what it's doing and continues, in the same session, with the new instruction — no waiting
for the turn to finish or time out, and no colliding second turn.

## 2. What already exists (reuse, don't rebuild)

The human-facing scaffold is complete; the missing piece is narrow and deep.

- **Steer box** — `POST /channels/{channel}/sessions/{session}/steer` → `steer_session` (`web.rs:688`).
- **Live tail** — `GET /channels/{channel}/sessions/{session}/stream` → `stream_session` SSE
  (`web.rs:768`), driven by `LiveSessions` (`launch.rs:642`).
- **`LiveSessions`** — per-session in-memory feed: a bounded replay `buffer` + a `broadcast` sender
  (host → human). Keyed by the junto Agent Session `EntryId`. Ephemeral, never the record.
- **Between-turns resume** — `launch::steer` + `spawn_turn` + `run_turn_acp(resume = …)` via
  `session/load`. Records the `SessionUpdated` steer note (ADR 0023).
- **`TurnOutcome`** — `{ result, harness_session, failed }` (`acp.rs`), persisted by `record_outcome`.

The one thing missing: a **reverse channel** (human → running turn) and a turn driver that can act on
it. Everything else is plumbing we already own.

## 3. Architecture — one new primitive, two phases

### 3.1 The new primitive: a per-session control channel

`LiveSessions` is one-directional today (host → human via `broadcast`). Add the missing direction.

```rust
/// A human's mid-turn signal to a running turn.
enum TurnControl {
    /// Stop the current prompt; end the turn (Phase 1).
    Interrupt,
    /// Stop the current prompt and re-prompt in place with this text (Phase 2).
    Steer(String),
}
```

`LiveFeed` gains a control sender alongside its `broadcast` sender. A single-slot/latest-wins channel
fits (one human, one in-flight signal): a `tokio::sync::mpsc` (capacity 1) or a `watch`. **`mpsc`
(capacity 1)** is the choice — a steer is a discrete delivered message, not a latched state, and we
want "no signal" to be the absence of a message, not a sentinel.

```rust
struct LiveFeed {
    buffer: Vec<LiveEvent>,
    sender: broadcast::Sender<LiveEvent>,
    control: mpsc::Sender<TurnControl>,        // human → turn
}
```

- `begin(session)` creates the control pair and **returns / stores the receiver** so the turn driver
  can `select!` on it. (Shape: `begin` returns the `mpsc::Receiver<TurnControl>`, threaded into
  `run_turn` → `run_turn_acp`. The receiver is owned by the running turn, not the registry.)
- New `LiveSessions::control(session, TurnControl) -> Result<(), NotLive>` — the web handler's entry
  point; `Err` means no turn is currently live (caller falls back to between-turns resume).
- `finish(session)` drops both senders (unchanged semantics; live tail sees stream end).

This is also the normalized seam a future `AgentHarnessAdapter` converges on (the ADR-0023 comment
already anticipates this for the event shape; control is its mirror).

### 3.2 Phase 1 — in-session cancel (graceful)

Restructure **only the prompt step** of `run_turn_acp` (`acp.rs:207-221`). Instead of `pump_until`
blocking solely on the line reader, race the stream pump against the control receiver:

```text
select! {
    result = pump_prompt_stream(...) => return outcome(result),   // normal completion
    ctl    = control.recv()          => handle_control(ctl),       // human interrupted
}
```

On `Interrupt` (or `Steer`, in Phase 2): send the ACP `session/cancel` notification over stdin for
the live `sessionId`, then keep draining the reader until the prompt request resolves with
`stopReason == "cancelled"`. Return a `TurnOutcome` marked **interrupted** — a new state distinct
from `failed` (agent error) and the timeout kill:

```rust
enum TurnEnd { Completed, Interrupted, Failed, TimedOut }
```

`TurnOutcome` gains an `end: TurnEnd` field and the bare `failed: bool` is removed; existing callers
read `outcome.end != TurnEnd::Completed` where they previously checked `failed`. `record_outcome` and
the live `result` event branch on `end` (so an interrupted turn reads as interrupted, not failed).

**Why graceful cancel, not kill:** `session/cancel` lets the adapter tear down its own child tree
(adapter → Claude SDK → children). This sidesteps the Windows process-tree-kill problem CLAUDE.md
warns about (`kill_on_drop` reaps only the direct child, leaking grandchildren on Windows). The kill
path remains for the timeout/error cases, but graceful cancel becomes the common interrupt path.

**Web (Phase 1):** an interrupt button on the live card → `POST .../interrupt` →
`LiveSessions::control(session, Interrupt)`. The turn ends; the feed closes; the card reloads to the
landed (interrupted) outcome.

### 3.3 Phase 2 — in-session re-prompt (the steer)

`run_turn_acp` becomes a **steerable loop** around the prompt step. On `Steer(message)`: after the
`session/cancel` drains to `cancelled`, send a **new** `session/prompt` with `message` into the
**same live process and ACP session** and continue pumping — no reload, no second process. Loop until
a prompt completes normally (`end_turn`) or a bare `Interrupt`/timeout ends it.

```text
loop {
    issue session/prompt(text)
    select pump vs control:
        completed(end_turn)        -> break (Completed)
        control = Interrupt        -> cancel, drain, break (Interrupted)
        control = Steer(next)      -> cancel, drain; text = next; continue loop
}
```

Each in-session steer **records a `SessionUpdated` note** (ADR 0023 — the record keeps who steered
and what they said), exactly as today's between-turns `steer()` does. The transient redirection lives
in the feed; the durable steering lives in the ledger.

### 3.4 Data flow — one steer box, two paths

`steer_session` (`web.rs:688`) routes on **liveness**:

```text
POST .../steer { message }
  -> LiveSessions::control(session, Steer(message))
       Ok            -> in-session steer (turn is live)        [Phase 2]
       Err(NotLive)  -> launch::steer(...) resume path (turn landed)  [existing]
```

One box, works mid-turn *and* between-turns. This also **removes the current race**: a mid-turn steer
no longer spawns a colliding second turn — it is delivered to the running one. The `SessionUpdated`
note is recorded on both paths (in-session path records it before delivering control).

## 4. Three baked-in decisions

1. **Timeout is per-prompt, not per-session.** Each prompt (initial or post-steer) gets a fresh
   `TURN_TIMEOUT` budget — a steer is a new instruction, so it earns a new budget. The steerable
   loop resets the timeout around each `session/prompt`. (No overall session cap in v1; rule of
   three before adding one.)
2. **CLI fallback degrades to interrupt + resume.** `run_turn_cli` (`claude -p`, one-shot process)
   cannot be steered in place. Under the CLI backend, `Interrupt`/`Steer` kill the process and, for
   `Steer`, route through the existing `launch::steer` resume path. ACP is primary (ADR 0024); this
   degradation is acceptable and noted in the outcome.
3. **Verify the cancel contract FIRST.** Before building the loop, pin that the Claude ACP adapter
   (`@agentclientprotocol/claude-agent-acp`) honors `session/cancel` and resolves the in-flight
   `session/prompt` with `stopReason == "cancelled"`. This is the external risk; a probe test (or a
   recorded fixture of the wire exchange) gates the rest of the work.

## 5. Components touched

| File | Change |
|---|---|
| `crates/junto/src/launch.rs` | `TurnControl` enum; `LiveFeed.control` + `LiveSessions::{begin returns receiver, control()}`; thread the receiver through `spawn_turn` → `run_turn` → `run_turn_acp`; `TurnEnd` and `record_outcome` branch; CLI-fallback degradation. |
| `crates/junto/src/acp.rs` | `run_turn_acp`: prompt step becomes a `select!`'d, steerable loop; `session/cancel` send + drain-to-`cancelled`; per-prompt timeout; `TurnEnd`. |
| `crates/junto/src/web.rs` | `steer_session` routes on liveness (control vs resume); new `interrupt_session` handler + route; interrupt/steer affordance on the live card. |
| `docs/adr/` | New ADR extending 0023/0024: mid-turn interrupt + in-session steer; the control-channel seam; graceful-cancel rationale (cross-platform). |
| `docs/domain-model.md` | Note the *interrupted* turn end-state if it earns a glossary line. |

## 6. Testing

Cross-platform (Win + Mac) is first-class; the cancel path is exactly where ConPTY/openpty and
process-control differ, so both arms get tested.

**Phase 1 (cancel):**
- **Cancel contract probe** (gates everything): a `session/prompt` followed by `session/cancel`
  resolves with `stopReason == "cancelled"` against the real adapter (or a pinned wire fixture).
- Control channel: `interrupt` on a live session is delivered; `interrupt` on a non-live session
  returns `Err(NotLive)`.
- A running turn receiving `Interrupt` ends `Interrupted` (not `Failed`/`TimedOut`); the feed closes;
  the outcome records the interrupted end-state.
- No grandchild leak after a graceful cancel (no surviving adapter/Claude process) — asserted on both
  OSes.

**Phase 2 (steer):**
- Mid-turn `Steer(msg)`: the current prompt cancels and a new prompt with `msg` runs in the **same**
  process/session (assert no second adapter spawn; same `harness_session`).
- A `SessionUpdated` steer note is recorded for the in-session steer.
- `steer_session` routes correctly: live → control path; landed → existing `launch::steer` resume.
- Per-prompt timeout: a steer after a long first prompt gets a fresh budget.
- The pre-existing race is gone: steering a live turn never spawns a concurrent second turn.

## 7. Out of scope (YAGNI)

- Multiple concurrent human steerers / steer queueing (one human, capacity-1 channel).
- An overall per-session time/turn cap (per-prompt timeout only; revisit on the rule of three).
- A full `AgentHarnessAdapter` extraction — the control channel is *shaped* to be that seam, but we
  build the concrete ACP case first (rule of three).
- Fully fixing Windows kill-tree for the timeout/error path — graceful cancel covers the common
  interrupt case; the kill-path grandchild leak is a separate, pre-existing concern.
- The MCP write-surface ceremony slice (connection-bound identity + softening the frame convention) —
  designed, deferred to its own spec per the agreed sequencing.
