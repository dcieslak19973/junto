# The collapse: cheap channels, derived standing, and the noun that stays `Channel`

Status: accepted (Dan, 2026-08-21) — **implemented** · realizes spec §2 of [`2026-08-21-multiplayer-first-rethink-design.md`](../superpowers/specs/2026-08-21-multiplayer-first-rethink-design.md); amends [`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md)

[`0014`](0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md) removed implicit channel creation deliberately: *"A channel is something a member opens, not a side effect of a stray write — implicit create-on-first-`record` goes away."* The thing that defended against — a stray agent write minting a channel nobody asked for — still stands and is not weakened here. What the collapse removes is the *ceremony* a human pays for an act that, for a person, is not a form at all: typing a question and pressing enter is already explicit.

## The human/agent asymmetry

The seam this splits on is the one [`0021`](0021-member-codes-guard-agent-surfaces-only.md) already drew for a different reason:

| Surface | Opening a channel |
|---|---|
| **Human** (native) | permitted to open with no name, substrate, or playbook — the record already accepts and renders an unnamed `ChannelOpened` genesis (`name: Option<String>`). The genesis is still author-attributed and still explicit; there is simply no form to fill in first. |
| **Agent** (MCP) | **unchanged.** `open_channel` keeps its explicit, required arguments and member code. |

The defence `0014` built was specifically against an *agent* write minting a channel by accident — an agent's write surface is exactly where "identity is claimed, not verified" (`0012`) makes an unnoticed side effect expensive, because nothing stops a malformed or adversarial tool call from trying it repeatedly. A human pressing enter on their own machine is not that failure mode; the OS boundary is still the trust boundary (`0021`), and no new accident-proofing is needed for a surface that was never the threat. So the collapse is asymmetric on purpose: the ceremony comes off where it was only ever friction, and stays exactly where it was only ever protection.

As of this writing the *record* permits an unnamed genesis; no shipped UI control yet drives it (the web form still requires a non-empty name, returning `400` on blank input) — the kernel capability is real and tested, the human-surface control that calls it is separate, later work, named here so the gap is not silently assumed closed.

## Derived channel standing

Entries have always had standing (`0002`/`0003`); channels did not, and a channel that is finally cheap to open needs somewhere for "cheap" to stop being free. Hundreds of unratified channels leave the ledger *sound* — an unratified entry never outranks a ratified one — but they break **recall**, and recall is the whole reason junto is useful day to day.

`ChannelStanding` (`crates/junto-kernel/src/ledger.rs`) is derived by projection from state the ledger already holds — no new entry kind, no user-facing action:

| Standing | Condition | Visible to |
|---|---|---|
| `Scratch` | not closed, no ratified entry | its author only |
| `Standing` | ≥1 ratified entry | the party; feeds recall |
| `Settled` | closed | recall, as history |

**Existence and standing become different things, and that split is what makes cheapness safe.** A channel can exist — be opened, hold a conversation, run a session, attach artifacts — while remaining `Scratch`: invisible to `junto brief`'s SessionStart recall (`main.rs::brief`, which reads `view.channel_standing` directly off the projection it already has in hand for `lineage_context`, at zero extra cost) until something in it is ratified. Opening ten scratch channels for half-formed threads costs nothing downstream, because none of them can pollute the one thing recall exists to protect: what an agent sees unprompted at the start of every session.

## Names stop being unique

`0014`'s original decision made a name unique *per home substrate*, enforced by scanning existing channels at open time. Cheap, unceremonious opening means many channels legitimately share a name — twelve different channels all plausibly called "auth stuff" — so `0014` already did the hard part (*"the name is a human-facing label, not identity"*) and this drops only the uniqueness rule layered on top of that label:

- `Host::open_channel` no longer scans for a same-named genesis before appending; two channels may carry the identical name in the identical home substrate.
- `Host::resolve` on a bare name now walks every registered substrate, keeps the channel whose **genesis** entry is greatest under `LedgerEntry::canonical_cmp` — the kernel's own `(timestamp, author email, entry id)` total order (`0010`) — and resolves to that one. A same-millisecond tie breaks on `EntryId` exactly as `canonical_cmp` would, so every replica resolves an ambiguous name identically without asking anyone to qualify it. This is deliberately the genesis's *own* timestamp, not a later rename's: a `Correction` that changes the display name does not re-open the channel it renamed.
- `Resolution::Ambiguous` — the variant `0014`/`0015` introduced to ask a caller to disambiguate — is deleted outright, not merely left unconstructed. Resolution now always picks a winner; there is nothing left to disambiguate about.

Identity remains exactly what `0014` minted: a random, globally unique `ChannelId`, carrying no information about name, substrate, or repos. Nothing about *that* half of `0014` changes.

## The noun stays `Channel`

The collapse could have been read as license to rename the kernel noun to match the shape it now has — a cheap, unceremonious, repo-free unit born from a message and related to its siblings by lineage sounds like what other tools call a "thread." That renaming was considered and rejected:

- **Renaming the kernel noun to `Thread`** — rejected: it ripples through `0025`'s terminology alignment, every doc in the corpus, and every type name, to buy a word competitors happen to use, for no semantic gain — the collapse changes what a Channel *is*, not what it should be *called*.
- **Renaming only the user-facing surface, keeping `Channel` in the kernel** — rejected as worse than doing nothing: it is cheap exactly because it is a lie by omission, opening a permanent gap between what the product says and what the record, the docs, and the agents say. `CLAUDE.md`'s requirement that *"names carry the ubiquitous language"* forbids exactly this kind of drift.

**The collapse is semantic, not lexical.** A Channel becomes what a thread-shaped unit of work is — the noun does not move. The binding consequence: **"thread" must not appear as a synonym for a channel anywhere in code, entries, ADRs, or UI copy.** Where the design spec's own prose says "thread" it means "a channel after the collapse"; that vocabulary is normalized to `Channel` as work lands, not treated as an acceptable second name for the same thing.

## Lineage becomes the primary way channels are born

No code changed for this part — `0027`/`0028`'s diverge/converge machinery was already built. What changes is its role: under the collapse, `diverge` stops being a special side-quest gesture reserved for consequential forks and becomes the *ordinary* way one channel spawns another, promoted from feature to organizing relation now that opening a channel costs one keystroke instead of a form.

## A known limit worth recording

`crate::launch::session_workdir` resolves a session's workdir by calling `mount_with_capability`, which walks a channel's *current* subjects against the *current* mount store and returns the first `Execute`-capable mount it finds. Nothing records which mount a session actually ran in at launch, so that resolution happens fresh every time it runs: on launch, and again on a steer **of a session that ran in a mount**. If a channel has two Execute-capable Repo subjects, mount A resolves first at launch, and A is later unmounted while B remains mounted, the *next* steer resolves to B instead — a resumed session silently moves to a different directory holding none of the session's prior work, rather than erroring or staying put. This applies to any channel with more than one mounted, Execute-capable subject.

A session that ran in **scratch** is exempt, and is the one case junto does pin: `crate::web::steer_session` checks for `<junto_home>/scratch/<session>` before it consults mounts at all, and resumes there when it exists. The directory is created by `session_workdir` and never removed, so its existence is itself the durable, machine-local record that this session ran in scratch — which is why the pin needs no new state. That check sits above both the `Execute`-capable mount lookup and the unmounted-Repo refusal, so a scratch session is pinned to its scratch directory for its whole life, and a Repo subject arriving later by sync cannot move it or strand it.

The honest fix is recording the session's workdir at launch — new durable state this design does not introduce, since a session's resolved directory is a fact about how that particular run executed, not about the channel it ran in. Building that recording is out of scope here; the gap is named so it is not later mistaken for an oversight rather than a deferred, deliberate decision.

## Considered

- **Enforce name uniqueness across the whole registry instead of dropping it** — rejected: it does not solve the actual problem (twelve legitimate "auth stuff" channels), it just moves the collision from "same substrate" to "everywhere," and it re-adds exactly the friction the collapse exists to remove.
- **Make `Resolution::Ambiguous` return the whole list instead of deleting it** — rejected: once resolution has a deterministic, total-order tie-break, "ambiguous" is no longer a real outcome to model; keeping the variant unconstructed would have been dead code with a live-looking name.
- **Rename to `Thread` at the surface layer only, revisit the kernel later** — rejected above; recorded again here because it is the most tempting shortcut and the one most likely to resurface.
