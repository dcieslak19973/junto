# ACP is junto's harness protocol (`claude -p` as fallback)

Status: accepted (Dan, 2026-06-13 — spike verified end-to-end) · builds on [`0023`](0023-launching-agent-sessions-oneshot-first-pty-next.md), `docs/pluggability.md` (AgentHarnessAdapter / ExecutionBackend) · landscape: [`competitive-landscape.md`](../competitive-landscape.md)

junto drives coding-agent harnesses over **ACP — the Agent Client Protocol** (Zed's standard for a *client driving an agent over stdio*) as the **primary** path, keeping `claude -p` stream-json as a **fallback**. ACP is *exactly* junto's job, and adopting it changes the shape of the harness seam: instead of "one `AgentHarnessAdapter` trait, N bespoke per-vendor implementations," junto writes **one ACP client** and reaches each harness through its **ACP adapter**, branching on the **capability flags ACP returns** — the literal realization of constraint #4 ("capability flags, not vendor names").

## Why ACP over per-vendor integration (e.g. `claude -p`, t3code's native protocols)

- **One client, many harnesses.** Claude (`@agentclientprotocol/claude-agent-acp`), and adapters exist for Codex, Gemini, etc. junto integrates the protocol once.
- **Richer than `claude -p`.** ACP advertises persistent **sessions** (`fork`/`resume`/`list`/`close`), **permission modes** (`dontAsk`/`auto`/`default`), MCP servers, embedded context — a superset of what `-p` exposes.
- **Capability-driven.** `initialize` returns the agent's capabilities; junto branches on those, never on a vendor name.

t3code's per-vendor-native approach (Codex app-server, Claude stream-json, normalized internally) was the alternative considered and not taken — ACP collapses that N-integrations-plus-normalizer into one client.

## Auth & economics (verified)

The Claude adapter runs **Anthropic's official Claude Code SDK**, so it authenticates exactly like `claude -p`: with the **subscription login, no API token**. The spike (2026-06-13) ran a real turn whose usage carried `rateLimitType: "five_hour"` with `ANTHROPIC_API_KEY` unset — the subscription rate-limit model, not API billing. ACP is *transport*; it introduces no new usage-terms posture beyond 0023's sanctioned Agent-SDK path. The adapter is Apache-2.0 (the ACP Rust crate too) — clean to depend on under constraint #1.

## Hybrid: ACP primary, `claude -p` fallback

junto attempts ACP, falling back to the `claude -p` CLI when: a **test stub** is set (`JUNTO_HARNESS_CMD`), the protocol is forced (`JUNTO_HARNESS_PROTOCOL=cli`), **Node is absent** (the adapter is a Node package), or **ACP setup fails** (adapter won't spawn / handshake fails — surfaced as a feed status line, then the CLI runs the turn). `claude -p` stays until ACP is proven in daily use; **retiring it is deferred, not decided** (Dan: "keep `claude -p` as a fallback").

## Implementation

A **hand-rolled minimal ACP client** (`crates/junto/src/acp.rs`): newline-delimited JSON-RPC over the adapter's stdio — three requests (`initialize`, `session/new` or `session/load`, `session/prompt`) and the `session/update` notification stream, mapped onto junto's existing `LiveEvent` feed and `TurnOutcome`. The typed `agent-client-protocol` crate (Apache-2.0) is the upgrade path if this grows. The ACP `sessionId` is stored in the existing harness-session mapping (what `session/load` resumes for steering).

## Costs & gotchas

- **An extra Node adapter process per harness.** On Windows the launcher is `npx.cmd` (Rust's spawn won't auto-append `.cmd`).
- **The `CLAUDECODE` nesting guard.** The adapter refuses to launch Claude Code inside another Claude Code session; junto strips `CLAUDECODE` from the adapter's env (and the host runs as its own process anyway).
- **ACP does *not* fix the Windows console-window flashing.** That's Claude Code's SDK spawning Bash children (same as `claude -p`); the **WSL ExecutionBackend** (0023-era) remains the flashing fix, orthogonal to the protocol. ACP-under-WSL is future work.
- **Session ids are protocol-specific** (ACP `sessionId` vs claude `session_id`), so resuming a session under a *different* protocol than it started is a known edge; rare (protocol choice is stable per machine).

## Considered

- **The `agent-client-protocol` Rust crate now** — deferred: the wire surface junto needs is tiny and the crate has v1/v2 churn; hand-rolling is the simpler thing that works (rule of three), with the crate as the named upgrade path.
- **ACP-only, retire `claude -p` now** — deferred (Dan): don't drop a working path before ACP is proven in daily use.
- **Permission routing into junto's UI** — ACP's `default`/`auto` modes + `session/request_permission` make this possible (the pty-drive goal from 0023, for free); v1 stays yolo (`dontAsk` + auto-allow), with UI routing as a future slice.
