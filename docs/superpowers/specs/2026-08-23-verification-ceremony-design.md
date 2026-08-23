# Verification ceremony — design

**Date:** 2026-08-23
**Status:** approved in brainstorming (Dan, 2026-08-23); spec for review
**Ledger:** `c74bf4e2` (reducers 1–3), `87fb9a29` (reducer 4), `b10ffdc6` (the session link this partly implements)

## Summary

junto's attention board has quietly become the queue its own design forbids. `attention_for_view` (`crates/junto/src/host.rs:1363-1370`) admits **every** provisional assertion, with no cap, no aging and no distinction between a decision that needs a verdict and a finding that merely needs to exist. The brief does the same (`crates/junto/src/render.rs:143-144`, feeding the `needs attention` section at `:358`). Meanwhile `docs/attention.md:85` states the board is explicitly *"not a queue"* and `:337` sets the guardrail *"be the place attention goes, not a predictor of when to interrupt."*

The consequence is structural rather than aesthetic: **an agent can manufacture obligations faster than a human can discharge them**, and every finding recorded becomes an item addressed to one person. One session on 2026-08-23 produced five long assertions in a few hours; three were ratified within minutes of landing, which is the evidence that per-item cost is *not* the problem. Supply is.

Four reducers, each attacking a different part of the load:

| # | Reducer | Attacks |
|---|---|---|
| 1 | Findings are not obligations | what becomes an item at all |
| 2 | Aging | items nobody ever needed |
| 3 | Ratify-by-consequence, behind a dialog | items worth settling, settled where you already decide |
| 4 | Answer-linking | the *count* — an open question and its answer become one review |

Non-negotiable throughout: nothing is verified without a human act. Reducers 1, 2 and 4 change *what is asked of you*; reducer 3 changes *when and how many at once*. None of them ratifies anything on its own.

## 1. Findings are not obligations

`Assertion` gains two optional fields, using the additive
`#[serde(skip_serializing_if = "Option::is_none", default)]` pattern already established by `LedgerEntry.signature`, `DivergedFrom.at`, `Assertion.frame`, `Proposal.kind`, `ChannelOpened.name` and (as of `5f45728`) `SessionCommitted.branch`. Existing canonical bytes are unchanged and existing signatures stay valid; `golden_canonical_form_is_byte_stable` must pass untouched.

```rust
Assertion {
    statement: String,
    rationale: String,
    provenance: Vec<ProvenanceRef>,
    frame: Option<DecisionFrame>,
    /// The Agent Session that recorded this, when one did.
    session: Option<EntryId>,
    /// What kind of claim this is. Absent behaves as `Decision`.
    kind: Option<AssertionKind>,
    /// Open entries this one bears on (§4).
    answers: Option<Vec<EntryId>>,
}

pub enum AssertionKind { Finding, Decision }
```

`AssertionKind` is a kernel enum rather than `Proposal.kind`'s free string: this is generic epistemic state like `Standing`, not playbook vocabulary, and `CLAUDE.md:128` wants illegal states unrepresentable. The kernel stores it; the **app layer decides what it means for attention**, which keeps the kernel/playbook seam intact (hard constraint #5).

**Absent `kind` behaves as `Decision`.** Legacy entries keep demanding verification rather than silently ceasing to; §2's aging is what clears the historical tail. This is the conservative direction: a migration that quietly discharges obligations would be indistinguishable from losing them.

`session` implements the first half of ratified `b10ffdc6` and closes the second break in decision blame's chain that entry identified: `SessionUpdated`, `SessionCommitted` and `ArtifactAttached` target a session, while assertions floated free — so `git blame → commit → session` works after `5f45728`, but `session → the decision that session recorded` did not. **The rest of `b10ffdc6` — an agent identified by harness *and* session rather than by an email smuggling the harness in by convention — is a `Member`-shape change touching every author block and remains owed as its own slice.**

### The Scratch interaction (decided)

`ChannelStanding::Standing` requires at least one **Ratified** entry (`crates/junto-kernel/src/ledger.rs:858-864`). If findings stop being ratified, a findings-only channel stays `Scratch` — *"visible to its author only"*, filtered out of recall by the mechanism `cf5743b8` shipped to keep cheap channels cheap.

**Decision (Dan, 2026-08-23): findings do not promote a channel out of `Scratch`.** A channel that has produced no verified decision has not yet produced anything. The cost is accepted: its findings reach only their author until one decision is ratified. The alternative — findings promote — would feed unverified agent output into every agent's brief, which is precisely the risk `f1cb3110` recorded against the collapse.

## 2. Aging — projection only

A provisional assertion past a horizon leaves the attention board and renders as *recorded, unverified*. Nothing in the record changes; this is a filter in `attention_for_view` plus a quieter tier in the brief, so it is fully reversible and touches no bytes.

One constant with a doc comment, in the style of `MILESTONE_CAP` (`host.rs:1390`):

```rust
/// How long a provisional assertion stays an attention item before it
/// becomes recorded-but-unverified. A finding nobody has needed to verify
/// in this long is not waiting on anyone.
const VERIFICATION_HORIZON_DAYS: i64 = 14;
```

Start at 14 and let it be wrong cheaply. The brief keeps aged items readable — they leave the *act* list, not the record.

## 3. Ratify-by-consequence, behind a confirmation dialog

At the three moments a human is already deciding something about a channel — **approving a gate, converging, closing** — the surface offers the in-scope provisional entries for verification in the same act.

- Rows render the statement plus the entry's decision frame options (ADR 0019), so the choice is between articulated positions, not a blank checkbox.
- **`Finding` rows default ticked. `Decision` rows default unticked** and must be chosen deliberately.
- Confirming appends real `Ratification` entries authored by the human, each carrying the rationale from the chosen frame option. No new payload kind, no bulk mutation, no implicit verdict — the record shows ordinary signed acts.

Dan's dialog is what makes this option safe: on its own, ratify-by-consequence would ratify text the human may not have read, which `docs/junto.md:121` names as worse than no record. The dialog is the difference between *batching* an act and *skipping* it.

Kernel: no change. Host: one endpoint that appends N ratifications under the ledger lock. Surfaces: the dialog on both.

## 4. Answer-linking

### Retrieval, at record time

`record`'s response today is `recorded <id> in channel '<name>'`. It gains a bounded tail: the channel's **open** entries ranked against the new entry's statement + rationale, top few, one line each.

This reuses `dead_ends_markdown`'s ranker verbatim in shape (`render.rs:545-712`): `tokens()` — lowercased alphanumeric runs of 3+ characters — and IDF-weighted overlap normalised against document length, bounded by a constant. That ranker's own doc comment already names the upgrade path (*"an embedding-based `MemoryProvider` is the designed upgrade path"*), so pointing it at the living instead of the dead adds no new dependency and no new idea. Extract the shared scoring into one function used by both surfaces rather than copying it.

Zero extra calls and zero obligation: it fires at the one moment the agent has the context to judge relevance for free.

### The link

`answers: Option<Vec<EntryId>>` on `Assertion` — *"this bears on those open entries"*. The precedent is `Annotation.supersedes` (`crates/junto-kernel/src/anchor.rs:254-257`), the same same-kind link one layer down.

- An open entry renders with **"answer proposed by `<id>` (unverified)"**.
- **The link is inert until the answering entry is verified.** A claim to have answered something is itself just a claim.
- On ratifying the answer, §3's dialog offers the answered targets in the same act — with **ratify or park**, because an answer sometimes disproves the thing it answers.

### Instrument boundaries

| Relation | Instrument |
|---|---|
| Contradicts an open entry | `Correction` — supersedes the target (exists) |
| Kills it | `Park` — a human verdict (exists) |
| Bears on / resolves it | `answers` — a worker's claim, human-confirmed (new) |

### Terminology

The field is **`answers`**, never `outcome`. ADR 0025 retired `Outcome` for *the target* (what done looks like, plus its Rubric) and `Deliverable` for what a channel produced. Reusing either word here opens exactly the drift `CLAUDE.md:127` forbids when it requires that names carry the ubiquitous language.

### Risk

Lexical retrieval will suggest false candidates, and a false *"this answers that"* is worse than no link. Three gates stand between a bad suggestion and a consequence: retrieval only suggests, the link is authored with rationale, and it is inert until verified.

Second benefit, possibly larger than the first: this **mechanises the consult convention**. `9006f5f2` recorded the failure it prevents — two sessions deciding the same invariant differently — and 2026-08-23 produced another instance, with two sessions working decision-blame territory without either seeing the other's open items.

## Build sequence

| # | Slice | Proves |
|---|---|---|
| 1 | Kernel: `session`, `kind`, `answers` on `Assertion`; projection exposes them; writers set `session` and `kind` | the record can distinguish a finding from a decision, and an assertion knows its run |
| 2 | Board + brief: filter to decisions and gates, group pending by session | the load drops, and the board stops being a queue |
| 3 | Aging horizon in `attention_for_view` + the brief's quieter tier | the historical tail clears without touching the record |
| 4 | Retrieval tail on `record`; shared ranker extracted | an agent is shown what it is about to duplicate |
| 5 | Confirm-and-ratify dialog at gate approval / converge / close | one act settles a run, deliberately |

Slice 5 requires splitting `crates/junto-iced/src/main.rs` (6267 lines, one file) along its existing view boundaries — `pane_body` (:3497), `entry_card` (:4172), `artifact_body` (:4361), the settings/agents panels (:2589/:2747). That split also unblocks the surface plan's other queued items.

## Non-goals

- **Automatic verification of anything.** Every reducer either changes what is asked or batches a human act behind a dialog.
- **The rest of `b10ffdc6`** (harness-and-session agent identity). Owed, separate slice.
- **Embedding-based retrieval.** The IDF ranker is deliberately crude, local and deterministic; `MemoryProvider` is the designed upgrade path when it stops being enough.
- **Party-wide attention routing.** Per-member boards are designed (`docs/attention.md:40-44`) and out of scope here.

## Testing

- **Canonical bytes:** an `Assertion` with all three new fields absent serialises byte-identically to a pre-change one; `golden_canonical_form_is_byte_stable` passes untouched; each new field is asserted absent from the canonical JSON when `None` (the pattern of `absent_frame_leaves_canonical_bytes_unchanged`).
- **Kind default:** an assertion with no `kind` is treated as a `Decision` by attention selection — pinned, because a wrong default here silently discharges obligations.
- **Attention selection:** a `Finding` never appears as an attention item; a `Decision` does; a pending gate always does regardless of age.
- **Aging:** an assertion one day inside the horizon is an item, one day outside is not, and neither changes its `Standing`.
- **Grouping:** three assertions from one session collapse to one attention row; three from three sessions stay three.
- **Scratch:** a channel with only ratified *findings* stays `Scratch`; one ratified `Decision` promotes it to `Standing`.
- **Answer-linking:** an unverified answer leaves its target an open item; ratifying the answer offers the target and does not settle it silently; a target may be parked from the same dialog.
- **Retrieval:** ranked, bounded, deterministic — the shared ranker keeps `dead_ends`' existing ranking tests green after extraction.

## ADRs owed

Written with the implementation, citing this spec: **findings are not obligations** (`AssertionKind`, attention selection, and the `Scratch` decision) · **assertions carry their session** (the first half of `b10ffdc6`) · **answer-linking** (`answers`, its inertness until verified, and the instrument boundaries).
