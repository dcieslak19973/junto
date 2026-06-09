# The git-refs substrate stores an NDJSON log per author

Status: accepted (Dan, 2026-06-09) · builds on [`0001`](0001-ledger-is-the-durable-record.md), [`0008`](0008-canonical-entry-serialization-is-jcs-json.md)

The durable record is stored in a local git repository under dedicated **`refs/junto/*`** refs (hard constraint #3: dedicated refs, never working-tree files). The layout is **one append-only NDJSON log per author**:

```text
refs/junto/<channel-id>/<author-slug>  ->  commit
  └─ entries.ndjson   (one canonical JSON line per entry)
```

Each author's ref points to a commit whose tree holds a single file, `entries.ndjson` — that author's entries for the channel, one [`LedgerEntry::to_canonical_bytes`] line each. Appending reads the current log, adds a line, and writes blob → tree → commit → `update-ref` via the **system `git` CLI** (the assessed substrate decision in CLAUDE.md), touching only the object DB and refs. Reading unions every author ref for the channel; ordering is left to `Ledger::project` (`(timestamp, author.email)`).

Lives in its own adapter crate, **`junto-substrate-git`**, so the process/IO/temp-repo dependencies stay out of the pure, generic kernel (CLAUDE.md: one crate per adapter boundary).

## Why partition by author

Git refs are single-writer. If everyone appended to one shared ref, concurrent writes would collide (non-fast-forward) and require *merging* divergent append-only histories — the exact problem CRDTs exist for, which constraint #3 forbids. Giving each author their own ref makes the collision impossible: each writer only ever fast-forwards their own ref, and later forge **sync** is push-your-ref / fetch-theirs with no merge step (prior art: **git-bug**'s `refs/bugs/*`). The only residual race — the same author writing from two places — is handled by an optimistic `update-ref` old-value guard with bounded retry.

## Why NDJSON (and why a log, not a tree of blobs)

The canonical form is JCS JSON with **no raw newline bytes** ([`0008`](0008-canonical-entry-serialization-is-jcs-json.md)), so newline-framing a log is unambiguous. The result is a readable, auditable history — `git show` of any append is a one-line diff — that is naturally append-only and keeps a small number of refs (one per author per channel) that push cleanly to a forge later.

- **Considered: a tree with one blob per entry** (git-bug-style). Rejected — more plumbing per append (tree mutation) with no readability gain over the one-line-diff log.
- **Considered: one ref per entry** (`…/<author>/<entry-id>` → blob). Rejected — a ref per entry makes `packed-refs` grow unbounded and floods the forge on sync; an unusual, poorly-scaling git shape.

## Consequences

- **No working tree is touched** — only objects and refs — so there is no `git status` pollution, and Windows file-locking is sidestepped (git manages its own ref `.lock` files; we hold no files open). A test asserts `git status --porcelain` stays empty.
- **Commits are deterministic and honestly attributed.** `commit-tree` runs with `GIT_AUTHOR_*`/`GIT_COMMITTER_*` from the Member and the dates pinned to the entry `Timestamp` (`@<epoch> +0000`), so `git log` shows the real author and re-running yields the same commit.
- **The substrate is order-free.** It returns the complete set for a channel in no particular order; `Ledger::project` sorts. The `SubstrateProvider::entries` contract was relaxed accordingly (`InMemorySubstrate` still returns append order incidentally).
- **The author slug is safe, not reversible.** The email is percent-encoded to `[A-Za-z0-9_-]` (dropping `.` and `@` to avoid forbidden `..` / leading-dot ref components); the real email lives in each entry's JSON, so the slug need only be unique and ref-safe.
- **Scope: this is the *local* half.** Forge sync (`git push`/`fetch` of `refs/junto/*`), the `supports_arbitrary_refs` capability + Bitbucket `refs/heads/junto/*` fallback, a `Capabilities` type, and `ForgeAdapter` are deferred to the sync slice — rule of three, one substrate so far (see `architecture.md` §Substrate for the sync design).

## Prior art (clean-room inspiration only)

git-bug (a tracker stored in `refs/bugs/*`, partition-by-author, no working tree); git's own use of NDJSON-ish line logs; `git notes`. Pattern reused clean-room, no copyleft source.
