# Live Session Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Watchers join a running Session mid-flight across machines, see its conversation and worktree evolve with presence, and attach span-anchored signed comments that flow back to the driving agent as steering context.

**Architecture:** A new `junto-live` crate wraps one loro CRDT document per live session (driver-only `conversation`/`worktree` containers, multi-actor `annotations`, ephemeral presence). The driver's junto host serves a WebSocket endpoint with ed25519 challenge-response auth; validated watcher annotations feed the shipped mid-turn steer machinery. Anchor types live in `junto-kernel`; git re-anchoring in `junto-substrate-git`. The durable record is untouched.

**Tech Stack:** Rust, loro 1.13 (MIT), axum 0.8 (`ws` feature), tokio-tungstenite (host tests + iced client), ed25519-dalek (via junto-kernel), existing git shell-out pattern.

**Spec:** `docs/superpowers/specs/2026-08-20-live-session-plane-design.md`

## Global Constraints

- MIT-compatible deps only; loro is MIT (verified v1.13.9 — pin `loro = "1.13"`).
- NO changes to `LedgerEntry`, entry kinds, ADR 0011 sync, or `refs/junto/*` layout.
- The live plane may never block or corrupt the session loop: publishing into a LiveDoc is fire-and-forget; every live-plane error is logged and swallowed at the session-loop boundary.
- Loro peer IDs are random per process (never derived from identity — duplicate PeerIDs corrupt a doc); authorship authority is the ed25519 signature inside each annotation, never the peer ID.
- Windows is the primary dev platform: no unix-only APIs; subprocesses use the existing `CREATE_NO_WINDOW` pattern where applicable.
- Workspace members today: `crates/junto-kernel`, `crates/junto`, `crates/junto-substrate-git` (root `Cargo.toml`). `crates/junto-iced` is a SEPARATE workspace (own `Cargo.lock`).
- Commit after every task (steps include commits). Do not run formatters/linters beyond `cargo fmt` on touched files.

---

### Task 1: Kernel anchor + annotation types

**Files:**
- Create: `crates/junto-kernel/src/anchor.rs`
- Modify: `crates/junto-kernel/src/lib.rs` (module decl + re-exports, pattern at lines 1–61)
- Modify: `crates/junto-kernel/src/sign.rs` (expose byte-level sign/verify; today `fn sign` at line 116 is private)

**Interfaces:**
- Consumes: `ContentDigest`, `Uri` (provenance.rs), `EntryId` (ids.rs), `Member` (member.rs), `Timestamp` (time.rs), `SigningKey`/`PublicKey`/`Signature` (sign.rs), `serde_json_canonicalizer` (already a dep).
- Produces (later tasks rely on these exact names):
  - `pub struct Span { pub start: u32, pub end: u32 }` — 1-indexed inclusive lines; `Span::new(start, end) -> Result<Self>` rejects `start == 0 || end < start`.
  - `pub struct CommitOid(String)` — `CommitOid::new(impl Into<String>) -> Result<Self>` validates 40 lowercase hex; `as_str()`; serde `try_from`/`into` String like `Uri`.
  - `pub struct CodeAnchor { pub commit: CommitOid, pub path: String, pub blob: ContentDigest, pub span: Span }`
  - `pub struct StreamAnchor { pub session: EntryId, pub op_id: String }`
  - `pub enum Anchor { Code(CodeAnchor), Stream(StreamAnchor) }` — `#[serde(tag = "kind", rename_all = "snake_case")]`.
  - `pub struct AnnotationId(Uuid)` — same transparent-UUID pattern as `EntryId` (ids.rs lines 8–35): `new()`, `Display`, `FromStr`, `Default`.
  - `pub struct Annotation { pub id: AnnotationId, pub author: Member, pub anchor: Anchor, pub body: String, pub excerpt: Option<String>, pub supersedes: Option<AnnotationId>, pub urgent: bool, pub timestamp: Timestamp, pub signature: Option<Signature> }` with `signing_bytes()`, `sign(&mut self, &SigningKey)`, `verifies_with(&self, &PublicKey) -> bool`, `to_canonical_bytes()`, `from_canonical_bytes()` — mirroring `LedgerEntry`'s pattern (sign.rs lines 158–210, serial.rs lines 24–40).
  - `sign.rs`: `pub fn sign_bytes(&self, message: &[u8]) -> Signature` on `SigningKey` (rename the private `sign`, keep entry signing calling it); `pub fn verify_bytes(&self, message: &[u8], signature: &Signature) -> bool` on `PublicKey`.

- [ ] **Step 1: Write failing tests** in `anchor.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn span_rejects_zero_and_inverted() {
    assert!(Span::new(0, 5).is_err());
    assert!(Span::new(7, 3).is_err());
    assert!(Span::new(3, 3).is_ok());
}

#[test]
fn commit_oid_validates_shape() {
    assert!(CommitOid::new("a".repeat(40)).is_ok());
    assert!(CommitOid::new("A".repeat(40)).is_err()); // uppercase
    assert!(CommitOid::new("abc123").is_err()); // short
}

#[test]
fn annotation_round_trips_and_signs() {
    let key = crate::SigningKey::from_secret_bytes([7; 32]);
    let mut a = sample_annotation(); // helper: builds a CodeAnchor annotation with fixed fields
    a.sign(&key).unwrap();
    let bytes = a.to_canonical_bytes().unwrap();
    let parsed = Annotation::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(a, parsed);
    assert!(parsed.verifies_with(&key.public_key()));
}

#[test]
fn tampered_annotation_fails_verification() {
    let key = crate::SigningKey::from_secret_bytes([7; 32]);
    let mut a = sample_annotation();
    a.sign(&key).unwrap();
    a.body = "forged".into();
    assert!(!a.verifies_with(&key.public_key()));
}

#[test]
fn unsigned_annotation_never_verifies() {
    let key = crate::SigningKey::from_secret_bytes([7; 32]);
    assert!(!sample_annotation().verifies_with(&key.public_key()));
}

#[test]
fn stream_anchor_round_trips() {
    // Anchor::Stream serializes with kind tag and survives canonical round-trip.
    let a = Anchor::Stream(StreamAnchor { session: crate::EntryId::new(), op_id: "12@7".into() });
    let json = serde_json::to_string(&a).unwrap();
    assert!(json.contains("\"kind\":\"stream\""));
    assert_eq!(a, serde_json::from_str::<Anchor>(&json).unwrap());
}
```

Also add to `sign.rs` tests:

```rust
#[test]
fn byte_level_sign_verify_round_trip() {
    let key = SigningKey::from_secret_bytes([3; 32]);
    let sig = key.sign_bytes(b"nonce-bytes");
    assert!(key.public_key().verify_bytes(b"nonce-bytes", &sig));
    assert!(!key.public_key().verify_bytes(b"other", &sig));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p junto-kernel anchor -- --nocapture` and `cargo test -p junto-kernel byte_level`
Expected: FAIL (module/functions not defined).

- [ ] **Step 3: Implement**

`sign.rs`: rename private `fn sign` → `pub fn sign_bytes` (update the internal caller in `LedgerEntry::sign`); add on `PublicKey`:

```rust
/// Verify a detached signature over arbitrary bytes (WS auth, annotations).
#[must_use]
pub fn verify_bytes(&self, message: &[u8], signature: &Signature) -> bool {
    let (Ok(key), Ok(sig)) = (self.to_dalek(), signature.to_dalek()) else {
        return false;
    };
    key.verify_strict(message, &sig).is_ok()
}
```

`anchor.rs`: newtypes with the same `Error::Invariant` validation + serde `try_from`/`into` pattern as `provenance.rs` (lines 14–84). `Annotation::signing_bytes()` = canonical bytes of a clone with `signature: None` (exactly the `LedgerEntry::signing_bytes` trick, sign.rs line 158). Canonical bytes via `serde_json_canonicalizer::to_vec`. Derive `Debug, Clone, PartialEq, Eq, Serialize, Deserialize` throughout; skip-serializing-if-none on `signature`, `excerpt`, `supersedes` (matches the pre-existing omitted-field convention, see `ProvenanceRef.digest`).

`lib.rs`: `pub mod anchor;` + `pub use anchor::{Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, Span, StreamAnchor};`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p junto-kernel`
Expected: PASS, including all pre-existing tests (the `sign` rename must not break entry signing).

- [ ] **Step 5: Commit**

```bash
git add crates/junto-kernel
git commit -m "feat(kernel): anchor, annotation, and byte-level sign/verify types"
```

---

### Task 2: Git re-anchoring

**Files:**
- Create: `crates/junto-substrate-git/src/reanchor.rs`
- Modify: `crates/junto-substrate-git/src/lib.rs` (add `pub mod reanchor;`)

**Interfaces:**
- Consumes: `junto_kernel::{CodeAnchor, Span}` (Task 1).
- Produces:
  - `pub enum Reanchor { Exact { span: Span }, Moved { span: Span }, Orphaned }`
  - `pub async fn reanchor(worktree: &Path, anchor: &CodeAnchor) -> junto_kernel::Result<Reanchor>` — read-only; shells out to `git` in `worktree` (this module MAY touch a working tree, unlike the ledger substrate — it only reads).
  - `pub(crate) fn map_span(span: Span, hunks: &[Hunk]) -> Reanchor` — pure hunk math, unit-tested without git.
  - `pub(crate) struct Hunk { pub old_start: u32, pub old_len: u32, pub new_start: u32, pub new_len: u32 }` + `pub(crate) fn parse_hunks(diff: &str) -> Vec<Hunk>`.

**Algorithm (conservative v1):** run `git -C <worktree> diff --unified=0 <commit> -- <path>`. Empty output → `Exact`. File deleted (diff contains `deleted file mode` or `+++ /dev/null`) → `Orphaned`. Otherwise parse `@@ -a,b +c,d @@` headers (`b`/`d` default 1 when omitted); any hunk whose old range `[a, a+b)` intersects `[span.start, span.end]` → `Orphaned`; else shift by the summed `(d_len - b_len)` of hunks strictly above `span.start`; zero shift → `Exact`, nonzero → `Moved` with the shifted span. Use the existing subprocess pattern (`git_raw`, lib.rs lines 87–133) — factor a free function `async fn git_in(dir: &Path, args: &[&str]) -> Result<Vec<u8>>` in `reanchor.rs` with the same `CREATE_NO_WINDOW` Windows guard.

- [ ] **Step 1: Write failing unit tests for the pure hunk math** (in `reanchor.rs` tests):

```rust
#[test]
fn parse_hunks_reads_headers_with_and_without_lengths() {
    let diff = "@@ -3,2 +5,4 @@\n@@ -10 +14 @@\n";
    let hunks = parse_hunks(diff);
    assert_eq!(hunks[0], Hunk { old_start: 3, old_len: 2, new_start: 5, new_len: 4 });
    assert_eq!(hunks[1], Hunk { old_start: 10, old_len: 1, new_start: 14, new_len: 1 });
}

#[test]
fn untouched_file_is_exact() {
    let span = Span::new(5, 8).unwrap();
    assert_eq!(map_span(span, &[]), Reanchor::Exact { span });
}

#[test]
fn insertion_above_moves_span() {
    // 3 lines inserted at old line 2 → span shifts down by 3.
    let hunks = [Hunk { old_start: 2, old_len: 0, new_start: 2, new_len: 3 }];
    assert_eq!(
        map_span(Span::new(5, 8).unwrap(), &hunks),
        Reanchor::Moved { span: Span::new(8, 11).unwrap() }
    );
}

#[test]
fn edit_inside_span_orphans() {
    let hunks = [Hunk { old_start: 6, old_len: 1, new_start: 6, new_len: 1 }];
    assert_eq!(map_span(Span::new(5, 8).unwrap(), &hunks), Reanchor::Orphaned);
}

#[test]
fn deletion_below_span_is_exact() {
    let hunks = [Hunk { old_start: 20, old_len: 4, new_start: 20, new_len: 0 }];
    let span = Span::new(5, 8).unwrap();
    assert_eq!(map_span(span, &hunks), Reanchor::Exact { span });
}
```

- [ ] **Step 2: Run to verify failure**: `cargo test -p junto-substrate-git reanchor` → FAIL.
- [ ] **Step 3: Implement `Hunk`, `parse_hunks`, `map_span`** (pure Rust, no git). Run: PASS.
- [ ] **Step 4: Write failing scripted-repo tests** for the async `reanchor` fn, following the crate's tempdir pattern (lib.rs test helpers lines 600–625, `git_out` at 754):

```rust
#[tokio::test]
async fn reanchor_end_to_end_states() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    run_git(repo, &["init", "-q"]);
    std::fs::write(repo.join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
    run_git(repo, &["add", "."]);
    run_git(repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);
    let commit = String::from_utf8(git_stdout(repo, &["rev-parse", "HEAD"])).unwrap().trim().to_string();
    let anchor = CodeAnchor {
        commit: CommitOid::new(commit).unwrap(),
        path: "f.txt".into(),
        blob: ContentDigest::new("sha256:unused-here").unwrap(),
        span: Span::new(3, 4).unwrap(), // lines "c","d"
    };
    // Unchanged → Exact
    assert!(matches!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Exact { .. }));
    // Insert two lines at top → Moved to 5..6
    std::fs::write(repo.join("f.txt"), "x\ny\na\nb\nc\nd\ne\n").unwrap();
    assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Moved { span: Span::new(5, 6).unwrap() });
    // Edit inside the span → Orphaned
    std::fs::write(repo.join("f.txt"), "a\nb\nZZZ\nd\ne\n").unwrap();
    assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
    // Delete the file → Orphaned
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
}
```

(`run_git`/`git_stdout` are small local test helpers modeled on `git_out`; write them in this test module.)

- [ ] **Step 5: Implement `reanchor` + `git_in`**, run `cargo test -p junto-substrate-git` → PASS (all pre-existing tests too).
- [ ] **Step 6: Commit**

```bash
git add crates/junto-substrate-git
git commit -m "feat(substrate-git): span re-anchoring via unified-diff hunk mapping"
```

---

### Task 3: `junto-live` crate — LiveDoc

**Files:**
- Create: `crates/junto-live/Cargo.toml`, `crates/junto-live/src/lib.rs`, `crates/junto-live/src/doc.rs`
- Modify: root `Cargo.toml` (add `"crates/junto-live"` to `members`; add `loro = "1.13"` to `[workspace.dependencies]`)

**Interfaces:**
- Consumes: `junto_kernel::{Annotation, AnnotationId}` (Task 1), `loro::{LoroDoc, ExportMode}`.
- Produces:
  - `pub struct LiveDoc { doc: LoroDoc }` — `LiveDoc::new() -> Self` (random peer id — loro's default; do NOT call `set_peer_id`).
  - `pub fn push_conversation(&self, event: serde_json::Value)` / `pub fn push_worktree(&self, event: serde_json::Value)` — push JSON string onto LoroList `"conversation"` / `"worktree"`, then `commit()`.
  - `pub fn insert_annotation(&self, a: &Annotation) -> junto_kernel::Result<()>` — `LoroMap "annotations"`: key = `a.id.to_string()`, value = canonical-JSON string; then `commit()`.
  - `pub fn annotations(&self) -> Vec<Annotation>` — parse every map value; skip (don't error) unparseable ones.
  - `pub fn annotation_ids(&self) -> std::collections::HashSet<String>`
  - `pub fn export_snapshot(&self) -> Vec<u8>` (`ExportMode::Snapshot`), `pub fn import_update(&self, bytes: &[u8]) -> Result<(), String>` (loro import, stringified error), `pub fn subscribe_local_update(&self, f: impl Fn(&Vec<u8>) -> bool + Send + Sync + 'static) -> loro::Subscription`, `pub fn fork(&self) -> LiveDoc`.

Crate `Cargo.toml`: `junto-kernel = { path = "../junto-kernel" }`, `loro.workspace = true`, `serde.workspace = true`, `serde_json.workspace = true`. Match the workspace-dep declaration style of `crates/junto-substrate-git/Cargo.toml`.

- [ ] **Step 1: Scaffold crate + workspace membership; `cargo check -p junto-live`** passes with empty lib.
- [ ] **Step 2: Write failing tests** in `doc.rs`:

```rust
#[test]
fn concurrent_annotations_converge() {
    let key_a = junto_kernel::SigningKey::from_secret_bytes([1; 32]);
    let key_b = junto_kernel::SigningKey::from_secret_bytes([2; 32]);
    let a = LiveDoc::new();
    let b = LiveDoc::new();
    // Seed b from a's snapshot (watchers join from a snapshot).
    b.import_update(&a.export_snapshot()).unwrap();
    let mut ann_a = test_annotation("from a"); ann_a.sign(&key_a).unwrap();
    let mut ann_b = test_annotation("from b"); ann_b.sign(&key_b).unwrap();
    a.insert_annotation(&ann_a).unwrap();
    b.insert_annotation(&ann_b).unwrap();
    // Cross-import full snapshots (idempotent, order-free).
    b.import_update(&a.export_snapshot()).unwrap();
    a.import_update(&b.export_snapshot()).unwrap();
    assert_eq!(a.annotation_ids(), b.annotation_ids());
    assert_eq!(a.annotations().len(), 2);
}

#[test]
fn conversation_events_survive_snapshot() {
    let a = LiveDoc::new();
    a.push_conversation(serde_json::json!({"kind": "assistant", "text": "hi", "seq": 1}));
    let b = LiveDoc::new();
    b.import_update(&a.export_snapshot()).unwrap();
    // Read back via the doc's deep value; one list entry.
    assert_eq!(b.conversation_len(), 1);
}
```

(Add `pub fn conversation_len(&self) -> usize` to the interface — the watcher UI needs it anyway.)

- [ ] **Step 3: Run** `cargo test -p junto-live` → FAIL. **Implement `LiveDoc`.** Run → PASS.
- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/junto-live
git commit -m "feat(live): junto-live crate with per-session LiveDoc (loro)"
```

---

### Task 4: `junto-live` — wire frames + validated annotation import

**Files:**
- Create: `crates/junto-live/src/frame.rs`, `crates/junto-live/src/validate.rs`
- Modify: `crates/junto-live/src/lib.rs` (exports)
- Modify: `crates/junto-live/Cargo.toml` (add `base64.workspace = true`; add `base64` to root workspace deps if absent)

**Interfaces:**
- Produces:
  - `pub enum Frame { Challenge { nonce: String }, Auth { email: String, signature: String }, AuthOk, Update { data: String }, Ephemeral { data: String }, Rejected { reason: String }, End }` — `#[serde(tag = "t", rename_all = "snake_case")]`; `data`/`nonce` are base64/hex strings; helpers `Frame::update(bytes: &[u8]) -> Frame`, `Frame::update_bytes(&self) -> Option<Vec<u8>>` (base64 decode), same pair for `Ephemeral`.
  - `pub fn validate_annotation_update(doc: &LiveDoc, bytes: &[u8], sender_email: &str, keyring: &HashMap<String, junto_kernel::PublicKey>) -> Result<Vec<junto_kernel::Annotation>, String>`
    — fork `doc`, import `bytes` into the fork (reject on import error), diff `annotation_ids` fork-vs-doc; every NEW annotation must (a) parse, (b) have `author.email == sender_email`, (c) verify against `keyring[sender_email]`. Any failure → `Err(reason)` (whole frame rejected, per spec: dropped, never merged). Success → `Ok(new_annotations)`. The caller imports `bytes` into the real doc only on `Ok`.
    — A frame that adds NO annotations (e.g. presence rides Ephemeral; a stray conversation-write from a watcher) is `Err("watchers may only write annotations")` if it touches `conversation`/`worktree` (detect: fork's conversation_len/worktree len changed), else `Ok(vec![])`.

- [ ] **Step 1: Write failing tests** in `validate.rs`:

```rust
fn keyring_of(email: &str, key: &junto_kernel::SigningKey) -> HashMap<String, junto_kernel::PublicKey> {
    HashMap::from([(email.to_string(), key.public_key())])
}

#[test]
fn valid_signed_annotation_is_accepted_and_returned() {
    let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
    let server = LiveDoc::new();
    let watcher = LiveDoc::new();
    watcher.import_update(&server.export_snapshot()).unwrap();
    let mut ann = test_annotation_by("w@x.com", "looks wrong"); ann.sign(&key).unwrap();
    watcher.insert_annotation(&ann).unwrap();
    let update = watcher.export_snapshot(); // full snapshot is a valid update payload
    let got = validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key)).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].body, "looks wrong");
    // Server doc untouched until caller imports.
    assert!(server.annotations().is_empty());
}

#[test]
fn unsigned_annotation_rejects_whole_frame() { /* same setup, skip sign → Err */ }

#[test]
fn wrong_author_email_rejects() { /* annotation author x@y but sender_email w@x → Err */ }

#[test]
fn signature_by_other_key_rejects() { /* sign with key2, keyring holds key1's public → Err */ }

#[test]
fn watcher_writing_conversation_rejects() {
    let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
    let server = LiveDoc::new();
    let watcher = LiveDoc::new();
    watcher.import_update(&server.export_snapshot()).unwrap();
    watcher.push_conversation(serde_json::json!({"kind": "fake"}));
    let err = validate_annotation_update(&server, &watcher.export_snapshot(), "w@x.com", &keyring_of("w@x.com", &key)).unwrap_err();
    assert!(err.contains("only write annotations"));
}

#[test]
fn frame_update_round_trips_base64() {
    let f = Frame::update(b"\x00\x01binary");
    let json = serde_json::to_string(&f).unwrap();
    let back: Frame = serde_json::from_str(&json).unwrap();
    assert_eq!(back.update_bytes().unwrap(), b"\x00\x01binary");
}
```

- [ ] **Step 2: Run** → FAIL. **Implement.** Run `cargo test -p junto-live` → PASS.
- [ ] **Step 3: Commit**

```bash
git add crates/junto-live Cargo.toml Cargo.lock
git commit -m "feat(live): wire frames and fork-validated annotation import"
```

---

### Task 5: `junto-live` — presence

**Files:**
- Create: `crates/junto-live/src/presence.rs`
- Modify: `crates/junto-live/src/lib.rs` (export)

**Interfaces:**
- Produces: `pub struct Presence` wrapping `loro::EphemeralStore` (timeout **30_000 ms** — the unit is milliseconds):
  - `Presence::new() -> Self`
  - `pub fn set_watching(&self, email: &str)` — key = email, value = `true`.
  - `pub fn watchers(&self) -> Vec<String>` — `remove_outdated()` first, then sorted non-expired keys.
  - `pub fn encode_all(&self) -> Vec<u8>`, `pub fn apply(&self, data: &[u8]) -> Result<(), String>`.

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn presence_merges_between_stores() {
    let a = Presence::new();
    let b = Presence::new();
    a.set_watching("dan@x.com");
    b.apply(&a.encode_all()).unwrap();
    assert_eq!(b.watchers(), vec!["dan@x.com".to_string()]);
}

#[test]
fn watchers_sorted_and_deduped() {
    let a = Presence::new();
    a.set_watching("z@x.com");
    a.set_watching("a@x.com");
    a.set_watching("z@x.com");
    assert_eq!(a.watchers(), vec!["a@x.com".to_string(), "z@x.com".to_string()]);
}
```

- [ ] **Step 2: Run → FAIL; implement; run `cargo test -p junto-live` → PASS.**
- [ ] **Step 3: Commit** — `git add crates/junto-live && git commit -m "feat(live): presence via loro ephemeral store"`

---

### Task 6: Host — LivePlane registry, conversation/worktree taps, archive-on-end

**Files:**
- Create: `crates/junto/src/live_plane.rs`
- Modify: `crates/junto/src/main.rs` (module decl beside the others)
- Modify: `crates/junto/src/host.rs` (field + accessor beside `live: LiveSessions`, lines 266–310)
- Modify: `crates/junto/src/launch.rs` (taps in `LiveSessions::publish`/`begin`/`finish`, lines 733–820; archive inside the finish path near `record_outcome`, lines 1627+)
- Modify: `crates/junto/Cargo.toml` (`junto-live = { path = "../junto-live" }`)

**Interfaces:**
- Consumes: `junto_live::{LiveDoc, Presence, Frame}`, `LiveSessions` publish/begin/finish, `store_artifact` + `ArtifactAttached` pattern from `record_outcome` (launch.rs 1627+), `EntryId`.
- Produces (Task 7/8 rely on):
  - `pub(crate) struct LivePlane { sessions: Mutex<HashMap<EntryId, Arc<SessionLive>>> }`
  - `pub(crate) struct SessionLive { pub doc: junto_live::LiveDoc, pub presence: junto_live::Presence, pub outbound: tokio::sync::broadcast::Sender<junto_live::Frame>, pub pending: Mutex<Vec<junto_kernel::Annotation>> }` (`outbound` capacity 256; `LiveDoc` is internally synchronized — loro handles are `Send + Sync`; wrap in `Arc<SessionLive>` only)
  - `LivePlane::begin(&self, session: EntryId) -> Arc<SessionLive>`; `get(&self, session) -> Option<Arc<SessionLive>>`; `finish(&self, session) -> Option<Vec<u8>>` (removes entry, returns `export_snapshot()` bytes for archiving, broadcasts `Frame::End`).
  - `SessionLive::publish_conversation(&self, event: &crate::launch::LiveEvent)` — `serde_json::to_value(event)` → `doc.push_conversation` → broadcast `Frame::update(...)` of the resulting local update (wire via `subscribe_local_update` forwarding into `outbound` at construction — one subscription per SessionLive, set up in `begin`).
- Taps (all fire-and-forget, errors logged with `tracing`/existing log macro, never propagated):
  - `LiveSessions::begin` → `host.live_plane().begin(session)`.
  - `LiveSessions::publish` → if a `SessionLive` exists, `publish_conversation(&event)`; if the event is a tool event whose label starts with `Edit`/`Write` (labels produced by `tool_label`, acp.rs), also `doc.push_worktree` with the same value.
  - `LiveSessions::finish` → `live_plane.finish(session)`; if `Some(snapshot)`, archive it as artifact `live.loro` using the exact `store_artifact` + `ArtifactAttached` sequence `record_outcome` uses for `diff.patch` (copy that call shape; same signing + append path).
  - Note: `LiveSessions` methods don't currently see `Host`; thread the plane through by storing `Arc<LivePlane>` inside `LiveSessions` (add a field, default-constructed) rather than reworking call sites — `begin/publish/finish` then tap it directly. Keep `Host::live_plane()` accessor delegating to `self.live.plane` for handlers.
- Worktree diff snapshots: in `spawn_turn` (launch.rs 1585–1620), after `run_turn` returns and before `finish`, push one `{"kind":"diff","text":<git diff output>}` value into `doc.push_worktree` (compute with the same workspace-diff helper `record_outcome` already uses for `diff.patch`). A 30s in-turn ticker is explicitly OUT of v1 scope (spec allows "periodic"; turn-end is the periodic floor).

- [ ] **Step 1: Write failing test** in `live_plane.rs`:

```rust
#[tokio::test]
async fn plane_lifecycle_publishes_and_archives() {
    let plane = LivePlane::default();
    let session = junto_kernel::EntryId::new();
    let live = plane.begin(session);
    let mut rx = live.outbound.subscribe();
    live.publish_conversation(&test_live_event("assistant", "hello")); // helper constructing crate::launch::LiveEvent
    let frame = rx.recv().await.unwrap();
    assert!(matches!(frame, junto_live::Frame::Update { .. }));
    assert_eq!(live.doc.conversation_len(), 1);
    let snapshot = plane.finish(session).expect("snapshot");
    let replay = junto_live::LiveDoc::new();
    replay.import_update(&snapshot).unwrap();
    assert_eq!(replay.conversation_len(), 1);
    assert!(plane.get(session).is_none());
}
```

- [ ] **Step 2: Run** `cargo test -p junto live_plane` → FAIL. **Implement `LivePlane` + taps + archive.** Run `cargo test -p junto` → PASS (existing launch/web tests must stay green — taps are additive).
- [ ] **Step 3: Commit** — `git add crates/junto Cargo.toml Cargo.lock && git commit -m "feat(host): live plane registry with session taps and archive-on-end"`

---

### Task 7: Host — WebSocket endpoint + ed25519 challenge-response auth

**Files:**
- Create: `crates/junto/src/live_ws.rs`
- Modify: `crates/junto/src/web.rs` (route registration in `router`, lines 40–89: `GET /channels/{channel}/sessions/{session}/live`)
- Modify: `crates/junto/Cargo.toml` (axum feature `ws`; dev-dep `tokio-tungstenite = "0.24"`)

**Interfaces:**
- Consumes: `LivePlane`/`SessionLive` (Task 6), `Frame`/`validate_annotation_update` (Task 4), `ChannelView.party` keyring (`member.public_key`, ledger.rs `project_unverified` shows the keyring build at lines 402–409), `PublicKey::verify_bytes` (Task 1), `steer bridge` entry point (Task 8 — this task only enqueues to `SessionLive.pending` and calls `crate::live_bridge::deliver` which Task 8 provides; define the fn signature here, Task 8 implements: `pub(crate) async fn deliver(host: Arc<Host>, channel: String, session: EntryId, annotations: Vec<Annotation>)`).
- Produces: `pub(crate) async fn live_session(State(host): State<Arc<Host>>, Path((channel, session)): Path<(String, String)>, ws: axum::extract::ws::WebSocketUpgrade) -> Response`.

**Protocol (server side of one connection):**
1. Project the channel (same lookup `stream_session` uses, web.rs 841–888); parse `session` to `EntryId`; `live_plane.get(session)` else close with `Frame::End`.
2. Build keyring: `view.party` → `HashMap<String, PublicKey>` from `member.public_key` (members without keys are unauthenticatable → excluded).
3. Send `Frame::Challenge { nonce }` — 32 random bytes hex (`uuid::Uuid::new_v4()` twice, or `getrandom`; use whatever randomness source the crate already links — uuid v4 twice concatenated is fine and adds no dep).
4. Expect `Frame::Auth { email, signature }`: `Signature::new(signature)` then `keyring[email].verify_bytes(&hex::decode(nonce), &sig)`. Fail → send `Frame::Rejected` + close. (Add `hex` via workspace deps if not present; ed25519-dalek already pulls it — check root Cargo.toml first.)
5. On success: send `Frame::AuthOk`, then `Frame::update(&live.doc.export_snapshot())`, then `Frame::Ephemeral(presence.encode_all())`. Subscribe to `live.outbound` broadcast → forward frames to the socket.
6. Inbound loop: `Update` frames → `validate_annotation_update(&live.doc, bytes, &email, &keyring)`; `Ok(anns)` → `live.doc.import_update(bytes)`, rebroadcast the same `Update` frame on `outbound` (other watchers), and hand `anns` to the bridge (urgent split is the bridge's job). `Err(reason)` → send `Frame::Rejected { reason }`, keep connection. `Ephemeral` frames → `presence.apply` + rebroadcast + set nothing else.
7. Socket close / lagged broadcast (`RecvError::Lagged`) → resend a fresh snapshot Update rather than dying.

- [ ] **Step 1: Write failing integration test** `crates/junto/tests/live_ws.rs` (dev-deps: tokio-tungstenite, futures-util). Model channel setup on the existing web.rs test pattern (web.rs tests at 2201+, 3158+: tempdir repo + `Host::fixed_with_member_home`):

```rust
#[tokio::test]
async fn watcher_authenticates_receives_snapshot_and_posts_annotation() {
    // 1. tempdir repo + git init; Host::fixed_with_member_home(vec![repo], Some(member_home)).
    // 2. Open a channel whose genesis member "Dan <dan@x.com>" carries key.public_key()
    //    (Member::human("Dan", "dan@x.com").with_key(...)) — reuse the channel-opening
    //    helper the web.rs tests use (grep `open_channel` in web.rs tests and copy it).
    // 3. session = EntryId::new(); host.live().begin(session) so the plane has a doc;
    //    host.live().publish(session, <one LiveEvent>) so the snapshot is non-empty.
    // 4. Serve: let app = web::router(host.clone());
    //    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    //    let addr = listener.local_addr().unwrap();
    //    tokio::spawn(axum::serve(listener, app).into_future());
    // 5. Connect ws://{addr}/channels/{channel}/sessions/{session}/live via tokio_tungstenite::connect_async.
    // 6. Read Challenge; sign hex-decoded nonce with the member SigningKey via sign_bytes;
    //    send Auth; assert AuthOk; assert first Update imports into a fresh LiveDoc with
    //    conversation_len() == 1.
    // 7. Build a signed Annotation (urgent: false), insert into the local LiveDoc,
    //    send Frame::update(local.export_snapshot()).
    // 8. Assert the server doc gained it: poll host live_plane get(session).doc.annotations()
    //    until len == 1 (with a 5s timeout loop).
    // 9. Negative: send an Update containing an unsigned annotation → expect Frame::Rejected.
}
```

Write this as REAL code (the comments above are the checklist for its body — every step is concrete against existing helpers; copy exact channel-opening lines from the web.rs test module).

- [ ] **Step 2: Run** `cargo test -p junto --test live_ws` → FAIL (route absent). **Implement `live_ws.rs` + route + a stub `live_bridge::deliver` that only extends `SessionLive.pending`** (Task 8 replaces the stub's body; keep the signature). Run → PASS.
- [ ] **Step 3: Run the full crate suite** `cargo test -p junto` → PASS.
- [ ] **Step 4: Commit** — `git add crates/junto && git commit -m "feat(host): authenticated live websocket with snapshot sync and validated annotations"`

---

### Task 8: Host — annotation → steer bridge

**Files:**
- Create: `crates/junto/src/live_bridge.rs` (replaces the Task 7 stub)
- Modify: `crates/junto/src/launch.rs` (flush pending on `LiveSessions::begin`)

**Interfaces:**
- Consumes: `steer_live` (launch.rs 1532–1548: `steer_live(host, channel: ChannelId, channel_ref: String, session: EntryId, steered_by: Member, message: String) -> Result<(), NotLive>`), `SessionLive.pending`, `reanchor` (Task 2) for rendering current position, session workspace path (the same `workspace` `run_turn` receives — store it on `SessionLive` at `begin`: add `pub workspace: Mutex<Option<PathBuf>>`, set from `spawn_turn`).
- Produces:
  - `pub(crate) fn format_steer(annotations: &[Annotation], reanchored: &[Option<junto_substrate_git::reanchor::Reanchor>]) -> String` — pure; one block per annotation:
    ```text
    [watcher comment — dan@x.com on src/foo.rs:12-14 (moved to 15-17)]
    > pinned excerpt lines
    the comment body
    ```
    Stream anchors render as `on conversation event <op_id>`; `Orphaned` renders `(code has changed since — excerpt shows the commented version)`; missing excerpt omits the `>` block.
  - `pub(crate) async fn deliver(host: Arc<Host>, channel: String, session: EntryId, annotations: Vec<Annotation>)` — split urgent/normal. Urgent: re-anchor each CodeAnchor (best-effort; `None` on error), `format_steer`, call `steer_live` with `steered_by` = first annotation's author; on `NotLive` push onto `pending`. Normal: push onto `pending`.
  - Flush: in `LiveSessions::begin` (after the plane tap from Task 6), drain `pending`; if non-empty spawn a task that waits 2 s (lets the turn's control channel come up — mirror how `steer_live` is called elsewhere) then delivers the batch via the same path.
- Resolve `channel_ref`/`ChannelId` the same way `steer_session`'s handler does (web.rs — grep `steer_session` and copy its channel resolution).

- [ ] **Step 1: Failing unit tests** for `format_steer` in `live_bridge.rs`:

```rust
#[test]
fn format_renders_anchor_excerpt_and_body() {
    let ann = code_annotation("dan@x.com", "src/foo.rs", 12, 14, Some("let x = 1;"), "off by one");
    let out = format_steer(&[ann], &[Some(Reanchor::Moved { span: Span::new(15, 17).unwrap() })]);
    assert!(out.contains("dan@x.com"));
    assert!(out.contains("src/foo.rs:12-14"));
    assert!(out.contains("moved to 15-17"));
    assert!(out.contains("> let x = 1;"));
    assert!(out.contains("off by one"));
}

#[test]
fn orphaned_anchor_is_stated_not_hidden() {
    let ann = code_annotation("dan@x.com", "src/foo.rs", 3, 3, Some("old line"), "why?");
    let out = format_steer(&[ann], &[Some(Reanchor::Orphaned)]);
    assert!(out.contains("code has changed since"));
}

#[test]
fn batch_concatenates_in_order() {
    let a = code_annotation("a@x.com", "a.rs", 1, 1, None, "first");
    let b = code_annotation("b@x.com", "b.rs", 2, 2, None, "second");
    let out = format_steer(&[a, b], &[None, None]);
    assert!(out.find("first").unwrap() < out.find("second").unwrap());
}
```

- [ ] **Step 2: Run → FAIL; implement `format_steer`; run → PASS.**
- [ ] **Step 3: Delivery test** — extend `crates/junto/tests/live_ws.rs`: after the accepted annotation (urgent: true this time), assert the running turn's control channel receives `TurnControl::Steer` containing the body. `LiveSessions::begin` returns the `mpsc::Receiver<TurnControl>` (launch.rs 733–820) — the test already holds it; `recv()` with timeout and assert the message contains `"off by one"`-style body text.
- [ ] **Step 4: Implement `deliver` + begin-flush; run `cargo test -p junto` → PASS.**
- [ ] **Step 5: Commit** — `git add crates/junto && git commit -m "feat(host): annotation-to-steer bridge with urgent interrupt and boundary flush"`

---

### Task 9: junto-iced — remote watch over WebSocket

**Files:**
- Modify: `crates/junto-iced/src/main.rs`
- Modify: `crates/junto-iced/Cargo.toml` (add `tokio-tungstenite = "0.24"`, `junto-kernel = { path = "../junto-kernel" }`, `junto-live = { path = "../junto-live" }`, `toml = "0.8"`, `base64`)

**Interfaces:**
- Consumes: `Frame` (Task 4), `LiveDoc` (Task 3), `SigningKey::from_secret_hex` + `sign_bytes` (Task 1), keys file format (`crates/junto/src/keys.rs`: `~/.junto/keys.toml`, records `{ email, secret }`).
- Produces (inside main.rs, following its existing single-file conventions):
  - `fn load_signing_key(email: &str) -> Option<junto_kernel::SigningKey>` — read `keys.toml` from the junto home (same dir resolution as the host: `dirs`-style home + `.junto`; copy the path logic from `crates/junto/src/keys.rs`), parse with `toml`, find record by email, `from_secret_hex`.
  - `fn live_ws_stream(base: String, channel: String, session: String, email: String) -> impl Stream<Item = Message>` — mirrors `session_stream` (main.rs 3581–3607) but over tokio-tungstenite: connect to `ws://{host}/channels/{c}/sessions/{s}/live` (derive from `base` by swapping the scheme), answer `Challenge` with `sign_bytes`, then maintain a local `LiveDoc`, import every `Update`, and emit `Message::Live(session, LiveEvent{...})` for each NEW conversation entry (track `conversation_len` high-water mark; parse the JSON value back into the existing `LiveEvent` struct — same fields, main.rs 293–312). `Ephemeral` frames → emit `Message::Watchers(session, Vec<String>)` (new message variant) from a local `Presence` after `apply`. `End`/close → `Message::LiveEnded(session)`.
  - Pane state: `remote: Option<String>` (base URL override) + a small "remote" text input beside the existing channel controls; when set, subscriptions and REST fetches for that pane use it instead of the `HOST` const (main.rs line 24) — thread through `fetch_channel`/`post_*` calls for that pane by replacing the constant with a `fn base(&pane) -> &str` helper.
  - View: a `watching: a@x, b@y` caption row above the feed when `Message::Watchers` is non-empty.
  - Heartbeat: every 10 s while connected, send `Frame::Ephemeral` with own presence (`set_watching(email)` + `encode_all`).

- [ ] **Step 1: Implement `load_signing_key` + a unit test** (tempdir keys.toml with a known secret; assert public key matches — same shape as keys.rs's own test at lines 105–108).
- [ ] **Step 2: Implement `live_ws_stream` + `Message::Watchers` + remote pane input + base-URL threading.** This is UI plumbing; unit-test the pure part only: extract `fn ws_url(base: &str, channel: &str, session: &str) -> String` and test `http://h:1727` → `ws://h:1727/channels/c/sessions/s/live` (and `https` → `wss`).
- [ ] **Step 3: Build**: `cargo build --manifest-path crates/junto-iced/Cargo.toml` → compiles clean.
- [ ] **Step 4: Commit** — `git add crates/junto-iced && git commit -m "feat(iced): remote live-session watching over authenticated websocket"`

---

### Task 10: junto-iced — annotation composer

**Files:**
- Modify: `crates/junto-iced/src/main.rs`

**Interfaces:**
- Consumes: `Annotation`/`Anchor`/`Span`/`AnnotationId` (Task 1), the pane's local `LiveDoc` + WS sink (Task 9 — keep the write half of the socket in the subscription and expose an `mpsc::Sender<Frame>` on the pane state, stored when the stream connects: `annotate_tx: Option<tokio::sync::mpsc::Sender<Frame>>`).
- Produces:
  - Composer UI under the steer box (visible only while remote-watching): text inputs for `path`, `lines` (`"12-14"` or `"12"`), comment body; an `urgent` checkbox (iced `checkbox`); a `comment` button. Empty path → StreamAnchor on the most recent conversation event (op_id = that event's index as string).
  - `fn parse_span(s: &str) -> Option<Span>` — `"12-14"`/`"12"`; unit-tested.
  - On submit: build `Annotation` (author = `Member::human(<git user name>, <email>)` — reuse however the iced app currently learns the local identity; if it has none, add an `email` text input beside `remote`), `excerpt: None` (v1 composer has no file view to quote; the host renders `pinned excerpt unavailable` gracefully — Task 8's `format_steer` already omits the `>` block when excerpt is None), sign with `load_signing_key`, insert into the local `LiveDoc`, send `Frame::update(local.export_snapshot())` through `annotate_tx`.
  - **Anchor sourcing rule (v1):** the composer emits `Anchor::Stream` UNLESS the pane's worktree feed has carried a `{"kind":"diff","commit":<oid>}` event; only then may it build a `CodeAnchor`, using that `commit` plus the typed path/span. Never fabricate a commit oid. `blob` is set to `ContentDigest::new("sha256:unpinned")` — blob pinning is drift *detection* only, and `reanchor` (Task 2) never reads the field; add a code comment marking this as the v1 cut. This requires Task 6's turn-end diff push to include `"commit"` (one extra `git rev-parse HEAD`), which Step 3 below adds.

- [ ] **Step 1: Unit tests** for `parse_span` ("12" → 12..12, "12-14" → 12..14, "0" / "9-3" / "x" → None).
- [ ] **Step 2: Implement composer + submit path; build junto-iced clean.**
- [ ] **Step 3: Extend Task 6's diff event** with `"commit"` (host side, `crates/junto/src/launch.rs`) + adjust its test to assert the field exists. Run `cargo test -p junto` → PASS.
- [ ] **Step 4: Commit** — `git add crates/junto-iced crates/junto && git commit -m "feat(iced): annotation composer with code and stream anchors"`

---

### Task 11: End-to-end smoke

**Files:**
- Modify: `crates/junto/tests/live_ws.rs` (extend into the full loop) — or verify the Task 7/8 test already covers it and only add the missing assertions.

The full loop that must be observably true in ONE test:
1. Host up (in-process axum on `127.0.0.1:0`), channel with keyed member, session begun, one conversation event published.
2. Watcher connects over WS, authenticates via challenge-response, imports snapshot (sees the event).
3. Watcher inserts a signed **urgent** annotation, sends the update.
4. The server doc converges (annotation present server-side) AND the turn's `TurnControl::Steer` arrives containing the annotation body (Task 8 Step 3 assertion).
5. A second watcher connects and receives the annotation in its snapshot (multi-watcher fan-out).
6. `host.live().finish(session)` → both sockets receive `Frame::End`; the archived `live.loro` artifact exists under the session's artifact dir and re-imports with `conversation_len() == 1` and one annotation.

- [ ] **Step 1: Extend the integration test to cover 5 and 6** (4 was Task 8; 1–3 were Task 7).
- [ ] **Step 2: Run** `cargo test -p junto --test live_ws` → PASS.
- [ ] **Step 3: Manual smoke (Windows, real binaries):** `cargo run -p junto -- serve` in a test repo; start a session from the web UI; run junto-iced with the pane's remote URL pointed at it (same machine, second junto home simulating a watcher is fine); watch the feed render, post a comment, see the agent receive it. This is the deliverable proof for the felt experience — record what was observed in the PR/commit message.
- [ ] **Step 4: Commit** — `git add crates/junto && git commit -m "test(host): end-to-end live plane smoke (auth, fan-out, steer, archive)"`

---

### Task 12: ADR, unpark record, docs

**Files:**
- Create: `docs/adr/00NN-crdt-confined-to-live-plane.md` — NN = one past the highest existing number in `docs/adr/` (references exist up to 0033; check with a directory listing and take the next free).
- Modify: `docs/domain-model.md` (add LiveDoc/Annotation/Anchor vocabulary), `README.md` (one feature bullet), `CLAUDE.md` **only** the crate list line if it enumerates workspace members.

**ADR content (write it fully, not a stub):** Title "CRDT confined to the ephemeral live plane". Context: hard constraint "zero CRDT / presence / shared-buffer" (junto.md) vs the Delta reopening (competitive-landscape.md §"The reopening"). Decision: the constraint is scoped to the **durable record** (ADR 0011's actual argument); an ephemeral, per-session loro document (conversation/worktree driver-only, annotations multi-actor, presence) is permitted; durable outcomes still fold into append-only entries; the LiveDoc snapshot archives as a session artifact. Consequences: rungs 3–4 remain policy changes; any future CRDT use outside a live plane or versioned artifact requires a new ADR. Reference spec + this plan.

**Unpark record (process, not code):** draft the two ledger assertions for Dan to record in `junto-dev` (or record via the junto MCP tools if this session has them):
1. Citing `1d9cf9b1`: "Unparked: worktree isolation landed (`185fd301`); Delta (Zed, 2026-08) is the real-use-case evidence; live plane ships as single-writer sessions + multi-actor annotations per the 2026-08-20 spec."
2. Citing `b405a1cb`: "The live session plane realizes the collaborative space's rung 1: span-anchored, append-only annotations on a versioned artifact; ratified outcomes still fold into entries."

- [ ] **Step 1: Write the ADR; link it from the spec's Process obligations section.**
- [ ] **Step 2: Update domain-model.md + README.**
- [ ] **Step 3: Draft the two assertions into the ADR's appendix (so the wording is preserved even if recording happens later).**
- [ ] **Step 4: Run the FULL workspace suite once:** `cargo test --workspace` and `cargo build --manifest-path crates/junto-iced/Cargo.toml` → all green.
- [ ] **Step 5: Commit** — `git add docs README.md CLAUDE.md && git commit -m "docs: ADR scoping the no-CRDT constraint to the durable record; live-plane vocabulary"`

---

## Self-Review (performed at write time)

- **Spec coverage:** LiveDoc containers/policies → T3/T4/T6; transport+auth → T7; anchors+re-anchoring → T1/T2; lavish loop urgent/boundary → T8; presence → T5/T9; archive-on-end → T6/T11; watcher surface → T9/T10; failure invariant → Global Constraints + T6 (fire-and-forget taps); process obligations → T12; non-goals respected (no FS watcher — turn-end diff only; no relay/p2p — but frames/sync are topology-free; no record changes).
- **Known v1 cuts, stated in-plan:** blob pinning is `sha256:unpinned` from the remote composer (T10 — reanchor never reads it); no in-turn diff ticker (T6); composer has no file view so `excerpt` is None from iced (host formats gracefully, T8).
- **Type consistency:** `Frame` defined once (T4) and consumed by T6/T7/T9; `deliver` signature fixed in T7, implemented in T8; `SessionLive.pending`/`workspace` declared where first needed (T6/T8); `conversation_len` added in T3, used in T6/T7/T9/T11.
