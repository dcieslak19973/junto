# One host per machine/user, serving many registered substrates

Status: accepted (Dan, 2026-06-09) · builds on [`0012`](0012-mcp-over-http-is-the-first-write-surface.md), [`0013`](0013-host-serves-the-read-surface-recall-via-hook.md), [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md)

When the single-player wedge lands on a second project, the machine could run a second `junto serve` on a second port — or the host could become what 0012 already called it: "a long-running host process that many members connect to." Decided: **the host is a per-machine/per-user singleton** (default port 1727) **serving every registered home substrate**, not a per-repo sidecar.

## Why a singleton

- **The thesis is one surface.** One host per substrate fragments the read surface into a browser tab per port — the tab-juggling junto exists to kill. The singleton makes `localhost:1727` the place where *all* of a user's channels live, across every project.
- **Channels are repo-agnostic (0014).** A session may work in repo A while recording into an unrelated records repo R. With per-substrate hosts the session's MCP connection must somehow point at R's host; with a singleton, any session reaches any registered substrate through the same port.
- **No port bookkeeping.** Every `.mcp.json` on the machine says the same thing; worktrees inherit it for free.

This deliberately overrides the rule-of-three caution ("wait for the third project before generalizing"): fragmenting the surface per-port walks away from the product's point, so the generalization is bought at two.

## Mechanics

- The host keeps a **machine-local registry of substrate repos** (a config file under the user's junto directory). This is *not* the global registry 0012/0014 refuse: channel identity and name→id bindings stay in the substrates themselves; the registry only says "these repos exist on this machine" — losing it loses nothing durable.
- **Name resolution spans registered substrates:** a bare channel name resolves when exactly one registered substrate has it; ambiguity is an error asking for substrate qualification. ChannelIds (globally unique per 0014) always resolve unqualified.
- Registering a substrate is a natural part of project setup (`junto init` or equivalent).
- Localhost-only, identity-claimed limits of 0012 are unchanged; "per machine/user" means one host per user profile — multi-user machines run one per user, not one per box.

## Considered: one host per home substrate

Simplest code, no registry file — rejected because the pain scales with project count (ports, processes, fragmented surface) and the repo-agnostic-channel case (work in A, record in R) makes the session→host binding non-deducible.
