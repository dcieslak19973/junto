# Canonical order is (timestamp, author email, entry id); projection dedups by id

Status: accepted (Dan, 2026-06-09) · builds on [`0002`](0002-ledger-entries-are-immutable.md), [`0009`](0009-git-refs-substrate-ndjson-per-author.md)

`Ledger::project` orders entries by **`(timestamp, author.email, id)`** and **deduplicates by `EntryId`** (keeping the first occurrence in canonical order) before folding.

## Why the `id` tie-break

The architecture's `(ts, author)` interleave rule has a hole: two entries from the *same* author in the same millisecond (easy for an agent) have no defined order, and the substrate contract deliberately returns entries in no particular order. Because assertion standing is **last-applicable-wins** ([`0002`](0002-ledger-entries-are-immutable.md)), two replicas that sorted such a pair differently would derive *different standings from the same record* — silent divergence, surfacing only after forge sync. Adding the entry id (already `Ord`) as the final key makes the order a deterministic **total** order on every platform. The id carries no meaning (it is a random UUID); it is only an arbitrary-but-stable coin flip that all replicas share.

## Why dedup in the projection

Append is not idempotent (a retried append after an ambiguous failure writes the same line twice), and the coming **sync slice** will union author logs fetched from multiple remotes — overlap is expected, and `architecture.md` already promises dedup. Owning it in `Ledger::project` keeps every `SubstrateProvider` dumb (the contract now explicitly tolerates duplicates) and gives one shared definition of "the same entry": **same `EntryId`**.

## Consequences

- The fold rules themselves are unchanged; only their input order/uniqueness is now fully pinned (test: same-author same-millisecond pair projects identically when appended in either order; a double-appended entry projects once).
- Two *different* entries reusing one id is unguarded for now — first-in-canonical-order wins silently. A content-addressed id (the git substrate's eventual direction, noted in `ids.rs`) would make this unrepresentable; until then ids are random UUIDs and accidental collision is negligible.
