//! The machine-local single-use invite store (device-key-enrollment plan,
//! Task 5).
//!
//! Mirrors `members.rs`'s shape: machine-local, per-identity, a `.toml` file
//! beside the member codes and signing keys in `<junto-home>`. Where it
//! differs is deliberate — an invite's [`enroll::InvitePayload::invite_token`]
//! (`crate::enroll`) IS a bearer secret: presenting it proves the founder
//! made the grant it names. So unlike `members.rs`'s plaintext codes (an
//! accident-proofing device, not a security boundary — see that module's
//! docs), this store never holds the token itself, only `sha256(token)`.
//! Hashing costs nothing here: the store only ever needs to check "does a
//! presented token match a stored one", which a hash answers as well as the
//! plaintext would, without the plaintext ever touching disk.
//!
//! `consume` is the single-use gate: a token redeems a grant at most once,
//! and must distinguish *why* a redemption failed — `Unknown` (no such
//! token), `AlreadyUsed`, `Expired`, `WrongMember` (the enroll payload's
//! email does not match the invite's, so a leaked invite cannot enroll
//! someone other than its intended member), or `WrongChannel` (checked
//! independently of `WrongMember`, and reported second when both are wrong
//! — identity is the more fundamental error) — because Task 8 surfaces this
//! diagnosis to a human, not just a pass/fail.
//!
//! One token may cover several channels, and `channels_for` is the read that
//! recovers them: all channels the token still covers (unconsumed and unexpired).
//!
//! `consume`'s single-use guarantee is a plain read-modify-write over
//! `invites.toml` (`load` → check → mutate → `save`, the same shape as
//! `members.rs::mint`) — it holds against **sequential** redemption
//! attempts within one process, not against two processes racing the same
//! token concurrently, and a crash mid-`save` can leave the file
//! unparseable. `members.rs` makes the identical trade for the identical
//! reason: this host's threat model (`docs/adr/0017`) is one machine, one
//! OS user, one founder at a terminal — not concurrent writers — so there
//! is no file-locking convention in this crate to reuse, and adding one
//! here alone would be scope this task doesn't need.
//!
//! `junto invite` (Task 6) calls `issue`, `junto add-member --enroll`
//! (Task 8) calls `consume`, and `junto invite` itself calls `prune` at
//! its own top (final fix wave, finding 1 — `prune` fell between Task 9's
//! brief, which shipped `keys list`/`revoke-member`/`retire-device`
//! instead, and Task 6's, written before `prune` existed). `channels_for`
//! (Task 2) carries `#[allow(dead_code)]` because it is called from Task 4
//! (the UI layer).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// A record expired more than this long ago is prunable — long enough that
/// a just-expired invite is still visible for a moment (e.g. in diagnostics)
/// before `prune` reclaims it.
const PRUNE_AFTER_EXPIRY_MS: i64 = 24 * 60 * 60 * 1000;

/// One issued invite, as stored: everything needed to check a presented
/// token except the token itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InviteRecord {
    token_sha256: String,
    member_email: String,
    channel: String,
    expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consumed_at: Option<i64>,
}

/// The serialized shape of `<junto-home>/invites.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct InvitesFile {
    #[serde(default)]
    invites: Vec<InviteRecord>,
}

/// The outcome of [`consume`]ing a presented token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumed {
    /// The token matched an unexpired, unused invite for `member_email`; it
    /// is now marked used and cannot redeem again.
    Ok,
    /// No invite on file hashes to this token.
    Unknown,
    /// The invite matched but was already consumed by an earlier call.
    AlreadyUsed,
    /// The invite matched but its `expires_at` has passed.
    Expired,
    /// The invite matched but was issued for a different member's email.
    WrongMember,
    /// The invite matched (and the member is correct) but was issued for a
    /// different channel — checked independently of `WrongMember` (see
    /// `consume`) so a misrouted redemption gets the diagnosis that
    /// actually explains it, instead of pointing at identity.
    WrongChannel,
}

fn invites_path(junto_home: &Path) -> PathBuf {
    junto_home.join("invites.toml")
}

fn token_sha256(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn load(junto_home: &Path) -> Result<InvitesFile> {
    let path = invites_path(junto_home);
    if !path.exists() {
        return Ok(InvitesFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn save(junto_home: &Path, file: &InvitesFile) -> Result<()> {
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        invites_path(junto_home),
        toml::to_string_pretty(file).context("serializing invite store")?,
    )
    .with_context(|| format!("writing {}", invites_path(junto_home).display()))
}

/// Record an issued invite: `sha256(token)` and its grant, never `token`
/// itself.
///
/// # Errors
/// Returns an error if `<junto-home>/invites.toml` cannot be read or
/// written.
pub fn issue(
    junto_home: &Path,
    token: &str,
    member_email: &str,
    channel: &str,
    expires_at: i64,
) -> Result<()> {
    let mut file = load(junto_home)?;
    file.invites.push(InviteRecord {
        token_sha256: token_sha256(token),
        member_email: member_email.to_string(),
        channel: channel.to_string(),
        expires_at,
        consumed_at: None,
    });
    save(junto_home, &file)
}

/// Redeem `token` for `member_email`/`channel` — the single-use gate. A
/// second call for the same token, once [`Consumed::Ok`], returns
/// [`Consumed::AlreadyUsed`] rather than redeeming again.
///
/// # Errors
/// Returns an error if `<junto-home>/invites.toml` cannot be read or (on a
/// successful redemption) written.
pub fn consume(
    junto_home: &Path,
    token: &str,
    member_email: &str,
    channel: &str,
) -> Result<Consumed> {
    let mut file = load(junto_home)?;
    let hash = token_sha256(token);

    // Collect all records with matching token hash.
    let mut matching_hash: Vec<_> = file
        .invites
        .iter_mut()
        .filter(|record| record.token_sha256 == hash)
        .collect();

    if matching_hash.is_empty() {
        return Ok(Consumed::Unknown);
    }

    // Check if ANY matching-hash record has the right member email.
    // Identity is the more fundamental error: if both member and channel are
    // wrong, the human needs to see WrongMember, not WrongChannel.
    if !matching_hash.iter().any(|r| r.member_email == member_email) {
        return Ok(Consumed::WrongMember);
    }

    // Find the record for the specific channel.
    let Some(record) = matching_hash.iter_mut().find(|r| r.channel == channel) else {
        return Ok(Consumed::WrongChannel);
    };

    // Check the state of THIS specific record.
    if record.consumed_at.is_some() {
        return Ok(Consumed::AlreadyUsed);
    }
    if now_ms() > record.expires_at {
        return Ok(Consumed::Expired);
    }

    // Mark this record consumed.
    record.consumed_at = Some(now_ms());
    save(junto_home, &file)?;
    Ok(Consumed::Ok)
}

/// Drop records expired more than [`PRUNE_AFTER_EXPIRY_MS`] ago, so
/// `invites.toml` cannot grow without bound. Returns how many were removed.
///
/// # Errors
/// Returns an error if `<junto-home>/invites.toml` cannot be read or (when
/// anything is pruned) written.
pub fn prune(junto_home: &Path) -> Result<usize> {
    let mut file = load(junto_home)?;
    let cutoff = now_ms() - PRUNE_AFTER_EXPIRY_MS;
    let before = file.invites.len();
    file.invites.retain(|record| record.expires_at >= cutoff);
    let removed = before - file.invites.len();
    if removed > 0 {
        save(junto_home, &file)?;
    }
    Ok(removed)
}

/// Recover the channels this token still covers — all records whose hash
/// matches, `consumed_at` is `None`, and `expires_at >= now_ms()`, in file
/// order (which is issue order). An unknown token yields an empty vec, not an
/// error. This is a read: it does not consume, write, or prune anything.
///
/// # Errors
/// Returns an error if `<junto-home>/invites.toml` cannot be read or parsed.
/// Called from Task 4 (the UI layer); alive but not yet used internally.
#[allow(dead_code)]
pub fn channels_for(junto_home: &Path, token: &str) -> Result<Vec<String>> {
    let file = load(junto_home)?;
    let hash = token_sha256(token);
    let current_time = now_ms();
    let channels = file
        .invites
        .iter()
        .filter(|record| {
            record.token_sha256 == hash
                && record.consumed_at.is_none()
                && record.expires_at >= current_time
        })
        .map(|record| record.channel.clone())
        .collect();
    Ok(channels)
}

fn now_ms() -> i64 {
    junto_kernel::Timestamp::now().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future() -> i64 {
        now_ms() + 60_000
    }

    fn past() -> i64 {
        now_ms() - 60_000
    }

    #[test]
    fn an_issued_invite_consumes_once_then_refuses() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Ok
        ));
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(),
            Consumed::AlreadyUsed
        ));
    }

    #[test]
    fn the_token_is_never_written_to_disk() {
        // The security property of hashing: assert the file does NOT contain the
        // token, and DOES contain something (the hash).
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
        let stored = std::fs::read_to_string(home.path().join("invites.toml")).unwrap();
        assert!(!stored.contains(&token));
        assert!(stored.contains(&token_sha256(&token)));
    }

    #[test]
    fn an_invite_cannot_enroll_a_different_member() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
        assert!(matches!(
            consume(home.path(), &token, "someone-else@x.com", "junto-dev").unwrap(),
            Consumed::WrongMember
        ));
        // Refused, not silently consumed — the rightful member can still redeem it.
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Ok
        ));
    }

    #[test]
    fn an_invite_cannot_redeem_into_a_different_channel() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "some-other-channel").unwrap(),
            Consumed::WrongChannel
        ));
        // Refused, not silently consumed — the right channel can still redeem it.
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Ok
        ));
    }

    #[test]
    fn a_wrong_member_and_channel_both_report_wrong_member_first() {
        // Identity is the more fundamental error: when both the presented
        // member and channel are wrong, the diagnosis is WrongMember, not
        // WrongChannel — pins the check order, not just that both exist.
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", future()).unwrap();
        assert!(matches!(
            consume(
                home.path(),
                &token,
                "someone-else@x.com",
                "some-other-channel"
            )
            .unwrap(),
            Consumed::WrongMember
        ));
    }

    #[test]
    fn an_unknown_token_is_refused() {
        let home = tempfile::tempdir().unwrap();
        assert!(matches!(
            consume(home.path(), &"z".repeat(43), "dan@x.com", "junto-dev").unwrap(),
            Consumed::Unknown
        ));
    }

    #[test]
    fn an_expired_invite_is_refused_even_if_unconsumed() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "junto-dev", past()).unwrap();
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Expired
        ));
    }

    #[test]
    fn prune_drops_only_long_expired_records() {
        let home = tempfile::tempdir().unwrap();
        let stale = "s".repeat(43);
        let fresh = "f".repeat(43);
        issue(
            home.path(),
            &stale,
            "dan@x.com",
            "junto-dev",
            now_ms() - PRUNE_AFTER_EXPIRY_MS - 1_000,
        )
        .unwrap();
        issue(home.path(), &fresh, "dan@x.com", "junto-dev", future()).unwrap();

        let removed = prune(home.path()).unwrap();
        assert_eq!(removed, 1);

        assert!(matches!(
            consume(home.path(), &stale, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Unknown
        ));
        assert!(matches!(
            consume(home.path(), &fresh, "dan@x.com", "junto-dev").unwrap(),
            Consumed::Ok
        ));
    }

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
        assert_eq!(
            channels_for(home.path(), &token).unwrap(),
            vec!["chan-b".to_string()]
        );
    }

    #[test]
    fn channels_for_omits_an_expired_channel() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "stale", past()).unwrap();
        issue(home.path(), &token, "dan@x.com", "live", future()).unwrap();
        assert_eq!(
            channels_for(home.path(), &token).unwrap(),
            vec!["live".to_string()]
        );
    }

    #[test]
    fn channels_for_an_unknown_token_is_empty_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            channels_for(home.path(), &"z".repeat(43))
                .unwrap()
                .is_empty()
        );
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

    #[test]
    fn consume_redeems_a_later_channel_first() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "chan-a", future()).unwrap();
        issue(home.path(), &token, "dan@x.com", "chan-b", future()).unwrap();
        // Redeem "chan-b" FIRST to test that the second record is found and redeemed.
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "chan-b").unwrap(),
            Consumed::Ok
        ));
        // Then redeem "chan-a".
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "chan-a").unwrap(),
            Consumed::Ok
        ));
    }

    #[test]
    fn consume_reports_already_used_per_channel_not_per_token() {
        let home = tempfile::tempdir().unwrap();
        let token = "t".repeat(43);
        issue(home.path(), &token, "dan@x.com", "chan-a", future()).unwrap();
        issue(home.path(), &token, "dan@x.com", "chan-b", future()).unwrap();
        // Consume "chan-a" twice.
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "chan-a").unwrap(),
            Consumed::Ok
        ));
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "chan-a").unwrap(),
            Consumed::AlreadyUsed
        ));
        // But "chan-b" can still be redeemed; the token is not burned for all channels.
        assert!(matches!(
            consume(home.path(), &token, "dan@x.com", "chan-b").unwrap(),
            Consumed::Ok
        ));
    }
}
