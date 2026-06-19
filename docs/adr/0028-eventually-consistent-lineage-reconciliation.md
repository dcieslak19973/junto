# Eventually-consistent lineage reconciliation (the far-side write retries)

Status: accepted (Dan, 2026-06-18) · serves [`0027`](0027-channel-lineage-is-diverge-converge-edge-entries.md) · builds on [`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md), [`0015`](0015-one-host-per-machine-serving-many-substrates.md)

A lineage edge (`0027`) is two entries, one in each endpoint's ledger, possibly in **different substrates** (and reaching other machines only via sync). Requiring both writes to succeed synchronously would make every cross-substrate diverge/converge fail when the far end is momentarily unreachable. Instead the **near side always writes immediately and the far side is reconciled eventually** — the same posture as `0011`'s convergent sync.

## Mechanism

- **The near side writes unconditionally.** The op validates membership + code on the **near** channel only and records the near-side entry (`DivergedFrom` / `ConvergedInto`). The far end is just a `ChannelId` — no resolution needed to write the near side.
- **The far side is enqueued, not required.** The op builds the **fully-formed** far-side entry (`ChildDiverged` / `ConvergenceReceived`), including its final canonical bytes and id, and tries to write it now; on any failure (far substrate not registered here, not yet synced, far-channel membership not yet visible, transient IO) it lands in a **machine-local pending-lineage queue** — `~/.junto/pending-lineage.ndjson`, beside `members.toml` / `substrates.toml`, one parked entry's canonical bytes per line (the same NDJSON-of-entries shape the git substrate uses, `0009`, so no second serialization). This is **operational state, never ledger content** (it doesn't sync — cf. `0023`'s Workspace).
- **A reconciliation pass drains the queue**, retrying each far-side write. Because the enqueued entry carries its **own fixed bytes/id**, a retry that lands writes the *identical* entry, so the ledger's content-addressed dedup (`0010`) makes re-attempts harmless (idempotent).
- **It runs on `sync` and at host startup**, draining the **whole** queue each pass (a pending far side may live in any substrate) — sync is exactly when far channels and far-channel membership become reachable. No background timer for v1.
- **Reconciliation writes directly to the substrate**, bypassing the agent-facing code check — it is an internal host process (like `junto open`), and the operator's code was already verified when the op was initiated (`0017`/`0021`).
- **A 30-day bound.** A pending entry that never lands within 30 days of its creation (e.g. the operator is genuinely never a member of the far channel) is **dropped from the queue with a host-log warning**; the near side then stays **permanently** marked "unresolved" in projection (`0027` already renders that state).

## Consequences

- Cross-machine lineage needs no new network surface — it rides `0011`'s git sync (`0027` §Surface): a member with a synced clone of the far channel's substrate authors the far side locally, and reconciliation + sync carry it to the shared remote.
- Dangling edges (`0027`) are an expected, possibly longer-lived state, not only a sync race — bounded at 30 days.
- The queue is per-machine and disposable: losing it loses only *pending* far-side writes (the near side is durable in the ledger); a re-issued edge would re-enqueue.

## Considered and rejected

- **Require both endpoints resolvable, refuse otherwise.** Simpler (no queue, dangling only ever transient) — rejected (Dan): too brittle for cross-substrate/cross-machine work; the eventually-consistent path matches junto's existing sync model.
- **A background timer / daemon thread.** Rejected for v1: sync + startup is the natural trigger (far ends become reachable precisely at sync) and avoids a scheduler.
- **Unbounded retry.** Rejected: an edge whose operator is never a member of the far channel would retry forever; 30 days is the give-up horizon.
