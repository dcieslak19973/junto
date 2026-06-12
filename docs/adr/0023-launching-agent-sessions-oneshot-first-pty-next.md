# Launching Agent Sessions from the surface: oneshot-exec first, pty-drive next

Status: accepted (Dan, 2026-06-12 — grill session) · builds on [`0020`](0020-agent-sessions-and-artifacts-are-ledger-entries.md), `docs/pluggability.md` (AgentHarnessAdapter / ExecutionBackend)

The channel page gains the missing half of "interact with an agent": a launch box that spawns a real harness session, recorded live through the ADR 0020 entry kinds. Per the rule of three, **no trait is extracted yet** — v1 is Claude Code on the local backend, concretely; the `AgentHarnessAdapter` trait gets extracted when OpenCode (the designated second harness, via OpenRouter) lands.

## The Workspace — where a session runs

A channel's home substrate is *not* where work happens (the junto-dev record lives in a different repo than the code it describes). The **Workspace** is the machine-local mapping channel → repo(s), stored in `~/.junto/workspaces.toml` — set at first launch, prefilled thereafter. **Paths never enter the ledger**: they are machine facts and don't sync. The file stores a *list* of repos per channel so multi-repo inquiries are additive later; v1 reads exactly one, and it must be a git repo (diff capture leans on git).

## Invocation is a harness capability: `oneshot-exec` now, `pty-drive` next

- **`oneshot-exec` (v1):** launch = `claude -p "<composed prompt>" --output-format json` in the workspace; steering = `claude -p --resume <harness-session-id> "<message>"`, between turns. State lives in the harness's own session storage, **not** in a host child process — the host restarts constantly (every rebuild), and `--resume` makes that harmless. The junto-session → harness-session mapping is machine-local config, like the workspace.
- **`pty-drive` (expected soon after v1):** spawn the *interactive* harness in a `portable-pty`; junto relays human turns and captures output. Buys subscription-pool billing, mid-turn steering, and routes the harness's own permission prompts into junto's UI ("harness gates mechanics, junto gates outcomes" made literal). Costs: TUI screen-state parsing is the most brittle engineering in the project, and it must remain **human-driven** to stay clearly inside Anthropic's usage terms. Also eventually required for harnesses with no headless mode.

Both modes share everything else: workspace resolution, session entries, capture, the launch UI. The mode is a declared capability, never an `if vendor == …`.

## Steering is recorded, then transported

A human follow-up to a working session is appended as `SessionUpdated { state: working, note: "steer: <message>" }` **authored by the human**, then transported via `--resume`. The record keeps the steering (provenance of why the agent changed direction); pure-transport was rejected as a provenance hole. When the **Message/Conversation** noun lands (chat connectors will force it), steering migrates there and `SessionUpdated` returns to pure state changes — recorded here so that migration is anticipated, not a re-litigation.

## Permissions: the launched harness runs unattended

v1 launches with `--dangerously-skip-permissions` (Dan: "definitely yolo mode"). A headless session that stalls on an invisible prompt is worthless; the workspace is a git repo whose damage radius is a reset away; and junto's own gate layer — not the harness's prompt layer — is where consequential actions get approved. `pty-drive` later inverts this: harness prompts become junto approvals.

## Capture: artifacts on completion, never scrollback

On exit, the host attaches: the result text as a `memo` artifact, and `git diff` of the workspace (when dirty) as a `diff` artifact — then marks the session `done` (or `error`, with the output tail as a memo). No live token streaming in v1; the session card shows *state*. This is the "verifiable artifacts, never scrollback" invariant, not a shortcut.

## Death handling

A configurable per-session timeout (default 30 minutes) kills the process tree (the cross-platform spawn/timeout/kill wrapper CLAUDE.md mandates) and marks the session `error`. Host restarts orphan nothing: between turns there is no process to orphan.

## Economics (assessed 2026-06-12)

Headless `claude -p` is the officially sanctioned "third-party app built on the Agent SDK" category. From 2026-06-15 it bills against a **subscription-included monthly Agent SDK credit** (Pro $20 / Max-5x $100 / Max-20x $200; claim instructions arrive by email before the 15th), separate from interactive limits. No API billing anywhere. If the credit proves tight: OpenCode/OpenRouter diversifies, and `pty-drive` (human-driven) uses the subscription pool.

## Considered

- **Owning the agent loop (the Mux approach)** — rejected: junto's premise is harness adapters ("junto must not be wedded to one agent runtime"); owning a loop forfeits every harness's tooling and the wedge.
- **Persistent `--input-format stream-json` process (v1)** — rejected for v1: ties session liveness to host process lifetime (six host restarts today alone). Remains the structured middle path if between-turn steering proves too coarse before pty-drive lands.
- **Workspace derived from `.junto.toml` bindings** — rejected: the host no longer scans work repos, several repos may bind one channel, and a committed file silently controlling where agents execute is the wrong trust shape.
- **Message noun now** — rejected (rule of three); see steering section.
