# Subject, Mount, and the Collapse — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A channel can be about zero, one, or many typed Subjects — a repo, a document, a ticket — and can be opened with no ceremony and no repo at all, while the ledger stays append-only and machine paths stay out of it.

**Architecture:** Split today's `Workspace` into a durable, portable **Subject** (`{kind, uri, digest?}`, recorded as ledger entries, synced) and a machine-local **Mount** (`~/.junto/mounts.toml`, how *this* machine resolves a Subject to a path, never synced). Capabilities are computed from `(kind, provider, mount)` at use time, never recorded. `ChannelOpened.name` becomes optional using the exact `Option` + `skip_serializing_if` pattern ADR 0033 used for `signature`, so existing canonical bytes are unchanged. Channel standing (`scratch | standing | settled`) is derived by projection from entries already in the ledger.

**Tech Stack:** Rust 2024, `serde` + `toml`, `serde_json_canonicalizer` (JCS), `thiserror` in `junto-kernel`, `anyhow` in `junto`, `tempfile` + `dunce` in tests.

**Spec:** [`docs/superpowers/specs/2026-08-21-multiplayer-first-rethink-design.md`](../specs/2026-08-21-multiplayer-first-rethink-design.md) §1 and §2.

**Out of scope — separate plans:** the native surface (§5; `junto-iced` is a separate workspace), and everything in Track B (§3, §4 — transport, ownership, handoff, `DocAnchor`).

## Global Constraints

- **The noun is `Channel`, never `Thread`** — in code, entries, docs, and UI copy (spec §2, ledger `febe66f2`). Where the spec's prose says "thread" it means "a channel after the collapse".
- **No vendor name reaches the kernel.** Branch on capability flags only (CLAUDE.md hard constraint #4).
- **No `unwrap()` / `expect()` / `panic!` in library code.** Return `Result`. Fine in tests.
- **Machine paths never enter the ledger** (`domain-model.md:32`). Subjects are portable URIs; Mounts are machine-local.
- **The durable record stays append-only, no CRDT** (hard constraint #3, ADR 0011). New entry kinds are additive; nothing mutates.
- **Existing canonical bytes must not change.** Any new optional field uses `#[serde(skip_serializing_if = "Option::is_none", default)]`, exactly as `LedgerEntry::signature` does at `crates/junto-kernel/src/entry.rs:34`.
- **Test naming is a full-sentence assertion**, e.g. `non_git_workspaces_are_refused`, `unverified_is_a_surfaced_fact_not_a_drop`. Tests are inline `#[cfg(test)] mod tests` in the file under test.
- **Pre-commit, in order, stop on first failure:** `cargo fmt --check`, then `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace`. Prefix with `rtk` per the repo convention.
- **Windows + macOS are equal targets.** Use `std::path::Path`/`PathBuf` and `join`, never string concatenation. Use `dunce::canonicalize`, as `remember_workspace` already does.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/junto-kernel/src/subject.rs` | `SubjectKind`, `Subject` — the durable, portable noun | **create** |
| `crates/junto-kernel/src/lib.rs` | module declaration + re-export | modify (`:24-52`) |
| `crates/junto-kernel/src/entry.rs` | `SubjectAttached` / `SubjectDetached` variants; `target()` arm; `ChannelOpened.name` optional | modify (`:101-105`, `:301-314`, `:345-369`) |
| `crates/junto-kernel/src/ledger.rs` | `ChannelView::subjects`, `project_subjects`, `ChannelStanding`, `project_channel_standing`; `project_standings` continue-arm | modify (`:140-212`, `:760-803`) |
| `crates/junto-kernel/src/serial.rs` | round-trip coverage for the new variants | modify (`:81-230`) |
| `crates/junto/src/mounts.rs` | the Mount store — `Mount`, `mounts_for`, `remember_mount`, `capabilities` | **create** |
| `crates/junto/src/launch.rs` | delete the Workspace store; call Mounts instead; scratch-dir sessions | modify (`:405-504`, `:2733`) |
| `crates/junto/src/host.rs` | `open_channel` accepts an absent name; name resolution stops requiring uniqueness; **`preview()`'s entry-kind match** (`:1261`) | modify |
| `crates/junto/src/render.rs` | **entry-kind display/badge matches** (`:481`, `:734`, `:2575`, `:2601`) — provisional copy for the two new kinds | modify |
| `crates/junto/src/web.rs` | call-site cutover (`:581, 681, 695, 698, 789, 1921`); **`EntryDto::from_entry`'s entry-kind match** (`:2047`) | modify |
| `crates/junto/src/main.rs` | module declaration | modify |

Subjects live in the kernel because they are recorded; Mounts live in `crates/junto` because they are machine config, the same seam `workspaces.toml` already sits on (ADR 0020).

**Correction, found during execution (ledger ruling, Task 2):** the bolded rows above were missing from this table's first draft. `EntryPayload` matches are exhaustive, so adding a variant breaks compilation at **six** sites in crate `junto` that render entries for humans — and unlike the projection folds, those arms need actual copy, not an inert `continue`. They are a compile obligation of Task 2 and land in its diff with deliberately plain provisional copy, each marked `// Provisional copy — the surface plan owns subject rendering.` The surface plan owns their design. Anyone widening `EntryPayload` again should expect this blast radius: four kernel sites plus six rendering sites.

---

### Task 1: The `Subject` kernel noun

**Files:**
- Create: `crates/junto-kernel/src/subject.rs`
- Modify: `crates/junto-kernel/src/lib.rs:24-52`

**Interfaces:**
- Consumes: `crate::provenance::{ContentDigest, Uri}` — both already exist and are re-exported at `lib.rs:48`.
- Produces: `pub enum SubjectKind { Repo, Document }`; `pub struct Subject { pub kind: SubjectKind, pub uri: Uri, pub digest: Option<ContentDigest> }`; `Subject::new(kind, uri) -> Self`; `Subject::with_digest(kind, uri, digest) -> Self`. Tasks 2, 3, 5 depend on these exact names.

- [ ] **Step 1: Write the failing test**

Create `crates/junto-kernel/src/subject.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentDigest, Uri};

    #[test]
    fn a_subject_carries_kind_uri_and_optional_digest() {
        let uri = Uri::new("git+https://github.com/dcieslak19973/junto.git").expect("valid uri");
        let subject = Subject::new(SubjectKind::Repo, uri.clone());
        assert_eq!(subject.kind, SubjectKind::Repo);
        assert_eq!(subject.uri, uri);
        assert!(subject.digest.is_none());
    }

    #[test]
    fn a_document_subject_pins_the_version_it_was_attached_at() {
        let uri = Uri::new("file:///notes/spec.md").expect("valid uri");
        let digest = ContentDigest::new("sha256:deadbeef").expect("valid digest");
        let subject = Subject::with_digest(SubjectKind::Document, uri, digest.clone());
        assert_eq!(subject.kind, SubjectKind::Document);
        assert_eq!(subject.digest, Some(digest));
    }

    #[test]
    fn subject_kinds_round_trip_through_json() {
        for kind in [SubjectKind::Repo, SubjectKind::Document] {
            let text = serde_json::to_string(&kind).expect("serialize");
            let parsed: SubjectKind = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(kind, parsed);
        }
    }
}
```

`serde_json` is a direct dependency of `junto-kernel` (`crates/junto-kernel/Cargo.toml`) — verified, so this test needs no `Cargo.toml` change.

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto-kernel subject`
Expected: FAIL — `cannot find type Subject in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `crates/junto-kernel/src/subject.rs`:

```rust
//! **Subjects** — what a Channel is *about* (`docs/adr/0014`, and the spec at
//! `docs/superpowers/specs/2026-08-21-multiplayer-first-rethink-design.md` §1).
//!
//! A Subject is durable and **portable**: it names a thing by URI, never by a
//! path on somebody's disk. How *this* machine resolves a Subject to something
//! it can read or run in is a **Mount**, which is machine-local config and
//! never enters the ledger (`domain-model.md:32`).
//!
//! A channel may have zero, one, or many Subjects. A git repo is simply the
//! kind that supports every capability; a document supports fewer, and says so.

use serde::{Deserialize, Serialize};

use crate::provenance::{ContentDigest, Uri};

/// The kinds of thing a Channel can be about.
///
/// Deliberately a closed enum in the kernel: adding a kind is a kernel change,
/// because each kind's capabilities are kernel-visible. The *providers* that
/// reach these things (a forge, a chat connector, a knowledge connector) stay
/// behind adapters — no vendor name appears here (constraint #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubjectKind {
    /// A git repository. The only kind that can be executed in and diffed.
    Repo,
    /// A document: a file, a wiki page, a spec. Readable and anchorable,
    /// never executable.
    Document,
}

/// One thing a Channel is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    /// What kind of thing this is.
    pub kind: SubjectKind,
    /// Where it lives, machine-independently.
    pub uri: Uri,
    /// Its content digest as of attachment, so later drift is detectable.
    /// Absent for subjects with no stable content hash (a live repo).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<ContentDigest>,
}

impl Subject {
    /// A subject with no pinned version.
    #[must_use]
    pub fn new(kind: SubjectKind, uri: Uri) -> Self {
        Self {
            kind,
            uri,
            digest: None,
        }
    }

    /// A subject pinned to the content it had when it was attached.
    #[must_use]
    pub fn with_digest(kind: SubjectKind, uri: Uri, digest: ContentDigest) -> Self {
        Self {
            kind,
            uri,
            digest: Some(digest),
        }
    }
}
```

Then in `crates/junto-kernel/src/lib.rs`, add `pub mod subject;` to the module list (alphabetically, after `pub mod substrate;` is wrong — it sorts before it, so place it between `pub mod sign;` at `:34` and `pub mod substrate;` at `:35`), and add the re-export after the `session` re-export at `:49`:

```rust
pub use subject::{Subject, SubjectKind};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `rtk cargo test -p junto-kernel subject`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel/src/subject.rs crates/junto-kernel/src/lib.rs
git commit -m "feat(kernel): the Subject noun — what a channel is about"
```

---

### Task 2: `SubjectAttached` and `SubjectDetached` entry kinds

**Files:**
- Modify: `crates/junto-kernel/src/entry.rs:301-314` (add variants after `ArtifactAttached`), `:345-369` (`target()`)
- Modify: `crates/junto-kernel/src/ledger.rs:778-793` (the `project_standings` continue arm)
- Modify: `crates/junto-kernel/src/serial.rs:81-230` (round-trip coverage)

**Interfaces:**
- Consumes: `Subject` from Task 1; `EntryId` from `crate::ids`.
- Produces: `EntryPayload::SubjectAttached { subject: Subject }` and `EntryPayload::SubjectDetached { target: EntryId }`. Task 3 folds these.

**Why both `entry.rs` and `ledger.rs` in one task:** `EntryPayload` matches are exhaustive, so adding a variant breaks compilation in every match arm at once. The task is not independently testable until all arms compile.

- [ ] **Step 1: Write the failing test**

In `crates/junto-kernel/src/serial.rs`, inside `mod tests`, add to `round_trips_every_payload_kind` (after the `ArtifactAttached` assertion) and add one new test:

```rust
        assert_round_trips(&entry(EntryPayload::SubjectAttached {
            subject: crate::Subject::new(
                crate::SubjectKind::Repo,
                Uri::new("git+https://github.com/dcieslak19973/junto.git").expect("valid uri"),
            ),
        }));
        assert_round_trips(&entry(EntryPayload::SubjectDetached { target }));
```

```rust
    #[test]
    fn an_absent_subject_digest_is_omitted_from_the_canonical_bytes() {
        let without = entry(EntryPayload::SubjectAttached {
            subject: crate::Subject::new(
                crate::SubjectKind::Document,
                Uri::new("file:///notes/spec.md").expect("valid uri"),
            ),
        });
        let bytes = without.to_canonical_bytes().expect("serialize");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(
            !text.contains("digest"),
            "an absent digest must not appear in the canonical bytes: {text}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto-kernel --lib serial`
Expected: FAIL — `no variant named SubjectAttached found for enum EntryPayload`.

- [ ] **Step 3: Write the minimal implementation**

In `crates/junto-kernel/src/entry.rs`, immediately after the `ArtifactAttached` variant closes at `:313` and before the enum's closing brace at `:314`:

```rust
    /// Binds a **Subject** — a thing this channel is about — to the channel
    /// (spec §1). A channel may carry zero, many, or none; a repo is one kind
    /// beside a document. The subject is a portable URI: how a given machine
    /// resolves it to a path is a **Mount**, machine-local config that never
    /// enters the ledger (`domain-model.md:32`). This entry's id identifies
    /// the attachment, so `SubjectDetached` can target it.
    SubjectAttached {
        /// What the channel is now about.
        subject: Subject,
    },
    /// Withdraws a previously attached Subject. Append-only: the attachment
    /// entry stays in the log and the detachment is a new entry that targets
    /// it, exactly as verification acts target assertions (`docs/adr/0002`).
    SubjectDetached {
        /// The `SubjectAttached` entry being withdrawn.
        target: EntryId,
    },
```

Add the import at the top of `entry.rs` alongside the other `crate::` imports:

```rust
use crate::subject::Subject;
```

In `target()` at `:345-369`, add `SubjectAttached` to the `None` group (it introduces a subject, it does not target an entry) and `SubjectDetached` to the `Some` group:

```rust
            | EntryPayload::SessionStarted { .. }
            | EntryPayload::SubjectAttached { .. } => None,
```

```rust
            | EntryPayload::ArtifactAttached { target, .. }
            | EntryPayload::SubjectDetached { target } => Some(*target),
```

In `crates/junto-kernel/src/ledger.rs`, add both to the `project_standings` continue arm at `:793` — neither is standing-bearing:

```rust
                | EntryPayload::ArtifactAttached { .. }
                | EntryPayload::SubjectAttached { .. }
                | EntryPayload::SubjectDetached { .. } => continue,
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto-kernel`
Expected: PASS. If any other match on `EntryPayload` fails to compile, add the two variants to whichever arm is semantically correct — for anything that folds standings, gates, sessions, or lineage, that is the inert/`continue` arm.

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel/src/entry.rs crates/junto-kernel/src/ledger.rs crates/junto-kernel/src/serial.rs
git commit -m "feat(kernel): SubjectAttached/SubjectDetached entry kinds"
```

---

### Task 3: Project the attached subjects

**Files:**
- Modify: `crates/junto-kernel/src/ledger.rs:140-212` (add the field), and add `project_subjects` beside `project_sessions`

**Interfaces:**
- Consumes: `EntryPayload::SubjectAttached` / `SubjectDetached` from Task 2.
- Produces: `ChannelView::subjects: Vec<(EntryId, Subject)>` — attachment id paired with its subject, in canonical order, detached ones removed. Task 5 and the host read this.

- [ ] **Step 1: Write the failing test**

In `crates/junto-kernel/src/ledger.rs`, inside `mod tests`, following the pattern of the existing projection tests:

```rust
    #[tokio::test]
    async fn detaching_a_subject_removes_it_but_keeps_both_entries() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");

        // One author throughout: `project_subjects` folds recognized entries
        // only, and the genesis author is the founding member (ADR 0017).
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "subjects".into(),
                },
            ))
            .await
            .expect("append genesis");

        let repo = Subject::new(
            SubjectKind::Repo,
            Uri::new("git+https://example.com/a.git").expect("valid uri"),
        );
        let doc = Subject::new(
            SubjectKind::Document,
            Uri::new("file:///notes/spec.md").expect("valid uri"),
        );

        let repo_attach = EntryId::new();
        ledger
            .append(entry(
                repo_attach,
                channel,
                dan.clone(),
                2,
                EntryPayload::SubjectAttached {
                    subject: repo.clone(),
                },
            ))
            .await
            .expect("attach repo");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                3,
                EntryPayload::SubjectAttached {
                    subject: doc.clone(),
                },
            ))
            .await
            .expect("attach doc");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                4,
                EntryPayload::SubjectDetached {
                    target: repo_attach,
                },
            ))
            .await
            .expect("detach repo");

        let view = ledger.project(&channel).await.expect("project");
        let subjects: Vec<_> = view.subjects.iter().map(|(_, s)| s.clone()).collect();
        assert_eq!(subjects, vec![doc], "the detached repo must not project");
        assert_eq!(
            view.entries.len(),
            4,
            "append-only: genesis plus three entries all stay in the log"
        );
    }

    #[tokio::test]
    async fn a_channel_with_no_subjects_projects_an_empty_list() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan,
                1,
                EntryPayload::ChannelOpened {
                    name: "empty".into(),
                },
            ))
            .await
            .expect("append genesis");
        let view = ledger.project(&channel).await.expect("project");
        assert!(view.subjects.is_empty());
    }
```

**Fixtures, verified — use these, do not invent others.** `mod tests` in `ledger.rs` provides exactly two helpers: `entry(id, channel, author, millis, payload)` at `:922` and `assertion(statement)` at `:939`. There is **no** `opened_channel()` and **no** `append()` helper. `Ledger::append` (`:293`) is `async` and takes `&mut self` plus a fully built `LedgerEntry`; `Ledger::project` (`:331`) is `async`. Tests in this module are `#[tokio::test] async fn`. Add `Subject`, `SubjectKind`, and `Uri` to the module's `use crate::{…}` list at `:915-919`.

**Note the pre-Task-7 shape:** `ChannelOpened.name` is still `String` at this point in the plan — pass `"subjects".into()`, not `Some(…)`. Task 7 updates these constructions when it makes the field optional.

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto-kernel --lib detaching_a_subject`
Expected: FAIL — `no field subjects on type ChannelView`.

- [ ] **Step 3: Write the minimal implementation**

Add the field to `ChannelView`, after `lineage` at `:211`:

```rust
    /// The Subjects this channel is about (spec §1), in canonical attachment
    /// order, each paired with the id of the `SubjectAttached` entry that
    /// introduced it. Detached subjects are folded out; both entries stay in
    /// [`entries`](ChannelView::entries), because the record is append-only.
    /// Members only, like every other fold (`docs/adr/0017`).
    pub subjects: Vec<(EntryId, Subject)>,
```

Add the fold beside the other `project_*` functions:

```rust
    /// Fold the live Subjects out of an ordered list of *recognized* entries.
    /// Two passes, mirroring `project_sessions`: collect the attachments in
    /// canonical order, then drop the ones a later `SubjectDetached` targets.
    fn project_subjects(entries: &[&LedgerEntry]) -> Vec<(EntryId, Subject)> {
        let mut attached: Vec<(EntryId, Subject)> = Vec::new();
        let mut detached: HashSet<EntryId> = HashSet::new();
        for entry in entries {
            match &entry.payload {
                EntryPayload::SubjectAttached { subject } => {
                    attached.push((entry.id, subject.clone()));
                }
                EntryPayload::SubjectDetached { target } => {
                    detached.insert(*target);
                }
                _ => {}
            }
        }
        attached
            .into_iter()
            .filter(|(id, _)| !detached.contains(id))
            .collect()
    }
```

Wire it into `project_uncached` beside the other folds, and add `subjects` to the `ChannelView` construction there. Add `use crate::subject::Subject;` to the imports at the top of `ledger.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto-kernel`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel/src/ledger.rs
git commit -m "feat(kernel): project a channel's attached subjects"
```

---

### Task 4: The Mount store replaces the Workspace store

**Files:**
- Create: `crates/junto/src/mounts.rs`
- Modify: `crates/junto/src/launch.rs:405-504` (delete the Workspace store), `:2733` (call site)
- Modify: `crates/junto/src/web.rs:581, 681, 695, 698, 789, 1921` (call sites)
- Modify: `crates/junto/src/main.rs` (add `mod mounts;`)

**Interfaces:**
- Consumes: `ChannelView::subjects` from Task 3; `junto_kernel::{Subject, SubjectKind, Uri}`.
- Produces:
  - `pub struct Mount { pub uri: Uri, pub path: PathBuf }`
  - `pub fn mounts_for(junto_home: &Path, subjects: &[Subject]) -> Result<Vec<Mount>>`
  - `pub fn mount_path(junto_home: &Path, uri: &Uri) -> Result<Option<PathBuf>>`
  - `pub fn remember_mount(junto_home: &Path, uri: &Uri, path: &Path) -> Result<()>`
  - Task 5 adds `capabilities` to this module; Task 6 consumes `mounts_for`.

**Clean cutover, no shim:** `workspaces.toml` is regenerable machine config with one known user. Delete `WorkspacesFile`, `WorkspaceRecord`, `workspaces_path`, `workspace_for`, `all_workspaces`, `remember_workspace` outright, along with their tests `workspace_store_remembers_and_updates` (`launch.rs:3322`) and `non_git_workspaces_are_refused` (`:3343`) — the second is testing behaviour this task deliberately removes.

- [ ] **Step 1: Write the failing test**

Create `crates/junto/src/mounts.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::tests::HomeGuard;
    use junto_kernel::{Subject, SubjectKind, Uri};

    fn uri(text: &str) -> Uri {
        Uri::new(text).expect("valid uri")
    }

    #[test]
    fn a_mount_is_remembered_by_uri_and_updated_in_place() {
        let home = HomeGuard::new();
        let repo = uri("git+https://example.com/a.git");
        assert!(mount_path(home.path(), &repo).unwrap().is_none());

        let first = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &repo, first.path()).unwrap();
        assert_eq!(
            mount_path(home.path(), &repo).unwrap().unwrap(),
            dunce::canonicalize(first.path()).unwrap()
        );

        let second = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &repo, second.path()).unwrap();
        assert_eq!(
            mount_path(home.path(), &repo).unwrap().unwrap(),
            dunce::canonicalize(second.path()).unwrap(),
            "remembering the same uri replaces rather than duplicating"
        );
    }

    #[test]
    fn a_non_git_directory_is_a_perfectly_good_mount() {
        let home = HomeGuard::new();
        let notes = tempfile::tempdir().unwrap();
        let doc = uri("file:///notes/spec.md");
        remember_mount(home.path(), &doc, notes.path())
            .expect("a document mount must not require a .git directory");
        assert!(mount_path(home.path(), &doc).unwrap().is_some());
    }

    #[test]
    fn unmounted_subjects_are_skipped_rather_than_erroring() {
        let home = HomeGuard::new();
        let mounted = uri("git+https://example.com/a.git");
        let dir = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &mounted, dir.path()).unwrap();

        let subjects = vec![
            Subject::new(SubjectKind::Repo, mounted.clone()),
            Subject::new(SubjectKind::Repo, uri("git+https://example.com/never.git")),
        ];
        let mounts = mounts_for(home.path(), &subjects).unwrap();
        assert_eq!(mounts.len(), 1, "the unmounted subject is simply absent");
        assert_eq!(mounts[0].uri, mounted);
    }
}
```

`HomeGuard` is at `crates/junto/src/host.rs:1311` and is `pub(crate)`; if it is not currently reachable as `crate::host::tests::HomeGuard`, make its `mod tests` `pub(crate)` in `host.rs` rather than duplicating the fixture.

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto mounts`
Expected: FAIL — `cannot find function mount_path in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `crates/junto/src/mounts.rs`:

```rust
//! The **Mount** store — how *this* machine resolves a Subject to a path.
//!
//! The counterpart to the kernel's `Subject` (spec §1). A Subject is portable
//! and lives in the ledger; a Mount is a machine fact and never leaves this
//! disk, exactly as the Workspace store it replaces never did
//! (`domain-model.md:32`). Unlike that store there is **no `.git`
//! requirement**: a document subject mounts to a directory or file and simply
//! reports fewer capabilities.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::{Subject, Uri};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct MountsFile {
    #[serde(default)]
    mounts: Vec<MountRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MountRecord {
    uri: String,
    path: PathBuf,
}

/// One subject, resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The subject this resolves.
    pub uri: Uri,
    /// Where it lives here.
    pub path: PathBuf,
}

fn mounts_path(junto_home: &Path) -> PathBuf {
    junto_home.join("mounts.toml")
}

fn read_mounts(junto_home: &Path) -> Result<MountsFile> {
    let path = mounts_path(junto_home);
    if !path.exists() {
        return Ok(MountsFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Where this machine keeps the given subject, if anywhere.
pub fn mount_path(junto_home: &Path, uri: &Uri) -> Result<Option<PathBuf>> {
    Ok(read_mounts(junto_home)?
        .mounts
        .into_iter()
        .find(|record| record.uri == uri.as_str())
        .map(|record| record.path))
}

/// Resolve every subject this machine can. Subjects with no mount are
/// **skipped, not an error** — a teammate may hold a checkout you do not, and
/// the channel still reads perfectly well without it.
pub fn mounts_for(junto_home: &Path, subjects: &[Subject]) -> Result<Vec<Mount>> {
    let file = read_mounts(junto_home)?;
    Ok(subjects
        .iter()
        .filter_map(|subject| {
            file.mounts
                .iter()
                .find(|record| record.uri == subject.uri.as_str())
                .map(|record| Mount {
                    uri: subject.uri.clone(),
                    path: record.path.clone(),
                })
        })
        .collect())
}

/// Remember (or update) where this machine keeps a subject.
pub fn remember_mount(junto_home: &Path, uri: &Uri, path: &Path) -> Result<()> {
    let path = dunce::canonicalize(path)
        .with_context(|| format!("mount path {} not found", path.display()))?;
    let mut file = read_mounts(junto_home)?;
    match file
        .mounts
        .iter_mut()
        .find(|record| record.uri == uri.as_str())
    {
        Some(record) => record.path = path,
        None => file.mounts.push(MountRecord {
            uri: uri.as_str().to_owned(),
            path,
        }),
    }
    let target = mounts_path(junto_home);
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        &target,
        toml::to_string_pretty(&file).context("serializing mounts")?,
    )
    .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}
```

`Uri::as_str()` already exists at `crates/junto-kernel/src/provenance.rs:43` — verified, no kernel change needed. `Uri` is also `#[serde(into = "String", try_from = "String")]` (`:23`), so a `Subject`'s uri serializes as a plain JSON string in the canonical bytes.

Then delete `launch.rs:405-504` in full, add `mod mounts;` to `crates/junto/src/main.rs`, and fix the call sites listed in **Files** to resolve a path through the channel's projected subjects instead of `workspace_for`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto`
Expected: PASS. `rtk cargo clippy --workspace --all-targets -- -D warnings` must also be clean — the deleted functions will surface as unused imports otherwise.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/mounts.rs crates/junto/src/launch.rs crates/junto/src/web.rs crates/junto/src/main.rs
git commit -m "feat(host): the Mount store replaces the Workspace store"
```

---

### Task 4b: `Host::attach_subject` and the write path

**Added during execution.** The plan as written had a read side with no write side: nothing in Tasks 1-10 ever appended a `SubjectAttached` entry, so `view.subjects` was empty for every real channel and Task 4's whole cutover had nothing to resolve. The plan's own self-review flagged this and failed to fix it. Task 4 therefore left the write-side sites refusing loudly; this task closes the gap. Ledger ruling recorded under `Task 4: Ruling: THE PLAN HAD NO WRITE PATH`.

**Files:**
- Modify: `crates/junto/src/host.rs` — add `Host::attach_subject`
- Modify: `crates/junto/src/web.rs` — the launch form attaches before mounting
- Modify: `crates/junto/src/mounts.rs` — drop the `#[allow(dead_code)]` on `remember_mount`/`mount_path` once they have production callers

**Interfaces:**
- Consumes: `EntryPayload::SubjectAttached { subject }` (Task 2); `Subject::{new, with_digest}`, `SubjectKind::{Repo, Document}` (Task 1); `ChannelView::subjects` (Task 3); `remember_mount`, `mount_path` (Task 4); the existing `WriteAuth` enum at `host.rs:37` and `Host::check_write_auth` at `:1006`.
- Produces: `Host::attach_subject(&self, channel: &str, subject: Subject, author: Member, auth: WriteAuth<'_>) -> Result<EntryId>` — appends one `SubjectAttached` and returns its entry id. Task 10's dogfood calls this.

**Two decisions already ruled, do not re-litigate:**
1. **The write surface is the existing launch form**, not new UI. The UX is unchanged — the user types a repo path and launches — it just attaches a Subject first when the channel has none.
2. **A git repo's Subject URI is its `origin` remote URL when it has one, else `file://<canonical path>`.** Portable in the common case, degraded honestly when no portable identity exists.

- [ ] **Step 1: Write the failing test**

In `crates/junto/src/host.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn attaching_a_subject_records_it_and_projects_it() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let host = test_host(&home, &repo);
        let dan = Member::human("Dan", "dan@example.com");
        let opened = host
            .open_channel(None, "subjects", dan.clone(), None)
            .await
            .expect("open");

        let subject = Subject::new(
            SubjectKind::Repo,
            Uri::new("git+https://example.com/a.git").expect("valid uri"),
        );
        let id = host
            .attach_subject(&opened.id.to_string(), subject.clone(), dan.clone(), WriteAuth::Human)
            .await
            .expect("attach");

        let view = host.project(&opened.id.to_string()).await.expect("project");
        assert_eq!(view.subjects, vec![(id, subject)]);
    }

    #[tokio::test]
    async fn attaching_a_subject_refuses_a_non_member() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let host = test_host(&home, &repo);
        let dan = Member::human("Dan", "dan@example.com");
        let opened = host
            .open_channel(None, "subjects", dan, None)
            .await
            .expect("open");

        let stranger = Member::human("Stranger", "stranger@example.com");
        let subject = Subject::new(
            SubjectKind::Repo,
            Uri::new("git+https://example.com/a.git").expect("valid uri"),
        );
        let err = host
            .attach_subject(&opened.id.to_string(), subject, stranger, WriteAuth::Human)
            .await
            .expect_err("a non-member must not attach a subject");
        assert!(format!("{err}").to_lowercase().contains("member"), "{err}");
    }
```

Use the crate's real host-construction fixture rather than `test_host` if it is named differently — read `mod tests` in `host.rs` first, and reuse `open_channel`'s actual signature rather than the shape sketched here.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto attaching_a_subject`
Expected: FAIL — `no method named attach_subject found`.

- [ ] **Step 3: Write `Host::attach_subject`**

Mirror `Host::diverge` (`host.rs:700-747`), which is the established pattern for a member-authored write: resolve the channel for write, project it, `check_write_auth`, then append. Authorization is **not** re-implemented — dispatch through `check_write_auth` so both surfaces are served, exactly as the lineage ops do.

```rust
    /// Attach a **Subject** — something this channel is about (spec §1) — by
    /// recording one `SubjectAttached` entry. Returns the attachment's entry
    /// id, which is what a later `SubjectDetached` targets.
    ///
    /// The subject's URI is portable by construction; where *this* machine
    /// keeps it is a Mount, machine-local and never recorded.
    ///
    /// # Errors
    /// Refuses an author who is not in the channel's Party, or whose member
    /// code is missing or wrong on the agent surface (`docs/adr/0017`/`0021`).
    pub async fn attach_subject(
        &self,
        channel: &str,
        subject: Subject,
        author: Member,
        auth: WriteAuth<'_>,
    ) -> Result<EntryId> {
        let (_substrate, ledger, channel_id) = self.resolve_for_write(channel).await?;
        let mut guard = ledger.lock().await;
        let view = guard.project(&channel_id).await?;
        self.check_write_auth(&view, &author, &auth)?;
        let entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: channel_id,
            author,
            timestamp: Timestamp::now(),
            payload: EntryPayload::SubjectAttached { subject },
        };
        let id = entry.id;
        guard.append(entry).await?;
        Ok(id)
    }
```

Sign the entry if `diverge` signs its own — match whatever the sibling does about signatures rather than leaving this one unsigned when its neighbours are signed.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto attaching_a_subject`
Expected: PASS, 2 tests.

- [ ] **Step 5: Re-point the launch form**

In `web.rs`'s `launch_session`, the branch Task 4 left refusing a typed path now has work to do: when the channel has **no** mountable subject and the user typed a path, derive a repo Subject from that path, attach it, remember the mount, and proceed to launch. Derivation, per the ruling:

```rust
/// A git repo's portable identity: its `origin` remote when it has one, else
/// a `file://` URI of the canonical path. The fallback is machine-shaped on
/// purpose — a repo with no remote has no portable identity to offer.
fn repo_subject_uri(path: &std::path::Path) -> Result<Uri> { /* … */ }
```

Use `git -C <path> remote get-url origin`, trimmed; on any non-zero exit or empty output, fall back. Reuse whatever helper the crate already has for running git rather than adding a second way to shell out.

Add a test that a channel with no subject, given a typed path to a real git repo, ends up with one attached `SubjectAttached` whose uri is the repo's `origin`, a remembered mount, and a launched session. Then drop the now-unnecessary `#[allow(dead_code)]` from `remember_mount` and `mount_path`.

- [ ] **Step 6: Full green, then commit**

```bash
rtk cargo fmt --check
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
git add crates/junto/src/host.rs crates/junto/src/web.rs crates/junto/src/mounts.rs
git commit -m "feat(host): attach_subject, and the launch form attaches before mounting"
```

---


### Task 5: Capabilities are computed, never recorded

**Files:**
- Modify: `crates/junto/src/mounts.rs`

**Interfaces:**
- Consumes: `Subject`, `SubjectKind`, `Mount` from Tasks 1 and 4.
- Produces: `pub enum Capability { Read, Watch, Anchor, Diff, Execute, Mutate }`; `pub fn capabilities(subject: &Subject, mount: Option<&Mount>) -> BTreeSet<Capability>`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/junto/src/mounts.rs`:

```rust
    #[test]
    fn a_mounted_repo_can_do_everything_and_an_unmounted_one_can_only_be_read() {
        let repo = Subject::new(SubjectKind::Repo, uri("git+https://example.com/a.git"));
        let dir = tempfile::tempdir().unwrap();
        let mount = Mount {
            uri: repo.uri.clone(),
            path: dir.path().to_path_buf(),
        };

        let mounted = capabilities(&repo, Some(&mount));
        for expected in [
            Capability::Read,
            Capability::Watch,
            Capability::Anchor,
            Capability::Diff,
            Capability::Execute,
            Capability::Mutate,
        ] {
            assert!(mounted.contains(&expected), "missing {expected:?}");
        }

        let unmounted = capabilities(&repo, None);
        assert_eq!(
            unmounted,
            [Capability::Read].into_iter().collect(),
            "without a mount there is nothing to run in, diff, or write back to"
        );
    }

    #[test]
    fn a_document_is_never_executable_even_when_mounted() {
        let doc = Subject::new(SubjectKind::Document, uri("file:///notes/spec.md"));
        let dir = tempfile::tempdir().unwrap();
        let mount = Mount {
            uri: doc.uri.clone(),
            path: dir.path().to_path_buf(),
        };
        let caps = capabilities(&doc, Some(&mount));
        assert!(!caps.contains(&Capability::Execute));
        assert!(!caps.contains(&Capability::Diff));
        assert!(caps.contains(&Capability::Anchor));
        assert!(caps.contains(&Capability::Mutate));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto mounts::tests::a_mounted_repo`
Expected: FAIL — `cannot find function capabilities in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Append to `crates/junto/src/mounts.rs`:

```rust
use std::collections::BTreeSet;

use junto_kernel::SubjectKind;

/// What a Subject affords, on this machine, right now.
///
/// **Never recorded.** Capabilities vary by machine — one host has the
/// checkout and the credentials, another has neither — so putting them in the
/// ledger would smuggle machine facts into the record (spec §1). They are
/// recomputed at use time from the kind and the mount, and resolve against the
/// **executing host**, not the viewing human (spec §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Fetch its current state.
    Read,
    /// Receive change events.
    Watch,
    /// Attach a span that survives the content moving.
    Anchor,
    /// Produce a mechanical before/after.
    Diff,
    /// Run an agent session inside it.
    Execute,
    /// Write back to it — always through a gate.
    Mutate,
}

/// The capability set for a subject on this machine.
#[must_use]
pub fn capabilities(subject: &Subject, mount: Option<&Mount>) -> BTreeSet<Capability> {
    let mut caps = BTreeSet::new();
    // Reading is the floor: a URI is enough to fetch or open something.
    caps.insert(Capability::Read);
    if mount.is_none() {
        return caps;
    }
    caps.insert(Capability::Watch);
    caps.insert(Capability::Anchor);
    caps.insert(Capability::Mutate);
    match subject.kind {
        SubjectKind::Repo => {
            caps.insert(Capability::Diff);
            caps.insert(Capability::Execute);
        }
        // A document has no working tree to run in and no mechanical diff;
        // its provenance is a content digest instead (spec §1).
        SubjectKind::Document => {}
    }
    caps
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto mounts`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/mounts.rs
git commit -m "feat(host): compute subject capabilities from kind and mount"
```

---

### Task 6: A session runs without a repo

**Files:**
- Modify: `crates/junto/src/launch.rs` — `launch()` at `:1651`, `spawn_turn()` at `:1827`, and the `prepare_pr_branch` call site at `:2733`

**Interfaces:**
- Consumes: `mounts_for`, `capabilities`, `Capability::Execute` from Tasks 4 and 5.
- Produces: `pub fn session_workdir(junto_home: &Path, view: &ChannelView, session: EntryId) -> Result<PathBuf>` — the first `Execute`-capable mount, or a per-session scratch directory under `<junto_home>/scratch/<session>` when there is none.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/junto/src/launch.rs`:

```rust
    #[tokio::test]
    async fn a_channel_with_no_executable_subject_runs_in_a_scratch_directory() {
        let home = HomeGuard::new();
        let session = EntryId::new();
        let view = channel_view_with_subjects(&[]).await;

        let dir = session_workdir(home.path(), &view, session).expect("a workdir");
        assert!(dir.exists(), "the scratch directory must be created");
        assert!(
            dir.starts_with(home.path().join("scratch")),
            "scratch dirs live under the junto home, not in a repo: {}",
            dir.display()
        );
        assert!(
            !dir.join(".git").exists(),
            "a scratch dir is deliberately not a git repo"
        );
    }

    #[tokio::test]
    async fn a_mounted_repo_subject_wins_over_the_scratch_directory() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let uri = junto_kernel::Uri::new("git+https://example.com/a.git").expect("valid uri");
        crate::mounts::remember_mount(home.path(), &uri, repo.path()).unwrap();

        let subject = junto_kernel::Subject::new(junto_kernel::SubjectKind::Repo, uri);
        let view = channel_view_with_subjects(&[subject]).await;

        let dir = session_workdir(home.path(), &view, EntryId::new()).expect("a workdir");
        assert_eq!(dir, dunce::canonicalize(repo.path()).unwrap());
    }
```

Add the fixture beside them:

```rust
    /// A `ChannelView` carrying just the subjects a workdir test needs.
    ///
    /// Built through the kernel's in-memory substrate rather than a git-refs
    /// one: `session_workdir` reads only `view.subjects`, so an in-memory
    /// ledger is the honest minimum here.
    async fn channel_view_with_subjects(subjects: &[junto_kernel::Subject]) -> ChannelView {
        use junto_kernel::{
            ChannelId, EntryId, EntryPayload, InMemorySubstrate, Ledger, LedgerEntry, Member,
            Timestamp,
        };
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        let mut millis = 1;
        let mut append = |ledger: &mut Ledger<InMemorySubstrate>, payload: EntryPayload| {
            let built = LedgerEntry {
                signature: None,
                id: EntryId::new(),
                channel,
                author: dan.clone(),
                timestamp: Timestamp::from_millis(millis),
                payload,
            };
            millis += 1;
            ledger.append(built)
        };
        append(
            &mut ledger,
            EntryPayload::ChannelOpened {
                name: "workdir".into(),
            },
        )
        .await
        .expect("append genesis");
        for subject in subjects {
            append(
                &mut ledger,
                EntryPayload::SubjectAttached {
                    subject: subject.clone(),
                },
            )
            .await
            .expect("attach subject");
        }
        ledger.project(&channel).await.expect("project")
    }
```

**These two tests are `#[tokio::test] async fn`**, because `channel_view_with_subjects` awaits the kernel's async `append`/`project`. `crates/junto` already has `tokio` available and uses `#[tokio::test]` elsewhere. If the closure borrow above fights the borrow checker, inline the entry construction in the loop rather than reaching for `Rc`/`RefCell` — this is test scaffolding, keep it dumb.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto session_workdir`
Expected: FAIL — `cannot find function session_workdir in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Add to `crates/junto/src/launch.rs`, near the other session helpers:

```rust
/// Where a session's agent should run.
///
/// The first `Execute`-capable mounted subject wins (spec §1); a channel with
/// none — a research or document inquiry — gets a per-session scratch
/// directory. This is the mechanism that makes a repo-free channel work:
/// nothing in the launch path requires git any more.
pub fn session_workdir(
    junto_home: &Path,
    view: &ChannelView,
    session: EntryId,
) -> Result<PathBuf> {
    let subjects: Vec<_> = view.subjects.iter().map(|(_, s)| s.clone()).collect();
    let mounts = crate::mounts::mounts_for(junto_home, &subjects)?;
    for subject in &subjects {
        let mount = mounts.iter().find(|m| m.uri == subject.uri);
        if crate::mounts::capabilities(subject, mount)
            .contains(&crate::mounts::Capability::Execute)
            && let Some(mount) = mount
        {
            return Ok(mount.path.clone());
        }
    }
    let scratch = junto_home.join("scratch").join(session.to_string());
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating scratch dir {}", scratch.display()))?;
    Ok(scratch)
}
```

Change `launch()`'s `workspace: PathBuf` parameter to be produced by this function at its call sites rather than passed in from the web layer, and guard the `prepare_pr_branch` call at `:2733` so a scratch-dir session skips branch preparation instead of failing: `prepare_pr_branch` only makes sense where `Capability::Diff` holds.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/launch.rs
git commit -m "feat(host): sessions run in a scratch dir when no subject is executable"
```

---

### Task 7: A channel opens without a name

**Files:**
- Modify: `crates/junto-kernel/src/entry.rs:101-105` (`ChannelOpened.name`), `crates/junto-kernel/src/ledger.rs:140-146` (the `name` projection), `crates/junto-kernel/src/serial.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `EntryPayload::ChannelOpened { name: Option<String> }`. `ChannelView::name` keeps its existing `Option<String>` type, so its consumers are unaffected.

**Why this is byte-safe:** an existing entry serializes `"name":"junto-dev"`, which deserializes into `Some("junto-dev")`; a new nameless entry omits the field entirely. This is precisely the pattern `LedgerEntry::signature` uses at `entry.rs:34` to keep pre-ADR-0033 bytes unchanged.

- [ ] **Step 1: Write the failing test**

In `crates/junto-kernel/src/serial.rs`, inside `mod tests`:

```rust
    #[test]
    fn an_unnamed_channel_omits_the_name_from_its_canonical_bytes() {
        let unnamed = entry(EntryPayload::ChannelOpened { name: None });
        assert_round_trips(&unnamed);
        let text = String::from_utf8(unnamed.to_canonical_bytes().expect("serialize"))
            .expect("utf8");
        assert!(
            !text.contains("name"),
            "an absent name must not appear in the canonical bytes: {text}"
        );
    }

    #[test]
    fn a_named_channels_canonical_bytes_are_unchanged_by_the_name_becoming_optional() {
        let named = entry(EntryPayload::ChannelOpened {
            name: Some("junto-dev".into()),
        });
        let text = String::from_utf8(named.to_canonical_bytes().expect("serialize"))
            .expect("utf8");
        assert!(
            text.contains(r#""name":"junto-dev""#),
            "a present name must serialize exactly as before: {text}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto-kernel --lib channel`
Expected: FAIL — `expected String, found Option<String>`.

- [ ] **Step 3: Write the minimal implementation**

In `entry.rs`, change the `ChannelOpened` variant:

```rust
    ChannelOpened {
        /// The human-facing label — *not* identity (`docs/adr/0014`), and
        /// since the collapse (spec §2) no longer unique within the home
        /// substrate and no longer required. `None` is an unnamed channel:
        /// opened by a human's first message, named later or never. Omitted
        /// from the canonical bytes when absent, so every entry written before
        /// the name became optional serializes byte-identically.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        name: Option<String>,
    },
```

Then fix every construction and match of `ChannelOpened` the compiler reports — in `host.rs`'s open path, in `ledger.rs`'s name projection (which already yields `Option<String>`, so it becomes a flatten rather than a wrap), and in existing tests, which should pass `Some("…".into())`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS. There is a golden byte-stability test at `serial.rs:334` — `golden_canonical_form_is_byte_stable`. **It must still pass unchanged.** If it fails, the `skip_serializing_if` attribute is missing or misplaced; fix that rather than updating the golden.

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel/src/entry.rs crates/junto-kernel/src/ledger.rs crates/junto-kernel/src/serial.rs crates/junto/src/host.rs
git commit -m "feat(kernel): a channel may open without a name"
```

---

### Task 8: Derived channel standing

**Files:**
- Modify: `crates/junto-kernel/src/ledger.rs` — add `ChannelStanding`, `project_channel_standing`, and the `ChannelView` field
- Modify: `crates/junto-kernel/src/lib.rs:43-46` (re-export)

**Interfaces:**
- Consumes: `ChannelView::standings` and `ChannelView::closed`, both already projected.
- Produces: `pub enum ChannelStanding { Scratch, Standing, Settled }`; `ChannelView::channel_standing: ChannelStanding`. The brief, `list_channels`, and the focus board filter on this.

- [ ] **Step 1: Write the failing test**

In `crates/junto-kernel/src/ledger.rs`, inside `mod tests`:

```rust
    /// Build a channel whose genesis is authored by `dan`, then run `body`'s
    /// extra entries through it. Kept local to these three tests rather than
    /// added to the module's shared fixtures — `entry` and `assertion` are the
    /// only helpers this module has, and it stays that way.
    async fn standing_of(extra: Vec<EntryPayload>) -> ChannelStanding {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "standing".into(),
                },
            ))
            .await
            .expect("append genesis");
        for (offset, payload) in extra.into_iter().enumerate() {
            let millis = 2 + i64::try_from(offset).expect("small offset");
            ledger
                .append(entry(EntryId::new(), channel, dan.clone(), millis, payload))
                .await
                .expect("append entry");
        }
        ledger
            .project(&channel)
            .await
            .expect("project")
            .channel_standing
    }

    #[tokio::test]
    async fn a_channel_with_nothing_ratified_is_scratch_and_stays_out_of_recall() {
        let standing = standing_of(vec![assertion("a half-formed thought")]).await;
        assert_eq!(standing, ChannelStanding::Scratch);
    }

    #[tokio::test]
    async fn one_ratified_entry_promotes_a_channel_to_standing() {
        // The ratification must target the assertion's real id, so this test
        // builds its entries directly rather than through `standing_of`.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "standing".into(),
                },
            ))
            .await
            .expect("append genesis");
        let claim = EntryId::new();
        ledger
            .append(entry(
                claim,
                channel,
                dan.clone(),
                2,
                assertion("a real finding"),
            ))
            .await
            .expect("append assertion");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                3,
                EntryPayload::Ratification {
                    target: claim,
                    rationale: "checked".into(),
                },
            ))
            .await
            .expect("append ratification");
        let view = ledger.project(&channel).await.expect("project");
        assert_eq!(view.channel_standing, ChannelStanding::Standing);
    }

    #[tokio::test]
    async fn closing_a_channel_settles_it_even_with_nothing_ratified() {
        let standing = standing_of(vec![EntryPayload::ChannelClosed {
            rationale: "abandoned".into(),
        }])
        .await;
        assert_eq!(standing, ChannelStanding::Settled);
    }
```

**Verified field lists — use exactly these:** `Assertion { statement, rationale, provenance, frame }` (`entry.rs:170`, and the `assertion(…)` helper at `ledger.rs:939` builds one for you); `Ratification { target, rationale }` (`entry.rs:184`); `ChannelClosed { rationale }` (`entry.rs:119`). `ChannelOpened.name` is still `String` here — Task 7 makes it optional afterwards.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto-kernel --lib channel_standing`
Expected: FAIL — `cannot find type ChannelStanding in this scope`.

- [ ] **Step 3: Write the minimal implementation**

In `ledger.rs`, beside `Standing` at `:39-47`:

```rust
/// A whole channel's derived standing (spec §2) — the filter that lets
/// channels be cheap without drowning recall.
///
/// Derived by projection from entries the ledger already holds: no new entry
/// kind, no user action, nothing to declare. A channel that has produced
/// nothing verified is invisible to the brief, so opening one costs nothing
/// epistemically. **Existence and standing are different things.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelStanding {
    /// Nothing ratified and not closed — visible to its author only.
    Scratch,
    /// At least one ratified entry: it has produced something. Feeds recall.
    Standing,
    /// Closed or converged. Feeds recall as history.
    Settled,
}
```

And the fold:

```rust
    /// Derive the channel's standing from what its ledger already contains.
    fn project_channel_standing(
        standings: &HashMap<EntryId, Standing>,
        closed: bool,
    ) -> ChannelStanding {
        if closed {
            return ChannelStanding::Settled;
        }
        if standings
            .values()
            .any(|standing| *standing == Standing::Ratified)
        {
            return ChannelStanding::Standing;
        }
        ChannelStanding::Scratch
    }
```

Add the field to `ChannelView` after `subjects`:

```rust
    /// This channel's derived standing (spec §2). Recall and the focus board
    /// filter on it so that cheap channels cost nothing.
    pub channel_standing: ChannelStanding,
```

Call it in `project_uncached` **after** `standings` and `closed` are computed, and add `ChannelStanding` to the `lib.rs` re-export at `:43-46`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel/src/ledger.rs crates/junto-kernel/src/lib.rs
git commit -m "feat(kernel): derive channel standing — scratch, standing, settled"
```

---

### Task 9: Names stop being unique, and recall filters on standing

**Files:**
- Modify: `crates/junto/src/host.rs` — the uniqueness check in `open_channel`, and name resolution
- Modify: wherever `junto brief` / the recall projection enumerates channels

**Interfaces:**
- Consumes: `ChannelStanding` from Task 8.
- Produces: name resolution returning the most recently opened match; recall listing only `Standing` and `Settled` channels.

- [ ] **Step 1: Write the failing test**

In `crates/junto/src/host.rs`, inside `mod tests`:

```rust
    #[test]
    fn two_channels_may_share_a_name_and_resolution_prefers_the_newest() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let host = test_host(&home, &repo);

        let first = host.open_channel(Some("auth stuff"), /* … */).expect("open");
        let second = host.open_channel(Some("auth stuff"), /* … */).expect(
            "a duplicate name must be allowed after the collapse (spec §2)",
        );
        assert_ne!(first.id, second.id);
        assert_eq!(host.resolve("auth stuff").expect("resolve"), second.id);
    }

    #[test]
    fn recall_skips_scratch_channels() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let host = test_host(&home, &repo);
        let scratch = host.open_channel(None, /* … */).expect("open");
        let listed = host.channels_for_recall().expect("recall list");
        assert!(
            !listed.iter().any(|c| c.id == scratch.id),
            "a channel with nothing ratified must not reach the brief"
        );
    }
```

Fill the `/* … */` placeholders with `open_channel`'s real remaining parameters, and use the crate's existing host-construction fixture rather than inventing `test_host` if one already exists.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto two_channels_may_share_a_name`
Expected: FAIL — the duplicate open is refused by the existing uniqueness check.

- [ ] **Step 3: Write the minimal implementation**

Delete the name-uniqueness check from `open_channel`. Change name resolution to collect every genesis whose name matches and return the one with the greatest `timestamp`, breaking ties by `EntryId` so it is deterministic on every replica — the same tie-break `canonical_cmp` uses at `entry.rs:326-331`. Add a `channels_for_recall` that filters out `ChannelStanding::Scratch`.

Update the ADR: amend `docs/adr/0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md` to record that names are no longer unique and that the human surface may open a channel without one, citing this plan and the spec.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/host.rs docs/adr/0014-channel-identity-is-minted-names-are-substrate-scoped-labels.md
git commit -m "feat(host): channel names need not be unique; recall skips scratch"
```

---

### Task 10: Dogfood it, then write the ADRs

**Files:**
- Create: `docs/adr/0037-subjects-and-mounts.md`
- Create: `docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md`

- [ ] **Step 1: Dogfood — open a repo-free channel and run a session in it**

With the host running (`cargo run -p junto -- serve`), open a channel with no name and no repo, attach a `document` subject pointing at this plan, and launch a session in it. Confirm: the session runs in `<junto_home>/scratch/<session>`, no git command is invoked, the channel reports `Scratch` until something is ratified, and it does not appear in `junto brief`.

- [ ] **Step 2: Record the dogfood result**

Record an assertion in channel `3c38ead9-4907-4646-99b7-23b21933da35` stating what was run and what was observed, with the session id as provenance. If anything failed, record that instead — a dogfood that found a bug is the more valuable entry.

- [ ] **Step 3: Write ADR 0037 — Subjects and Mounts**

Cover: the durable/machine-local split and why paths stay out of the ledger; capabilities computed per executing host rather than recorded; why `SubjectKind` is a closed kernel enum while providers stay behind adapters; and the rule-of-three deferral of `SubjectProvider` until a third kind exists.

- [ ] **Step 4: Write ADR 0038 — the collapse**

Cover: the human/agent asymmetry that preserves ADR 0014's defence against stray writes while deleting the ceremony; derived channel standing as the filter that makes cheap channels safe; names no longer unique; and the decision that the noun stays `Channel`.

- [ ] **Step 5: Full green, then commit**

```bash
rtk cargo fmt --check
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
git add docs/adr/0037-subjects-and-mounts.md docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md
git commit -m "docs: ADRs 0037/0038 — subjects, mounts, and the collapse"
```

---

## Self-Review

**Spec coverage.** §1 Subject/Mount → Tasks 1-5; §1 provenance-degrades-honestly → the `digest` field (Task 1) and `Capability::Diff` absence (Task 5); §1 naming → Task 1 plus the ADR 0019 prose fix, **which no task owns — folded into Task 10's ADR work**. §2 zero-ceremony open → Task 7; §2 playbook optional → **not covered: no `Playbook` field exists in `ChannelOpened` today, so there is nothing to make optional. It becomes real when playbooks are stamped, which is not in this plan.** §2 derived standing → Task 8; §2 names not unique → Task 9; §2 lineage → no code change, as the spec states. §3, §4, §5 → out of scope by design.

**Placeholder scan.** Tasks 9's test bodies carry `/* … */` where `open_channel`'s current signature is unknown to me without reading it at implementation time; every other step carries complete code. That is flagged in-place with instructions rather than left silent, and Task 9 is the one task whose implementer must read a signature first.

**Type consistency.** `Subject`/`SubjectKind` (Task 1) are used unchanged in Tasks 2, 3, 5, 6. `Mount` (Task 4) is consumed by `capabilities` (Task 5) and `session_workdir` (Task 6). `ChannelStanding` (Task 8) is consumed by Task 9's `channels_for_recall`. `mounts_for` returns `Vec<Mount>` in Task 4 and is destructured as such in Task 6. `ChannelView::subjects` is `Vec<(EntryId, Subject)>` in Task 3 and mapped as such in Task 6.

**Known gap I am not silently absorbing:** Task 4 says "fix the call sites" for `web.rs:581, 681, 695, 698, 789, 1921` without showing each one's replacement. Those six sites need reading at implementation time; the shape of the fix is identical in each (project the channel, take `view.subjects`, call `mounts_for`), but the surrounding handler code differs and I have not read it.
