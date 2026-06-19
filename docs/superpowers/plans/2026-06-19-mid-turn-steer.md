# Mid-Turn Steer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a human interrupt a running agent turn and redirect it in place — the Claude-Code "ESC-then-type" experience — over ACP.

**Architecture:** Add the missing reverse direction to `LiveSessions` (a per-session control channel, human → running turn). Phase 1 makes a turn's prompt step cancellable via a graceful ACP `session/cancel`. Phase 2 turns the prompt step into a steerable loop that re-prompts in the same live process/session. The existing steer box, SSE live tail, `SessionUpdated`-note recording, and resume machinery are reused; one steer box routes on liveness.

**Tech Stack:** Rust 2024, tokio (`mpsc`, `broadcast`, `select!`, `timeout`), axum (web), ACP (newline-delimited JSON-RPC over the adapter's stdio).

**Spec:** `docs/superpowers/specs/2026-06-19-harness-mid-turn-steer-design.md` (commit 10c70da). Ledger decision: `mid-turn-steer` channel, entry `a907e37a`.

## Global Constraints

- Rust **edition 2024**, resolver 3; **MSRV 1.94**.
- This work is entirely in the **`crates/junto` binary crate** (`anyhow` for errors is fine here; no `thiserror` needed). **Do not touch `junto-kernel`** — no playbook/harness logic enters the kernel.
- **No `unwrap()`/`expect()`/`panic!` in non-test code.** Return `Result`/handle the `None`. `expect("reason")` only where truly unreachable, with a reason.
- **No vendor-name branching** in shared paths — branch on harness capability (`harness.id == "claude"` already gates the Claude-only `_meta`; `session/cancel` is standard ACP, vendor-neutral).
- **Cross-platform (Windows + macOS), tested on both.** The graceful-cancel path must not depend on Unix signals; never reach for `nix`/`kill(2)`. Process teardown stays via the adapter's own `session/cancel` for the interrupt path.
- **Comment the *why*** around the cancel handshake and the steerable loop (non-obvious control flow).
- Run commands with the **`rtk`** prefix. Green gate, in order, stop on first failure:
  `rtk cargo fmt --check; if ($?) { rtk cargo clippy --workspace --all-targets -- -D warnings; if ($?) { rtk cargo test --workspace } }`
- Commit messages end with: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/junto/src/launch.rs` | live registry, turn orchestration, outcome recording | `TurnControl` enum; `LiveFeed.control`; `LiveSessions::{begin → receiver, control()}`; `TurnEnd` replaces `failed`; `outcome_state` helper; thread receiver through `spawn_turn`→`run_turn`; `steer_live` + `record_steer_note`; CLI-fallback degradation |
| `crates/junto/src/acp.rs` | the ACP wire client / turn driver | `run_turn_acp` prompt step → cancellable, then steerable loop; `session/cancel` send + drain-to-`cancelled`; per-prompt timeout; consume the control receiver |
| `crates/junto/src/web.rs` | human surface routes | new `interrupt_session` route+handler; `steer_session` routes on liveness; interrupt/steer affordance on the live card |
| `docs/adr/` | settled decisions | new ADR extending 0023/0024 |

---

## Phase 0 — Gate on the cancel contract

### Task 0: Verify the ACP adapter honors `session/cancel`

**Files:**
- Create (throwaway): `crates/junto/examples/acp_cancel_probe.rs` *(delete after — or keep behind a doc note)*

**Interfaces:**
- Produces: documented confirmation that a `session/prompt` interrupted by a `session/cancel` notification resolves with `stopReason == "cancelled"` against `@agentclientprotocol/claude-agent-acp`. Everything downstream depends on this contract.

- [ ] **Step 1: Write a minimal probe** that spawns the adapter (`acp_adapter_command` shows the command shape), does `initialize` → `session/new` → `session/prompt` with a long-running instruction (e.g. "count slowly to 100, one number per line"), then after ~2s sends `{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":<id>}}` over stdin (note: `session/cancel` is a **notification** — no `id`), and prints the `stopReason` from the prompt's response.

- [ ] **Step 2: Run it** (requires Node + the adapter installed):

Run: `rtk cargo run -p junto --example acp_cancel_probe`
Expected: prints `stopReason: cancelled` (or the adapter's documented cancel reason).

- [ ] **Step 3: Record the result.** If `cancelled` is confirmed → proceed. If the adapter does **not** support `session/cancel`, STOP and surface to Dan: the design's graceful-cancel premise fails and we fall back to the S1 kill-and-resume path (the parked steelman in ledger entry `a907e37a`). Capture the actual stop reason string for use in Phase 1.

- [ ] **Step 4: Remove the probe** (or move its findings into the ADR), commit nothing or a doc note only.

---

## Phase 1 — In-session cancel

### Task 1: The control channel on `LiveSessions`

**Files:**
- Modify: `crates/junto/src/launch.rs` (the `// ---- live progress ----` region, ~602-690)
- Test: same file's `#[cfg(test)]` module (mirror `live_registry_replays_buffer_and_tails`, ~2603)

**Interfaces:**
- Produces:
  - `pub(crate) enum TurnControl { Interrupt, Steer(String) }`
  - `LiveFeed { buffer, sender, control: mpsc::Sender<TurnControl> }`
  - `LiveSessions::begin(&self, session: EntryId) -> mpsc::Receiver<TurnControl>` (was `-> ()`)
  - `LiveSessions::control(&self, session: EntryId, signal: TurnControl) -> Result<(), NotLive>` where `pub(crate) struct NotLive;`
  - `subscribe`, `publish`, `finish` unchanged.

- [ ] **Step 1: Write the failing test** in the test module:

```rust
#[tokio::test]
async fn control_channel_delivers_to_live_turn_and_errors_when_idle() {
    let live = LiveSessions::default();
    let session = EntryId::new();

    // No feed yet → control reports NotLive.
    assert!(live.control(session, TurnControl::Interrupt).is_err());

    let mut control_rx = live.begin(session);
    live.control(session, TurnControl::Steer("focus on the parser".into()))
        .expect("delivered to the live turn");
    match control_rx.recv().await {
        Some(TurnControl::Steer(msg)) => assert_eq!(msg, "focus on the parser"),
        other => panic!("expected steer, got {other:?}"),
    }

    // After finish, control is NotLive again.
    live.finish(session);
    assert!(live.control(session, TurnControl::Interrupt).is_err());
}
```

- [ ] **Step 2: Run it, verify it fails to compile** (`TurnControl`, `control`, new `begin` signature don't exist).

Run: `rtk cargo test -p junto control_channel_delivers`
Expected: FAIL (does not compile).

- [ ] **Step 3: Implement.** Add the enum (derive `Debug`), add `control` to `LiveFeed`, change `begin` to create the pair and return the receiver, add `control()` + `NotLive`:

```rust
/// A human's mid-turn signal to a running turn (the reverse of the live feed).
#[derive(Debug)]
pub(crate) enum TurnControl {
    /// Stop the current prompt and end the turn.
    Interrupt,
    /// Stop the current prompt and re-prompt in place with this text.
    Steer(String),
}

/// No turn is currently live for the session, so control could not be delivered.
#[derive(Debug)]
pub(crate) struct NotLive;

struct LiveFeed {
    buffer: Vec<LiveEvent>,
    sender: broadcast::Sender<LiveEvent>,
    control: mpsc::Sender<TurnControl>,
}
```

```rust
/// Open a fresh feed for a session about to run, returning the control
/// receiver the running turn selects on (human → turn). Replaces any stale feed.
fn begin(&self, session: EntryId) -> mpsc::Receiver<TurnControl> {
    let (sender, _rx) = broadcast::channel(256);
    // Capacity 1: one human, one in-flight signal at a time.
    let (control, control_rx) = mpsc::channel(1);
    let mut map = self.inner.lock().expect("live sessions registry lock");
    map.insert(session, LiveFeed { buffer: Vec::new(), sender, control });
    control_rx
}

/// Deliver a human's control signal to the running turn, or `Err(NotLive)`
/// if no turn is currently streaming for the session.
pub(crate) fn control(&self, session: EntryId, signal: TurnControl) -> Result<(), NotLive> {
    let map = self.inner.lock().expect("live sessions registry lock");
    let feed = map.get(&session).ok_or(NotLive)?;
    feed.control.try_send(signal).map_err(|_| NotLive)
}
```

Add `use tokio::sync::mpsc;` near the existing `use tokio::sync::broadcast;`.

- [ ] **Step 4: Fix the one existing caller of `begin`** (`spawn_turn`, ~1375). For now bind the receiver and ignore it so it compiles: `let _control_rx = host.live().begin(session);` (Task 3 threads it through). Same for the test-only `live.begin(session)` at ~2610 — change to `let _ = live.begin(session);`.

- [ ] **Step 5: Run the test, verify it passes.**

Run: `rtk cargo test -p junto control_channel_delivers`
Expected: PASS.

- [ ] **Step 6: Green gate + commit.**

```bash
git add crates/junto/src/launch.rs
git commit -m "feat: add per-session control channel to LiveSessions

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `TurnEnd` replaces the `failed: bool`

**Files:**
- Modify: `crates/junto/src/launch.rs` (`TurnOutcome` ~799-806; every `failed:` construction site: ~843, 858, 957, 971, 1019, 1037; `record_outcome` mapping ~889-902)
- Modify: `crates/junto/src/acp.rs` (the two `TurnOutcome` constructions: success ~227-235, timeout ~243-249)
- Test: `launch.rs` test module (new pure-helper test)

**Interfaces:**
- Produces:
  - `pub(crate) enum TurnEnd { Completed, Interrupted, Failed, TimedOut }` (derive `Debug, Clone, Copy, PartialEq, Eq`)
  - `TurnOutcome { result: String, harness_session: Option<String>, end: TurnEnd }` (`failed` removed)
  - `fn outcome_state(end: TurnEnd, turn: u32, result: &str) -> (SessionState, String)` (extracted from `record_outcome`)
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test** for the extracted mapping:

```rust
#[test]
fn outcome_state_maps_each_turn_end() {
    use SessionState::*;
    assert!(matches!(outcome_state(TurnEnd::Completed, 1, "ok"), (Done, _)));
    assert!(matches!(outcome_state(TurnEnd::Failed, 1, "boom"), (Error, _)));
    assert!(matches!(outcome_state(TurnEnd::TimedOut, 1, "slow"), (Error, _)));
    // An interrupt is a human choice, not an error: the session lands Done.
    let (state, note) = outcome_state(TurnEnd::Interrupted, 2, "stopped mid-edit");
    assert_eq!(state, Done);
    assert!(note.contains("interrupted"));
}
```

- [ ] **Step 2: Run it, verify it fails to compile** (`TurnEnd`, `outcome_state` missing).

Run: `rtk cargo test -p junto outcome_state_maps`
Expected: FAIL (does not compile).

- [ ] **Step 3: Implement.** Add `TurnEnd`; change `TurnOutcome.failed` → `end: TurnEnd`. Replace every `failed: true` with the right variant: ACP setup/CLI-missing/error paths → `TurnEnd::Failed`; the timeout path (`acp.rs` ~243, `launch.rs` ~1019) → `TurnEnd::TimedOut`; the CLI `failed: is_error || !exit_ok` (~1037) → `if is_error || !exit_ok { TurnEnd::Failed } else { TurnEnd::Completed }`; the ACP success path (~234) → `if stop != "end_turn" { TurnEnd::Failed } else { TurnEnd::Completed }`. Extract the mapping:

```rust
/// Map a finished turn's end-state to the session state + note recorded for it.
fn outcome_state(end: TurnEnd, turn: u32, result: &str) -> (SessionState, String) {
    match end {
        TurnEnd::Completed => (SessionState::Done, format!("turn {turn} complete: {}", snippet(result, 160))),
        TurnEnd::Interrupted => (SessionState::Done, format!("turn {turn} interrupted: {}", snippet(result, 160))),
        TurnEnd::Failed => (SessionState::Error, format!("turn {turn} failed: {}", snippet(result, 160))),
        TurnEnd::TimedOut => (SessionState::Error, format!("turn {turn} timed out: {}", snippet(result, 160))),
    }
}
```

Replace the inline `let (state, note) = if outcome.failed { … }` in `record_outcome` with `let (state, note) = outcome_state(outcome.end, turn, &outcome.result);`.

- [ ] **Step 4: Run the test + full build, verify pass.**

Run: `rtk cargo test -p junto outcome_state_maps; if ($?) { rtk cargo check -p junto }`
Expected: PASS, compiles (all `failed` references resolved).

- [ ] **Step 5: Green gate + commit.**

```bash
git add crates/junto/src/launch.rs crates/junto/src/acp.rs
git commit -m "refactor: replace TurnOutcome.failed with TurnEnd enum

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Make the ACP prompt step cancellable

**Files:**
- Modify: `crates/junto/src/acp.rs` (`run_turn_acp` signature + prompt step ~207-221; `pump_until` ~277-325)
- Modify: `crates/junto/src/launch.rs` (`run_turn` ~815-861 + `run_turn_cli` ~917 signatures; `spawn_turn` ~1364-1412 to pass the receiver)
- Test: `acp.rs` test module (pure helper for the cancel decision)

**Interfaces:**
- Consumes: `TurnControl`, `TurnEnd`, `LiveSessions::begin → receiver` (Tasks 1–2).
- Produces:
  - `run_turn_acp(..., control: &mut mpsc::Receiver<TurnControl>) -> Result<TurnOutcome>` (new last param)
  - `run_turn(..., control: &mut mpsc::Receiver<TurnControl>) -> TurnOutcome`
  - `run_turn_cli(..., control: &mut mpsc::Receiver<TurnControl>) -> TurnOutcome`
  - `pump_until` gains a `control: &mut mpsc::Receiver<TurnControl>` param and returns `PumpEnd { result: Value, interrupted_with: Option<TurnControl> }` (so the caller knows whether a steer arrived). Define `struct PumpEnd { result: Value, interrupted_with: Option<TurnControl> }`.

- [ ] **Step 1: Write the failing test** for a small pure helper that decides what to do with a drained cancel. Add to `acp.rs`:

```rust
/// Classify how a prompt that drained to a stop reason ended, given whether a
/// human control signal interrupted it. Pure — the wire I/O is tested by the
/// Task 0 probe + dogfood run.
fn classify_prompt_end(stop_reason: &str, interrupted: bool) -> TurnEnd {
    if interrupted { TurnEnd::Interrupted }
    else if stop_reason == "end_turn" { TurnEnd::Completed }
    else { TurnEnd::Failed }
}
```

Test:

```rust
#[test]
fn classify_prompt_end_distinguishes_interrupt_from_failure() {
    assert_eq!(classify_prompt_end("end_turn", false), TurnEnd::Completed);
    assert_eq!(classify_prompt_end("refusal", false), TurnEnd::Failed);
    // A human interrupt wins regardless of the adapter's stop reason.
    assert_eq!(classify_prompt_end("cancelled", true), TurnEnd::Interrupted);
    assert_eq!(classify_prompt_end("end_turn", true), TurnEnd::Interrupted);
}
```

- [ ] **Step 2: Run it, verify it fails to compile.**

Run: `rtk cargo test -p junto classify_prompt_end`
Expected: FAIL (does not compile).

- [ ] **Step 3: Implement the cancellable pump.** Make `pump_until` `select!` between the line reader and the control receiver. On the first control signal: send `session/cancel` once, remember the signal, and keep reading until `awaited_id` resolves; return `PumpEnd { result, interrupted_with }`. The `session/cancel` needs the live `sessionId`, so pass it in (add `session_id: &str` param). Sketch:

```rust
async fn pump_until(
    reader: &mut Lines<BufReader<ChildStdout>>,
    stdin: &mut ChildStdin,
    awaited_id: i64,
    session_id: &str,
    live: &LiveSessions,
    session: EntryId,
    answer: &mut String,
    control: &mut mpsc::Receiver<TurnControl>,
) -> Result<PumpEnd> {
    let mut pending = String::new();
    let mut interrupted_with: Option<TurnControl> = None;
    loop {
        let line = tokio::select! {
            // Once interrupted we stop watching control and just drain to the response.
            ctl = control.recv(), if interrupted_with.is_none() => {
                // Notification: no id. Cancels the in-flight prompt for this session.
                write_message(stdin, &json!({
                    "jsonrpc": "2.0", "method": "session/cancel",
                    "params": { "sessionId": session_id }
                })).await?;
                interrupted_with = ctl; // None means the human dropped; treat as interrupt below
                continue;
            }
            line = reader.next_line() => line.context("reading ACP output")?,
        };
        let Some(line) = line else {
            bail!("ACP adapter closed before responding to request {awaited_id}");
        };
        // ... existing parse/match body, but on the awaited-id response return:
        //     return Ok(PumpEnd { result, interrupted_with });
    }
}
```

Update the prompt-step caller (step 4 of the exchange, ~207-221) to use the new return and set the outcome via `classify_prompt_end`. For the **non-prompt** `pump_until` calls (initialize/session-new/set_mode, ~148/162/178/203) pass the control receiver but those steps complete fast; keep them returning the result via `?` and `.result`.

- [ ] **Step 4: Thread the receiver through `run_turn` / `run_turn_acp` / `run_turn_cli`.** Add the `control` param to each. In `run_turn_cli`, wire a minimal interrupt: `select!` the line read against `control.recv()`, and on any signal kill the child and return `TurnOutcome { end: TurnEnd::Interrupted, .. }` (steer-in-place is ACP-only; CLI re-prompt is handled at the `launch::steer` resume layer — Phase 2 Task 6).

- [ ] **Step 5: Pass the receiver from `spawn_turn`.** Change `let _control_rx = host.live().begin(session);` → `let mut control_rx = host.live().begin(session);` and pass `&mut control_rx` into `run_turn(...)`.

- [ ] **Step 6: Run the unit test + build.**

Run: `rtk cargo test -p junto classify_prompt_end; if ($?) { rtk cargo check -p junto }`
Expected: PASS, compiles.

- [ ] **Step 7: Green gate + commit.**

```bash
git add crates/junto/src/acp.rs crates/junto/src/launch.rs
git commit -m "feat: make the ACP prompt step cancellable via session/cancel

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Web — the interrupt affordance

**Files:**
- Modify: `crates/junto/src/web.rs` (router ~52-59; new handler near `steer_session` ~688; the live card template where the steer form / `EventSource` live)
- Test: `web.rs` test module (handler returns the right status for live vs idle)

**Interfaces:**
- Consumes: `LiveSessions::control` + `TurnControl::Interrupt`.
- Produces: route `POST /channels/{channel}/sessions/{session}/interrupt` → `interrupt_session`.

- [ ] **Step 1: Write the failing test.** Mirror an existing `web.rs` handler test: a session with no live feed → interrupting returns a 4xx/redirect-with-notice (NotLive), and with `host.live().begin(session)` → returns success/redirect. (Follow the exact assertion style already used for `steer_session` tests in the module; if none exist, assert on the `Response` status.)

- [ ] **Step 2: Run it, verify it fails** (route/handler missing).

Run: `rtk cargo test -p junto interrupt_session`
Expected: FAIL.

- [ ] **Step 3: Implement** the handler (parse `session` as `EntryId`, project the channel like `steer_session`, authorize the human, then `host.live().control(session, TurnControl::Interrupt)`; map `Err(NotLive)` to a "no running turn to interrupt" `BAD_REQUEST`, `Ok` to `Redirect::to("/channels/{id}")`). Add the route. Add an **Interrupt** button to the live card next to the steer box (a `POST` form to `.../interrupt`).

- [ ] **Step 4: Run the test, verify pass.**

Run: `rtk cargo test -p junto interrupt_session`
Expected: PASS.

- [ ] **Step 5: Green gate + commit.**

```bash
git add crates/junto/src/web.rs
git commit -m "feat: interrupt a running turn from the live card

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

- [ ] **Step 6: Manual dogfood checkpoint (Phase 1 done).** Rebuild + restart the host (`rtk cargo run -p junto -- serve`), launch a session that does something slow, click **Interrupt**, confirm: the turn stops within ~1–2s, the card lands as *interrupted* (Done, interrupted note), and no adapter/Claude process survives (Task-0 graceful teardown). Verify on Windows **and** macOS.

---

## Phase 2 — In-session re-prompt (the steer)

### Task 5: The steerable loop in `run_turn_acp`

**Files:**
- Modify: `crates/junto/src/acp.rs` (`run_turn_acp` prompt step → loop; per-prompt timeout)
- Test: `acp.rs` test module (pure helper for the next-prompt decision)

**Interfaces:**
- Consumes: `PumpEnd.interrupted_with`, `classify_prompt_end`, `TurnControl`.
- Produces: a prompt loop where `PumpEnd.interrupted_with == Some(Steer(msg))` issues a new `session/prompt(msg)` into the same session and continues; `Some(Interrupt)`/`None` ends the turn `Interrupted`; a clean response ends per `classify_prompt_end`.

- [ ] **Step 1: Write the failing test** for the loop's decision helper:

```rust
/// Given how a prompt ended, decide the next step of the steerable loop.
enum LoopStep { Reprompt(String), Done(TurnEnd) }

fn next_loop_step(stop_reason: &str, interrupted_with: Option<TurnControl>) -> LoopStep {
    match interrupted_with {
        Some(TurnControl::Steer(msg)) => LoopStep::Reprompt(msg),
        Some(TurnControl::Interrupt) | None if /* was interrupted */ true => unreachable!(),
        _ => LoopStep::Done(classify_prompt_end(stop_reason, false)),
    }
}
```

*(Refine the signature while implementing so it's total; the test below pins the behavior — keep the helper pure.)*

```rust
#[test]
fn steer_reprompts_other_signals_end_the_turn() {
    match next_loop_step("cancelled", Some(TurnControl::Steer("do X instead".into()))) {
        LoopStep::Reprompt(m) => assert_eq!(m, "do X instead"),
        _ => panic!("steer should re-prompt"),
    }
    assert!(matches!(next_loop_step("cancelled", Some(TurnControl::Interrupt)), LoopStep::Done(TurnEnd::Interrupted)));
    assert!(matches!(next_loop_step("end_turn", None), LoopStep::Done(TurnEnd::Completed)));
}
```

- [ ] **Step 2: Run it, verify it fails to compile.**

Run: `rtk cargo test -p junto steer_reprompts`
Expected: FAIL.

- [ ] **Step 3: Implement** `next_loop_step` (total/pure), then wrap the prompt step of `run_turn_acp` in a loop: issue `session/prompt(text)` under a **per-prompt** `tokio::time::timeout(TURN_TIMEOUT, …)`; on `PumpEnd`, call `next_loop_step`; `Reprompt(msg)` → publish a `status` feed line ("steering: …"), set `text = msg`, loop; `Done(end)` → build the `TurnOutcome`. Keep the overall adapter process alive across iterations (do **not** drop `child`). Each timeout firing ends the loop with `TurnEnd::TimedOut`.

- [ ] **Step 4: Run the test + build.**

Run: `rtk cargo test -p junto steer_reprompts; if ($?) { rtk cargo check -p junto }`
Expected: PASS, compiles.

- [ ] **Step 5: Green gate + commit.**

```bash
git add crates/junto/src/acp.rs
git commit -m "feat: steerable prompt loop — re-prompt in the same ACP session

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Route the one steer box on liveness

**Files:**
- Modify: `crates/junto/src/launch.rs` (extract `record_steer_note`; add `steer_live`; keep `steer` for the resume path)
- Modify: `crates/junto/src/web.rs` (`steer_session` ~743-756 routes on liveness)
- Test: `launch.rs` test module (`record_steer_note` appends the right `SessionUpdated`)

**Interfaces:**
- Consumes: `LiveSessions::control` + `TurnControl::Steer`; existing `steer`.
- Produces:
  - `async fn record_steer_note(host, channel_ref, channel, session, steered_by, message) -> Result<()>` (the `SessionUpdated { Working, "steer: …" }` append, extracted from `steer`)
  - `async fn steer_live(host, channel, channel_ref, session, steered_by, message) -> Result<(), NotLive>` — record the note, then `live().control(session, Steer(message))`.

- [ ] **Step 1: Write the failing test** that `record_steer_note` appends a `SessionUpdated` with state `Working` and a note containing the message (project the channel after, assert the entry exists). Follow the existing append/projection test pattern in the module.

- [ ] **Step 2: Run it, verify it fails** (`record_steer_note` missing).

Run: `rtk cargo test -p junto record_steer_note`
Expected: FAIL.

- [ ] **Step 3: Implement.** Extract `record_steer_note` from `steer` (and call it inside `steer` so behavior is unchanged for the resume path). Add `steer_live`. In `web.rs` `steer_session`, replace the direct `launch::steer(...)` call with:

```rust
match crate::launch::steer_live(host.clone(), id, channel.clone(), session, author.clone(), message.clone()).await {
    Ok(()) => Redirect::to(&format!("/channels/{id}")).into_response(),         // delivered to the live turn
    Err(crate::launch::NotLive) => match crate::launch::steer(host.clone(), id, channel.clone(), workspace, session, author, message).await {
        Ok(()) => Redirect::to(&format!("/channels/{id}")).into_response(),     // turn had landed → resume
        Err(err) => (StatusCode::BAD_REQUEST, format!("{err:#}")).into_response(),
    },
}
```

- [ ] **Step 4: Run the test + build.**

Run: `rtk cargo test -p junto record_steer_note; if ($?) { rtk cargo check -p junto }`
Expected: PASS, compiles.

- [ ] **Step 5: Green gate + commit.**

```bash
git add crates/junto/src/launch.rs crates/junto/src/web.rs
git commit -m "feat: route the steer box on liveness (in-session vs resume)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: ADR + full verification

**Files:**
- Create: `docs/adr/0032-mid-turn-steer.md` (next free number — confirm against `docs/adr/README.md`)
- Modify: `docs/adr/README.md` (index entry)

- [ ] **Step 1: Write the ADR** extending 0023/0024: the control-channel seam, graceful `session/cancel` (cross-platform rationale), per-prompt timeout, the live-vs-resume routing, CLI degradation, and the parked S1 alternative (ledger `a907e37a`). Add the index line.

- [ ] **Step 2: Full green gate.**

Run: `rtk cargo fmt --check; if ($?) { rtk cargo clippy --workspace --all-targets -- -D warnings; if ($?) { rtk cargo test --workspace } }`
Expected: all PASS.

- [ ] **Step 3: Manual dogfood checkpoint (Phase 2 done).** Launch a slow session, type a redirection in the steer box mid-turn, submit. Confirm: the current prompt cancels, a new prompt with your text runs **in the same process** (no second adapter spawn — watch process list), the feed shows a "steering" line then new work, and a `SessionUpdated` steer note is in the record. Then steer a *landed* session and confirm it still resumes. Verify on Windows **and** macOS.

- [ ] **Step 4: Commit + open PR.**

```bash
git add docs/adr/0032-mid-turn-steer.md docs/adr/README.md
git commit -m "docs: ADR 0032 — mid-turn steer

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

- [ ] **Step 5: Record completion in the ledger.** `record` into the `mid-turn-steer` channel that the feature shipped (with the PR link as provenance), then `sync_channel`. Surface the PR to Dan for the gate/merge.

---

## Self-Review

**Spec coverage:** control channel → Task 1; phased cancel → Tasks 3–4; in-session re-prompt → Task 5; one-steer-box liveness routing → Task 6; per-prompt timeout → Task 5; CLI degradation → Task 3 (cli) + Task 6 (resume); verify-cancel-first → Task 0; `TurnEnd`/interrupted state → Task 2; race removal → Task 6; ADR + cross-platform tests → Task 7. All spec sections map to a task.

**Placeholder scan:** No "TBD"/"handle edge cases"/"similar to". Each code step shows code; the wire-loop steps that resist unit testing extract a pure helper that *is* unit-tested and defer the I/O to the Task-0 probe + dogfood checkpoints (an honest seam, not a placeholder).

**Type consistency:** `TurnControl{Interrupt,Steer(String)}`, `NotLive`, `TurnEnd{Completed,Interrupted,Failed,TimedOut}`, `PumpEnd{result,interrupted_with}`, `classify_prompt_end`, `next_loop_step`/`LoopStep`, `outcome_state`, `begin → mpsc::Receiver<TurnControl>`, `control() -> Result<(),NotLive>`, `record_steer_note`, `steer_live` — names used consistently across Tasks 1–7.
