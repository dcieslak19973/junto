# Channel identity is a minted id; names are substrate-scoped labels bound at open time

Status: accepted (Dan, 2026-06-09) · supersedes the "Channels get names (UUIDv5)" clause of [`0012`](0012-mcp-over-http-is-the-first-write-surface.md)

Two facts about Channels, settled while modelling the channel ↔ repo relationship, that the UUIDv5 scheme cannot satisfy together:

1. **A Channel is repo-agnostic.** No repo is part of a channel's identity or scope — a unit of inquiry may work through one repo, several, or none, and at open time it may not yet be known which. The channel's record lives in exactly one **home substrate** (today: a git repo's `refs/junto/*`), but that is a storage fact, possibly a repo entirely unrelated to the work.
2. **ChannelIds must be globally unique.** Records from different substrates must never be able to collide on id (aggregation across substrates — "one surface over many projects" — keys by id).

`ChannelId::from_name` (UUIDv5 over a fixed namespace, per 0012) violates (2) by construction: every substrate that names a channel `dev` derives the *same* uuid for what are different inquiries with different ledgers. Salting the substrate into the derivation would fix uniqueness but break (1) — and would weld identity to a storage location, breaking record migration. Demanding globally unique *names* by convention enforces uniqueness by hope.

## The decision

- **A ChannelId is minted (random UUIDv4) when the channel is opened.** It is globally unique and carries no information about name, substrate, or repos.
- **Opening a channel is an explicit, recorded act** — the domain verb *open*. It mints the id and writes a **genesis record** into the home substrate binding name → id, plus creation metadata (who opened it, when; later the playbook). A channel is something a member opens, not a side effect of a stray write — implicit create-on-first-`record` goes away.
- **The name is a human-facing label, not identity.** It is unique only *within its home substrate*, enforced at open time by the host (which can see the substrate's existing genesis records). Two substrates may both have a `dev` channel; their ids differ, so their records can never be confused.
- **Name resolution reads the substrate.** Surfaces address channels by name; the host resolves name → id via the genesis records (and can enumerate `refs/junto/*` for discovery). No global registry exists or is needed — the substrate is the registry of its own channels.

## Grandfathering `junto-dev`

The dogfood channel's id (`c441363c-…`, UUIDv5-derived) is already synced and referenced. Identity is just *an* id: its genesis record declares the existing id rather than minting a fresh one. `ChannelId::from_name` survives only as long as that migration needs it, then is deleted along with its golden test.

## Consequences

- The Channel noun gets its first real model (id, name, opened-by, opened-at) — pulled forward from the not-built-yet list by addressing pressure, exactly how 0012 predicted ("the first modelling pressure dogfooding put on the unbuilt Channel noun").
- MCP tools that today derive an id from the `channel` argument on every call must instead resolve the name against the substrate, and fail on a name no one has opened (new failure mode: *channel not found*; new tool or parameter: *open*).
- The ref layout `refs/junto/<channel-id>/<author>` is unchanged.
