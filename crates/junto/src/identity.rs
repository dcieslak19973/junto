//! Founder identity and key-fingerprint helpers shared by every surface
//! that needs to answer "who may grant" — the CLI's `invite`,
//! `add-member`, `revoke-member`, `retire-device`, and `keys list`
//! commands today, and the HTTP endpoints (Tasks 6-10) tomorrow. Kept in
//! one module so the CLI and the endpoints cannot drift on who counts as
//! a channel's founder — see [`is_founder`] and [`require_founder`].

use anyhow::{Result, bail};
use junto_kernel::{ChannelView, EntryId, Member, PublicKey};

/// Whether `email` is `view`'s founding member — the boolean form the
/// HTTP endpoints and `keys.json` need. `require_founder` is defined in
/// terms of this so the CLI's error path and the endpoints' predicate
/// can never disagree on the same `ChannelView`.
pub(crate) fn is_founder(view: &ChannelView, email: &str) -> bool {
    view.party
        .first()
        .is_some_and(|founder| founder.email == email)
}

/// Refuse unless `caller` is `view`'s founding member (device-key-
/// enrollment plan, Task 9) — granting membership, revoking keys, and
/// every other founder-only act share this one guard.
pub(crate) fn require_founder(view: &ChannelView, caller: &Member, channel: &str) -> Result<()> {
    let Some(founder) = view.party.first() else {
        bail!(
            "channel '{channel}' has no genesis, so it has no founding member to authorize \
             founder-only acts (membership is not enforced on pre-genesis channels)"
        );
    };
    if !is_founder(view, &caller.email) {
        bail!(
            "only the founding member ({} <{}>) may perform founder-only acts in '{channel}' \
             (docs/adr/0017)",
            founder.display_name,
            founder.email
        );
    }
    Ok(())
}

/// Every currently active grant for `email` — the set `revoke-member` parks
/// in one act (device-key-enrollment plan, Task 9). Already-retired grants
/// are skipped: parking one again would append a `Park` that
/// [`junto_kernel::KeyGrant`]'s earliest-wins fold (Task 2) makes a no-op,
/// reporting a success that changed nothing.
pub(crate) fn grants_to_park(view: &ChannelView, email: &str) -> Vec<EntryId> {
    view.keyring
        .get(email)
        .into_iter()
        .flatten()
        .filter(|grant| grant.retired_at.is_none())
        .map(|grant| grant.granted_by)
        .collect()
}

/// The warning `add-member --enroll` prints when `email` currently holds a
/// revocation cutoff (finding 2, final fix wave): at least one grant and
/// every one of them retired — the exact condition
/// [`junto_kernel::ChannelView::unrecognized`]'s cutoff fold requires
/// (`crates/junto-kernel/src/ledger.rs`'s `project_unrecognized`). `None`
/// when there is no cutoff to disturb: no grant at all, or at least one
/// still active.
///
/// Re-enrolling such a member is legitimate — `Host::add_member`
/// deliberately re-grants a previously retired key — but the fresh grant
/// it appends is unconditionally active, so it drops the cutoff and
/// restores every entry `email` wrote after it back to recognized
/// (standings, gate approvals, session and lineage folds included),
/// silently unless this warns (`docs/adr/0035`'s "Re-admitting a revoked
/// member" consequence). This only detects and names the condition; it
/// never refuses — re-admission must still go through.
pub(crate) fn revocation_cutoff_warning(
    view: &ChannelView,
    email: &str,
    channel: &str,
) -> Option<String> {
    let grants = view.keyring.get(email)?;
    if grants.is_empty() || grants.iter().any(|grant| grant.retired_at.is_none()) {
        return None;
    }
    Some(format!(
        "warning: {email} currently has a revocation cutoff in channel '{channel}' (every \
         grant they hold is retired) — re-enrolling them grants a fresh ACTIVE key, which \
         restores every entry they wrote after that cutoff (standings, gate approvals, \
         session and lineage folds) back to recognized"
    ))
}

/// A stable, 16-hex-char fingerprint for `key` (device-key-enrollment plan,
/// Task 9) — safe to print on a shared terminal, unlike the full
/// `ed25519:<64 hex>` public key. The 16 hex characters *after* the
/// prefix, not the prefix itself, so two distinct keys never collide on
/// the printed prefix.
pub(crate) fn fingerprint(key: &PublicKey) -> String {
    key.as_str()
        .strip_prefix("ed25519:")
        .unwrap_or(key.as_str())
        .chars()
        .take(16)
        .collect()
}

/// The formatted lines `junto keys list` prints, one per grant: member,
/// signing fingerprint (never the full public key), the transport
/// fingerprint labelled `transport=` (or `transport=none` for a grant made
/// before Task 16, or a keyless member) so a reader can tell which device
/// is reachable over a federation transport (`docs/adr/0033` two-key
/// separation), the granting entry id (`retire-device`'s `--grant`
/// handle), and `active` or its retirement timestamp (device-key-
/// enrollment plan, Task 9). Pure and sorted by email so it is testable
/// without capturing stdout — `keys_list` prints exactly what this
/// returns.
pub(crate) fn keys_list_lines(view: &ChannelView, member: Option<&str>) -> Vec<String> {
    let mut emails: Vec<&String> = match member {
        Some(email) => view
            .keyring
            .keys()
            .filter(|e| e.as_str() == email)
            .collect(),
        None => view.keyring.keys().collect(),
    };
    emails.sort();

    let mut lines = Vec::new();
    for email in emails {
        for grant in &view.keyring[email] {
            let status = match grant.retired_at {
                Some(ts) => format!("retired {}", crate::render::iso_utc(ts.as_millis())),
                None => "active".to_string(),
            };
            let transport = grant
                .transport_key
                .as_ref()
                .map_or_else(|| "none".to_string(), fingerprint);
            lines.push(format!(
                "{email}  {}  transport={transport}  granted_by={}  {status}",
                fingerprint(&grant.key),
                grant.granted_by
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::Timestamp;

    /// A minimal `ChannelView` carrying only `keyring` — every other field
    /// defaulted, since `grants_to_park` reads nothing else.
    fn channel_view_with_keyring(keyring: junto_kernel::Keyring) -> ChannelView {
        ChannelView {
            name: None,
            entries: Vec::new(),
            party: Vec::new(),
            keyring,
            unrecognized: std::collections::HashSet::new(),
            unverified: std::collections::HashSet::new(),
            standings: std::collections::HashMap::new(),
            gate_status: std::collections::HashMap::new(),
            gate_executions: std::collections::HashMap::new(),
            sessions: std::collections::HashMap::new(),
            closed: false,
            lineage: Vec::new(),
        }
    }

    /// A minimal `ChannelView` carrying only `party` — every other field
    /// defaulted, since `is_founder`/`require_founder` read nothing else.
    fn view_with_party(emails: &[&str]) -> ChannelView {
        ChannelView {
            name: None,
            entries: Vec::new(),
            party: emails
                .iter()
                .enumerate()
                .map(|(i, email)| Member::human(format!("M{i}"), *email))
                .collect(),
            keyring: junto_kernel::Keyring::new(),
            unrecognized: std::collections::HashSet::new(),
            unverified: std::collections::HashSet::new(),
            standings: std::collections::HashMap::new(),
            gate_status: std::collections::HashMap::new(),
            gate_executions: std::collections::HashMap::new(),
            sessions: std::collections::HashMap::new(),
            closed: false,
            lineage: Vec::new(),
        }
    }

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

    /// The only difference between the retired grant and the two active
    /// ones is `retired_at` — same key, same email, distinct `granted_by`
    /// ids — so a mutation that drops the filter (returns all three) or
    /// inverts it (returns only the retired one) fails this exact
    /// assertion. Two active grants, not one, so a mutation that narrows
    /// to `.next()`/`.last()` of the filtered iterator (satisfying "active"
    /// but not "every") also fails it.
    #[test]
    fn grants_to_park_returns_every_active_grant_when_the_email_has_a_mix() {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let active_a = EntryId::new();
        let active_b = EntryId::new();
        let retired_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: retired_id,
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: active_a,
                    retired_at: None,
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: active_b,
                    retired_at: None,
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        assert_eq!(
            grants_to_park(&view, "alice@example.com"),
            vec![active_a, active_b]
        );
    }

    #[test]
    fn grants_to_park_returns_empty_for_an_email_with_no_grants_at_all() {
        let view = channel_view_with_keyring(junto_kernel::Keyring::new());
        assert!(grants_to_park(&view, "nobody@example.com").is_empty());
    }

    /// Pinned to a literal expected string, not to calling `fingerprint`
    /// again — proves the 16 chars come from *after* the 8-char
    /// `ed25519:` prefix, not the prefix itself (which would wrongly read
    /// `ed25519:01234567`).
    #[test]
    fn fingerprint_is_the_16_hex_chars_after_the_prefix_not_the_prefix_itself() {
        let key =
            PublicKey::new(format!("ed25519:{}{}", "0123456789abcdef", "0".repeat(48))).unwrap();
        assert_eq!(fingerprint(&key), "0123456789abcdef");
    }

    #[test]
    fn fingerprint_differs_between_distinct_keys() {
        let key_a = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let key_b = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();
        assert_ne!(fingerprint(&key_a), fingerprint(&key_b));
    }

    #[test]
    fn keys_list_lines_prints_the_fingerprint_never_the_full_key() {
        let key = PublicKey::new(format!("ed25519:{}", "7".repeat(64))).unwrap();
        let grant_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key,
                transport_key: None,
                granted_by: grant_id,
                retired_at: None,
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, None);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("7777777777777777"), "{}", lines[0]);
        assert!(!lines[0].contains(&"7".repeat(64)), "{}", lines[0]);
        assert!(
            lines[0].contains(&format!("granted_by={grant_id}")),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("active"), "{}", lines[0]);
    }

    /// Pinned to the literal expected date string, like `invite_line`'s own
    /// test — not derived by calling `render::iso_utc` again.
    #[test]
    fn keys_list_lines_renders_the_retirement_timestamp_when_set() {
        let key = PublicKey::new(format!("ed25519:{}", "8".repeat(64))).unwrap();
        let grant_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "bob@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key,
                transport_key: None,
                granted_by: grant_id,
                retired_at: Some(Timestamp::from_millis(1_700_000_000_000)),
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, None);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("retired 2023-11-14 22:13 UTC"),
            "{}",
            lines[0]
        );
        assert!(!lines[0].contains("active"), "{}", lines[0]);
    }

    #[test]
    fn keys_list_lines_with_member_filters_to_exactly_that_email() {
        let key_a = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let key_b = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key: key_a,
                transport_key: None,
                granted_by: EntryId::new(),
                retired_at: None,
            }],
        );
        keyring.insert(
            "bob@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key: key_b,
                transport_key: None,
                granted_by: EntryId::new(),
                retired_at: None,
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, Some("alice@example.com"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("alice@example.com"), "{}", lines[0]);
    }

    #[test]
    fn revocation_cutoff_warning_is_none_when_the_email_has_no_grants_at_all() {
        let view = channel_view_with_keyring(junto_kernel::Keyring::new());
        assert!(revocation_cutoff_warning(&view, "nobody@example.com", "acme").is_none());
    }

    /// The mixed fixture — one retired grant, one still active — is the
    /// one that actually distinguishes "every grant retired" from "has A
    /// retired grant": a wrong `.any()`/`.all()` swap, or dropping the
    /// active-grant check entirely, would both still pass a fixture built
    /// from only-retired or only-active grants but fails this one.
    #[test]
    fn revocation_cutoff_warning_is_none_while_any_grant_is_still_active() {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "b".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: None,
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        assert!(revocation_cutoff_warning(&view, "alice@example.com", "acme").is_none());
    }

    #[test]
    fn revocation_cutoff_warning_fires_and_names_the_email_and_channel_when_every_grant_is_retired()
    {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "c".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(20)),
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        let warning = revocation_cutoff_warning(&view, "alice@example.com", "acme")
            .expect("every grant retired must produce a warning");
        assert!(warning.contains("alice@example.com"), "{warning}");
        assert!(warning.contains("acme"), "{warning}");
        assert!(warning.contains("cutoff"), "{warning}");
    }
}
