# Assertions carry their session: closing the `session → decision` arrow

Status: accepted (Dan, 2026-08-23) — **implemented** · realizes spec §1 (the `session` field) of [`2026-08-23-verification-ceremony-design.md`](../superpowers/specs/2026-08-23-verification-ceremony-design.md) · closes the second half of the chain ledger `b10ffdc6` identified, alongside `5f45728`'s `SessionCommitted.branch` · extends [`0020`](0020-agent-sessions-and-artifacts-are-ledger-entries.md) · companion to [`0039`](0039-findings-are-not-obligations.md) (same `Assertion` change)

Decision blame — *"from any file/line, surface the ledger entries and Sessions that touched it"* (`docs/competitive-landscape.md`'s strongest borrow) — needs an unbroken chain from a line of code to the decision that justified it. Ledger `b10ffdc6` named the break: `SessionUpdated`, `SessionCommitted`, and `ArtifactAttached` (`crates/junto-kernel/src/entry.rs`) all carry a `target: EntryId` pointing back at their `SessionStarted` entry — the artifact/commit arrow already existed. `Assertion` — a decision, a finding, a claim — carried no such link. It floated free of the run that produced it.

The asymmetry mattered because it broke the chain at exactly the point decision blame needs most: after `5f45728` added `SessionCommitted.branch`, `git blame → commit → session` resolved. But `session → the decision that session recorded` did not — a session could point at what it changed, never at what it decided.

## The fix

`Assertion` gains `session: Option<EntryId>`, the `SessionStarted` entry's id when a Session recorded it — additive, `#[serde(skip_serializing_if = "Option::is_none", default)]`, following the pattern already established by `LedgerEntry.signature`, `DivergedFrom.at`, `Assertion.frame`, `Proposal.kind`, `ChannelOpened.name`, and `SessionCommitted.branch`. Existing canonical bytes are unchanged, existing signatures stay valid, and `golden_canonical_form_is_byte_stable` passes untouched.

`session` uses the same `target`-at-a-`SessionStarted`-id convention `SessionUpdated`/`SessionCommitted`/`ArtifactAttached` already use — no new linking shape was invented for this one case. `record` (`crates/junto/src/mcp.rs`) accepts an optional `session` parameter, parses it as an `EntryId`, and refuses a value that doesn't parse (`"session '{raw}' is not an entry id"`) rather than storing garbage.

`session` stays optional. An assertion recorded outside a Session — a human typing directly, or an agent with no active Session — remains legal; the kernel stays permissive (`0004`'s spirit) and nothing pre-existing or human-authored is retroactively made illegal by requiring a session that may not exist.

## What this unblocks

With `session` in hand, decision blame's query slice — walk a file/line to its `CodeAnchor` (`0034`), the commit, the session (`5f45728`'s `SessionCommitted.branch`), and now the decisions/findings that session recorded — has every link it needs to be a straight-line projection instead of one that infers the last hop from timestamps or prose proximity. **This ADR records that the data now exists, not that the query was built.** No new decision-blame read endpoint, projection, or rendering shipped alongside `session`; the reverse-provenance projection domain-model.md names (`CodeAnchor` → entries + Sessions) remains future work over a type that already exists.

## Explicitly deferred: the rest of `b10ffdc6`

`b10ffdc6` also asked for an agent identified by **harness *and* session**, rather than by an email that smuggles the harness in by convention. Today a stock agent's `Member.email` *is* its harness identity by convention — the stock Claude agent reuses `claude-code@anthropic.com` as both its Member email and its harness marker (`crates/junto/src/agent.rs`, `launch.rs`) — which conflates "which config authored this" with "which harness ran it" into one string with no structure. Fixing that is a `Member`-shape change touching every author block: the kernel struct, MCP request parsing, member listing/display, signing, and every place a `Member` is matched or rendered. It remains owed as its own slice; landing it alongside `session`/`kind`/`answers` would have tripled the surface area of one additive-field change for no benefit to the parts that ship here.

## Considered

- **Making `session` required** — rejected: forces every pre-existing and every human-authored assertion to either become illegal or carry a fabricated session. Optionality is the point of the additive-field pattern this reuses.
- **A new entry kind linking `SessionStarted` → `Assertion`** (mirroring an edge entry rather than a field) — rejected: `SessionUpdated`/`SessionCommitted`/`ArtifactAttached` already express "produced by this session" as an inline `target`; a fourth, edge-shaped way to say the same thing would fork a convention that doesn't need forking.
- **Landing the full `b10ffdc6` (harness + session identity) in this slice** — deferred, not rejected: correct, but a `Member`-shape change with a much larger blast radius than one optional field on `Assertion`. Named explicitly above so it isn't mistaken for done.
