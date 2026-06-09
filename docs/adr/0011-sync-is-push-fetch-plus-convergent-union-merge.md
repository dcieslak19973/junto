# Sync is push/fetch of author refs, reconciled by convergent union-merge

Status: accepted (Dan, 2026-06-09) · builds on [`0009`](0009-git-refs-substrate-ndjson-per-author.md), [`0010`](0010-canonical-order-and-dedup-by-entry-id.md)

`GitRefsSubstrate::sync(remote, channel)` exchanges a channel's record with **any git remote** (name, URL, or path — the forge-as-hub model, `architecture.md` §Substrate) in one bounded cycle: **fetch** every `refs/junto/<channel>/*` author ref, **reconcile** each into the local record, **push** every local author ref back. A push rejected because the remote advanced mid-cycle re-runs the loop (bounded), exactly like the append guard in [`0009`](0009-git-refs-substrate-ndjson-per-author.md).

## Reconciliation: the union is the merge

Per fetched ref: **create** it if unknown locally · **fast-forward** if the remote is strictly ahead · **keep ours** if we are ahead · on true divergence (the same author wrote on two machines while apart — the one conflict shape partition-by-author leaves possible), mint a **union-merge commit**: the deduplicated-by-id union of both logs, rewritten in canonical order ([`0010`](0010-canonical-order-and-dedup-by-entry-id.md)), with both tips as parents.

This is why the no-CRDT bet (hard constraint #3) holds at sync time: entries are immutable and identified by id, so *set union is the entire merge* — there is nothing to diff, no conflict to resolve, no convergence proof beyond "unions commute."

## The merge commit is deterministic on purpose

Sorted parent oids · fixed identity (`junto sync <sync@junto.invalid>`) · date pinned to the newer parent. Two replicas reconciling the same divergence therefore mint the **same commit oid** and converge immediately, instead of ping-ponging fresh merge commits at each other. (Honest attribution is not lost: a union-merge contains no new entries, and every entry carries its true author inside its JSON.)

## Consequences / scope

- Local refs only ever move under the optimistic `update-ref` guard, so a sync racing a local append cannot clobber it.
- A union-merge **rewrites the log file in canonical order**, so the "every append is a one-line diff" property holds on the normal path but not across a divergence merge. Acceptable: projection ignores file order, and the merge diff is still readable.
- Fetch deliberately uses **no destination refspec** (objects land in the odb; we move refs ourselves, guarded) — avoids fetch-time fast-forward failures and keeps one code path for create/FF/merge.
- **Still deferred** (unchanged from [`0009`](0009-git-refs-substrate-ndjson-per-author.md)): a `Capabilities` type, the `supports_arbitrary_refs` flag and Bitbucket `refs/heads/junto/*` fallback, `ForgeAdapter`, and any sync *cadence* (debounce/poll/webhook — `architecture.md` open decision #3). Verify real-forge behavior (GitHub/GitLab/Bitbucket, https + ssh auth) empirically before locking the capability design.

## Prior art (clean-room inspiration only)

git-bug's push-your-ref / fetch-theirs sync over forge remotes; Kahn–set-union CRDT intuition (a grow-only set needs no CRDT machinery when elements are immutable); git's own octopus-of-independent-histories tolerance (`commit-tree` accepts unrelated parents).
