# Device Pairing in the Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pair a second machine from the native surface instead of three terminal invocations across two machines, and let one pass grant every channel the founder ticks.

**Architecture:** The invite envelope goes to v2 with `channels: Vec<String>`; the channel set never travels in a code, because the founder's own `invites.toml` already holds one record per `(token_sha256, channel)` and a new `invites::channels_for` recovers it by token hash. Six host endpoints expose invite/enroll/redeem/roster/retire/revoke, authorizing as `WriteAuth::Human` exactly like `diverge_channel` and `verify` already do. The native Iced surface gains a Settings "this device" section (join) and a channel-pane members/devices disclosure (invite, redeem, retire, revoke). The CLI cuts over to v2 with no shim.

**Tech Stack:** Rust; `crates/junto` (host/CLI, axum), `crates/junto-iced` (Iced 0.13 GUI, **its own workspace**), `crates/junto-kernel` (unchanged by this plan). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-21-device-pairing-surface-design.md` — read it before Task 1; the plan argues from it.

## Global Constraints

- **CI gate (binding):** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **`crates/junto-iced` is NOT in the root workspace.** `--workspace` never builds or tests it. Its gate is `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings` and `cargo test --manifest-path crates/junto-iced/Cargo.toml`. Same for `cargo fmt` (`--manifest-path`).
- **No kernel change.** `crates/junto-kernel` is not modified by any task in this plan. No new entry kinds; `KeyGrant`/`Keyring`/`Member` are untouched.
- **No new dependencies** in any crate.
- **Secrets never leave the machine.** No code may write, print, transmit, or accept a private key or seed outside `<junto-home>/keys.toml` on the machine that minted it. Only `POST /devices/enroll` may cause a mint, and only for the email its decoded invite carries.
- **`--kind` never defaults** (ADR 0035). Neither does `rationale` on any revoke/retire path.
- **Fingerprints only on any shared surface:** the 16 hex chars after the `ed25519:` prefix, via the existing `fingerprint` helper. A full `PublicKey` never enters an HTTP response or a rendered page.
- **Identity endpoints return JSON, never redirects.** The web *pages* do not use them; the native surface does. (The pre-existing page handlers keep their redirects.)
- **Every identity read re-folds: use `project_fresh` (web.rs:127), never the cached `project`.** Its doc comment names this exact hazard — "a `revoke-member`/`retire-device` run in a separate process never invalidates this process's cache" — and identity state is precisely what the CLI mutates behind a long-running `junto serve`. A stale fold would show a retired device as active, or hide a member the CLI just added. Inside a write handler's guard, call `guard.project_fresh(&id)` rather than `guard.project(&id)`. These acts are rare; a full re-fold per act costs nothing that matters.
- **Envelope constants are unchanged:** `MAX_INVITE_TTL_MS = 600_000`, `EXPIRY_CLOCK_SKEW_MS = 30_000`, `MAX_CODE_CHARS = 132_096`, `MAX_FIELD_CHARS = 4_096`. New: `MAX_INVITE_CHANNELS = 32`.
- Windows is the primary dev platform.
- 358 tests pass in the root workspace at plan time, plus 3 in `junto-iced`; they must still pass.

---

### Task 1: Envelope v2 — an invite names many channels

**Files:**
- Modify: `crates/junto/src/enroll.rs` (consts, `InvitePayload`, `decode_invite`, module docs, tests)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub const MAX_INVITE_CHANNELS: usize = 32;`
  - `const PAYLOAD_VERSION: u8 = 2;`
  - `pub struct InvitePayload { pub v: u8, pub invite_token: String, pub member_email: String, pub channels: Vec<String>, pub expires_at: i64 }` — `channel: String` is **replaced**, not supplemented.
  - `EnrollPayload` is **unchanged in shape**; only the `v` it carries moves to 2.

Rules, in the existing decode order (`enroll.rs:122-135` — do not reorder): `decode_code` length cap first, JSON parse, `check_version`, **then** the channel-set checks, then `check_expiry`. The channel-set checks are: non-empty, at most `MAX_INVITE_CHANNELS`, and every element inside `MAX_FIELD_CHARS` (feed them through the existing `check_field_bounds` alongside `invite_token` and `member_email`). `check_version`'s error already names the expected version; extend it so a v1 code says what to do.

- [ ] **Step 1: Write the failing tests** in `enroll.rs`'s test module. Update the existing `sample_invite()` helper to build `channels: vec!["junto-dev".to_string()]`, then add:

```rust
#[test]
fn a_v2_invite_round_trips_a_multi_channel_set() {
    let mut p = sample_invite();
    p.channels = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    let url = encode_invite(&p).unwrap();
    assert!(url.starts_with("junto://invite?code="));
    assert_eq!(decode_invite(&url).unwrap(), p, "the whole set survives the round trip");
}

#[test]
fn an_empty_channel_set_is_refused() {
    // An invite that grants nothing is a bug in the caller, not a valid code:
    // redemption would burn a token and append nothing.
    let mut p = sample_invite();
    p.channels = Vec::new();
    let url = encode_invite(&p).unwrap();
    let err = decode_invite(&url).unwrap_err().to_string();
    assert!(err.contains("at least one channel"), "{err}");
}

#[test]
fn a_channel_set_beyond_the_cap_is_refused() {
    let mut p = sample_invite();
    p.channels = (0..MAX_INVITE_CHANNELS + 1).map(|i| format!("c{i}")).collect();
    let url = encode_invite(&p).unwrap();
    let err = decode_invite(&url).unwrap_err().to_string();
    assert!(err.contains(&MAX_INVITE_CHANNELS.to_string()), "{err}");
}

#[test]
fn a_channel_name_beyond_the_field_bound_is_refused() {
    // The per-element bound, not the set bound: one absurd element in an
    // otherwise sane set. A mutation that only checks the Vec length passes
    // the test above and fails this one.
    let mut p = sample_invite();
    p.channels = vec!["fine".to_string(), "x".repeat(MAX_FIELD_CHARS + 1)];
    let url = encode_invite(&p).unwrap();
    assert!(decode_invite(&url).is_err());
}

#[test]
fn a_v1_invite_is_refused_with_instructions_to_mint_a_new_one() {
    // Hand-build a v1 body: the struct can no longer express it.
    let body = r#"{"v":1,"invite_token":"t","member_email":"dan@x.com","channel":"junto-dev","expires_at":0}"#;
    let url = format!(
        "junto://invite?code={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)
    );
    let err = decode_invite(&url).unwrap_err().to_string();
    assert!(err.contains("mint a new one"), "{err}");
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto enroll::` → FAIL (`channels` does not exist; `MAX_INVITE_CHANNELS` unresolved).
- [ ] **Step 3: Implement.** Add the const with a doc comment explaining the number ("a founder ticking more than 32 channels in one pass is a mistake, and an unbounded set makes redemption fan out unboundedly"); flip `PAYLOAD_VERSION` to 2; change the field; add the checks in the stated order; extend `check_version`'s message to `"unsupported payload version {v} (expected {PAYLOAD_VERSION}) — this code came from an older junto; mint a new one"`. Update the module docs' payload sketch to show `channels`.
- [ ] **Step 4: Run** — `cargo test -p junto enroll::` → PASS. Then `cargo test -p junto` and expect **known** failures only in `main.rs` call sites (Task 3/4 fix them); note them in the commit body rather than patching them here.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/enroll.rs
git commit -m "feat(junto): invite envelope v2 names many channels"
```

---

### Task 2: The invite store recovers a token's channel set

**Files:**
- Modify: `crates/junto/src/invites.rs` (`channels_for`, module docs, tests)

**Interfaces:**
- Consumes: the existing private `load`, `token_sha256`, `now_ms`, `InviteRecord`.
- Produces: `pub fn channels_for(junto_home: &Path, token: &str) -> Result<Vec<String>>` — every channel this token still covers: records whose `token_sha256` matches, `consumed_at.is_none()`, and `expires_at >= now_ms()`. Order is file order (which is issue order). An unknown token yields an empty vec, not an error.

Rules: `issue` and `consume` are **not** modified — the store already holds one record per `(token_sha256, channel)`, which is exactly what a multi-channel invite needs. `channels_for` is a read: it must not write, prune, or consume.

- [ ] **Step 1: Write the failing tests** in `invites.rs`'s test module:

```rust
#[test]
fn channels_for_returns_every_channel_the_token_still_covers() {
    let home = tempfile::tempdir().unwrap();
    let token = "t".repeat(43);
    issue(home.path(), &token, "dan@x.com", "chan-a", future()).unwrap();
    issue(home.path(), &token, "dan@x.com", "chan-b", future()).unwrap();
    assert_eq!(
        channels_for(home.path(), &token).unwrap(),
        vec!["chan-a".to_string(), "chan-b".to_string()]
    );
}

#[test]
fn channels_for_omits_a_consumed_channel_and_keeps_the_rest() {
    // The property redemption retries depend on: a partial success leaves the
    // remainder recoverable from the same code.
    let home = tempfile::tempdir().unwrap();
    let token = "t".repeat(43);
    issue(home.path(), &token, "dan@x.com", "chan-a", future()).unwrap();
    issue(home.path(), &token, "dan@x.com", "chan-b", future()).unwrap();
    assert!(matches!(
        consume(home.path(), &token, "dan@x.com", "chan-a").unwrap(),
        Consumed::Ok
    ));
    assert_eq!(channels_for(home.path(), &token).unwrap(), vec!["chan-b".to_string()]);
}

#[test]
fn channels_for_omits_an_expired_channel() {
    let home = tempfile::tempdir().unwrap();
    let token = "t".repeat(43);
    issue(home.path(), &token, "dan@x.com", "stale", past()).unwrap();
    issue(home.path(), &token, "dan@x.com", "live", future()).unwrap();
    assert_eq!(channels_for(home.path(), &token).unwrap(), vec!["live".to_string()]);
}

#[test]
fn channels_for_an_unknown_token_is_empty_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    assert!(channels_for(home.path(), &"z".repeat(43)).unwrap().is_empty());
}

#[test]
fn channels_for_does_not_consume() {
    // A read that burned the token would make the redeem screen's preview
    // destructive — the exact bug this assertion exists to prevent.
    let home = tempfile::tempdir().unwrap();
    let token = "t".repeat(43);
    issue(home.path(), &token, "dan@x.com", "chan-a", future()).unwrap();
    channels_for(home.path(), &token).unwrap();
    assert!(matches!(
        consume(home.path(), &token, "dan@x.com", "chan-a").unwrap(),
        Consumed::Ok
    ));
}
```

If `past()` does not already exist beside the module's `future()` helper, add it: `fn past() -> i64 { now_ms() - 60_000 }`.

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto invites::` → FAIL (no `channels_for`).
- [ ] **Step 3: Implement** `channels_for` and extend the module docs to say a token may cover several channels and that this is the read that recovers them.
- [ ] **Step 4: Run** — `cargo test -p junto invites::` → PASS (all pre-existing store tests still green: `issue`/`consume` were not touched).
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/invites.rs
git commit -m "feat(junto): recover an invite's remaining channels by token hash"
```

---

### Task 3: `junto invite --channel` becomes repeatable

**Files:**
- Modify: `crates/junto/src/main.rs` — `Command::Invite` (~180-193), `async fn invite` (~591-667), `fn invite_line` (~669-673), tests

**Interfaces:**
- Consumes: Task 1's `InvitePayload.channels` / `MAX_INVITE_CHANNELS`; the existing `resolve_channel` (786-808), `require_founder` (810-829), `invites::issue`, `invites::prune`, `enroll::mint_invite_token`, `enroll::encode_invite`.
- Produces:
  - `Command::Invite { member: String, channel: Vec<String> }` — clap `#[arg(long = "channel", required = true, num_args = 1..)]`.
  - `async fn invite(channels: Vec<String>, member: String) -> Result<()>`
  - `fn invite_line(url: &str, expires_at: i64, channels: &[String]) -> String`

Rules: resolve **every** channel to its canonical id first, and `require_founder` on **every** one, before minting a token — an invite the caller cannot complete for even one channel is refused whole (nothing issued, nothing printed). Deduplicate canonical ids (two names for one channel must not produce two records). Refuse more than `MAX_INVITE_CHANNELS`. `issue` once per canonical id with the same token. Keep the leading `invites::prune` call (it is wired here and a test pins it).

- [ ] **Step 1: Write the failing tests** in `main.rs`'s test module:

```rust
/// The flag is repeatable and the parse keeps order and multiplicity.
#[test]
fn invite_accepts_repeated_channel_flags() {
    let cli = Cli::try_parse_from([
        "junto", "invite", "--member", "dan@x.com",
        "--channel", "one", "--channel", "two",
    ])
    .expect("parses");
    let Command::Invite { channel, .. } = cli.command else {
        panic!("expected invite");
    };
    assert_eq!(channel, vec!["one".to_string(), "two".to_string()]);
}

/// `--channel` with no value at all is still refused at parse time: an invite
/// for zero channels grants nothing.
#[test]
fn invite_requires_at_least_one_channel() {
    assert!(Cli::try_parse_from(["junto", "invite", "--member", "dan@x.com"]).is_err());
}

/// The printed line names every channel, so the founder can see what they are
/// about to hand over before they paste it.
#[test]
fn invite_line_names_every_channel_and_the_expiry() {
    let line = invite_line(
        "junto://invite?code=abc",
        1_781_000_000_000,
        &["alpha".to_string(), "beta".to_string()],
    );
    assert!(line.contains("junto://invite?code=abc"), "{line}");
    assert!(line.contains("alpha") && line.contains("beta"), "{line}");
    assert!(line.contains("2026"), "{line}");
}

/// Founder authority is checked for EVERY channel before a token exists: a
/// caller who founds one of two channels gets nothing issued at all.
#[tokio::test]
async fn invite_issues_nothing_when_the_caller_does_not_found_every_channel() {
    let _home = crate::host::test_home::HomeGuard::new();
    // setup_channel founds `mine` as the git user; found `theirs` with a
    // different founder by appending a genesis authored by someone else
    // (mirror the existing `grant_key` fixture's direct-append approach).
    // Then: invite(vec!["mine", "theirs"], "someone@x.com") must Err, and
    // invites.toml must contain no record for either channel.
}

/// One token, one record per channel — the shape `channels_for` reads back.
#[tokio::test]
async fn invite_issues_one_record_per_channel_for_a_single_token() {
    let _home = crate::host::test_home::HomeGuard::new();
    // Found two channels as the same git user, invite into both, then assert
    // invites::channels_for(home, token) returns both canonical ids. Recover
    // the token from the printed URL via enroll::decode_invite.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto invite` → FAIL.
- [ ] **Step 3: Implement.** Change the clap variant and the handler signature; loop-resolve and loop-check founders; dedupe; `issue` per id; build the v2 payload; widen `invite_line`. Update the `Command::Invite` doc comment to say one invite may cover several channels and that founder authority is required on all of them.
- [ ] **Step 4: Run** — `cargo test -p junto invite` → PASS, and `cargo test -p junto invites::` still green.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/main.rs
git commit -m "feat(junto): one invite covers many channels"
```

---

### Task 4: `add-member --enroll` redeems the whole set, and reports per channel

**Files:**
- Modify: `crates/junto/src/main.rs` — `Command::AddMember` (~119-179), `async fn add_member` (~372-523), `fn consumed_error` (~544-578), tests

**Interfaces:**
- Consumes: Task 2's `invites::channels_for`; `enroll::decode_enroll`; `invites::consume`; `host.add_member`; `resolve_channel`; `revocation_cutoff_warning` (846-873); `spent_token_context` (525-542).
- Produces:
  - `Command::AddMember` **loses `channel` on the `--enroll` path**: `channel` becomes `Option<String>` with `#[arg(long, required_unless_present = "enroll", conflicts_with = "enroll")]`.
  - `pub enum RedeemOutcome { Granted, AlreadyAMember, InviteAlreadyUsed, NotFounder, Failed(String) }` — derive `Debug, Clone, PartialEq, Eq`.
  - `fn redeem_line(channel: &str, outcome: &RedeemOutcome) -> String` — one printed line per channel.
  - `async fn redeem_enrollment(host: &host::Host, payload: &enroll::EnrollPayload, kind: MemberKind) -> Result<Vec<(String, RedeemOutcome)>>` — the shared engine Task 9's endpoint also calls.

Rules:
- The channel set comes from `channels_for(&junto_home()?, &payload.invite_token)`. An empty set is an error naming the two causes (never issued here / already fully redeemed).
- Per channel, in this order: resolve to canonical id → project → founder check → `consume` for **that** id → `add_member`. A channel whose `consume` returns anything but `Ok` maps to `InviteAlreadyUsed` (for `AlreadyUsed`) or `Failed(<consumed_error message>)`; a non-founder channel maps to `NotFounder` **without consuming**.
- Burn only what appends: `consume` immediately before `add_member`, and if `add_member` errors, report `Failed` for that channel and keep going. The token for other channels is untouched, so re-pasting the code retries exactly the remainder.
- `AlreadyAMember` is reported (not hidden) when `add_member` returns without appending — detect it by projecting after the call and checking the keyring for an active grant on the payload's key.
- `--kind` is asked once and applied to every channel. `revocation_cutoff_warning` still prints, per channel, before its append.
- Never `bail!` mid-set: a partial run must print its whole outcome list, then exit non-zero if **no** channel was granted.

- [ ] **Step 1: Write the failing tests:**

```rust
/// `--channel` is now meaningless on the enroll path: the set comes from the
/// invite store, and accepting a name here would reintroduce exactly the
/// name-vs-id divergence `add_member_enroll_resolves_channel_by_name_before_
/// consuming_the_invite` was written to prevent.
#[test]
fn add_member_enroll_refuses_a_channel_flag() {
    let err = Cli::try_parse_from([
        "junto", "add-member", "--enroll", "junto://enroll?code=x",
        "--kind", "human", "--channel", "junto-dev",
    ])
    .expect_err("--channel conflicts with --enroll");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

/// The keyless path still requires --channel: nothing about this task changes it.
#[test]
fn add_member_keyless_still_requires_channel() {
    assert!(Cli::try_parse_from([
        "junto", "add-member", "--email", "a@b.c", "--name", "A", "--kind", "agent",
    ])
    .is_err());
}

#[test]
fn redeem_line_reads_differently_for_every_outcome() {
    let cases = [
        RedeemOutcome::Granted,
        RedeemOutcome::AlreadyAMember,
        RedeemOutcome::InviteAlreadyUsed,
        RedeemOutcome::NotFounder,
        RedeemOutcome::Failed("append failed".into()),
    ];
    let lines: Vec<String> = cases.iter().map(|o| redeem_line("chan", o)).collect();
    for line in &lines {
        assert!(line.contains("chan"), "{line}");
    }
    let unique: std::collections::HashSet<&String> = lines.iter().collect();
    assert_eq!(unique.len(), cases.len(), "each outcome must read differently: {lines:?}");
}

/// The core guarantee: a mixed run grants what it can, reports the rest, and
/// burns ONLY the granted channel — so the same enroll code retries the
/// remainder.
#[tokio::test]
async fn redeeming_a_mixed_set_burns_only_what_it_granted() {
    let _home = crate::host::test_home::HomeGuard::new();
    // Found `mine-a` and `mine-b` as the git user; found `theirs` with another
    // founder. Invite into all three (issue directly for `theirs`, since
    // Task 3's invite refuses a set the caller does not wholly found).
    // Enroll on a second home to get a real enroll payload.
    // Redeem: expect Granted for mine-a and mine-b, NotFounder for theirs.
    // Then assert channels_for(token) == ["theirs"] — the unburned remainder.
}

#[tokio::test]
async fn redeeming_the_same_code_twice_reports_already_used_not_a_silent_success() {
    let _home = crate::host::test_home::HomeGuard::new();
    // One channel. Redeem once → Granted. Redeem again → InviteAlreadyUsed,
    // and the keyring still holds exactly one active grant for that key.
}

#[tokio::test]
async fn an_enroll_code_with_no_remaining_channels_is_an_error_naming_both_causes() {
    let _home = crate::host::test_home::HomeGuard::new();
    // A payload whose token was never issued on this machine.
    // assert the error mentions both "never issued" and "already redeemed".
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto add_member` / `redeem` → FAIL.
- [ ] **Step 3: Implement** `RedeemOutcome`, `redeem_line`, `redeem_enrollment`, and rewire `add_member`'s `--enroll` branch to call it and print the lines. Delete the now-dead single-channel enroll code path. Update the `AddMember` doc comment: `--enroll` takes no `--channel`, and the set comes from the invite.
- [ ] **Step 4: Run** — `cargo test -p junto` → all green, including the pre-existing enroll tests, which must be **edited** (not deleted) to drop `--channel`: `add_member_enroll_resolves_channel_by_name_before_consuming_the_invite` becomes a test that the canonical id from the store is used verbatim; `add_member_enroll_second_use_of_the_same_url_is_refused_as_already_used`, `add_member_enroll_failure_after_consume_names_the_spent_token`, `add_member_enroll_kind_agent_records_agent_with_the_payloads_key`, `add_member_enroll_kind_human_still_records_human_with_the_payloads_key`, and `add_member_enroll_still_succeeds_when_re_enrolling_a_revoked_member` all lose their `--channel` argument. Say in the commit body why each moved.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/main.rs
git commit -m "feat(junto): redeem an enrollment across every channel its invite covered"
```

---

### Task 5: One founder guard, one fingerprint, shared by CLI and host

**Files:**
- Create: `crates/junto/src/identity.rs`
- Modify: `crates/junto/src/main.rs` (module decl; delete the moved fns; update call sites)

**Interfaces:**
- Moved verbatim from `main.rs`, now `pub(crate)` in `identity.rs`: `require_founder` (810-829), `fingerprint` (875-887), `grants_to_park` (831-844), `revocation_cutoff_warning` (846-873), `keys_list_lines` (889-921). Their tests move with them.
- Produces additionally: `pub(crate) fn is_founder(view: &ChannelView, email: &str) -> bool` — the boolean the endpoints and `keys.json` need (`require_founder` keeps returning `Result` for the CLI's error text).

Rules: a pure move plus one addition. No behaviour change, no signature change on the moved functions. This exists because Tasks 6-10 must not grow a second founder check — the CLI and the endpoints drifting on who may grant is the failure this task prevents.

- [ ] **Step 1: Write the failing test** in `identity.rs`:

```rust
#[test]
fn is_founder_agrees_with_require_founder() {
    // The two must never disagree: one is the endpoints' predicate, the other
    // the CLI's error path. Same view, same answer, for founder and non-founder.
    let view = view_with_party(&["founder@x.com", "member@x.com"]);
    assert!(is_founder(&view, "founder@x.com"));
    assert!(require_founder(&view, &Member::human("F", "founder@x.com"), "c").is_ok());
    assert!(!is_founder(&view, "member@x.com"));
    assert!(require_founder(&view, &Member::human("M", "member@x.com"), "c").is_err());
}
```

Add a `fn view_with_party(emails: &[&str]) -> ChannelView` fixture modelled on `main.rs`'s existing `channel_view_with_keyring` (1836) — default every other field.

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto identity::` → FAIL (module does not exist).
- [ ] **Step 3: Implement** — create the module, move the five functions and their tests, add `is_founder`, add `mod identity;` to `main.rs`, and update every call site (`invite`, `add_member`, `keys_list`, `revoke_member`, `retire_device`).
- [ ] **Step 4: Run** — `cargo test -p junto` → green, same count as before plus the new test. `cargo clippy -p junto --all-targets -- -D warnings` → clean (watch for now-unused imports in `main.rs`).
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/identity.rs crates/junto/src/main.rs
git commit -m "refactor(junto): share the founder guard and fingerprint between surfaces"
```

---

### Task 6: `GET /channels/{channel}/keys.json`

**Files:**
- Modify: `crates/junto/src/web.rs` (route, handler, DTOs, tests)

**Interfaces:**
- Consumes: `project_fresh` (web.rs:127 — **not** the cached `project`; see Global Constraints), `host::git_user`, Task 5's `identity::{fingerprint, is_founder}`.
- Produces:

```rust
#[derive(Serialize)]
struct KeysDto {
    founder_email: String,
    /// The git identity this host writes as — the surface needs it to know
    /// whose devices these are.
    viewer_email: Option<String>,
    /// Whether that identity may perform the founder-only acts, so the GUI
    /// shows or hides them instead of guessing.
    viewer_is_founder: bool,
    members: Vec<KeyMemberDto>,
}

#[derive(Serialize)]
struct KeyMemberDto {
    display_name: String,
    email: String,
    /// "human" | "agent".
    kind: String,
    devices: Vec<KeyGrantDto>,
    /// Every grant retired: this email is revoked as of `cutoff`.
    revoked: bool,
}

#[derive(Serialize)]
struct KeyGrantDto {
    /// 16 hex chars. Never the whole public key.
    fingerprint: String,
    granted_by: String,
    /// Epoch millis, when retired.
    retired_at: Option<i64>,
}
```

Rules: members in `view.party` order (founder first). Devices in keyring order (canonical, hence stable across replicas). `revoked` is true iff the email has grants and **all** are retired — the same all-retired rule ADR 0035 defines, never re-derived by hand. A member with no grants gets an empty `devices` and `revoked: false`. `viewer_email` is `None` when `git_user` fails; the endpoint still answers 200 (reading a roster needs no identity). The projection is `project_fresh`, so a `junto revoke-member` run in a terminal is visible to the next GUI refresh instead of hiding behind this process's cache.

- [ ] **Step 1: Write the failing test** in `web.rs`'s test module (follow the existing handler tests' fixture style):

```rust
#[tokio::test]
async fn keys_json_lists_devices_by_fingerprint_and_never_the_whole_key() {
    // Fixture: a channel whose founder has two grants, one retired.
    // Assert: 200; the body contains both fingerprints; the body does NOT
    // contain the full public key hex of either; retired_at is set on exactly
    // one; viewer_is_founder is true for the git user; revoked is false
    // (one grant still active).
}

#[tokio::test]
async fn keys_json_marks_an_email_revoked_only_when_every_grant_is_retired() {
    // Same fixture, second grant also parked → revoked: true, and the member
    // is STILL present in members (ADR 0035: they stay in the party).
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto keys_json` → FAIL (404 / no handler).
- [ ] **Step 3: Implement** the DTOs, the handler, and `.route("/channels/{channel}/keys.json", get(keys_json))` beside the other `*.json` routes (web.rs:78-85).
- [ ] **Step 4: Run** — `cargo test -p junto keys_json` → PASS. Manual: `curl -s localhost:1727/channels/junto-dev/keys.json | jq` shows fingerprints only.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "feat(host): publish a channel's key grants as fingerprints"
```

---

### Task 7: `POST /invites`

**Files:**
- Modify: `crates/junto/src/web.rs` (route, handler, form + response DTOs, tests)

**Interfaces:**
- Consumes: `host.resolve`, `project`, `host::git_user`, `identity::is_founder`, `invites::{prune, issue}`, `enroll::{mint_invite_token, encode_invite, InvitePayload, MAX_INVITE_CHANNELS, MAX_INVITE_TTL_MS}`.
- Produces:
  - `struct InviteForm { member: String, channel: Vec<String> }` — repeated `channel` form keys, the same shape `save_agent` already accepts for `skill`/`plugin_path` (web.rs:2152-2164).
  - `#[derive(Serialize)] struct InviteMintedDto { url: String, expires_at: i64, channels: Vec<String> }` — `channels` are the resolved canonical ids, so the caller can show what it actually granted.
  - Route: `.route("/invites", post(mint_invite))`.

Rules: identical authority rules to Task 3's CLI — resolve every channel, `is_founder` on every one, dedupe, refuse an empty set or one beyond `MAX_INVITE_CHANNELS`, `prune` first, then mint once and `issue` per id. **Nothing is issued if any channel fails its check** (400/403 with a message naming the offending channel). `expires_at = now + MAX_INVITE_TTL_MS`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn post_invites_mints_one_token_covering_every_channel() {
    // Two founded channels. POST member + two channel keys.
    // Assert 200; decode the returned url with enroll::decode_invite and check
    // channels == both canonical ids; invites::channels_for returns both.
}

#[tokio::test]
async fn post_invites_issues_nothing_when_one_channel_is_not_the_callers() {
    // Founded + not-founded. Assert 403 and that invites.toml gained NO record
    // for either channel — the all-or-nothing rule.
}

#[tokio::test]
async fn post_invites_refuses_an_empty_channel_set() {
    // No channel keys at all → 400, nothing issued.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto post_invites` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p junto post_invites` → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "feat(host): mint a multi-channel enrollment invite over HTTP"
```

---

### Task 8: `POST /devices/enroll` — the joiner's machine mints its own key

**Files:**
- Modify: `crates/junto/src/web.rs` (route, handler, DTOs, tests)

**Interfaces:**
- Consumes: `enroll::{decode_invite, encode_enroll, EnrollPayload}`, `keys::signing_key`, `host::junto_home`, `host::git_user`, `identity::fingerprint`.
- Produces:
  - `struct EnrollForm { invite: String, name: Option<String> }`
  - `#[derive(Serialize)] struct EnrolledDto { url: String, email: String, fingerprint: String }`
  - Route: `.route("/devices/enroll", post(enroll_device))`.

Rules — read these twice, this is the one endpoint that creates secret material:
- The email comes **only** from the decoded invite. The form carries no email field, and adding one is prohibited; `name` supplies the display name only, defaulting to `git_user`'s name and then to the email's local part.
- `keys::signing_key(&junto_home()?, &invite.member_email)` mints-or-reuses on **this** machine. The response carries the public fingerprint and the `junto://enroll?code=…` URI. No secret, no seed, no full key in the response, the logs, or an error message.
- No founder check and no member-code check: the invite token is the authorization, and the caller is at this machine's localhost (ADR 0012).
- A malformed or expired invite is a 400 whose message distinguishes the two.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn post_devices_enroll_mints_for_the_invites_email_and_echoes_only_the_public_half() {
    // POST a valid invite. Assert 200; decode the returned url; the payload's
    // email equals the invite's; the response body does not contain the 64-hex
    // secret from keys.toml; keys.toml on this home now holds that email.
}

#[tokio::test]
async fn post_devices_enroll_ignores_an_email_supplied_by_the_caller() {
    // Send an extra `email=attacker@x.com` form key alongside a valid invite.
    // Assert the enrolled payload's email is still the INVITE's email.
    // (Serde ignores the unknown field; this test pins that it stays ignored.)
}

#[tokio::test]
async fn post_devices_enroll_refuses_an_expired_invite_before_minting() {
    // Expired invite → 400 mentioning expiry, and keys.toml gained no record.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto devices_enroll` → FAIL.
- [ ] **Step 3: Implement.** Add a module-level comment on the handler stating it is the only endpoint that may cause a mint, that it must never appear on the mobile/remote read-only role, and why the email cannot come from the request.
- [ ] **Step 4: Run** — `cargo test -p junto devices_enroll` → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "feat(host): enroll this machine's device key from an invite"
```

---

### Task 9: `POST /members` — redeem, per channel, with outcomes

**Files:**
- Modify: `crates/junto/src/web.rs` (route, handler, DTOs, tests)

**Interfaces:**
- Consumes: Task 4's `redeem_enrollment` and `RedeemOutcome` (make both `pub(crate)`), `enroll::decode_enroll`.
- Produces:
  - `struct RedeemForm { enroll: String, kind: String }`
  - `#[derive(Serialize)] struct RedeemedDto { outcomes: Vec<RedeemOutcomeDto> }`, `#[derive(Serialize)] struct RedeemOutcomeDto { channel: String, channel_name: Option<String>, result: String, detail: Option<String> }`
  - Route: `.route("/members", post(redeem_enrollment_endpoint))`.

Rules: `kind` is required and parsed strictly (`"human"` | `"agent"`; anything else is a 400 — no default, ADR 0035). The handler is a thin shell over Task 4's engine: decode, parse kind, call `redeem_enrollment`, serialize. `result` is the `RedeemOutcome` variant in snake_case; `detail` carries `Failed`'s message. HTTP status is 200 when at least one channel was granted, 409 when none were (the whole set was already used or not ours) — the body carries the per-channel truth either way. `channel_name` is resolved for display; `channel` is always the canonical id.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn post_members_grants_every_channel_and_reports_each() {
    // Two founded channels on one invite → 200, two outcomes, both "granted",
    // and both keyrings hold the device's key as an ACTIVE grant.
}

#[tokio::test]
async fn post_members_returns_409_with_outcomes_when_nothing_could_be_granted() {
    // Redeem twice; the second call → 409 and every outcome
    // "invite_already_used" (not an opaque error page).
}

#[tokio::test]
async fn post_members_refuses_a_missing_or_unknown_kind() {
    // kind absent → 400; kind="person" → 400. ADR 0035: never defaulted.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto post_members` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p junto post_members` → PASS, and Task 4's CLI tests still green (shared engine, one behaviour).
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs crates/junto/src/main.rs
git commit -m "feat(host): redeem an enrollment over HTTP with per-channel outcomes"
```

---

### Task 10: retire a device, revoke a member, over HTTP

**Files:**
- Modify: `crates/junto/src/web.rs` (routes, handlers, tests)

**Interfaces:**
- Consumes: `identity::{is_founder, grants_to_park}`, `host::git_user`, `host.authorize_human_write` (the guard `verify` uses at web.rs:1514), `host.sign_entry`, `EntryPayload::Park`.
- Produces:
  - `struct RationaleForm { rationale: String }`
  - `#[derive(Serialize)] struct ParkedDto { parked: usize }`
  - Routes: `.route("/channels/{channel}/keys/{grant}/retire", post(retire_device))` and `.route("/channels/{channel}/members/{email}/revoke", post(revoke_member))`.

Rules — mirror the CLI exactly (`main.rs:944-1059`), because two implementations of revocation is the drift this design must not introduce:
- Empty `rationale` → 400, in the same voice `verify` already uses ("it's a rationale, not a checkbox").
- Founder-only, and `revoke_member` **refuses to revoke the founder** (the CLI test `revoke_member_refuses_to_revoke_the_founder` pins this; `retire_device` deliberately still works on a founder grant, for rotation).
- `revoke_member` parks every **active** grant via `grants_to_park` and 400s when there are none. `retire_device` parks exactly the named grant, 404 for an unknown grant id and 409 for one already retired.
- Build the entry exactly as `verify` does: `signature: None`, fresh `EntryId::new()`, `Timestamp::now()`, `host.sign_entry`, `guard.append`, **drop the guard**, then spawn the fresh-handle sync. Never hold the ledger lock across the push (web.rs:1538-1553 explains why; PR #67 is what happens when a lock spans a projection). One deliberate difference from `verify`: project with `guard.project_fresh(&id)`, so a grant the CLI retired seconds ago in another process is not parked twice off a cached fold.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn post_revoke_parks_every_active_grant_and_leaves_the_member_in_the_party() {
    // Member with two active grants → 200 {"parked":2}; both grants show
    // retired_at; the member is still in view.party (ADR 0035).
}

#[tokio::test]
async fn post_revoke_refuses_the_founder_and_an_empty_rationale() {
    // founder target → 400/403; empty rationale → 400. Nothing appended in
    // either case (assert the entry count is unchanged).
}

#[tokio::test]
async fn post_retire_parks_one_grant_and_refuses_an_already_retired_one() {
    // Two grants: retire one → 200 {"parked":1}, the other still active.
    // Retire the same grant again → 409, no second Park appended.
}

#[tokio::test]
async fn post_retire_refuses_a_caller_who_is_not_the_founder() {
    // A member who is in the party but did not found the channel → 403.
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto post_re` → FAIL.
- [ ] **Step 3: Implement** both handlers.
- [ ] **Step 4: Run** — `cargo test --workspace` → green (this task appends `Park` entries on the shared path; the whole suite is the gate).
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "feat(host): retire a device and revoke a member over HTTP"
```

---

### Task 11: make revocation visible — `unverified` reaches the native timeline

**Files:**
- Modify: `crates/junto/src/web.rs` (`EntryDto` ~1959, its construction ~2122)
- Modify: `crates/junto-iced/src/main.rs` (`EntryDto` ~424-438, `entry_card` ~3128-3300)

**Interfaces:**
- Produces: `EntryDto.unverified: bool` on both sides (host serializes, GUI deserializes with `#[serde(default)]`).

Rules: `unverified` is `view.unverified.contains(&entry.id)`, computed beside the existing `unrecognized` line. In the card, render a yellow `unverified` badge next to the existing red `unrecognized` badge, and **suppress `unverified` when `unrecognized` is set** — the web already reasons this way ("redundant on an unrecognized card (that badge already signals distrust louder)", `render.rs:2798-2801`). Reuse the GUI's existing `badge(label, colour)` helper and its `YELLOW`/`RED` constants; do not introduce new colours.

- [ ] **Step 1: Write the failing tests.** Host side, in `web.rs`:

```rust
#[tokio::test]
async fn view_json_flags_an_unverified_entry() {
    // An entry signed by a key that does not match its author's grant projects
    // as unverified; assert the JSON carries unverified: true for it and false
    // for a properly signed neighbour.
}
```

GUI side, in `crates/junto-iced/src/main.rs`'s test module (it has three tests; add a fourth, pure and widget-free):

```rust
#[test]
fn entry_badges_suppress_unverified_on_an_unrecognized_card() {
    // Extract the decision into a pure helper so it is testable without a
    // renderer: fn entry_badges(entry: &EntryDto) -> (bool, bool) returning
    // (show_unrecognized, show_unverified).
    let both = EntryDto { unrecognized: true, unverified: true, ..sample_entry() };
    assert_eq!(entry_badges(&both), (true, false));
    let only_unverified = EntryDto { unrecognized: false, unverified: true, ..sample_entry() };
    assert_eq!(entry_badges(&only_unverified), (false, true));
    let clean = EntryDto { unrecognized: false, unverified: false, ..sample_entry() };
    assert_eq!(entry_badges(&clean), (false, false));
}
```

Add a `fn sample_entry() -> EntryDto` fixture (all fields defaulted/empty) in the same test module.

- [ ] **Step 2: Run to verify failure** — `cargo test -p junto view_json_flags` and `cargo test --manifest-path crates/junto-iced/Cargo.toml entry_badges` → both FAIL.
- [ ] **Step 3: Implement** the host field, the GUI field, the `entry_badges` helper, and the badge row in `entry_card`.
- [ ] **Step 4: Run** both test commands → PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/web.rs crates/junto-iced/src/main.rs
git commit -m "feat(surface): show unverified entries natively, not just on the web"
```

---

### Task 12: Settings → "this device", and joining a channel

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `App` (63-108), `Message` (500-646), `update` (648-1827), `settings_panel` (2068-2147), HTTP helpers (near 3920-3990)

**Interfaces:**
- Consumes: Task 8's `POST /devices/enroll`, Task 6's `keys.json` (for this identity's fingerprint), the existing `SettingsDto` (305-311), `load_signing_key` (4158), `junto_home` (4128).
- Produces:
  - `App` fields: `join_invite: String`, `join_pending: bool`, `join_error: Option<String>`, `join_result: Option<EnrolledDto>`.
  - `Message` variants: `JoinInviteChanged(String)`, `JoinSubmit`, `JoinDone(Result<EnrolledDto, String>)`, `CopyText(String)`.
  - `#[derive(Deserialize, Clone, Debug)] struct EnrolledDto { url: String, email: String, fingerprint: String }` — mirrors Task 8's response.
  - `fn post_device_enroll(base: String, invite: String, name: Option<String>) -> Task<Message>` — modelled on `post_save_agent` (3920) and `simple_post_result` (3967), but parsing a JSON body; add `fn post_json_result<T: DeserializeOwned>(url: String, form: Vec<(&'static str, String)>, what: &'static str) -> Result<T, String>` beside `simple_post_result` and reuse it in Tasks 13.

Rules: the section shows the git identity (`SettingsDto.identity`), whether this machine holds a key for it (`load_signing_key(email).is_some()` — never mint from the GUI), and that key's fingerprint (derive the same 16 chars the host does: strip `ed25519:`, take 16). The join box takes a pasted invite, disables its button while `join_pending`, and on success shows the enroll code with a copy button plus the literal line `your secret key never leaves this machine`. Errors render in the panel, never a dialog. Reuse `admin_card`, `chip_style`, and the existing `add_btn`/`remove_btn` visual vocabulary.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn device_fingerprint_matches_the_hosts_sixteen_chars() {
    // The GUI derives a fingerprint locally; it must agree with the host's
    // identity::fingerprint, or the two screens show different ids for one key.
    let key = "ed25519:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(device_fingerprint(key), "0123456789abcdef");
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --manifest-path crates/junto-iced/Cargo.toml device_fingerprint` → FAIL.
- [ ] **Step 3: Implement** `device_fingerprint`, the state, the messages, the update arms, `post_device_enroll`, `post_json_result`, and the Settings section.
- [ ] **Step 4: Run** — the GUI test command → PASS; `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings` → clean.
- [ ] **Step 5: Smoke it** — with `cargo run -p junto -- serve` running, `cargo run --manifest-path crates/junto-iced/Cargo.toml`, mint an invite with `junto invite`, paste it into Settings → this device, and confirm an enroll code comes back and `keys.toml` gained the record.
- [ ] **Step 6: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): join a channel from Settings, minting this device's key"
```

---

### Task 13: the members & devices disclosure — invite, redeem, retire, revoke

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `Pane` (117-240), `Message` (500-646), `update` (648-1827), `pane_body`'s party row (2573-2584), a new form fn beside `lifecycle_form` (2425-2505), HTTP helpers

**Interfaces:**
- Consumes: Tasks 6, 7, 9, 10's endpoints; Task 12's `post_json_result`; the existing inline-form-with-confirm pattern.
- Produces:
  - DTO mirrors: `KeysDto`, `KeyMemberDto`, `KeyGrantDto`, `InviteMintedDto`, `RedeemedDto`, `RedeemOutcomeDto` (field-for-field with Tasks 6/7/9).
  - `Pane` fields: `keys: Option<KeysDto>`, `members_open: bool`, `identity_form: Option<IdentityForm>`, `identity_pending: bool`, `identity_error: Option<String>`, `invite_minted: Option<InviteMintedDto>`, `redeem_outcomes: Vec<RedeemOutcomeDto>`, and the form inputs `identity_member: String`, `identity_channels: Vec<(String, bool)>`, `identity_paste: String`, `identity_kind: String`, `identity_rationale: String`.
  - `enum IdentityForm { Invite, Redeem, Retire { grant: String }, Revoke { email: String } }` — deriving `Debug, Clone, PartialEq, Eq`, the analogue of `LifecycleKind`.
  - `Message` variants following the pane-first convention: `KeysFetched(pane_grid::Pane, Result<KeysDto, String>)`, `MembersToggle(pane_grid::Pane)`, `IdentitySelect(pane_grid::Pane, IdentityForm)`, `IdentityCancel(pane_grid::Pane)`, `IdentityInput(pane_grid::Pane, IdentityField, String)`, `IdentityChannelToggle(pane_grid::Pane, usize)`, `IdentitySubmit(pane_grid::Pane)`, `IdentityDone(pane_grid::Pane, Result<IdentityResult, String>)`, `Tick`.
  - `enum IdentityResult { Minted(InviteMintedDto), Redeemed(RedeemedDto), Parked(usize) }`.
  - `fn fetch_keys(pane, base, channel) -> Task<Message>`, `fn post_invite(pane, base, member, channels) -> Task<Message>`, `fn post_redeem(pane, base, enroll, kind) -> Task<Message>`, `fn post_park(pane, base, channel, target: IdentityForm, rationale) -> Task<Message>`.

Rules:
- The party text row is **replaced** by a disclosure header (`members (N)` plus a `devices: M` count), expanding to member rows and, under each, its device rows: `fingerprint · granted <entry-id-prefix> · active|retired <iso>`. A revoked member reads `no active devices`, never anything implying removal.
- Founder-only acts are shown only when `keys.viewer_is_founder`; otherwise the buttons are absent (not disabled-and-mysterious).
- Every form is the lifecycle pattern: one open form at a time per pane, confirm shows `working…` while `identity_pending`, cancel always enabled, error text under the form.
- Retire and revoke require a rationale before the confirm button activates — the host refuses an empty one, and the GUI must not make the user discover that over HTTP.
- The invite form's channel list comes from the app's existing channel list, pre-ticking the current pane's channel. The minted code shows a **countdown**: add a 1-second `Tick` subscription that is live **only** while `invite_minted` is `Some` and unexpired, so the app does not wake every second for nothing. On expiry the code is replaced by "expired — mint another".
- Redeem shows a pre-append preview (email, fingerprint, channels from the response of a first "preview" — since the endpoint appends, preview is simply the confirmation screen built from `decode`-free data the user pasted; do not add a preview endpoint) then the outcome list, which persists until dismissed.
- After any successful act, refetch `keys.json` and the pane's `view.json` so the panel and the timeline agree.

- [ ] **Step 1: Write the failing tests** (pure helpers only — this crate has no widget tests):

```rust
#[test]
fn device_line_reads_active_and_retired_differently() {
    let active = KeyGrantDto { fingerprint: "abc".into(), granted_by: "e1".into(), retired_at: None };
    let retired = KeyGrantDto { fingerprint: "abc".into(), granted_by: "e1".into(), retired_at: Some(1_781_000_000_000) };
    assert!(device_line(&active).contains("active"));
    assert!(device_line(&retired).contains("retired"));
    assert!(device_line(&retired).contains("2026"));
}

#[test]
fn a_member_with_every_grant_retired_reads_as_no_active_devices() {
    let m = KeyMemberDto {
        display_name: "Dan".into(), email: "d@x.com".into(), kind: "human".into(),
        devices: vec![KeyGrantDto { fingerprint: "abc".into(), granted_by: "e1".into(), retired_at: Some(1) }],
        revoked: true,
    };
    let summary = member_summary(&m);
    assert!(summary.contains("no active devices"), "{summary}");
    assert!(!summary.to_lowercase().contains("removed"), "never imply removal: {summary}");
}

#[test]
fn countdown_reads_down_to_expiry_then_says_expired() {
    assert_eq!(countdown(60_000, 0), "expires in 1:00");
    assert_eq!(countdown(1_000, 0), "expires in 0:01");
    assert_eq!(countdown(0, 0), "expired");
    assert_eq!(countdown(-5_000, 0), "expired");
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --manifest-path crates/junto-iced/Cargo.toml device_line` → FAIL.
- [ ] **Step 3: Implement** the DTO mirrors, state, messages, update arms, HTTP helpers, the `identity_form` renderer, and the disclosure that replaces the party row.
- [ ] **Step 4: Run** — GUI tests → PASS; clippy on the iced manifest → clean.
- [ ] **Step 5: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): members and devices, with invite, redeem, retire and revoke"
```

---

### Task 14: ADR 0036 and the docs that must not lie

**Files:**
- Create: `docs/adr/0036-<slug>.md` — confirm 0036 is free by listing `docs/adr/` first; take the next number if not
- Modify: `docs/adr/README.md` (index row)
- Modify: `docs/domain-model.md` (device pairing, the multi-channel invite, the surface as an authorization site)
- Modify: `CLAUDE.md` — its identity/CLI section must reflect `invite --channel` repeatable and `add-member --enroll` without `--channel`

**ADR content** — write it fully, no placeholders:

- **Context.** ADR 0035 shipped the mechanism with `junto keys list` as its only read surface and no write surface; its own non-goals defer a device-management UI as "a later product question". Meanwhile the primary human surface is the native one, and it could not show a device even if it wanted to (no `/keys.json`). And because a keypair is per machine while a grant is per channel, publishing one machine key across N channels was N exchanges.
- **Decision.** (1) The invite envelope carries a **channel set** (v2), capped at 32, and one redemption grants every channel it covers, reporting **per-channel outcomes** and burning only what appended. (2) The channel set **never travels in a code**: the founder's `invites.toml` already holds one record per `(token_sha256, channel)`, so `invites::channels_for` recovers it locally by token hash. (3) The **surface becomes an authorization site** for identity: six endpoints authorizing as `WriteAuth::Human` (ADR 0021), including `POST /devices/enroll`, the first endpoint that may create secret material — localhost-only, minting only for the email its decoded invite carries, and never exposed to the mobile/remote read-only role. (4) Codes still travel by **paste**; OS deep links are deferred behind the missing distribution story.
- **Considered and rejected.** Device-level trust valid everywhere (needs a cross-channel identity concept that does not exist; one stolen device would reach channels the founder never considered). Host-to-host enrollment (dissolves the trust bootstrap the invite token exists to provide; breaks ADR 0012's localhost/no-auth posture). Echoing the channel set back in the enroll payload (puts the founder's channel names in a code that crosses machines). Accepting v1 codes alongside v2 (a shim for a window a 600-second TTL makes impossible).
- **Consequences.** Party membership and signing authority now diverge **visibly**. A lost laptop is still N retirements across N channels — named, not fixed. The CLI keeps its agent-facing role but loses `--channel` on `add-member --enroll`. `unverified` becomes visible natively, so ADR 0035's cutoff stops being invisible in the surface people use.

- [ ] **Step 1: List `docs/adr/` and confirm the number. Write the ADR**, citing the spec and this plan by path, and linking 0012/0017/0021/0033/0035.
- [ ] **Step 2: Update the ADR index row, the domain model, and `CLAUDE.md`'s CLI/identity text.**
- [ ] **Step 3: Verify** `cargo fmt --all --check` still clean (markdown only, so it should be).
- [ ] **Step 4: Commit**

```bash
git add docs/adr CLAUDE.md docs/domain-model.md
git commit -m "docs: ADR 0036 — device pairing is a surface flow over a multi-channel invite"
```

---

### Task 15: End-to-end verification, on both surfaces

**Files:**
- Modify: `crates/junto/src/web.rs` tests (the two-home end-to-end lives beside the endpoints it exercises; do not create an external `tests/` binary — this crate has no `[lib]` target)

**The one test that proves the feature, in a single run** (the pattern plan Task 12 of the enrollment plan used: two `junto_home`s on one machine standing in for two machines, via the existing `HomeGuard`/`--member-home` style overrides):

`POST /invites` for two channels → `POST /devices/enroll` against the **second** home → `POST /members` with `kind=human` → assert (a) both channels' keyrings hold the new grant, (b) an entry signed by that device's key projects **verified** in both, (c) the founder's `keys.toml` holds **no** key for the enrolled email, (d) `keys.json` shows two devices for the founder-invited email with the right fingerprints. Then `POST …/keys/{grant}/retire` on one grant and assert the other still verifies; then `POST …/members/{email}/revoke` and assert a later entry from that device is `unrecognized` while the earlier one keeps its standing.

- [ ] **Step 1: Write it. Run 3× back-to-back to prove it is not flaky.**

```bash
cargo test -p junto pairing_end_to_end -- --nocapture
cargo test -p junto pairing_end_to_end
cargo test -p junto pairing_end_to_end
```

- [ ] **Step 2: Full gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check
cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path crates/junto-iced/Cargo.toml
```

- [ ] **Step 3: Drive the GUI for real** — `cargo run -p junto -- serve`, then `cargo run --manifest-path crates/junto-iced/Cargo.toml`, and walk the whole flow on screen against a scratch channel: open members → invite a device (two channels ticked) → watch the countdown tick → paste into Settings → this device on a second `JUNTO_HOME` → redeem → read the per-channel outcome list → retire one device → revoke the member → confirm the timeline shows `unverified`/`unrecognized` badges. A GUI claim with no GUI run is not evidence; paste the observed sequence into the report.
- [ ] **Step 4: Commit**

```bash
git add crates/junto/src/web.rs
git commit -m "test: end-to-end device pairing across two homes and both surfaces"
```

---

## Self-Review (performed at write time)

- **Spec coverage:** envelope v2 → T1; `channels_for` + store → T2; CLI invite multi-channel → T3; CLI redeem + per-channel outcomes → T4; shared founder guard (spec's "reuse `require_founder` so CLI and endpoints cannot drift") → T5; `keys.json` → T6; `POST /invites` → T7; `POST /devices/enroll` → T8; `POST /members` → T9; retire/revoke endpoints → T10; `unverified` badges → T11; Settings "this device" → T12; members disclosure + invite + redeem + countdown → T13; ADR + docs → T14; two-home end-to-end + real GUI run → T15. Non-goals respected: no deep links, no QR, no device naming, no cross-channel revocation, no identity writes on the web pages, no automation of the human confirmation.
- **Type consistency:** `InvitePayload.channels` defined T1, consumed T3/T7/T8; `channels_for` defined T2, consumed T4/T9; `RedeemOutcome`/`redeem_enrollment` defined T4, consumed T9; `identity::{is_founder, fingerprint, grants_to_park}` defined T5, consumed T6/T7/T10; `KeysDto`/`KeyMemberDto`/`KeyGrantDto` defined T6, mirrored T13; `InviteMintedDto` defined T7, mirrored T13; `EnrolledDto` defined T8, mirrored T12; `RedeemedDto`/`RedeemOutcomeDto` defined T9, mirrored T13; `post_json_result` defined T12, reused T13; `EntryDto.unverified` defined T11 on both sides.
- **Sequencing:** T1→T2 independent of each other but both precede T3/T4. T5 precedes T6-T10 (they call its helpers). T4 precedes T9 (shared engine). T6/T7/T8/T9/T10 are independent of one another and can run in parallel. T11 is independent of all endpoints. T12 precedes T13 (`post_json_result`). T14/T15 last. T1 deliberately leaves `main.rs` failing to compile until T3/T4 — an executor must run T1→T4 as a block before expecting a green `cargo test -p junto`.
- **Known risks carried:** T10 appends `Park` entries on the shared write path and T11 changes a DTO every surface reads, so both name the full-workspace suite as their gate. T13 is the largest single task; if it needs splitting during execution, the seam is (a) the read-only disclosure and (b) the four act forms.
