# Device-Key Enrollment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A member may hold one signing key per machine, enrolled through a founder-issued invite, so a person can write and watch live sessions from every device they use — with secrets never leaving the machine that minted them.

**Architecture:** The keyring becomes its own projection (email → many key grants) beside the unchanged party projection. Enrollment is a three-step invite/enroll/add-member exchange carrying only public keys in versioned, size-bounded, short-lived `junto://` codes. Revocation is a founder-authored Park on the granting entry, which retires a key as of its timestamp and — for a member revocation — also stops that member's later entries counting.

**Tech Stack:** Rust; `junto-kernel` (projection, no new entry kinds), `junto` (CLI, invite store, live-plane consumer), `sha2` + `base64` (both already workspace deps).

**Spec:** `docs/superpowers/specs/2026-08-21-device-key-enrollment-design.md`

## Global Constraints

- **No new entry kinds. `Member` is unchanged.** Multi-device is expressed by multiple `MemberAdded` entries. Every existing entry must deserialize byte-identically — verify with the existing `serial.rs` round-trip tests.
- **No change to ADR 0011 sync or `refs/junto/*`.**
- **Secrets never move.** No code may write, print, transmit, or accept a private key or seed outside `<junto-home>/keys.toml` on the machine that minted it. `keys::signing_key` MUST NOT be called for an identity this host has no authority over — that is the bug this plan fixes (`Host::keyed`, host.rs:365-376, currently mints the *new member's* key on the *founder's* machine).
- **Envelope constants, taken from Orca's shipped pairing offer, not invented:** TTL `600_000` ms, clock skew allowance `30_000` ms, invite token 256 bits rendered as 43 base64url chars, per-field length bounds, and a total cap enforced *before* parsing.
- **ADR 0017 is amended, not silently changed** (Task 11). `ledger.rs:290-292` currently asserts recognition is "set-based, not temporal"; that comment must be corrected in the same change that makes it temporal for revoked members.
- CI gate (binding): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. 342 tests pass at plan time; they must still pass.
- Windows is the primary dev platform.

---

### Task 1: Kernel — `KeyGrant`, `Keyring`, and the keyring projection

**Files:**
- Modify: `crates/junto-kernel/src/ledger.rs` (add types + `project_keyring`; wire into `project`)
- Modify: `crates/junto-kernel/src/lib.rs` (re-export `KeyGrant`, `Keyring`)

**Interfaces:**
- Consumes: `Member`, `PublicKey`, `EntryId`, `Timestamp`, `EntryPayload::{ChannelOpened, MemberAdded}`.
- Produces:
  - `pub struct KeyGrant { pub key: PublicKey, pub granted_by: EntryId, pub retired_at: Option<Timestamp> }` — derive `Debug, Clone, PartialEq, Eq`.
  - `pub type Keyring = std::collections::HashMap<String, Vec<KeyGrant>>;`
  - `fn project_keyring(entries: &[LedgerEntry], founder_email: &str) -> Keyring` (private).
  - `ChannelView.keyring: Keyring` — new public field, documented like its neighbours.
  - `impl KeyGrant { pub fn active_at(&self, ts: Timestamp) -> bool }` — `retired_at.is_none_or(|r| ts <= r)`.

Rules: the genesis author contributes a grant (its `granted_by` is the `ChannelOpened` entry id); a `MemberAdded` contributes one iff `entry.author.email == founder_email` AND `member.public_key.is_some()`. `retired_at` stays `None` in this task — Task 2 fills it. Grants accumulate in canonical order, so a member's grants are deterministically ordered on every replica.

- [ ] **Step 1: Write failing tests** in `ledger.rs`'s test module:

```rust
#[tokio::test]
async fn keyring_unions_multiple_grants_for_one_email() {
    let k1 = crate::SigningKey::from_secret_bytes([1; 32]);
    let k2 = crate::SigningKey::from_secret_bytes([2; 32]);
    let dan = Member::human("Dan", "dan@x.com").with_key(k1.public_key());
    // founder opens, then grants a SECOND key to their own email
    // (the founder's own second device — see spec "Enrollment flow")
    let mut ledger = Ledger::new(InMemorySubstrate::new());
    let channel = ChannelId::new();
    // genesis authored by dan (carrying k1), then MemberAdded{dan with k2}
    // … build and append …
    let view = ledger.project(&channel).await.unwrap();
    let grants = view.keyring.get("dan@x.com").expect("dan has grants");
    assert_eq!(grants.len(), 2, "genesis key plus the enrolled device key");
    assert!(grants.iter().any(|g| g.key == k1.public_key()));
    assert!(grants.iter().any(|g| g.key == k2.public_key()));
}

#[tokio::test]
async fn non_founder_member_added_contributes_no_key() {
    // A MemberAdded authored by a non-founder member carrying a key must not
    // appear in the keyring — grant authority is the founder's alone.
}

#[tokio::test]
async fn keyless_member_added_contributes_no_grant() {
    // member.public_key == None => no entry in the keyring for that email.
}

#[tokio::test]
async fn party_projection_is_unchanged_by_the_keyring() {
    // Two MemberAdded entries for the SAME email: the party still holds ONE
    // row (first-write-wins, ledger.rs:378-387) while the keyring holds two
    // grants. This is the decision that keeps devices out of the roster.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto-kernel keyring` → FAIL (no `keyring` field).
- [ ] **Step 3: Implement** `KeyGrant`, `Keyring`, `active_at`, `project_keyring`, the `ChannelView` field, and its wiring in `project` (after `project_party`, since it needs the founder's email). Run → PASS.
- [ ] **Step 4: Confirm nothing moved** — `cargo test -p junto-kernel` (all 71+), especially `serial.rs` round-trips: no entry shape changed.
- [ ] **Step 5: Commit** — `git commit -m "feat(kernel): project a per-email keyring of key grants"`

---

### Task 2: Kernel — retirement via a founder-authored Park

**Files:**
- Modify: `crates/junto-kernel/src/ledger.rs` (`project_keyring` fills `retired_at`; `project_unverified` uses grants)

**Interfaces:**
- Consumes: Task 1's `KeyGrant`/`Keyring`, `EntryPayload::Park { target, .. }`.
- Produces: `project_unverified` now marks an entry verified iff **some** grant for its author's email satisfies `grant.active_at(entry.timestamp)` AND `entry.verifies_with(&grant.key)`.

Rules: a `Park` counts iff authored by the founder AND its `target` is an entry that granted a key. `retired_at` = the Park entry's `timestamp`. Multiple parks on one grant: earliest wins (retirement is not un-doable here). A park targeting something that granted no key is ignored, exactly as dangling verification targets already are.

- [ ] **Step 1: Write failing tests:**

```rust
#[tokio::test]
async fn a_retired_grant_verifies_before_its_park_and_not_after() {
    // Entry A stamped BEFORE the park, entry B stamped AFTER, both signed by
    // the retired key. A must be verified; B must be unverified.
    // This is decision 6: retiring a device does not rewrite history.
}

#[tokio::test]
async fn a_non_founder_park_does_not_retire_a_grant() {
    // Only the founder may revoke, matching the grant rule.
}

#[tokio::test]
async fn an_entry_verifies_against_any_active_grant() {
    // Two active grants for one email; an entry signed by EITHER verifies.
}
```

- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS.**
- [ ] **Step 4: Full `cargo test -p junto-kernel`** — the pre-existing `keyring_is_per_member_from_member_added` test must still pass unchanged; if it needs editing, say why in the commit body.
- [ ] **Step 5: Commit** — `git commit -m "feat(kernel): retire a key grant at its park's timestamp"`

---

### Task 3: Kernel — revocation stops post-cutoff entries counting

**Files:**
- Modify: `crates/junto-kernel/src/ledger.rs` (`project`'s `unrecognized` computation, and the comment at lines 290-292)

**Interfaces:**
- Produces: a member revocation cutoff, derived in `project`: for each email, if **every** grant for it is retired, the cutoff is the latest `retired_at` among them — the moment it held no valid key at all (corrected by Task 9c from an initial earliest-`retired_at` draft, which let a member's first device retirement retroactively unrecognize legitimate work written from a still-active later grant). An entry is `unrecognized` iff its author is absent from the party (existing rule) **OR** its author has a cutoff and `entry.timestamp > cutoff`.

This is decision 7 and it is the one change to a documented invariant. Rationale, to carry in the code comment: unverified entries are still *recognized*, so they still carry standings, close gates, and appear in sessions and lineage — retiring keys alone would let a revoked member keep contributing entries that count.

**Deliberately NOT done:** the member is never removed from `party`. Recognition is party-set membership, so removal would mark every entry that author ever wrote unrecognized and erase their history from every projection.

- [ ] **Step 1: Write failing tests:**

```rust
#[tokio::test]
async fn revoked_members_post_cutoff_entries_are_unrecognized() {
    // Member with one grant; founder parks it at T. Entry before T is
    // recognized; entry after T is unrecognized.
}

#[tokio::test]
async fn a_revoked_members_pre_cutoff_contributions_still_count() {
    // The strongest guard: an assertion made before the cutoff KEEPS its
    // standing, and a ratification given before the cutoff still resolves its
    // target. Proves we retired the member without rewriting history.
}

#[tokio::test]
async fn a_partially_retired_member_is_not_revoked() {
    // Two grants, one retired: no cutoff, recognition unchanged. Retiring one
    // device must not offboard the person.
}

#[tokio::test]
async fn an_unrevoked_members_recognition_is_still_set_based() {
    // Regression guard on ADR 0017's rule for everyone else.
}

#[tokio::test]
async fn a_revoked_member_remains_in_the_party() {
    // The rejected alternative, pinned as a test so nobody "fixes" it later.
}
```

- [ ] **Step 2: Run → FAIL. Step 3: Implement, and correct the `ledger.rs:290-292` comment to state the amended rule and cite the new ADR from Task 11.** Run → PASS.
- [ ] **Step 4: `cargo test --workspace`** — this changes a projection every crate reads; the whole suite is the gate.
- [ ] **Step 5: Commit** — `git commit -m "feat(kernel): a revoked member's post-cutoff entries stop counting"`

---

### Task 4: `junto` — the enrollment envelope

**Files:**
- Create: `crates/junto/src/enroll.rs`
- Modify: `crates/junto/src/main.rs` (module decl)
- Modify: `crates/junto/Cargo.toml` (add `base64.workspace = true`; `sha2` is already present)

**Interfaces:**
- Produces:
  - `pub struct InvitePayload { pub v: u8, pub invite_token: String, pub member_email: String, pub channel: String, pub expires_at: i64 }`
  - `pub struct EnrollPayload { pub v: u8, pub invite_token: String, pub email: String, pub display_name: String, pub public_key: PublicKey, pub expires_at: i64 }`
  - `pub fn encode_invite(&InvitePayload) -> Result<String>` → `junto://invite?code=<base64url>`; `encode_enroll` likewise → `junto://enroll?code=…`
  - `pub fn decode_invite(url: &str) -> Result<InvitePayload>`; `decode_enroll` likewise.
  - `pub fn mint_invite_token() -> String` — 32 random bytes as 43 base64url chars (use `uuid::Uuid::new_v4()` twice for entropy, already a dep; no new RNG dependency).
  - Constants, each with its rationale in a doc comment: `MAX_INVITE_TTL_MS: i64 = 600_000`, `EXPIRY_CLOCK_SKEW_MS: i64 = 30_000`, `MAX_CODE_CHARS: usize = 132_096`, `MAX_FIELD_CHARS: usize = 4_096`.

Rules, in this order: reject if the URL exceeds `MAX_CODE_CHARS` **before parsing anything**; require scheme `junto:` and host `invite`/`enroll`; base64url-decode; parse JSON; reject `v != 1`; reject any field longer than `MAX_FIELD_CHARS`; reject `expires_at` that is in the past beyond the skew allowance, or further ahead than `MAX_INVITE_TTL_MS` plus skew.

- [ ] **Step 1: Write failing tests** in `enroll.rs`:

```rust
#[test]
fn invite_round_trips_through_its_uri() {
    let p = sample_invite();
    let url = encode_invite(&p).unwrap();
    assert!(url.starts_with("junto://invite?code="));
    assert_eq!(decode_invite(&url).unwrap(), p);
}

#[test]
fn enroll_round_trips_and_carries_no_secret() {
    let url = encode_enroll(&sample_enroll()).unwrap();
    // The enroll code is safe to paste or read aloud: assert the encoded form
    // contains no 64-hex secret-shaped run.
    let decoded = decode_enroll(&url).unwrap();
    assert!(!format!("{decoded:?}").contains("secret"));
}

#[test]
fn an_oversized_code_is_refused_before_parsing() {
    let url = format!("junto://invite?code={}", "A".repeat(MAX_CODE_CHARS + 1));
    assert!(decode_invite(&url).is_err());
}

#[test]
fn an_expired_invite_is_refused() { /* expires_at well in the past → Err */ }

#[test]
fn an_invite_inside_the_skew_allowance_is_accepted() {
    // expires_at a few seconds in the past, within EXPIRY_CLOCK_SKEW_MS → Ok.
    // Two machines' clocks differ; this is why the allowance exists.
}

#[test]
fn an_invite_expiring_beyond_the_ttl_is_refused() { /* now + 2*TTL → Err */ }

#[test]
fn a_wrong_version_is_refused() { /* v: 2 → Err */ }

#[test]
fn a_minted_token_is_43_base64url_chars() {
    let t = mint_invite_token();
    assert_eq!(t.len(), 43);
    assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
}
```

- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS.**
- [ ] **Step 4: Commit** — `git commit -m "feat(junto): versioned, bounded, short-lived enrollment codes"`

---

### Task 5: `junto` — the invite store

**Files:**
- Create: `crates/junto/src/invites.rs`
- Modify: `crates/junto/src/main.rs` (module decl)

**Interfaces:**
- Produces (modelled on `members.rs`, which this mirrors):
  - `pub fn issue(junto_home: &Path, token: &str, member_email: &str, channel: &str, expires_at: i64) -> Result<()>` — stores `sha256(token)`, never the token.
  - `pub fn consume(junto_home: &Path, token: &str, member_email: &str, channel: &str) -> Result<Consumed>` where `pub enum Consumed { Ok, Unknown, AlreadyUsed, Expired, WrongMember }`.
  - `pub fn prune(junto_home: &Path) -> Result<usize>` — drops records expired more than 24h ago, so the file cannot grow without bound.
  - File: `<junto-home>/invites.toml`, records `{ token_sha256, member_email, channel, expires_at, consumed_at }`.

Rules: the token's preimage is never written. `consume` is single-use — a second call for the same token returns `AlreadyUsed`. `WrongMember` fires when the enroll payload's email does not match the invite's, so an invite for one person cannot enroll another.

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn an_issued_invite_consumes_once_then_refuses() {
    let home = tempfile::tempdir().unwrap();
    let token = "t".repeat(43);
    issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
    assert!(matches!(consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(), Consumed::Ok));
    assert!(matches!(consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(), Consumed::AlreadyUsed));
}

#[test]
fn the_token_is_never_written_to_disk() {
    // The security property of hashing: assert the file does NOT contain the
    // token, and DOES contain something (the hash).
    let stored = std::fs::read_to_string(home.path().join("invites.toml")).unwrap();
    assert!(!stored.contains(&token));
}

#[test]
fn an_invite_cannot_enroll_a_different_member() { /* → WrongMember */ }

#[test]
fn an_unknown_token_is_refused() { /* → Unknown */ }

#[test]
fn an_expired_invite_is_refused_even_if_unconsumed() { /* → Expired */ }

#[test]
fn prune_drops_only_long_expired_records() { /* keeps a fresh one */ }
```

- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS. Step 4: Commit** — `git commit -m "feat(junto): machine-local single-use invite store (hashed tokens)"`

---

### Task 6: CLI — `junto invite`

**Files:**
- Modify: `crates/junto/src/main.rs` (new `Command::Invite` variant + handler)

**Interfaces:**
- Consumes: Task 4's `mint_invite_token`/`encode_invite`, Task 5's `issue`, `host::Host::from_registry`, `host::junto_home`, the channel resolution pattern from `add_member` (main.rs:258-268).
- Produces: `junto invite --member <email> --channel <name>` → prints the `junto://invite?code=…` URI and the human-readable expiry.

Rules: resolve the channel and refuse early if it does not exist. Verify the caller is the channel's founder (`view.party.first()`) before issuing, so an invite is never minted by someone who cannot complete it. Mint, `issue`, print.

- [ ] **Step 1: Write the failing test** — a unit test over a helper `fn invite_line(url: &str, expires_at: i64) -> String` so the output shape is pinned without a full CLI harness; assert it contains the URI and an expiry.
- [ ] **Step 2: Run → FAIL. Step 3: Implement the command and helper. Run → PASS.**
- [ ] **Step 4: Manual check** — run it against a scratch channel and paste the output into the next task's test fixture.
- [ ] **Step 5: Commit** — `git commit -m "feat(junto): junto invite mints a founder-issued enrollment invite"`

---

### Task 7: CLI — `junto enroll`

**Files:**
- Modify: `crates/junto/src/main.rs` (new `Command::Enroll` + handler)

**Interfaces:**
- Consumes: `decode_invite`, `encode_enroll`, `keys::signing_key`, `host::junto_home`.
- Produces: `junto enroll --invite <url>` → mints this machine's keypair for the invite's `member_email` (or reuses the existing one) and prints a `junto://enroll?code=…` carrying only the **public** key.

Rules: decode and validate the invite first, so an expired or malformed invite fails before any key is minted. `--name` supplies the display name; default it to the local git user's name. Print a one-line reminder that the secret stays on this machine — this is the moment a user might expect to copy something.

- [ ] **Step 1: Failing test** — `fn enroll_payload_from_invite(invite: &InvitePayload, key: &PublicKey, name: &str) -> EnrollPayload`, pure: asserts the invite token is echoed verbatim, the email is carried from the invite (NOT re-typed by the user), and the public key is the one passed.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS.**
- [ ] **Step 4: Commit** — `git commit -m "feat(junto): junto enroll mints a device key and emits its public half"`

---

### Task 8: CLI — `add-member --enroll`, and stop minting keys for remote identities

**Files:**
- Modify: `crates/junto/src/main.rs` (`Command::AddMember` gains `--enroll`)
- Modify: `crates/junto/src/host.rs` (`Host::keyed`, lines 365-376)

**Interfaces:**
- Consumes: `decode_enroll`, `invites::consume`, the existing `host.add_member`.
- Produces: `junto add-member --enroll <url> --channel <name>` — validates the echoed token via `consume`, builds the `Member` with the public key **from the payload**, and appends the `MemberAdded`.
- Changes: `Host::keyed` MUST stop minting for an identity it was handed. Give it an explicit key when one is supplied, and use `keys::has_signing_key` (added by the live-plane work) rather than `signing_key` when deciding whether a local key exists.

This is the plan's core correctness fix. Today `Host::keyed` calls `keys::signing_key(&home, &member.email)` for the member *being added*, so the founder's machine mints a keypair for someone else — a key that person never receives, while their own machine mints a different one. That is precisely why a remote member cannot authenticate to a live session.

Keep the existing keyless/interactive `add-member` path working for the local-agent case, where the host legitimately does have authority to mint.

- [ ] **Step 1: Write the failing test** in `host.rs`'s tests:

```rust
#[tokio::test]
async fn add_member_with_a_supplied_key_does_not_mint_locally() {
    // Add a member whose key comes from an enroll payload; then assert
    // <member_home>/keys.toml contains NO record for that email. The founder's
    // machine must not hold a key for someone else.
}

#[tokio::test]
async fn the_recorded_member_carries_the_supplied_public_key() {
    // Project the channel and assert the keyring grant equals the key passed
    // in, not a locally minted one.
}
```

- [ ] **Step 2: Run → FAIL** (today the key is minted locally and the assertion trips).
- [ ] **Step 3: Implement** the `--enroll` path and the `Host::keyed` change. Run → PASS.
- [ ] **Step 4: `cargo test --workspace`** — `keyed` is on the shared append path; the whole suite is the gate.
- [ ] **Step 5: Commit** — `git commit -m "fix(junto): take an enrolled member's key from their device, never mint it here"`

---

### Task 9: CLI — `keys list`, `revoke-member`, `retire-device`

**Files:**
- Modify: `crates/junto/src/main.rs` (three new commands)

**Interfaces:**
- Consumes: `ChannelView.keyring` (Task 1), the `EntryPayload::Park` append path used by the existing `verify` surface.
- Produces:
  - `junto keys list --channel <name> [--member <email>]` — prints each grant: member, key fingerprint (first 16 hex of the key, never the whole key on a shared terminal), `granted_by` entry id, and `retired_at` if set. The `granted_by` id is the handle `retire-device` needs.
  - `junto revoke-member --member <email> --channel <name> --rationale <text>` — appends a founder-authored `Park` for **every** active grant for that email, in one act.
  - `junto retire-device --grant <entry-id> --channel <name> --rationale <text>` — parks exactly one grant.

Rules: both revocation commands refuse unless the caller is the founder. `revoke-member` refuses if the email has no active grants (nothing to do) and reports how many it parked. Print a warning on `revoke-member` naming the consequence: the member stays in the party, and their entries after now stop counting.

- [ ] **Step 1: Failing tests** for the pure formatting/selection helpers: `fn grants_to_park(view: &ChannelView, email: &str) -> Vec<EntryId>` returns every active grant and skips already-retired ones; `fn fingerprint(key: &PublicKey) -> String` is stable and 16 chars.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS.**
- [ ] **Step 4: Commit** — `git commit -m "feat(junto): list key grants, revoke a member, retire one device"`

---

### Task 10: Live plane — use the projected keyring, and say why a handshake failed

**Files:**
- Modify: `crates/junto/src/live_ws.rs` (keyring construction ~lines 83-92, and the auth failure path)

**Interfaces:**
- Consumes: `ChannelView.keyring`.
- Changes: the handshake builds its keyring from `view.keyring` (all active grants) instead of `view.party` public keys, so any enrolled device authenticates. The failure message distinguishes three cases:
  1. email absent from the party → "not a member of this channel";
  2. email in the party but no active grant matches the presented signature → "this device's key is not enrolled for `<email>` — run `junto enroll --invite …`, then have the founder run `junto add-member --enroll …`";
  3. email in the party, grants exist, all retired → "signing access for `<email>` has been revoked".

The gate stays hard — an unenrolled device still cannot connect. Case 2's message is the point: the old text ("signature does not verify against the key on file") is what made this whole gap hard to diagnose.

- [ ] **Step 1: Write failing tests** in `live_ws.rs`'s existing test module, extending its websocket fixture:

```rust
// A second enrolled key authenticates (the feature).
#[tokio::test] async fn a_second_enrolled_device_authenticates() { … }

// An unenrolled key is refused WITH the enrollment guidance (the diagnosis).
#[tokio::test] async fn an_unenrolled_device_is_told_how_to_enroll() {
    // assert the Rejected reason names enrollment, not just "does not verify"
}

// A revoked member is refused, and told so.
#[tokio::test] async fn a_revoked_member_is_refused() { … }
```

- [ ] **Step 2: Run → FAIL. Step 3: Implement. Run → PASS.**
- [ ] **Step 4: `cargo test -p junto`** — the existing handshake tests (wrong signature, unknown email, fan-out) must all still pass.
- [ ] **Step 5: Commit** — `git commit -m "feat(host): authenticate any enrolled device, and diagnose the ones that are not"`

---

### Task 11: ADR 0035 and docs

**Files:**
- Create: `docs/adr/0035-membership-is-set-based-except-after-revocation.md` — confirm 0035 is free by listing `docs/adr/` first; take the next number if not.
- Modify: `docs/adr/README.md` (index row)
- Modify: `docs/adr/0034-crdt-confined-to-the-live-plane.md` (limitation 1 — key transport — is now addressed; point at ADR 0035 and the spec rather than deleting the entry, since the limitation was real when recorded)
- Modify: `docs/domain-model.md` (key grant, keyring, device enrollment, revocation cutoff)

**ADR 0035 content** — write it fully, no placeholders:

- **Context.** ADR 0017 made Party membership set-based and non-temporal on purpose: an entry counts iff its author is in the Party, wherever the grant falls in canonical order. ADR 0033 then made authorship verification a projection *fact* rather than a gate. Together these mean a member whose keys are all retired still has their later entries *recognized* — carrying standings, closing gates, appearing in sessions — merely flagged `unverified`. Offboarding therefore had no mechanism.
- **Decision.** Membership stays set-based for every member who has not been revoked. When every key grant for an email is retired by founder-authored Parks, that email acquires a **revocation cutoff** (the latest such retirement — the moment it held no valid key at all; corrected by Task 9c from an initial earliest-retirement draft, which let a member's first device retirement retroactively unrecognize legitimate work written from a still-active later grant), and entries from it stamped after the cutoff are `unrecognized`. Removing a member from the Party projection was considered and **rejected**: recognition is Party-set membership, so removal marks every entry that author ever wrote unrecognized and erases their history from every downstream projection.
- **Consequences.** Recognition is no longer purely set-based, and `ledger.rs`'s comment saying so is corrected. Party membership now means "was admitted", not "may currently write" — every surface reading one should be checked against the other. A revoked member remains visible in the record with their history intact, which is the honest account of what happened. Retroactive distrust of a compromised key remains out of scope and needs its own decision.

Reference the spec and this plan by path.

- [ ] **Step 1: List `docs/adr/` and confirm the number. Write the ADR.**
- [ ] **Step 2: Update the ADR index, ADR 0034's limitation 1, and the domain model.**
- [ ] **Step 3: Verify `cargo fmt --all --check` still clean** (markdown only, so it should be).
- [ ] **Step 4: Commit** — `git commit -m "docs: ADR 0035 — membership is set-based except after revocation"`

---

### Task 12: End-to-end verification

**Files:**
- Modify: `crates/junto/src/live_ws.rs` tests or `crates/junto/src/enroll.rs` tests — wherever the fixture fits best; do not create an external `tests/` binary (this crate has no `[lib]` target and all 16 of its test modules are in-crate).

The one test that proves the feature, in a single run: a scratch channel with a founder; `invite` → `enroll` → `add-member --enroll` executed through their real code paths (not re-implemented inline); then assert (a) the new grant appears in the projected keyring, (b) an entry signed by the new device's key projects as **verified**, (c) the founder's `keys.toml` holds **no** key for the enrolled member, and (d) a live-plane handshake with that device's key succeeds.

Then `revoke-member` and assert (e) a later entry from that device is `unrecognized` while (f) the earlier one keeps its standing.

- [ ] **Step 1: Write it. Run 3× back-to-back to prove it is not flaky.**
- [ ] **Step 2: Manual smoke** — two junto homes on one machine (`--member-home` style overrides as the existing tests use) standing in for two machines; run the three commands for real and paste the transcript into the report.
- [ ] **Step 3: Full gate** — `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- [ ] **Step 4: Commit** — `git commit -m "test: end-to-end device enrollment, verification, and revocation"`

---

## Self-Review (performed at write time)

- **Spec coverage:** keyring projection → T1; retirement semantics → T2; decision 7 temporal recognition → T3; envelope + constants → T4; invite store + hashing → T5; three-step flow → T6/T7/T8; `keys list`/revoke/retire → T9; live-plane consumer + diagnostics → T10; ADR 0017 amendment → T11; end-to-end proof → T12. Non-goals respected: no party removal (T3 pins it as a test), no retroactive distrust, no private-key transport, no device-management UI beyond `keys list`.
- **Type consistency:** `KeyGrant`/`Keyring`/`active_at` defined in T1 and consumed by T2/T3/T9/T10; `InvitePayload`/`EnrollPayload` defined in T4 and consumed by T6/T7/T8; `Consumed` defined in T5 and consumed by T8; `ChannelView.keyring` added in T1 and read in T9/T10/T12.
- **Sequencing:** T1→T2→T3 are strictly ordered (each builds on the previous projection). T4/T5 are independent of the kernel work and could run in parallel with T1-T3. T6-T8 need T4+T5. T10 needs T1. T12 needs everything.
- **Known risk carried:** T3 changes a projection every crate reads, and T8 changes `Host::keyed` on the shared append path. Both name `cargo test --workspace` as their gate rather than a crate-scoped run.
