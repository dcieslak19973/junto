# Channel lifecycle acts are ledger entries (`ChannelOpened` first)

Status: accepted (Dan, 2026-06-09) · builds on [`0003`](0003-ledger-entry-content-model.md), [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md)

0014 requires a **genesis record** binding name → id when a channel is opened, but left its representation open. Decided: the genesis is a **new ledger entry kind, `ChannelOpened`** — the first entry in the channel's own ledger — not a substrate-level manifest beside it.

## Why an entry, not a manifest

- **One durable mechanism.** A `ChannelOpened` entry syncs, merges, orders, and dedups exactly like every other entry — ADRs 0009–0011 apply unchanged. A manifest (e.g. a `refs/junto/<id>/channel` blob) would re-solve sync, merge, and mutability for a second storage shape.
- **Opening is an authored, consequential act** (0004: any member may author) — precisely what the ledger exists to hold. The opener and the moment are on the record.
- **Rename falls out for free.** Names are labels (0014); a later corrective entry can supersede the genesis binding — no mutable metadata anywhere, append-only spirit intact.
- Discovery cost is negligible: enumerate `refs/junto/*`, read each channel's genesis, cache name → id in the host.

This extends 0003's closed `kind` set — closed means *enumerated and decided*, not frozen; this ADR is the deciding.

## Lifecycle is a family, not a one-off

`ChannelOpened` is the first of an anticipated family of **channel lifecycle entries** — e.g. *forking* a channel from a point in time, *closing* a channel. Each future act gets the same treatment (an authored entry kind, decided when concrete), so channel lifecycle accumulates in the ledger like everything else. None besides `ChannelOpened` is designed yet — listed only so the next one extends a pattern rather than re-opening this question.

## Considered: substrate-level manifest

Cleaner metadata/decision separation and marginally faster discovery — rejected for the second-storage-shape cost above. Revisit only if channel metadata grows genuinely large (rich playbook config, rosters) *and* reading genesis entries measurably hurts; nothing forces metadata to stay in one entry even then.
