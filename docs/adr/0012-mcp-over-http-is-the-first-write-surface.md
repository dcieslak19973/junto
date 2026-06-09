# MCP over streamable HTTP is the first write surface; channels get names

Status: accepted (Dan, 2026-06-09) · builds on [`0009`](0009-git-refs-substrate-ndjson-per-author.md), [`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md)

Until now nothing could *author* a ledger entry. The first write surface is an **MCP server** — `junto serve` exposes `record / ratify / park / correct / propose / approve / reject / view_channel / sync_channel` as tools — because "agents post via MCP" is the designed agent-integration path (`architecture.md` §Conversation) and the first dogfood user is an agent (Claude Code recording junto's own development decisions). This is product code on the intended seam, not throwaway scaffolding.

## Why streamable HTTP, not stdio

The `junto` binary is meant to become a **long-running host process** that many members — several agents, later human surfaces — connect to. An HTTP endpoint (`junto serve --repo . --port 1727`, localhost, official `rmcp` SDK, Apache-2.0) is an embryonic version of exactly that host; a stdio server is one-process-per-client and would be thrown away. Practical corollary: multiple concurrent agent sessions share one server and one record. The cost — the user must start the process — is acceptable for dogfooding and is the host's real lifecycle anyway. (Port 1727: the year Franklin founded the Junto.)

## Channels get names (UUIDv5)

A `ChannelId` UUID is unusable in conversation — the first modelling pressure dogfooding put on the unbuilt Channel noun. Convention: a **named channel**'s id is `ChannelId::from_name(name)`, a UUIDv5 over a fixed namespace, so the same name resolves to the same channel on every machine **with no registry to sync**. The namespace constant is load-bearing (changing it would orphan every named channel); the derivation is pinned by a golden test. A future modelled Channel may carry richer metadata, but name-derived ids must remain stable.

## Known limits, accepted deliberately

- **Identity is claimed, not verified.** Tools take an `author` and the server records what it is told — there is no authn (consistent with [`0004`](0004-any-member-may-author-any-entry.md): authority lives at the Gate layer, and even gates do not yet check approver eligibility). Acceptable for a single-user localhost surface; real member identity arrives with Party/roster work. The server's instructions tell agents to author as themselves, never as their operator — a convention, not an enforcement.
- **Localhost only, no auth, no TLS.** Binding is hardcoded to 127.0.0.1. Exposing the host beyond the machine requires the MCP auth story (rmcp has OAuth support) — out of scope until a second machine genuinely needs it.
- **`view_channel` renders markdown for an agent to relay.** This is the dogfood read path, not the terminal-less human surface — the "one surface" bet stays untested by this slice (noted so dogfood comfort doesn't quietly defer it).

## Considered: stdio first

Rejected for the host-shape reason above. Revisit only if a client environment cannot reach localhost HTTP.
