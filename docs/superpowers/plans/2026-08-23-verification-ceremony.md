# Verification Ceremony — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the attention board manufacturing an obligation for every recorded finding, by distinguishing findings from decisions, aging out what nobody needed, and pairing an open question with the entry that answers it.

**Architecture:** Three additive `Option` fields on `EntryPayload::Assertion` (`session`, `kind`, `answers`) carry the new facts; every behavioural change lives in the **app layer** — `attention_for_view` decides what deserves attention, `render.rs` decides what the brief asks for, and `mcp.rs` decides what a writer is told about neighbouring open work. The kernel stores and folds; it never decides whether something needs a human.

**Tech Stack:** Rust 2024, `serde` + JCS canonical bytes, `rmcp` (MCP), `axum` (host read/write surface).

**Spec:** [`docs/superpowers/specs/2026-08-23-verification-ceremony-design.md`](../specs/2026-08-23-verification-ceremony-design.md)

## Global Constraints

- **Additive fields only, and the golden test must not move.** Every new field carries `#[serde(skip_serializing_if = "Option::is_none", default)]`. `golden_canonical_form_is_byte_stable` (`crates/junto-kernel/src/serial.rs`) must pass **untouched** — if it fails, the attribute is wrong; never update the golden.
- **Nothing is verified without a human act.** No task may append a `Ratification` that a human did not choose.
- **Absent `kind` behaves as `Decision`.** A migration that silently discharges obligations is indistinguishable from one that loses them.
- **The noun is `Channel`, never `Thread`** (ledger `febe66f2`).
- **No playbook logic in the kernel** (CLAUDE.md hard constraint #5). `AssertionKind` is generic epistemic state, like `Standing`; the *policy* that a `Finding` does not demand attention lives in `crates/junto`.
- **The field is `answers`, never `outcome`** — ADR 0025 reserved `Outcome` for the target and `Deliverable` for what a channel produced.
- **Findings do not promote a channel out of `Scratch`** (spec §1, Dan 2026-08-23). Do not touch `project_channel_standing`.
- **Commands** (repo root, PowerShell): `rtk cargo fmt --check`, `rtk cargo clippy --workspace --all-targets -- -D warnings`, `rtk cargo test --workspace`. Baseline at `5f45728` is **618 passing**.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/junto-kernel/src/entry.rs` | `AssertionKind`; three new `Assertion` fields | Modify `:180-192` (variant), add enum beside `DecisionFrame` at `:79` |
| `crates/junto-kernel/src/serial.rs` | canonical-bytes proof for the new fields | Modify `mod tests` |
| `crates/junto-kernel/src/lib.rs` | export `AssertionKind` | Modify `:40` |
| `crates/junto/src/mcp.rs` | `record` accepts `session`/`kind`/`answers`; response gains the related-open tail | Modify `RecordRequest` `:172-191`, `record` `:875` |
| `crates/junto/src/host.rs` | attention policy: findings excluded, aging, session grouping helper | Modify `attention_for_view` `:1337-1386` |
| `crates/junto/src/render.rs` | brief tiers; extract the shared ranker; the related-open surface | Modify `:143-144`, the `needs attention` section, `dead_ends_markdown` `:579` |
| `docs/adr/0039…0041` | the three ADRs owed | Create |

**Out of scope, and why:** the confirm-and-ratify dialog (spec §3) is **not in this plan**. It needs the 6267-line `crates/junto-iced/src/main.rs` split along its view boundaries first, which is a refactor with its own risk profile and its own reviewer gate. It gets its own plan; Tasks 1–6 here are shippable and useful without it.

---

### Task 1: `AssertionKind` and the three new fields

**Files:**
- Modify: `crates/junto-kernel/src/entry.rs:79` (add enum before `DecisionFrame`), `:180-192` (the variant)
- Modify: `crates/junto-kernel/src/lib.rs:40` (export)
- Test: `crates/junto-kernel/src/serial.rs` (`mod tests`)

**Interfaces:**
- Produces: `junto_kernel::AssertionKind::{Finding, Decision}`; `EntryPayload::Assertion { statement, rationale, provenance, frame, session: Option<EntryId>, kind: Option<AssertionKind>, answers: Option<Vec<EntryId>> }`

- [ ] **Step 1: Write the failing tests**

In `crates/junto-kernel/src/serial.rs`, inside `mod tests`, add to `round_trips_every_payload_kind` right after the existing `Assertion` assertions, and add one new test:

```rust
        // The new assertion facts: a finding that names its session and the
        // open entry it answers, and the all-absent case that must keep
        // pre-change bytes.
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "the ranker is reusable".into(),
            rationale: "IDF overlap, no deps".into(),
            provenance: vec![],
            frame: None,
            session: Some(target),
            kind: Some(crate::AssertionKind::Finding),
            answers: Some(vec![target]),
        }));
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "no new facts".into(),
            rationale: "legacy shape".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        }));
```

```rust
    #[test]
    fn absent_assertion_facts_leave_canonical_bytes_unchanged() {
        // Same additive rule as `frame` and `ChannelOpened::name`: an entry
        // that carries none of the new facts must serialise exactly as it did
        // before they existed, or every pre-change signature breaks.
        let e = entry(EntryPayload::Assertion {
            statement: "plain".into(),
            rationale: "plain".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(!json.contains("\"session\""), "{json}");
        assert!(!json.contains("\"kind\""), "{json}");
        assert!(!json.contains("\"answers\""), "{json}");
    }

    #[test]
    fn assertion_kind_is_snake_case_on_the_wire() {
        // The record is read by humans and by other tools; "finding" is the
        // wire form, not "Finding".
        let e = entry(EntryPayload::Assertion {
            statement: "x".into(),
            rationale: "y".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: Some(crate::AssertionKind::Finding),
            answers: None,
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(json.contains("\"kind\":\"finding\""), "{json}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto-kernel --lib serial`
Expected: FAIL — `struct variant EntryPayload::Assertion has no field named session`, and `cannot find type AssertionKind`.

- [ ] **Step 3: Write the minimal implementation**

In `crates/junto-kernel/src/entry.rs`, immediately before `pub struct DecisionFrame` at `:79`:

```rust
/// What kind of claim an [`EntryPayload::Assertion`] is making.
///
/// The kernel stores this and folds nothing from it: what it *means* for a
/// human's attention is an app-layer policy (`crates/junto`'s
/// `attention_for_view`), because "does this deserve a verdict" is a product
/// question and this crate is playbook-agnostic.
///
/// **Absent is read as [`Decision`](AssertionKind::Decision)** by every
/// consumer, so entries written before this field existed keep asking for a
/// verdict rather than silently ceasing to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionKind {
    /// An observation, measurement, or discovered fact. Worth recording and
    /// citing; it does not by itself ask anyone to decide anything.
    Finding,
    /// A choice, or a claim asserted for others to rely on. Wants a verdict.
    Decision,
}
```

Then replace the `Assertion` variant's field list (`:180-192`) — keep `statement`, `rationale`, `provenance`, `frame` exactly as they are and append:

```rust
        /// The Agent Session that recorded this, when one did — the
        /// `SessionStarted` entry's id. Closes the `session -> decision` arrow
        /// decision blame needs (ledger `b10ffdc6`): before this, only
        /// `SessionUpdated`, `SessionCommitted` and `ArtifactAttached` named a
        /// session, so an assertion floated free of the run that produced it.
        /// Omitted from the canonical bytes when absent.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        session: Option<EntryId>,
        /// Whether this is a finding or a decision. Absent reads as
        /// [`AssertionKind::Decision`]. Omitted from the canonical bytes when
        /// absent.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        kind: Option<AssertionKind>,
        /// Open entries this one bears on — a *claim* to have answered them,
        /// inert until this entry is itself verified. Omitted from the
        /// canonical bytes when absent.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        answers: Option<Vec<EntryId>>,
```

In `crates/junto-kernel/src/lib.rs:40`, add `AssertionKind` to the `entry` re-export:

```rust
pub use entry::{AssertionKind, DecisionFrame, EntryPayload, FrameAct, FrameOption, LedgerEntry};
```

- [ ] **Step 4: Fix every construction site the compiler names**

Run: `rtk cargo check --workspace --all-targets`

`Assertion` is a struct variant, so **every construction site must name the new fields**. Add `session: None, kind: None, answers: None` at each one the compiler reports — do not guess the list by reading, and do not widen any `match` pattern that already uses `..`. Expect sites in `crates/junto/src/mcp.rs`, `crates/junto/src/web.rs`, and test modules across `ledger.rs`, `render.rs`, `host.rs`, `web.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS. `golden_canonical_form_is_byte_stable` must be green **without being edited** — confirm with `rtk git diff --stat -- crates/junto-kernel/src/serial.rs` showing insertions only.

- [ ] **Step 6: Commit**

```bash
git add crates/junto-kernel/src/entry.rs crates/junto-kernel/src/lib.rs crates/junto-kernel/src/serial.rs crates/junto/src/mcp.rs crates/junto/src/web.rs
git commit -m "feat(kernel): assertions carry their session, kind, and answers"
```

---

### Task 2: The write surface sets them

**Files:**
- Modify: `crates/junto/src/mcp.rs:172-191` (`RecordRequest`), `:875` (`record`)
- Test: `crates/junto/src/mcp.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 1's `AssertionKind` and the three fields.
- Produces: `RecordRequest { …, session: Option<String>, kind: Option<String>, answers: Option<Vec<String>> }`; a `finding`/`decision` string is parsed to `AssertionKind`, anything else is a refusal.

- [ ] **Step 1: Write the failing tests**

In `crates/junto/src/mcp.rs`, inside `mod tests`:

```rust
    #[tokio::test]
    async fn a_recorded_finding_carries_its_kind_and_session() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: author_param(),
                code: code(&dirs),
                statement: "the ranker is reusable".into(),
                rationale: "IDF overlap, no new deps".into(),
                provenance: None,
                frame: None,
                session: None,
                kind: Some("finding".into()),
                answers: None,
            }))
            .await
            .expect("recorded");
        let id = entry_id_of(&recorded);
        let view = view_of(&mcp, &dirs, "junto-dev").await;
        let entry = view
            .entries
            .iter()
            .find(|e| e.id.to_string() == id)
            .expect("the entry projects");
        let EntryPayload::Assertion { kind, .. } = &entry.payload else {
            panic!("expected an assertion");
        };
        assert_eq!(*kind, Some(junto_kernel::AssertionKind::Finding));
    }

    #[tokio::test]
    async fn an_unknown_assertion_kind_is_refused_rather_than_defaulted() {
        // Silently coercing a typo to `decision` would put the entry in the
        // attention queue the author was trying to stay out of.
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: author_param(),
                code: code(&dirs),
                statement: "x".into(),
                rationale: "y".into(),
                provenance: None,
                frame: None,
                session: None,
                kind: Some("observation".into()),
                answers: None,
            }))
            .await
            .expect_err("unknown kind must be refused");
        assert!(
            format!("{err:?}").contains("finding"),
            "the refusal must name the accepted values: {err:?}"
        );
    }
```

If `entry_id_of`, `view_of`, `author_param` or `code` do not already exist in this test module under those names, use whatever the neighbouring tests (`record_then_view_shows_the_assertion` at `:1611`) use — do not add duplicates.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto mcp::tests::a_recorded_finding`
Expected: FAIL — `RecordRequest has no field named kind`.

- [ ] **Step 3: Write the minimal implementation**

Append to `RecordRequest` (`:190`, after `frame`):

```rust
    /// The Agent Session recording this, if one is (the id `start_session`
    /// returned). Binds the claim to the run that produced it.
    pub session: Option<String>,
    /// `"finding"` (an observation — recorded and citable, asks nobody to
    /// decide) or `"decision"` (a choice, or a claim for others to rely on —
    /// wants a verdict). Omitted behaves as `"decision"`.
    pub kind: Option<String>,
    /// Ids of open entries this one bears on. A claim to have answered them,
    /// inert until this entry is itself verified.
    pub answers: Option<Vec<String>>,
```

In `record` (`:875`), parse them before building the payload:

```rust
        let kind = match req.kind.as_deref() {
            None => None,
            Some("finding") => Some(junto_kernel::AssertionKind::Finding),
            Some("decision") => Some(junto_kernel::AssertionKind::Decision),
            Some(other) => {
                return Err(McpError::invalid_params(
                    format!("unknown kind '{other}' — use \"finding\" or \"decision\""),
                    None,
                ));
            }
        };
        let session = match req.session.as_deref() {
            None => None,
            Some(raw) => Some(raw.parse::<EntryId>().map_err(|_| {
                McpError::invalid_params(format!("session '{raw}' is not an entry id"), None)
            })?),
        };
        let answers = match req.answers {
            None => None,
            Some(raw) => Some(
                raw.iter()
                    .map(|id| {
                        id.parse::<EntryId>().map_err(|_| {
                            McpError::invalid_params(format!("answers '{id}' is not an entry id"), None)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
```

and pass `session`, `kind`, `answers` into the `EntryPayload::Assertion` it already constructs. Extend the tool's `description` so an agent learns the vocabulary: state that a finding does not ask for a verdict, that a decision does, and that `answers` links the open entries this one bears on.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto mcp`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/mcp.rs
git commit -m "feat(mcp): record accepts session, kind, and answers"
```

---

### Task 3: Findings leave the attention board

**Files:**
- Modify: `crates/junto/src/host.rs:1337-1386` (`attention_for_view`)
- Test: `crates/junto/src/host.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 1's fields.
- Produces: `attention_for_view(id, view, now: Timestamp) -> AttentionGroup` — **signature changes**, callers pass `Timestamp::now()`.

- [ ] **Step 1: Write the failing tests**

In `crates/junto/src/host.rs`, inside `mod tests`:

```rust
    fn assertion_of(kind: Option<junto_kernel::AssertionKind>, session: Option<EntryId>) -> EntryPayload {
        EntryPayload::Assertion {
            statement: "s".into(),
            rationale: "r".into(),
            provenance: vec![],
            frame: None,
            session,
            kind,
            answers: None,
        }
    }

    #[tokio::test]
    async fn a_finding_is_not_an_attention_item() {
        let view = view_with(vec![
            assertion_of(Some(junto_kernel::AssertionKind::Finding), None),
            assertion_of(Some(junto_kernel::AssertionKind::Decision), None),
        ]);
        let group = attention_for_view(&ChannelId::new(), &view, Timestamp::now());
        assert_eq!(
            group.items.len(),
            1,
            "only the decision deserves a verdict: {:?}",
            group.items
        );
    }

    #[tokio::test]
    async fn an_assertion_with_no_kind_still_asks_for_a_verdict() {
        // Legacy entries must not be silently discharged.
        let view = view_with(vec![assertion_of(None, None)]);
        let group = attention_for_view(&ChannelId::new(), &view, Timestamp::now());
        assert_eq!(group.items.len(), 1);
    }
```

`view_with` must build a `ChannelView` whose `standings` mark each assertion `Provisional` — mirror whatever the existing `render.rs` test helper at `render.rs:3872` does (`standings.insert(e.id, Standing::Provisional)`); if `host.rs`'s test module has no such helper, add one there rather than reaching across crates.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto a_finding_is_not_an_attention_item`
Expected: FAIL — `attention_for_view` takes 2 arguments, and the finding is still counted.

- [ ] **Step 3: Write the minimal implementation**

Change the assertion arm at `:1363-1370` to consult the kind, and add the helper below the function:

```rust
            EntryPayload::Assertion { kind, .. }
                if view.standing(&entry.id) == Some(junto_kernel::Standing::Provisional)
                    && !matches!(kind, Some(junto_kernel::AssertionKind::Finding)) =>
            {
                verifications.push(AttentionItem {
                    kind: AttentionKind::Verification,
                    entry: entry.clone(),
                });
            }
```

Session grouping is deliberately **not** added here. It needs a consumer to be worth anything, and its only consumer is the confirm-and-ratify dialog in the next plan — a `pub fn` with no caller is the unverified-reachability pattern ledger `2982d91a` names, so the grouping helper lands in the plan that uses it, reading `session` straight off the payload.

Add the `now: Timestamp` parameter to `attention_for_view` (unused until Task 4 — take it now so the callers change once), and update its callers: `Host::attention`/`overview` in this file and the channel page's attention strip in `crates/junto/src/render.rs`. The compiler names them.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/host.rs crates/junto/src/render.rs
git commit -m "feat(host): findings are recorded, not queued"
```

---

### Task 4: Aging

**Files:**
- Modify: `crates/junto/src/host.rs` (`attention_for_view`)
- Test: `crates/junto/src/host.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task 3's `now: Timestamp` parameter.
- Produces: `const VERIFICATION_HORIZON_DAYS: i64 = 14;`

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn a_provisional_decision_ages_off_the_board_but_keeps_its_standing() {
        let view = view_with(vec![assertion_of(
            Some(junto_kernel::AssertionKind::Decision),
            None,
        )]);
        let entry_at = view.entries[0].timestamp.as_millis();
        let inside = Timestamp::from_millis(entry_at + 13 * 24 * 60 * 60 * 1000);
        let outside = Timestamp::from_millis(entry_at + 15 * 24 * 60 * 60 * 1000);

        assert_eq!(
            attention_for_view(&ChannelId::new(), &view, inside).items.len(),
            1,
            "inside the horizon it still asks"
        );
        assert!(
            attention_for_view(&ChannelId::new(), &view, outside).items.is_empty(),
            "outside the horizon it stops asking"
        );
        assert_eq!(
            view.standing(&view.entries[0].id),
            Some(junto_kernel::Standing::Provisional),
            "aging is a projection filter, never a change of standing"
        );
    }

    #[tokio::test]
    async fn a_pending_gate_never_ages_out() {
        // A gate blocks its proposer; time does not unblock them.
        let view = view_with_pending_gate();
        let entry_at = view.entries[0].timestamp.as_millis();
        let outside = Timestamp::from_millis(entry_at + 400 * 24 * 60 * 60 * 1000);
        assert_eq!(
            attention_for_view(&ChannelId::new(), &view, outside).items.len(),
            1
        );
    }
```

If `Timestamp` has no `as_millis`/`from_millis` pair under those names, use the accessors `crates/junto-kernel/src/time.rs` actually exposes; `from_millis` is already used by `render.rs`'s tests.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto ages_off_the_board`
Expected: FAIL — the aged item is still returned.

- [ ] **Step 3: Write the minimal implementation**

Beside `MILESTONE_CAP` (`host.rs:1390`):

```rust
/// How long a provisional assertion stays an attention item before it becomes
/// recorded-but-unverified. A claim nobody has needed to verify in this long
/// is not waiting on anyone, and `docs/attention.md:85` is explicit that the
/// board is "not a queue".
///
/// This is a **projection filter only**: the entry keeps its `Provisional`
/// standing, stays in the brief's record, and can still be ratified whenever
/// someone wants to. Gates are exempt — a pending gate blocks its proposer,
/// and time does not unblock them.
const VERIFICATION_HORIZON_DAYS: i64 = 14;
```

Filter `verifications` before extending, using the `now` parameter Task 3 added:

```rust
    let horizon_ms = VERIFICATION_HORIZON_DAYS * 24 * 60 * 60 * 1000;
    verifications.retain(|item| now.as_millis() - item.entry.timestamp.as_millis() <= horizon_ms);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/host.rs
git commit -m "feat(host): provisional assertions age off the attention board"
```

---

### Task 5: The brief's tiers, and the shared ranker

**Files:**
- Modify: `crates/junto/src/render.rs:143-144` (the brief's shape), the `needs attention` section, and `dead_ends_markdown` at `:579`
- Test: `crates/junto/src/render.rs` (`mod tests`)

**Interfaces:**
- Produces: `fn rank_by_overlap<T: Copy>(query: &str, candidates: &[(T, String)], limit: usize) -> Vec<T>`; `pub fn related_open_markdown(view: &ChannelView, about: &str, exclude: EntryId) -> Option<String>`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_brief_asks_about_decisions_and_merely_records_findings() {
        let view = view_with(vec![
            assertion(Some(AssertionKind::Decision), "decide this"),
            assertion(Some(AssertionKind::Finding), "just noticed this"),
        ]);
        let brief = brief_markdown("c", &ChannelId::new(), &view, None);
        let needs = brief
            .split("## needs attention")
            .nth(1)
            .expect("the section exists")
            .split("\n## ")
            .next()
            .unwrap();
        assert!(needs.contains("decide this"), "{needs}");
        assert!(
            !needs.contains("just noticed this"),
            "a finding must not be in the act list: {needs}"
        );
        assert!(
            brief.contains("just noticed this"),
            "but it must still be readable somewhere in the brief"
        );
    }

    #[test]
    fn related_open_items_are_ranked_and_bounded() {
        // Eight open decisions; a query about websockets must rank the
        // websocket one first and return no more than the bound.
        let view = view_with_open_decisions(&[
            "the websocket handshake needs an ed25519 challenge",
            "subject uris are compared exactly",
            "gates execute on approval",
            "names stop being unique",
            "the ledger is append-only",
            "worktrees isolate a session",
            "presence rides an ephemeral store",
            "artifacts carry digests",
        ]);
        let out = related_open_markdown(&view, "websocket challenge handshake", EntryId::new())
            .expect("some open items are related");
        let first = out.lines().find(|l| l.starts_with("- ")).unwrap();
        assert!(first.contains("websocket"), "{out}");
        assert!(out.lines().filter(|l| l.starts_with("- ")).count() <= 3, "{out}");
    }

    #[test]
    fn the_entry_being_recorded_is_never_its_own_related_item() {
        let view = view_with_open_decisions(&["the ledger is append-only"]);
        let self_id = view.entries[0].id;
        assert!(related_open_markdown(&view, "append-only ledger", self_id).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto render::tests::related_open`
Expected: FAIL — `cannot find function related_open_markdown`.

- [ ] **Step 3: Extract the ranker, then build on it**

`dead_ends_markdown` (`:579`) currently inlines the IDF scoring at `:637-668`. Lift exactly that scoring into a free function beside `tokens()` (`:553`), and have `dead_ends_markdown` call it — its existing ranking tests (`dead_ends_are_ranked_and_bounded`) are the proof the extraction is faithful and **must not be edited**:

```rust
/// IDF-weighted token overlap, normalised against document length so long
/// candidates do not win by surface area, returning the best `limit`
/// candidates in descending score order. Ties keep input order, so the result
/// is deterministic on every replica.
///
/// Crude next to embeddings, but local, deterministic and dependency-free —
/// `MemoryProvider` is the designed upgrade path (`docs/pluggability.md`).
fn rank_by_overlap<T: Copy>(query: &str, candidates: &[(T, String)], limit: usize) -> Vec<T> {
    let query = tokens(query);
    if query.is_empty() || candidates.is_empty() {
        return Vec::new();
    }
    let docs: Vec<std::collections::HashSet<String>> =
        candidates.iter().map(|(_, text)| tokens(text)).collect();
    let total = docs.len();
    let idf = |token: &str| {
        let with = docs.iter().filter(|doc| doc.contains(token)).count();
        ((1.0 + total as f64) / (1.0 + with as f64)).ln() + 1.0
    };
    let mut scored: Vec<(f64, T)> = candidates
        .iter()
        .zip(&docs)
        .filter_map(|((item, _), doc)| {
            let score: f64 = query
                .iter()
                .filter(|token| doc.contains(*token))
                .map(|token| idf(token))
                .sum();
            (score > 0.0).then_some((score / (doc.len() as f64).sqrt().max(1.0), *item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, item)| item).collect()
}
```

Then the new surface:

```rust
/// The most related open items one `record` response carries.
const RELATED_OPEN_LIMIT: usize = 3;

/// The open entries a newly recorded one may bear on — provisional
/// assertions and pending gates, ranked against the new entry's own text.
///
/// This is [`dead_ends_markdown`]'s ranker pointed at the living instead of
/// the dead: it costs the writer nothing, fires at the one moment the context
/// is already loaded, and mechanises the consult convention that ledger
/// `9006f5f2` recorded the cost of skipping. Suggestion only — the link is
/// authored with `answers`, never inferred here.
pub fn related_open_markdown(
    view: &ChannelView,
    about: &str,
    exclude: EntryId,
) -> Option<String> {
    let candidates: Vec<(&LedgerEntry, String)> = view
        .entries
        .iter()
        .filter(|entry| entry.id != exclude)
        .filter_map(|entry| match &entry.payload {
            EntryPayload::Assertion { statement, rationale, .. }
                if view.standing(&entry.id) == Some(Standing::Provisional) =>
            {
                Some((entry, format!("{statement} {rationale}")))
            }
            EntryPayload::Proposal { action, .. }
                if view.gate_status(&entry.id) == Some(GateStatus::Pending) =>
            {
                Some((entry, action.clone()))
            }
            _ => None,
        })
        .collect();
    let ranked = rank_by_overlap(about, &candidates, RELATED_OPEN_LIMIT);
    if ranked.is_empty() {
        return None;
    }
    let mut out = String::from("\n\nrelated open items — link with `answers` if this settles one:\n");
    for entry in ranked {
        out.push_str(&format!(
            "- `{}` {}\n",
            entry.id,
            clamp(&summarize_open(entry), 100)
        ));
    }
    Some(out)
}
```

`summarize_open` is a two-arm helper returning the statement for an assertion and the action for a proposal; `clamp` already exists in this file. For the brief, change `:143-144` so a `Finding` lands in a new `shape.findings` bucket instead of `shape.open`, and render that bucket under a `## recorded, unverified` heading after `## needs attention` — same one-line-per-entry shape as the `recently` tail, ids included so they remain actable.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS, including the untouched `dead_ends_are_ranked_and_bounded`.

- [ ] **Step 5: Wire the tail into `record`**

In `crates/junto/src/mcp.rs`'s `record`, after the append succeeds, project the channel and append the tail to the confirmation text:

```rust
        let tail = crate::render::related_open_markdown(
            &view,
            &format!("{} {}", req.statement, req.rationale),
            id,
        )
        .unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "recorded {id} in channel '{name}'{tail}"
        ))]))
```

Add one test asserting a second `record` whose text matches the first mentions the first entry's id in its response.

- [ ] **Step 6: Full green, then commit**

```bash
rtk cargo fmt --check
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
git add crates/junto/src/render.rs crates/junto/src/mcp.rs
git commit -m "feat(render): brief tiers findings, and record surfaces related open items"
```

---

### Task 6: Dogfood, then the ADRs

**Files:**
- Create: `docs/adr/0039-findings-are-not-obligations.md`, `docs/adr/0040-assertions-carry-their-session.md`, `docs/adr/0041-answer-linking.md`
- Modify: `CLAUDE.md` (the consult/record convention gains the finding/decision distinction)

- [ ] **Step 1: Dogfood it against the running host**

Restart the singleton (`cargo run -p junto -- serve` — it rebuilds first; the host serves a **stale binary** otherwise, ledger `779ad00e`). Then, in channel `3c38ead9-4907-4646-99b7-23b21933da35`: record a finding and confirm it does **not** appear in `/focus.json`; record a decision and confirm it does; record a second finding whose text overlaps the first and confirm the response names the first; check the brief shows both under `recorded, unverified`.

- [ ] **Step 2: Record the dogfood result**

Record an assertion — `kind: "finding"`, `session` set — stating what was run and observed. If anything failed, record that instead: a dogfood that found a bug is the more valuable entry.

- [ ] **Step 3: Write ADR 0039 — findings are not obligations**

Cover: the board had become the queue `attention.md:85` forbids; `AssertionKind` as kernel state with app-layer policy; absent-reads-as-`Decision` and why the conservative default; aging as a projection filter that never touches standing; and the decision that findings do **not** promote a channel out of `Scratch`, with the cost accepted.

- [ ] **Step 4: Write ADR 0040 — assertions carry their session**

Cover: the `session -> decision` arrow `b10ffdc6` identified, why the artifact arrow existed and the decision arrow did not, what this unblocks for decision blame's query slice, and the explicitly deferred half (an agent identified by harness *and* session).

- [ ] **Step 5: Write ADR 0041 — answer-linking**

Cover: `answers` as an authored claim rather than an inferred link; inertness until the answering entry is verified; the instrument boundaries against `Correction` and `Park`; the terminology ruling that reserves `Outcome`; and retrieval as suggestion only, with the three gates standing between a false suggestion and a consequence.

- [ ] **Step 6: Full green, then commit**

```bash
rtk cargo fmt --check
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
git add docs/adr CLAUDE.md
git commit -m "docs(adr): findings, sessions on assertions, and answer-linking"
```

---

## Self-Review

**Spec coverage.** §1 findings-not-obligations → Tasks 1, 2, 3 (kernel, writer, policy). §1 `session` link → Tasks 1, 2. §1 Scratch decision → **no code change by design** (a Global Constraint forbids touching `project_channel_standing`), documented in Task 6's ADR 0039 and pinned by the spec's testing section. §2 aging → Task 4. §3 ratify-by-consequence → **explicitly out of scope**, own plan, stated under File Structure. §4 retrieval → Task 5. §4 the `answers` field → Task 1 (field) and Task 2 (writer); **its rendering on the answered entry is not covered here** — the open entry does not yet show "answer proposed by `<id>`", because that is a surface change belonging with the dialog plan. Recording this gap rather than pretending otherwise: `answers` lands as durable data and a `record`-time nudge in this plan, and becomes visible in the surface in the next one.

**Placeholder scan.** No TBD/TODO. Every code step carries the code. Two steps deliberately delegate a name to the codebase rather than guessing — the test helpers in Task 2 Step 1 and Task 4 Step 1 — and both say exactly which existing neighbour to copy, which is the honest form of "match the surrounding code" rather than a placeholder.

**Type consistency.** `AssertionKind::{Finding, Decision}` is spelled identically in Tasks 1–5. `attention_for_view(id, view, now)` gains its third parameter in Task 3 and is used with three arguments in Task 4's tests. `rank_by_overlap<T: Copy>(query, candidates, limit) -> Vec<T>` is defined in Task 5 Step 3 and used only there. `related_open_markdown(view, about, exclude) -> Option<String>` matches its call site in Task 5 Step 5. One defect found and fixed inline: Task 3 originally produced a `pub fn assertion_session` that no task in this plan consumed. It is removed — session grouping moves to the plan that has a consumer for it, since shipping an uncalled `pub fn` is the exact defect `2982d91a` describes.
