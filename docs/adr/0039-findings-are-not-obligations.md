# Findings are not obligations: `AssertionKind` as kernel state, attention as app policy

Status: accepted (Dan, 2026-08-23) — **implemented** · realizes spec §1 (`AssertionKind`, the `Scratch` decision) and §2 (aging) of [`2026-08-23-verification-ceremony-design.md`](../superpowers/specs/2026-08-23-verification-ceremony-design.md) · extends [`0038`](0038-the-collapse-cheap-channels-and-derived-standing.md)'s `ChannelStanding`; the kernel/app split follows the seam [`0007`](0007-routing-stays-out-of-the-kernel.md) drew for routing

junto's attention board had quietly become the queue its own design forbids. `docs/attention.md:85` states the board is explicitly *"not a queue"*, and `:337` sets the guardrail *"be the place attention goes, not a predictor of when to interrupt."* `attention_for_view` admitted **every** provisional assertion — no cap, no aging, no distinction between a decision that needs a verdict and a finding that merely needs to exist. The consequence was structural, not aesthetic: **an agent can manufacture obligations faster than a human can discharge them.** One session on 2026-08-23 produced five long assertions in a few hours; three were ratified within minutes of landing — evidence that per-item cost was never the problem. Supply was.

## `AssertionKind` is kernel state; what it means is app policy

`Assertion` gains `kind: Option<AssertionKind>` where `AssertionKind { Finding, Decision }` (`crates/junto-kernel/src/entry.rs`, `#[serde(rename_all = "snake_case")]`, so the wire form is `"finding"`/`"decision"`). This is a **kernel enum**, not a `Proposal.kind`-style free string: it is generic epistemic state, the same shape as `Standing`, not playbook-supplied vocabulary — and `CLAUDE.md`'s *"make illegal states unrepresentable"* constraint wants that distinction typed, not stringly.

The kernel stores `kind` and folds nothing from it. Whether a `Finding` deserves a human's attention is decided entirely in `crates/junto`'s `attention_for_view`, which excludes `Some(AssertionKind::Finding)` from the verification items it emits — the kernel never decides whether something needs a human (`CLAUDE.md` hard constraint #5, the kernel/playbook seam). This is the same shape `0007` used to keep gate *routing* out of the kernel: the kernel supplies the mechanism (fold, project, store), the app layer supplies the policy (what a folded value should prompt a human to do).

## Absent `kind` reads as `Decision`

Every consumer treats a missing `kind` as `AssertionKind::Decision` — pinned by `an_assertion_with_no_kind_still_asks_for_a_verdict`. This is the conservative direction on purpose: a migration that silently discharged every pre-existing obligation-bearing assertion would be indistinguishable from one that lost them. Legacy entries keep demanding verification; aging (below) is what clears the historical tail, on a schedule a human can see and reverse, rather than a flag day that erases it.

At the write surface, an unrecognized `kind` string is refused by name (`"unknown kind '{other}' — use \"finding\" or \"decision\""`) rather than silently coerced to a default — a typo in `kind` fails loudly instead of quietly becoming an unwanted `Decision` or `Finding`.

## Aging is a projection filter, never a `Standing` write

A provisional assertion past a horizon leaves the *act* list and renders in the brief's quieter `## recorded, unverified` tier instead. Nothing in the record changes:

```rust
/// How long a provisional assertion stays an attention item before it becomes
/// recorded-but-unverified. A claim nobody has needed to verify in this long
/// is not waiting on anyone, and `docs/attention.md:85` is explicit that the
/// board is "not a queue".
///
/// This is a **projection filter only**: the entry keeps its `Provisional`
/// standing and can still be ratified whenever someone wants to. ... Gates
/// are exempt — a pending gate blocks its proposer, and time does not
/// unblock them.
pub(crate) const VERIFICATION_HORIZON_DAYS: i64 = 14;
```

`attention_for_view` retains only verification items within `VERIFICATION_HORIZON_DAYS` of `now`; `brief_shape` (`crates/junto/src/render.rs`) applies the identical horizon to route an aged `Decision` into the same `findings` bucket a `Finding` lands in — one quieter tier serves both "never needed to ask" and "asked too long ago to still be waiting on anyone," deliberately not distinguished in the UI. Gates are the one exception: `attention_for_view` keeps gates in a separate vector never subject to the horizon, because a pending gate blocks its proposer and time does not unblock them. 14 days is a starting guess with a doc comment, in the style of `MILESTONE_CAP` — chosen to be wrong cheaply, reversible by editing one constant, and touching no bytes.

### The `Scratch` interaction (decided)

`ChannelStanding::Standing` requires at least one **Ratified** entry (`project_channel_standing`, `crates/junto-kernel/src/ledger.rs`). **Decision (Dan, 2026-08-23): findings do not promote a channel out of `Scratch`.** A channel that has produced no verified decision has not yet produced anything worth surfacing party-wide. The cost is accepted plainly: a findings-only channel's entries reach only their author (`0038`'s `Scratch` visibility) until one decision is ratified. The alternative — findings promote — was rejected because it would feed unverified agent output into every agent's brief, exactly the risk ledger `f1cb3110` recorded against `0038`'s collapse.

**A known limit worth recording:** this is achieved indirectly, not enforced. `project_channel_standing` was deliberately left untouched — it still promotes a channel to `Standing` on *any* ratified entry, a ratified `Finding` included. The guarantee is behavioral: because `attention_for_view` and the brief's act list exclude `Finding`-kind assertions, nothing *prompts* a human to ratify one, so in the ordinary flow a findings-only channel never accumulates a ratified entry and stays `Scratch`. A human who directly ratifies a `Finding` off the board — today, only via `view_channel`/`ratify` called by hand, since no dialog offers it — still promotes the channel. This is named here so the boundary is not later mistaken for a kernel-level lock that doesn't exist.

## Considered

- **`kind` as a `Proposal.kind`-style free string** — rejected: this is generic epistemic state belonging in the type system next to `Standing`, not playbook-supplied vocabulary a kernel type shouldn't know about.
- **Default absent `kind` to `Finding`** — rejected: silently discharges every legacy assertion's implicit obligation to be verified. The conservative default (`Decision`) keeps them demanding a verdict until aging — a reversible projection filter — clears the tail on a visible schedule.
- **Findings promote `Scratch` → `Standing`** — rejected: would feed unverified agent output into every agent's brief, the failure mode `0038` was built to keep cheap channels from causing.
- **Enforce the `Scratch` interaction at the kernel** (make `project_channel_standing` `kind`-aware, or refuse to ratify a `Finding`) — not done. Left as a behavioral consequence of removing the prompt rather than a hard rule; revisit only if the soft guarantee proves insufficient in practice.
