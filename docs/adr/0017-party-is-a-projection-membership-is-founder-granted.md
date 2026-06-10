# The Party is a ledger projection; membership is founder-granted

Status: accepted (Dan, 2026-06-10) · builds on [`0004`](0004-any-member-may-author-any-entry.md), [`0012`](0012-mcp-over-http-is-the-first-write-surface.md), [`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md)

Until now the **Party** — the set of Members on a channel — was implicit: whoever authored an entry participated. Decided: **membership is explicit, recorded, and founder-granted**, and **only members' entries project**.

- **The opener is the founding member.** The `ChannelOpened` genesis ([`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md)) already carries its author on the envelope; that author *is* the channel's founder. No new genesis field, no migration for existing channels.
- **The founder grants membership** by authoring a `MemberAdded` entry naming the new member — the second entry kind in 0016's anticipated lifecycle family. A `MemberAdded` authored by anyone else has no roster effect. *For now*: delegated invite authority (any-member invites, roles) is deferred until a team channel forces it.
- **The Party is a pure projection**: the founder, plus everyone named in founder-authored `MemberAdded` entries. No manifest, no second storage shape — the same argument that made the genesis an entry (0016). The roster-defining entries are always authored by the founder, who is a member by construction, so the projection has no chicken-and-egg.
- **Only members' entries count.** Projection computes the Party first, then folds standings and gate statuses over member-authored entries only. Non-member entries are **retained and surfaced as *unrecognized*** — never silently dropped: the substrate is append-only and sync can deliver anything a remote holds, so a misconfigured agent's work must stay visible rather than vanish mysteriously.
- **The membership check is set-based, not temporal**: an entry counts iff its author is in the Party, regardless of where the membership grant falls in canonical order relative to the entry. Temporal ("a member *as of* that entry's position") would be stricter but invites clock-skew landmines — a grant and a record written seconds apart on two machines could invalidate real work — and would orphan every pre-Party entry. Set-based is convergent, and migration is one `MemberAdded` entry per existing author.

## Light local authentication: member codes

Minting a member also mints a **member code** — a random 6-character alphanumeric secret linked to that member identity (human or agent), stored **machine-locally** beside the substrate registry (`~/.junto/`) and **never written to the ledger**: the record syncs to remotes, so a secret in an entry would be no secret. The host's write surfaces require the code alongside the claimed author identity and refuse a mismatch; the **projection cannot and does not check codes** (entries arriving by sync carry none) — its guardrail remains the set-based membership check.

Honest threat model: this is **accident-proofing, not security**. Everything lives on one machine under one OS user; any local process can read the codes file. What it buys is real, though: an agent can no longer author as its operator (or as another agent) by *mistake* — the proxy-identity smell [`0012`](0012-mcp-over-http-is-the-first-write-surface.md) flags becomes a hard error instead of a convention. Real authentication (tokens, TLS, key material) still arrives only when a second machine forces it.

- The code is **per member identity per machine**, minted on first grant and reused across channels.
- `junto open` / `junto add-member` mint and print the code **once**; the operator hands it to the member (e.g. via the agent's environment). The founder gets a code too — one rule, no exceptions.
- Codes are stored in plaintext: the file sits in the same trust domain as the repos themselves; hashing at this scope would add ceremony, not safety. Revisit alongside multi-machine authn.

## What this changes — and what it does not

- [`0004`](0004-any-member-may-author-any-entry.md) ("any Member may author any entry") is unchanged *within* the Party: the kernel still does not restrict authorship by entry kind. This ADR defines the boundary 0004 left open — who counts as a Member of a given channel.
- **Identity moves from claimed to claimed-plus-code-checked at this host's write surfaces** — a deliberate middle step that supersedes part of [`0012`](0012-mcp-over-http-is-the-first-write-surface.md)'s known-limit. The ledger itself still records a claim: a synced entry's authorship is only as trustworthy as the machine that wrote it.
- Host write surfaces (MCP tools, web verification forms) refuse non-member authors and wrong codes up front — a good error beats an orphaned entry — but the **projection is the real guardrail**, because the substrate cannot refuse appends.

## Deferred

- **`MemberLeft` / removal** — set-based checking makes removal genuinely hairy (do a removed member's old entries stop counting?). Decided when concrete, same as fork/close (0016).
- **Roles, gate eligibility, self-approval rules** — who may *approve* remains a Gate/Rubric-layer concern ([`0007`](0007-routing-stays-out-of-the-kernel.md)), untouched here.
- **Web-surface member management** — the web surface stays verification-acts-only; granting membership is `junto add-member` (CLI plumbing, like `junto open`) or an MCP tool for founder-agents.

## Considered

- **Any-member invites** — rejected for now: founder-granted is one rule with no exceptions, and matches the solo wedge (one human, their agents).
- **Temporal membership check** — rejected above (clock-skew, migration cost) in favor of set-based.
- **Implicit join-on-first-write** — rejected: the same smell as create-on-first-write, which [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md) killed; membership is an explicit, recorded act with the grantor on the record.
- **No authentication at all (pure claimed identity)** — rejected by Dan (2026-06-10): the member code is cheap and turns the author-as-operator mistake into a hard error. Codes in the *ledger* were never an option (secrets don't belong in a synced record).
