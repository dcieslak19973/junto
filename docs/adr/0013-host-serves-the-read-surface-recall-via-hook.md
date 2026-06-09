# The host serves the read surface; agent recall bridges via a SessionStart hook

Status: accepted (Dan, 2026-06-09) · builds on [`0012`](0012-mcp-over-http-is-the-first-write-surface.md) · decided in the junto-dev ledger (grill session; proposal `e368422a`, gated on Dan's approval)

Two read paths join the host, both projections of the same `ChannelView`:

- **`GET /channels/{name}`** — a read-only HTML page: the **first pixel of junto's one surface.** The grill session confirmed (against the reviewer's reframe) that *one surface remains the point*; a human reading the record in a browser — not `git show`, not agent-relayed markdown — is therefore product, not probe. Strictly read-only: every write goes through the MCP tools, where authoring identity is explicit.
- **`GET /channels/{name}/brief`** — the markdown brief (same renderer as the MCP `view_channel` tool), fetchable without an MCP handshake.

## Recall: membership-time injection is the design; the hook is the bridge

The single-player wedge's make-or-break loop is agents *consulting* the record before acting — memory nobody reads is decorative. The settled design (Dan, grill session): **recall is a membership concern** — when an agent joins a Channel it receives the channel's context, injected by junto at join time, once Channel/Party/Agent Session are modelled. Until then, the bridge is a harness **SessionStart hook** (`.claude/settings.json`) that curls `/channels/junto-dev/brief` into agent context at session start: one executable, cross-shell, `-sf -m 2` so a stopped or hung host degrades to a silent no-op rather than blocking the session.

## Considered

- **Convention only** (a CLAUDE.md instruction to call `view_channel`) — rejected as the design: instruction-following is best-effort exactly where it matters least (an agent about to contradict a ratified decision it never read). Kept as a *supplement* (the consult/record convention in CLAUDE.md).
- **Generated markdown file in the repo** — rejected: a derived working-tree file is the pollution hard-constraint #3 exists to avoid, plus staleness.
- **Built retrieval (search/filter tools)** — deferred until dogfooding shows what queries matter.

## Consequences

- The wedge story is "one surface, starting solo": solo users adopt the surface + record in one repo; teams extend it. (Thesis framing confirmed in the ledger, entry `7f354123`.)
- The brief is currently the *full* channel projection; when channels outgrow context budgets, the brief needs selection/summarization — that work lands on the brief endpoint, not on each consumer.
- The HTML page will grow toward the real surface (gates awaiting you, parked dead-ends, filters); its information design is now product surface, reviewed as such.
