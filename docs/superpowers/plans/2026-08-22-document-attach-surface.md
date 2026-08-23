# Document Attach Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a channel able to be *about a document* through a real agent surface, give an attached Subject a way to be withdrawn, and close three stale-documentation debts from PR #69.

**Architecture:** Two new `Host` behaviours (`detach_subject`, and a kind-aware idempotency guard on `attach_subject`) exposed through two new MCP tools, plus a machine-path guard that keeps unportable identities out of an append-only record. One four-line control-flow reorder in `web.rs` lets a scratch session keep being steered. No kernel types change, so no canonical bytes move.

**Tech Stack:** Rust, `rmcp` (`#[tool_router]` / `#[tool]` macros), `anyhow`, `tokio`, `schemars` for tool JSON-Schema derivation.

**Spec:** `docs/superpowers/specs/2026-08-22-document-attach-surface-design.md`

## Global Constraints

- The durable record is append-only. A wrong entry can only be superseded, never edited.
- Existing canonical bytes must not change. `golden_canonical_form_is_byte_stable` is the gate and is never edited.
- Machine paths never enter the ledger.
- No `unwrap()` / `expect()` / `panic!` in library code. Tests may use them.
- Never hardcode `/` or `\` when *building* paths; use `Path`/`PathBuf::join`. Guards that *inspect* strings must handle both separators explicitly.
- Windows and macOS are equal first-class targets.
- Pre-commit, in order, stopping on first failure: `rtk cargo fmt --check`, then `rtk cargo clippy --workspace --all-targets -- -D warnings`, then `rtk cargo test --workspace`.
- Baseline at `fd4fe89`: 596 tests pass in 7 suites.
- Merge through junto's own code-PR push-gate, never `gh pr create`.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/junto/src/host.rs` | `detach_subject`; kind-aware guard in `attach_subject` | modify (`:947-996`, tests near `:2367`) |
| `crates/junto/src/mcp.rs` | `subject_uri` guard; `attach_document` + `detach_subject` tools; `TargetKind::Subject`; server instructions | modify (`:25-28`, `:383-441`, `:612-642`, `:1053-1075`, tests near `:1186`) |
| `crates/junto/src/web.rs` | hoist the scratch pin above the refusal; invert one test, add one | modify (`:1130-1168`, `:5237`) |
| `crates/junto/src/mounts.rs` | `remember_mount` doc comment | modify (`:88-90`) |
| `docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md` | known-limit paragraph | modify |
| `docs/adr/0037-subjects-and-mounts.md` | Known limits: close two | modify (`:47-53`) |

Tasks 1 and 2 both touch `attach_subject`'s neighbourhood in `host.rs` and are ordered accordingly. Task 4 depends on 1, 2, and 3. Tasks 5 and 6 are independent of everything else.

---

### Task 1: `Host::detach_subject`

**Files:**
- Modify: `crates/junto/src/host.rs` (insert after `attach_subject`, which ends at `:996`)
- Test: `crates/junto/src/host.rs` (tests module, after `attaching_the_same_subject_twice_is_idempotent` at `:2397`)

**Interfaces:**
- Consumes: `Host::resolve_for_write`, `Host::check_write_auth`, `Host::sign_entry` — all existing private helpers used by `attach_subject`. `junto_kernel::{EntryId, EntryPayload, LedgerEntry, Member, Subject, SubjectKind, Timestamp, Uri}` are already imported in `host.rs`.
- Produces: `pub async fn detach_subject(&self, channel: &str, target: EntryId, author: Member, auth: WriteAuth<'_>) -> Result<EntryId>` — returns the *detachment's* own entry id. Task 4's `detach_subject` tool calls exactly this.

Test fixtures in this module: `lineage_host(1) -> (Vec<TempDir>, Host)`, `dan() -> Member`, `code_for(&dirs, &member) -> String`, `project(&host, name) -> (ChannelId, ChannelView)`.

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `crates/junto/src/host.rs`:

```rust
    #[tokio::test]
    async fn detaching_a_subject_withdraws_it_from_the_projection() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "subjects", dan(), None)
            .await
            .unwrap();

        let subject = Subject::new(
            SubjectKind::Document,
            Uri::new("https://example.com/spec.md").expect("valid uri"),
        );
        let attachment = host
            .attach_subject("subjects", subject, dan(), WriteAuth::Human)
            .await
            .expect("attach");

        let detachment = host
            .detach_subject("subjects", attachment, dan(), WriteAuth::Human)
            .await
            .expect("detach");
        assert_ne!(
            detachment, attachment,
            "a detachment is its own entry, not the attachment's id echoed back"
        );

        let (_, view) = project(&host, "subjects").await;
        assert!(
            view.subjects.is_empty(),
            "project_subjects must drop a detached attachment"
        );
    }

    #[tokio::test]
    async fn detaching_twice_refuses_and_appends_nothing() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "subjects", dan(), None)
            .await
            .unwrap();

        let subject = Subject::new(
            SubjectKind::Document,
            Uri::new("https://example.com/spec.md").expect("valid uri"),
        );
        let attachment = host
            .attach_subject("subjects", subject, dan(), WriteAuth::Human)
            .await
            .expect("attach");
        host.detach_subject("subjects", attachment, dan(), WriteAuth::Human)
            .await
            .expect("first detach");

        let (_, before) = project(&host, "subjects").await;
        let count_before = before.entries.len();

        let err = host
            .detach_subject("subjects", attachment, dan(), WriteAuth::Human)
            .await
            .expect_err("a second detach of the same attachment must refuse");
        assert!(
            format!("{err}").contains("already detached"),
            "the error must say the attachment is already gone, not that it never existed: {err}"
        );

        let (_, after) = project(&host, "subjects").await;
        assert_eq!(
            after.entries.len(),
            count_before,
            "a refused detach must append nothing"
        );
    }

    #[tokio::test]
    async fn detaching_something_that_is_not_an_attachment_refuses() {
        let (_dirs, host) = lineage_host(1);
        let (id, _) = host
            .open_channel(None, "subjects", dan(), None)
            .await
            .unwrap();
        let _ = id;

        let (_, view) = project(&host, "subjects").await;
        let genesis = view.entries.first().expect("genesis entry").id;

        let err = host
            .detach_subject("subjects", genesis, dan(), WriteAuth::Human)
            .await
            .expect_err("the genesis entry is not a subject attachment");
        assert!(
            format!("{err}").contains("not a subject attachment"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn detaching_a_subject_refuses_a_non_member() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "subjects", dan(), None)
            .await
            .unwrap();

        let subject = Subject::new(
            SubjectKind::Document,
            Uri::new("https://example.com/spec.md").expect("valid uri"),
        );
        let attachment = host
            .attach_subject("subjects", subject, dan(), WriteAuth::Human)
            .await
            .expect("attach");

        let stranger = Member::human("Stranger", "stranger@example.com");
        let err = host
            .detach_subject("subjects", attachment, stranger, WriteAuth::Human)
            .await
            .expect_err("a non-member must not detach a subject");
        assert!(format!("{err}").to_lowercase().contains("member"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto detaching_ 2>&1 | tail -20`

Expected: compile error — `no method named 'detach_subject' found for struct 'Host'`.

- [ ] **Step 3: Implement `detach_subject`**

Insert in `crates/junto/src/host.rs` immediately after `attach_subject`'s closing brace (`:996`):

```rust
    /// Withdraw an attached **Subject** by recording one `SubjectDetached`
    /// entry targeting its `SubjectAttached` (`docs/adr/0037`). Returns the
    /// detachment's own entry id.
    ///
    /// The attachment stays in the log — the record is append-only, exactly
    /// as a Park leaves its target standing (`docs/adr/0002`) — but
    /// `Ledger::project_subjects` stops projecting it, order-insensitively,
    /// so every replica agrees on the live set regardless of clock skew.
    ///
    /// **Refuses rather than no-ops**, and distinguishes the two failure
    /// modes: `project_subjects` drops detached attachments, so a bare "not
    /// found" would tell a caller retrying after a dropped connection that
    /// their attachment never existed. Nothing is appended on either
    /// refusal.
    ///
    /// This machine's Mount for the subject is deliberately left alone:
    /// `crate::launch::mount_with_capability` walks a channel's *subjects*,
    /// so a mount whose Subject is gone is already inert, and a durable
    /// ledger write should not delete machine-local config.
    ///
    /// Dispatches through [`Host::check_write_auth`], like
    /// [`Host::attach_subject`], so both write surfaces are served
    /// (`docs/adr/0021`).
    ///
    /// # Errors
    /// Refuses an author who is not in the channel's Party, or whose member
    /// code is missing or wrong on the agent surface
    /// (`docs/adr/0017`/`0021`); refuses a `target` that is not a live
    /// subject attachment in this channel; also errors if `channel` does not
    /// resolve.
    pub async fn detach_subject(
        &self,
        channel: &str,
        target: EntryId,
        author: Member,
        auth: WriteAuth<'_>,
    ) -> Result<EntryId> {
        let (_substrate, ledger, channel_id) = self.resolve_for_write(channel).await?;
        let mut guard = ledger.lock().await;
        let view = guard.project_fresh(&channel_id).await?;
        self.check_write_auth(&view, &author, &auth)?;
        if !view.subjects.iter().any(|(id, _)| *id == target) {
            let ever_attached = view.entries.iter().any(|entry| {
                entry.id == target && matches!(entry.payload, EntryPayload::SubjectAttached { .. })
            });
            if ever_attached {
                bail!(
                    "subject attachment {target} is already detached — nothing to withdraw \
                     (docs/adr/0037)"
                );
            }
            bail!(
                "{target} is not a subject attachment in this channel — check view_channel for \
                 the attachment id"
            );
        }
        let mut entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: channel_id,
            author,
            timestamp: Timestamp::now(),
            payload: EntryPayload::SubjectDetached { target },
        };
        self.sign_entry(&mut entry);
        let id = entry.id;
        guard.append(entry).await?;
        Ok(id)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto detaching_ 2>&1 | tail -20`

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/host.rs
git commit -m "feat(host): detach_subject withdraws an attached Subject"
```

---

### Task 2: `attach_subject` refuses a kind mismatch

**Files:**
- Modify: `crates/junto/src/host.rs:947-957` (doc comment) and `:981-983` (the guard)
- Test: `crates/junto/src/host.rs` (tests module, after Task 1's tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: no signature change. `attach_subject` now returns `Err` when the channel already carries `subject.uri` under a *different* `SubjectKind`. Task 4's `attach_document` tool surfaces this error text verbatim.

**Why this is safe:** the only production caller is `web.rs::launch_session` (`:932`), which always passes `SubjectKind::Repo`, so a mismatch is unreachable in shipped code paths. The existing test `attaching_the_same_subject_twice_is_idempotent` (`:2367`) attaches the *same* kind twice and stays green untouched.

- [ ] **Step 1: Write the failing test**

Append to the tests module in `crates/junto/src/host.rs`:

```rust
    #[tokio::test]
    async fn attaching_the_same_uri_under_a_different_kind_refuses() {
        // ADR 0037 makes a Subject's uri its identity, compared as an exact
        // string, and the mount store keys on uri alone — so one uri under
        // two kinds is incoherent rather than merely redundant. Refusing
        // appends nothing, which is the only safe answer in an append-only
        // record; the old behaviour silently handed back the Repo
        // attachment to a caller who asked for a Document.
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "subjects", dan(), None)
            .await
            .unwrap();

        let uri = Uri::new("https://example.com/thing").expect("valid uri");
        let as_repo = Subject::new(SubjectKind::Repo, uri.clone());
        let first = host
            .attach_subject("subjects", as_repo.clone(), dan(), WriteAuth::Human)
            .await
            .expect("attach as repo");

        let as_document = Subject::new(SubjectKind::Document, uri);
        let err = host
            .attach_subject("subjects", as_document, dan(), WriteAuth::Human)
            .await
            .expect_err("the same uri under a different kind must refuse");
        assert!(
            format!("{err}").contains("already attached"),
            "the error must name the conflict, not the authorization: {err}"
        );

        let (_, view) = project(&host, "subjects").await;
        assert_eq!(
            view.subjects,
            vec![(first, as_repo)],
            "a refused attach must append nothing and leave the original kind intact"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p junto attaching_the_same_uri_under_a_different_kind 2>&1 | tail -20`

Expected: FAIL — `the same uri under a different kind must refuse`, because today the guard returns `Ok(existing)`.

- [ ] **Step 3: Replace the guard**

In `crates/junto/src/host.rs`, replace the single guard line at `:981-983`:

```rust
        if let Some((existing, attached)) = view.subjects.iter().find(|(_, s)| s.uri == subject.uri)
        {
            if attached.kind != subject.kind {
                bail!(
                    "'{}' is already attached to this channel as {:?}, not {:?} — a Subject's \
                     uri is its identity (docs/adr/0037), so one uri cannot be two kinds. \
                     Detach {existing} first if the kind is wrong.",
                    subject.uri.as_str(),
                    attached.kind,
                    subject.kind
                );
            }
            return Ok(*existing);
        }
```

- [ ] **Step 4: Extend the doc comment**

In the same file, the `**Idempotent**` paragraph at `:947-948` currently reads "if the channel already carries a subject with this uri, nothing is appended". Replace those two sentences' worth of text so it reads:

```rust
    /// **Idempotent on `(uri, kind)`**: if the channel already carries this
    /// uri under the *same* kind, nothing is appended — the existing
    /// attachment's `EntryId` is returned instead. The same uri under a
    /// *different* kind is refused outright rather than silently resolving
    /// to the existing attachment: a Subject's uri is its identity
    /// (`docs/adr/0037`), and `crate::mounts` keys the mount store on uri
    /// alone, so admitting both kinds would make this machine's mount for
    /// it ambiguous. Refusing appends nothing.
```

Also update the stale sentence in the same doc comment at `:955`, which says "The record is append-only with no `SubjectDetached` write surface yet, so a duplicate here would be permanent". Replace with:

```rust
    /// The record is append-only, so a duplicate here would need an explicit
    /// [`Host::detach_subject`] to withdraw — worth the extra fold every
    /// caller inherits.
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `rtk cargo test -p junto attaching_ 2>&1 | tail -20`

Expected: all `attaching_*` tests pass, including the untouched `attaching_the_same_subject_twice_is_idempotent`.

- [ ] **Step 6: Commit**

```bash
git add crates/junto/src/host.rs
git commit -m "feat(host): attach_subject refuses one uri under two kinds"
```

---

### Task 3: The machine-path guard

**Files:**
- Modify: `crates/junto/src/mcp.rs` (add `subject_uri` after `parse_provenance`, which ends at `:459`)
- Test: `crates/junto/src/mcp.rs` (tests module)

**Interfaces:**
- Consumes: `junto_kernel::Uri`, the existing `invalid(…) -> McpError` helper (`:382`).
- Produces: `fn subject_uri(raw: &str) -> Result<Uri, McpError>`. Task 4's `attach_document` calls exactly this.

**What it does and does not do:** it refuses strings that are provably machine-local, and nothing else. It does not validate schemes, normalize, or canonicalize. `git@github.com:owner/repo.git` passes.

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `crates/junto/src/mcp.rs`:

```rust
    #[test]
    fn subject_uri_refuses_machine_local_paths() {
        // A Subject's uri enters the durable record and is compared as an
        // exact string, so a path that only means something on one machine
        // is a permanent, unportable identity. `Uri::new` alone rejects
        // only the empty string, which is why this guard exists.
        for raw in [
            "/home/dan/spec.md",
            "\\\\server\\share\\spec.md",
            "\\spec.md",
            "D:/git/junto/spec.md",
            "D:\\git\\junto\\spec.md",
            "c:/notes.md",
            "spec.md",
            "docs/spec.md",
            "",
        ] {
            assert!(
                subject_uri(raw).is_err(),
                "'{raw}' is machine-local or has no scheme and must be refused"
            );
        }
    }

    #[test]
    fn subject_uri_accepts_anything_with_a_real_scheme() {
        // Deliberately permissive past the machine-path rules: the guard
        // keeps unportable identities out, it does not police address
        // formats. `git@host:path` is the case an earlier, stricter draft
        // wrongly refused, so it is pinned here.
        for raw in [
            "https://example.com/spec.md",
            "file:///notes/spec.md",
            "git+https://github.com/owner/repo.git",
            "git+ssh://git@github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "urn:isbn:0451450523",
            "notion://page/abc123",
        ] {
            assert!(subject_uri(raw).is_ok(), "'{raw}' must be accepted");
        }
    }

    #[test]
    fn subject_uri_trims_and_preserves_the_rest_verbatim() {
        // No normalization (ADR 0037 keeps exact-string identity), so the
        // accepted value differs from the input only by surrounding
        // whitespace.
        let uri = subject_uri("  https://example.com/A%20Spec.md  ").expect("accepted");
        assert_eq!(uri.as_str(), "https://example.com/A%20Spec.md");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto subject_uri_ 2>&1 | tail -20`

Expected: compile error — `cannot find function 'subject_uri' in this scope`.

- [ ] **Step 3: Implement the guard**

Insert in `crates/junto/src/mcp.rs` immediately after `parse_provenance` (`:459`):

```rust
/// Validate a **Subject** uri: refuse what is provably machine-local, and
/// nothing else.
///
/// A Subject's uri is its *identity*, compared as an exact string with no
/// normalization (`docs/adr/0037`), and it enters an append-only record —
/// so unlike a [`ProvenanceRef`]'s uri, which merely decorates an entry, a
/// path that means something on only one machine is a permanent identity
/// nobody else can resolve. [`Uri::new`] rejects only the empty string, so
/// the check has to live here.
///
/// Three rules, and deliberately no fourth: a leading path separator (a
/// POSIX absolute path or a UNC share), no `:` at all (a bare or relative
/// path), or a single character before the first `:` (a Windows drive
/// letter — note `D:/x` is otherwise a perfectly well-formed uri whose
/// scheme is `d`, which is exactly why "require a scheme" does not work).
/// Anything else passes, including an SCP-style git remote
/// (`git@host:path`): the guard keeps unportable identities out, it does
/// not police address formats.
///
/// # Errors
/// [`McpError::invalid_params`] naming the rule and the fix, so an agent
/// self-corrects without a round trip.
fn subject_uri(raw: &str) -> Result<Uri, McpError> {
    const WHY: &str = "a Subject's uri is recorded durably and compared as an exact string \
                       (docs/adr/0037), so it must mean the same thing on every member's \
                       machine";
    let text = raw.trim();
    if text.starts_with('/') || text.starts_with('\\') {
        return Err(invalid(format!(
            "'{text}' is a path on this machine, not a portable identity — {WHY}. Give a uri \
             with a scheme, e.g. https://example.com/spec.md or file:///notes/spec.md"
        )));
    }
    let Some((scheme, _)) = text.split_once(':') else {
        return Err(invalid(format!(
            "'{text}' has no scheme — {WHY}. Give a uri with a scheme, e.g. \
             https://example.com/spec.md or file:///notes/spec.md"
        )));
    };
    if scheme.chars().count() < 2 {
        return Err(invalid(format!(
            "'{text}' looks like a Windows drive path, not a portable identity — {WHY}. Give a \
             uri with a scheme, e.g. file:///C:/notes/spec.md if you really mean a local file"
        )));
    }
    Uri::new(text).map_err(|err| invalid(err.to_string()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p junto subject_uri_ 2>&1 | tail -20`

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/mcp.rs
git commit -m "feat(mcp): subject_uri refuses machine-local Subject identities"
```

---

### Task 4: The `attach_document` and `detach_subject` MCP tools

**Files:**
- Modify: `crates/junto/src/mcp.rs` — imports (`:25-28`), `TargetKind` (`:383-390`), `resolve_target`'s `described`/`bears_kind` (`:399-409`), two request structs (beside the others, near `:166`), two tool handlers (after `converge_channel`, `:665`), `get_info` instructions (`:1053-1075`)
- Test: `crates/junto/src/mcp.rs` (tests module)

**Interfaces:**
- Consumes: `subject_uri` (Task 3); `Host::attach_subject` with Task 2's guard; `Host::detach_subject` (Task 1); existing `self.resolve`, `self.authorize`, `resolve_target`, `text`, `invalid`, `internal`.
- Produces: MCP tools `attach_document` and `detach_subject`; `TargetKind::Subject`.

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `crates/junto/src/mcp.rs`:

```rust
    #[tokio::test]
    async fn attach_document_makes_a_channel_be_about_a_document() {
        // The case ADR 0037 shipped in the kernel and no surface could
        // reach: PR #69's dogfood needed a temporary in-crate probe for it.
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "spec-work").await;

        let attached = mcp
            .attach_document(Parameters(AttachDocumentRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                uri: "https://example.com/design.md".into(),
                digest: None,
            }))
            .await
            .expect("attach a document");
        assert!(
            text_of(&attached).contains("attached document"),
            "{}",
            text_of(&attached)
        );

        let view = mcp
            .view_channel(Parameters(ViewRequest {
                channel: "spec-work".into(),
                full: true,
            }))
            .await
            .expect("view");
        assert!(
            text_of(&view).contains("design.md"),
            "the attached document must show on the channel: {}",
            text_of(&view)
        );
    }

    #[tokio::test]
    async fn attach_document_refuses_a_machine_path() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "spec-work").await;

        let err = mcp
            .attach_document(Parameters(AttachDocumentRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                uri: "D:/git/junto/docs/design.md".into(),
                digest: None,
            }))
            .await
            .expect_err("a machine path must not enter the ledger");
        assert!(
            format!("{err:?}").contains("drive path"),
            "the refusal must say why and how to fix it: {err:?}"
        );
    }

    #[tokio::test]
    async fn attach_document_keeps_a_caller_supplied_digest() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "spec-work").await;

        mcp.attach_document(Parameters(AttachDocumentRequest {
            channel: "spec-work".into(),
            author: claude(),
            code: code_of(&dirs, claude()),
            uri: "https://example.com/design.md".into(),
            digest: Some("sha256:deadbeef".into()),
        }))
        .await
        .expect("attach with a digest");

        let bad = mcp
            .attach_document(Parameters(AttachDocumentRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                uri: "https://example.com/other.md".into(),
                digest: Some("deadbeef".into()),
            }))
            .await
            .expect_err("a digest with no algorithm prefix must be refused");
        assert!(format!("{bad:?}").contains("algorithm"), "{bad:?}");
    }

    #[tokio::test]
    async fn detach_subject_withdraws_by_id_prefix() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "spec-work").await;

        let attached = mcp
            .attach_document(Parameters(AttachDocumentRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                uri: "https://example.com/design.md".into(),
                digest: None,
            }))
            .await
            .expect("attach");
        // "attached document <uri> to channel '<name>' (attachment <id>)."
        let confirmation = text_of(&attached);
        let id = confirmation
            .rsplit_once("attachment ")
            .and_then(|(_, tail)| tail.split(')').next())
            .expect("attachment id in confirmation")
            .to_string();

        let detached = mcp
            .detach_subject(Parameters(DetachSubjectRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                target: id[..8].to_string(),
            }))
            .await
            .expect("detach by prefix");
        assert!(
            text_of(&detached).contains("detached"),
            "{}",
            text_of(&detached)
        );

        let again = mcp
            .detach_subject(Parameters(DetachSubjectRequest {
                channel: "spec-work".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                target: id,
            }))
            .await
            .expect_err("a detached attachment is no longer a detach target");
        let _ = again;
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p junto attach_document 2>&1 | tail -20`

Expected: compile errors — `AttachDocumentRequest` and `attach_document` do not exist.

- [ ] **Step 3: Add the kernel imports**

In `crates/junto/src/mcp.rs`, extend the `junto_kernel` import at `:25-28` to include `Subject` and `SubjectKind`:

```rust
use junto_kernel::{
    ApprovalRequirement, ChannelId, ChannelView, ContentDigest, EntryId, EntryPayload, LedgerEntry,
    Member, ProvenanceRef, SessionState, Subject, SubjectKind, Timestamp, Uri,
};
```

- [ ] **Step 4: Add `TargetKind::Subject`**

In the `TargetKind` enum (`:383-390`), add:

```rust
    /// detach_subject acts on subject attachments.
    Subject,
```

In `resolve_target` (`:399-409`), add to `described`:

```rust
        TargetKind::Subject => "a subject attachment (detach_subject targets)",
```

and to `bears_kind`:

```rust
        TargetKind::Subject => view.subjects.iter().any(|(sid, _)| sid == id),
```

- [ ] **Step 5: Add the request structs**

In `crates/junto/src/mcp.rs`, beside the other request structs (after `ConvergeRequest`):

```rust
/// Attach a Document Subject — what a channel is *about* (`docs/adr/0037`).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AttachDocumentRequest {
    /// Channel name (bound at open_channel) or raw channel id.
    pub channel: String,
    /// Who is writing.
    pub author: AuthorParam,
    /// The author's member code (`docs/adr/0017`).
    pub code: Option<String>,
    /// Where the document lives, machine-independently — a uri with a
    /// scheme, e.g. `https://…`, `file:///…`, or a wiki/page uri. A path on
    /// your own machine is refused: this is recorded durably and must mean
    /// the same thing to every member.
    pub uri: String,
    /// Optional content digest in `algorithm:value` form (e.g. "sha256:…"),
    /// captured now so later drift of the document is detectable. Supply it
    /// yourself; junto never computes one.
    pub digest: Option<String>,
}

/// Withdraw an attached Subject (`docs/adr/0037`).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct DetachSubjectRequest {
    /// Channel name (bound at open_channel) or raw channel id.
    pub channel: String,
    /// Who is writing.
    pub author: AuthorParam,
    /// The author's member code (`docs/adr/0017`).
    pub code: Option<String>,
    /// The attachment entry to withdraw — a full entry id or an unambiguous
    /// prefix of 6+ characters, as shown by view_channel.
    pub target: String,
}
```

- [ ] **Step 6: Add the two tool handlers**

In `crates/junto/src/mcp.rs`, after `converge_channel`'s closing brace (`:665`):

```rust
    #[tool(
        description = "Attach a Document Subject — what this channel is about (docs/adr/0037): a spec, a design doc, a wiki page, a ticket. `uri` must carry a scheme and mean the same thing on every member's machine; a path on your own disk is refused, because a Subject's uri is recorded durably and compared as an exact string. `digest` is optional and yours to supply — junto never computes one. Returns the attachment id, which detach_subject targets. You must be a member; pass your `code`."
    )]
    async fn attach_document(
        &self,
        Parameters(req): Parameters<AttachDocumentRequest>,
    ) -> Result<CallToolResult, McpError> {
        let uri = subject_uri(&req.uri)?;
        let subject = match req.digest {
            Some(digest) => {
                let digest = ContentDigest::new(digest).map_err(|e| invalid(e.to_string()))?;
                Subject::with_digest(SubjectKind::Document, uri, digest)
            }
            None => Subject::new(SubjectKind::Document, uri),
        };
        let id = self
            .host
            .attach_subject(
                &req.channel,
                subject,
                req.author.into(),
                crate::host::WriteAuth::Agent(req.code.as_deref()),
            )
            .await
            .map_err(|err| invalid(err.to_string()))?;
        Ok(text(format!(
            "attached document {} to channel '{}' (attachment {id}). \
             detach_subject targets that id.",
            req.uri.trim(),
            req.channel
        )))
    }

    #[tool(
        description = "Withdraw an attached Subject (docs/adr/0037): record a detachment targeting its attachment entry, so the channel stops being about it. The attachment stays in the log — the record is append-only — but projections drop it. `target` is the attachment id or an unambiguous 6+ char prefix. You must be a member; pass your `code`."
    )]
    async fn detach_subject(
        &self,
        Parameters(req): Parameters<DetachSubjectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Subject)?;
        let id = self
            .host
            .detach_subject(
                &req.channel,
                target,
                author,
                crate::host::WriteAuth::Agent(req.code.as_deref()),
            )
            .await
            .map_err(|err| invalid(err.to_string()))?;
        Ok(text(format!(
            "detached subject attachment {target} from channel '{}' (detachment {id})",
            req.channel
        )))
    }
```

- [ ] **Step 7: Name both tools in the server instructions**

In `get_info` (`:1053-1075`), the instructions string is the only place enumerating the tool set. Insert after "…(attach_artifact), " and before "grant membership":

```
             say what a channel is about and stop saying it (attach_document/detach_subject), \
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `rtk cargo test -p junto attach_document 2>&1 | tail -20` then `rtk cargo test -p junto detach_subject 2>&1 | tail -20`

Expected: 4 passed across the two runs.

- [ ] **Step 9: Commit**

```bash
git add crates/junto/src/mcp.rs
git commit -m "feat(mcp): attach_document and detach_subject tools"
```

---

### Task 5: Let a scratch session keep being steered

**Files:**
- Modify: `crates/junto/src/web.rs:1119-1168`
- Test: `crates/junto/src/web.rs:5236-5357` (invert), plus one new test after it

**Interfaces:**
- Consumes: existing `crate::launch::{executable_mount, session_workdir}`, `channel_has_unmounted_repo_subject`, `NO_MOUNTABLE_SUBJECT`.
- Produces: no signature change. Behaviour: a session whose scratch directory exists is steered there without consulting mounts at all.

**Read this before editing the tests.** `steering_refuses_when_the_channel_now_carries_an_unmounted_repo_subject` (`:5237`) launches its session **in scratch** — its own comment at `:5276-5278` says so — and asserts the steer refuses. That is exactly the scenario this task makes succeed, so the test must be **inverted, not repaired**. Its stated rationale ("silently resume the agent in a fresh, empty `session_workdir` scratch directory holding none of the session's real work") was true when written and was invalidated by the scratch pin added in a later fix round: the resumed session lands in the directory it already ran in. It is also the *only* test covering `steer_session`'s refusal, so a replacement covering the case the refusal still catches is mandatory — otherwise this task deletes that arm's coverage while leaving the code in place.

- [ ] **Step 1: Invert the existing test**

In `crates/junto/src/web.rs`, rename `steering_refuses_when_the_channel_now_carries_an_unmounted_repo_subject` (`:5237`) to `steering_a_scratch_session_survives_a_teammates_unmounted_repo_subject` and replace its leading comment (`:5238-5246`) with:

```rust
        // A session that legitimately ran in `<junto_home>/scratch/<session>`
        // must stay steerable when the channel later gains a Repo subject
        // this machine has no mount for — which needs no local action at
        // all, since a teammate's `attach_subject` arrives by sync.
        //
        // This test asserted the opposite until the scratch pin landed. Its
        // original rationale (fix round 2, finding 3) was that resuming
        // would drop the agent in "a fresh, empty `session_workdir` scratch
        // directory holding none of the session's real work" — true then,
        // false now: the pin resumes the session in the scratch directory it
        // already ran in, which holds exactly its prior work. The refusal it
        // was defending is still correct for a session that ran in a *mount*
        // that has since vanished, which
        // `steering_still_refuses_when_the_mount_it_ran_in_is_gone` covers.
```

Then replace the assertion block at `:5345-5353` with:

```rust
        assert_eq!(
            steered.status(),
            StatusCode::SEE_OTHER,
            "a session that already ran in scratch keeps running there: {:?}",
            steered.status()
        );
```

- [ ] **Step 2: Run it to verify it fails**

Run: `rtk cargo test -p junto steering_a_scratch_session_survives 2>&1 | tail -20`

Expected: FAIL — got `400 Bad Request`, wanted `303 See Other`.

- [ ] **Step 3: Hoist the pin above the refusal**

In `crates/junto/src/web.rs`, replace the whole block from `:1119` (the comment opening "A channel with a mounted…") through `:1168` (the end of the `workspace` binding) with:

```rust
            // The same id's workdir `launch_session` resolved when this
            // session started (`session_workdir`): a mounted subject's
            // path, or the scratch directory a repo-free channel gets.
            //
            // A session that already ran in scratch keeps running there,
            // and that decision comes FIRST — before any mount check.
            // `<junto_home>/scratch/<session>` is created by
            // `session_workdir` and never removed, so its existence is
            // already a durable, machine-local record that this session ran
            // in scratch. Re-resolving would silently move a resumed turn
            // into a repo mounted after launch, running the agent in a
            // checkout this session was never launched against and
            // recording that repo's uncommitted changes as this session's
            // diff artifact (ADR 0038's recorded limit). It also means an
            // unmounted Repo subject arriving by sync — a teammate's
            // `attach_subject`, needing no local action at all — cannot
            // strand a scratch session that was never going to use a mount.
            let scratch = junto_home.join("scratch").join(session.to_string());
            let workspace = if scratch.is_dir() {
                scratch
            } else {
                // No scratch directory: this session ran in a mount. A
                // channel with a mounted, `Execute`-capable subject resolves
                // through it regardless of anything else the channel carries
                // (the same rule `launch_session` follows —
                // `a_mounted_repo_subject_still_launches_alongside_a_second_unmounted_one`).
                // Refuse only when *nothing* is `Execute`-capable *and* a
                // Repo subject this machine has no mount for exists
                // (`mounts.toml` edited, or the entry removed after launch):
                // that is a fixable gap, and silently resuming into a fresh
                // scratch directory would discard whatever work already
                // exists in the mount the human just lost.
                let executable = match crate::launch::executable_mount(&junto_home, &view) {
                    Ok(mount) => mount,
                    Err(err) => return internal(format!("reading mounts: {err}")),
                };
                if executable.is_none() {
                    match channel_has_unmounted_repo_subject(&junto_home, &view) {
                        Ok(true) => {
                            return (StatusCode::BAD_REQUEST, NO_MOUNTABLE_SUBJECT).into_response();
                        }
                        Ok(false) => {}
                        Err(response) => return response,
                    }
                }
                match crate::launch::session_workdir(&junto_home, &view, session) {
                    Ok(workspace) => workspace,
                    Err(err) => return internal(format!("preparing a workdir: {err}")),
                }
            };
```

- [ ] **Step 4: Run it to verify it passes**

Run: `rtk cargo test -p junto steering_ 2>&1 | tail -20`

Expected: all `steering_*` tests pass, including the untouched `steering_succeeds_when_a_mounted_repo_subject_is_present_alongside_an_unmounted_one`.

- [ ] **Step 5: Write the replacement refusal test**

The refusal now fires only for a session that ran in a real mount that has since gone. Append after the inverted test in `crates/junto/src/web.rs`, following the fixture pattern of `steering_succeeds_when_a_mounted_repo_subject_is_present_alongside_an_unmounted_one` (`:5359`) — write the harness stub, `set_var("JUNTO_HARNESS_CMD", …)`, launch with a mounted repo via `attach_and_mount_repo` (`:3567`), poll for the session to reach `Done`, then remove the mount before steering:

```rust
    #[tokio::test]
    async fn steering_still_refuses_when_the_mount_it_ran_in_is_gone() {
        // The case the NO_MOUNTABLE_SUBJECT refusal genuinely catches, and
        // the only remaining one: this session ran in a real mount, so it
        // has no scratch directory to be pinned to. If `mounts.toml` is
        // edited (or the checkout removed) between turns, resuming would
        // drop the agent into a fresh, empty scratch directory holding none
        // of the work — so it must refuse loudly instead.
        //
        // Without this test, hoisting the pin above the refusal would leave
        // `steer_session`'s refusal arm with no coverage at all.
        let home = crate::host::test_home::HomeGuard::new();
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = if cfg!(windows) {
            let path = stub_dir.path().join("stub.cmd");
            std::fs::write(
                &path,
                "@echo {\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\",\
                 \"session_id\":\"h-steer-mount-gone-1\",\"is_error\":false}\r\n",
            )
            .expect("write stub");
            path
        } else {
            let path = stub_dir.path().join("stub.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\
                 \"ok\",\"session_id\":\"h-steer-mount-gone-1\",\"is_error\":false}'\n",
            )
            .expect("write stub");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod stub");
            }
            path
        };
        unsafe { std::env::set_var("JUNTO_HARNESS_CMD", &stub) };

        let fx = host_with_entry(assertion()).await;
        let workspace = tempfile::tempdir().expect("workspace");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(workspace.path())
                .status()
                .expect("git init")
                .success()
        );
        attach_and_mount_repo(&fx, home.path(), workspace.path()).await;

        let launched = launch_session(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LaunchForm {
                intent: "do work in the repo".into(),
                workspace: String::new(),
                agent: String::new(),
                mode: String::new(),
            }),
        )
        .await;
        assert_eq!(launched.status(), StatusCode::SEE_OTHER);

        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("web-test").await.unwrap()
        else {
            panic!("channel resolves");
        };
        let mut session_id = None;
        for _ in 0..100 {
            let view = ledger.lock().await.project(&id).await.unwrap();
            if let Some((sid, _)) = view
                .sessions
                .iter()
                .find(|(_, s)| s.state == junto_kernel::SessionState::Done)
            {
                session_id = Some(*sid);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let session_id = session_id.expect("launch turn reached done");

        // The session ran in the mount, so it has no scratch directory.
        let junto_home = home.path();
        assert!(
            !junto_home
                .join("scratch")
                .join(session_id.to_string())
                .is_dir(),
            "a session launched in a mount must not have a scratch directory"
        );

        // Now the mount goes away, leaving the Repo subject unmounted.
        std::fs::remove_file(junto_home.join("mounts.toml")).expect("drop the mount store");

        let steered = steer_session(
            State(fx.host.clone()),
            Path(("web-test".into(), session_id.to_string())),
            Form(SteerForm {
                message: "keep going".into(),
            }),
        )
        .await;
        assert_eq!(
            steered.status(),
            StatusCode::BAD_REQUEST,
            "losing the mount a session ran in must refuse, not silently restart in scratch"
        );
        let bytes = axum::body::to_bytes(steered.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&bytes), NO_MOUNTABLE_SUBJECT);
        let _ = (home, workspace);

        unsafe { std::env::remove_var("JUNTO_HARNESS_CMD") };
    }
```

`attach_and_mount_repo(&fx, junto_home, path)` returns `()`; the
`workspace` TempDir must stay alive for the whole test, which is what the
final `let _ = (home, workspace);` is for.

- [ ] **Step 6: Run both tests**

Run: `rtk cargo test -p junto steering_ 2>&1 | tail -20`

Expected: all pass, including the new refusal test.

- [ ] **Step 7: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "fix(web): steer a scratch session past an unmounted repo subject"
```

---

### Task 6: The three documentation debts

**Files:**
- Modify: `crates/junto/src/mounts.rs:88-90`
- Modify: `docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md` (the known-limit paragraph)
- Modify: `docs/adr/0037-subjects-and-mounts.md:47-53` (Known limits)

**Interfaces:** none. Documentation only; no code behaviour changes.

- [ ] **Step 1: Fix `remember_mount`'s call order**

In `crates/junto/src/mounts.rs`, the doc comment at `:88-90` says `launch_session` "calls this once `Host::attach_subject` has recorded the Subject a typed path implies." The final fix wave of PR #69 reversed that so the irreversible ledger append lands last (`web.rs:917-924` states this explicitly). Replace with:

```rust
/// of the Mount store: `crate::web`'s `launch_session` calls this *before*
/// `Host::attach_subject`, not after. `remember_mount` needs only the uri
/// and the path, both already in hand, so it moves ahead of the append and
/// the one irreversible step — the `SubjectAttached` entry — runs last among
/// what can still fail (`web.rs:917-924`).
```

- [ ] **Step 2: Correct ADR 0038's known limit**

In `docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md`,
replace line 59 in full. The substantive limit survives — nothing records
which mount a session actually ran in — but the mechanism's reach is
overstated. Exact replacement:

```markdown
`crate::launch::session_workdir` resolves a session's workdir by calling `mount_with_capability`, which walks a channel's *current* subjects against the *current* mount store and returns the first `Execute`-capable mount it finds. Nothing records which mount a session actually ran in at launch, so that resolution happens fresh every time it runs: on launch, and again on a steer **of a session that ran in a mount**. If a channel has two Execute-capable Repo subjects, mount A resolves first at launch, and A is later unmounted while B remains mounted, the *next* steer resolves to B instead — a resumed session silently moves to a different directory holding none of the session's prior work, rather than erroring or staying put. This applies to any channel with more than one mounted, Execute-capable subject.

A session that ran in **scratch** is exempt, and is the one case junto does pin: `crate::web::steer_session` checks for `<junto_home>/scratch/<session>` before it consults mounts at all, and resumes there when it exists. The directory is created by `session_workdir` and never removed, so its existence is itself the durable, machine-local record that this session ran in scratch — which is why the pin needs no new state. That check sits above the `Execute`-capable mount lookup and the unmounted-Repo refusal, so a scratch session is pinned to its scratch directory for its whole life, and a Repo subject arriving later by sync cannot move it or strand it.
```

- [ ] **Step 3: Close two of ADR 0037's known limits**

In `docs/adr/0037-subjects-and-mounts.md`, replace line 48 in full:

```markdown
**An attached Subject can now be withdrawn — but only by an agent or the CLI.** `Host::detach_subject` and the `detach_subject` MCP tool emit `SubjectDetached`, targeting the attachment entry; `Ledger::project_subjects` drops it order-insensitively while the attachment itself stays in the append-only log, exactly as a Park leaves its target standing (`0002`). The remaining gap is the *web* surface: the launch form's free-text workspace path is still the only human attach control, and there is no human detach control at all, so a human who mistypes a path fixes it through an agent or the CLI rather than the page that created it. That is a surface gap, not a mechanism gap.
```

And replace line 50 in full:

```markdown
**A `Document` Subject can now be attached by an agent.** The `attach_document` MCP tool (`crates/junto/src/mcp.rs`) attaches one, guarded by `subject_uri` so a machine-local path — a POSIX absolute path, a UNC share, or a Windows drive path, the last of which is otherwise a well-formed URI whose scheme is a single letter — cannot become a durable identity. The digest is caller-supplied and optional: junto never computes one, matching `ContentDigest`'s standing note that digests are "not yet computed or verified by the kernel", and avoiding a value that two equal-target platforms would compute differently for the same text document. The guard deliberately stops there and does **not** validate schemes, so `git@host:path` is accepted — normalization stays rejected, per the limit above and this ADR's Considered section. The remaining gap is again the web surface: there is no human control for attaching a Document, and `web.rs::launch_session` is still the only path that attaches a `Repo`.
```

Leave the other three limits standing untouched — in particular exact-string
URI identity, which this plan does not change: the guard refuses machine
paths, it does not normalize.

Finally, append one sentence to the exact-string limit at line 44, since the
kind refusal is a direct consequence of uri being the identity:

```markdown
Because the uri alone is the identity, `Host::attach_subject` now refuses the same uri under a different `SubjectKind` rather than silently returning the existing attachment: one identity cannot be two kinds, and `crate::mounts` keys the mount store on uri alone, so admitting both would make this machine's mount for it ambiguous. The refusal appends nothing.
```

- [ ] **Step 4: Verify the docs claim nothing false**

Run: `rtk cargo test --workspace 2>&1 | tail -10`

Expected: still green — this task changes no code behaviour. Then re-read each edited paragraph against the code it describes.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/mounts.rs docs/adr/0037-subjects-and-mounts.md docs/adr/0038-the-collapse-cheap-channels-and-derived-standing.md
git commit -m "docs: close two of 0037's known limits; correct 0038 and remember_mount"
```

---

### Task 7: Full verification and the push-gate

**Files:** none modified.

- [ ] **Step 1: Run the pre-commit sequence in order, stopping on first failure**

```bash
rtk cargo fmt --check
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
```

Expected: clean, clean, and **more than 596 passing** — this plan adds 12 tests and inverts 1, so 608 is the target. `golden_canonical_form_is_byte_stable` must be among the passes, untouched.

- [ ] **Step 2: Dogfood the surface this plan built**

Attach this plan's own spec as a Document Subject on channel `6cd0cbb8` through the new tool, then detach it. That exercises `attach_document` and `detach_subject` against the live host rather than only in tests, and it is the first time a junto channel can be about the document it is working from.

- [ ] **Step 3: Open the PR through junto's own push-gate**

Not `gh pr create`. Propose the code-PR gate on channel `6cd0cbb8` and let the approved gate open the PR.
