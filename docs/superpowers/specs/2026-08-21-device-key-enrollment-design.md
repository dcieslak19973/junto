# Device-key enrollment — design

**Date:** 2026-08-21
**Status:** approved in brainstorming (Dan, 2026-08-21); spec for review
**Provenance:** the key-transport gap recorded as limitation 1 in [ADR 0034](../../adr/0034-crdt-confined-to-the-live-plane.md), found by the whole-branch review of the live session plane ([PR #65](https://github.com/dcieslak19973/junto/pull/65)). Prior art: Orca's `orca environment add --pairing-code` pairing-offer envelope, read from the shipped app.

## Summary

A member may hold **more than one signing key** — one per machine — so a person can write from, and watch live sessions on, every device they use. **Secrets never move.** Each device mints its own keypair locally; only the public half travels, and it travels inside a short-lived, founder-invited enrollment code.

The record's shape is unchanged: no new entry kinds, no change to `Member`, no change to [ADR 0011](../../adr/0011-sync-is-push-fetch-plus-convergent-union-merge.md) sync. Multi-device is expressed by *multiple entries*, which is what an append-only record is for.

## Why this exists

junto's identity model is one key per email, and it is load-bearing in two places: `Member.public_key` is a single `Option<PublicKey>` (`crates/junto-kernel/src/member.rs`), and the keyring is `HashMap<email, &PublicKey>` where an entry verifies against *the* key for its author (`ledger.rs::project_unverified`). Meanwhile `keys::signing_key` **mints on first use, per identity per machine** — so the same email on a second machine silently gets a *different* keypair, and there is no export or import path anywhere.

The consequence was invisible until the live session plane shipped:

| | Second machine | Result |
|---|---|---|
| Record writes | signs with a divergent local key | entries project as `unverified` — a **flag**, not a failure (ADR 0033: verification is a projection fact, not a gate) |
| Live plane handshake | same divergent key | **rejected** — a hard wall |

The live plane converted a soft, near-invisible degradation into a hard one. That is what surfaced the gap, but the gap predates it: writing from two machines has always produced unverified entries.

## Decisions

| # | Decision | Rejected alternatives |
|---|---|---|
| 1 | The unit of signing identity is the **person, holding many device keys** | one key per person, transported (moves private key material; every copy is another leak site); device-as-its-own-member (party fills with devices, attribution becomes per-device); delegation via certificate chains (most correct, most machinery) |
| 2 | Enrollment is **founder-granted**, extending the existing `add-member` authority | self-enrolment signed by an existing device key (a single compromised device could then enroll attacker keys permanently, and the record has no delete); bare out-of-band pairing code |
| 3 | The founder **issues a one-time invite first**; the device echoes it back | unsigned claim confirmed purely out-of-band (accepts an unsolicited code arriving from nowhere) |
| 4 | **Revocation is in scope**, expressed by parking granting entries | deferring it (multi-key members make a lost device strictly harder to reason about than when one key meant one person) |
| 5 | Revocation is **member-level by default**, with per-device retirement available on the same mechanism | per-grant only (serves a lost laptop but makes offboarding N manual acts); member-level only (leaves the lost-laptop case needing a full revoke-and-re-enroll) |
| 6 | A retired grant stops verifying **as of the park's timestamp** | excluding the key outright (would retroactively un-verify legitimate history when a device is honestly retired) |
| 7 | Revoking a member also makes their **post-cutoff entries unrecognized**, so they stop counting — but the member is **not** removed from the party | retiring keys only (unverified entries still project: they carry standings, close gates, appear in sessions — so a revoked member could keep contributing entries that count); removing from the party (recognition is party-set membership, so removal marks *every* entry that author ever wrote unrecognized and erases their history from every projection) |

## The keyring becomes a projection of its own

Today `project_unverified` builds its keyring inline from the deduped party. Two projections are being conflated: *who is a member* and *which keys may sign for them*. Separate them.

- **Party** — unchanged. `project_party` keeps deduping by email, first-write-wins (`ledger.rs:378-387`), so the roster people read stays one row per person and no existing behaviour moves.
- **Keyring** — new. Walk the genesis author plus every **founder-authored** `MemberAdded` entry, unioning keys per email:

```rust
/// Which keys may sign for an email, and until when.
pub struct KeyGrant {
    pub key: PublicKey,
    /// The `MemberAdded` (or genesis) entry that granted it.
    pub granted_by: EntryId,
    /// `Some(ts)` once a founder-authored Park retires the grant: the key
    /// verifies entries stamped at or before `ts` and nothing after.
    pub retired_at: Option<Timestamp>,
}

pub type Keyring = HashMap<String, Vec<KeyGrant>>;
```

`project_unverified` then marks an entry verified when **any** grant for its author's email verifies it and is not retired as of that entry's timestamp. `ChannelView` gains a `keyring` field, because consumers currently rebuild it from `party` and must stop.

Founder-granted survives untouched: the existing `entry.author.email == founder.email` check already gates which `MemberAdded` entries count. The founder's own second device works naturally — they author a `MemberAdded` for their own email carrying the new key.

## Enrollment flow

```
founder:     junto invite --member dan@example.com --channel junto-dev
             → junto://invite?code=<base64url payload>

new device:  junto enroll --invite junto://invite?code=…
             → mints this machine's keypair (secret stays here, forever)
             → junto://enroll?code=<base64url payload>

founder:     junto add-member --enroll junto://enroll?code=…
             → validates the echoed token, authors MemberAdded, marks it consumed
```

`keys::signing_key`'s mint-on-first-use stops being the hazard the live-plane review flagged and becomes the enrollment act itself: it mints *that machine's* key, and only the public half leaves.

**The flow is identity-kind agnostic.** Nothing in `invite`/`enroll`/`add-member --enroll` distinguishes a human from an agent — the exchange only ever proves the *device* holds a keypair, never who is behind it. `--kind` on `add-member` (required, on this path too, no default) is where that distinction is made: the founder declares whether the enrolled identity is a human or an agent, the same act of judgment `add-member`'s keyless path already demands. A remote agent — one whose key cannot legitimately be minted on the founder's machine, for the identical reason a remote human's cannot — enrolls exactly like a remote human: through this same three-step exchange.

**One keypair per machine, published per channel.** `keys::signing_key` is keyed by `(junto-home, email)` and is *not* channel-scoped, so a device mints exactly one keypair for an identity no matter how many channels it writes to. The keyring, by contrast, is a **channel** projection — so that one machine key must be published into each channel where the identity is a member, via that channel's own `MemberAdded`. Enrolling a device is therefore per-channel publication of a key that already exists locally, not a new key each time. This follows from membership itself being per-channel (ADR 0017) and is called out because a reader could reasonably assume one enrollment covers everything.

### Payloads

Both are versioned JSON, base64url-encoded into a `junto://` URI — pasteable, readable aloud, QR-able.

```
invite:  { v: 1, invite_token, member_email, channel, expires_at }
enroll:  { v: 1, invite_token, email, display_name, public_key, expires_at }
```

The **enroll** payload carries nothing sensitive: a public key, an identity claim, and an expiry. Authority comes entirely from the founder's act of authoring the entry, so unlike Orca's offer — which must carry an `inviteToken` because it grants network access to a runtime — junto's enrollment code grants nothing on its own.

The **invite** payload's `invite_token` *is* a bearer secret: it proves the founder made a grant. 256 bits of entropy, rendered as 43 base64url characters (Orca's shape).

### Envelope discipline, taken from Orca rather than invented

- **Versioned** (`v: 1`) so the format can evolve.
- **Every field length-bounded**, and a **total cap enforced before parsing** — Orca rejects >132096 characters on decode and >131072 on encode.
- **10-minute TTL** with a **30-second clock-skew allowance** (`MAX_INVITE_TTL_MS = 600_000`, `INVITE_EXPIRY_CLOCK_SKEW_MS = 30_000`). Expiry is validated as *must be in the future and no further away than the TTL*.

Taking Orca's constants rather than picking new ones: they are battle-tested, and the skew allowance is the same lesson this codebase learned twice already (presence expiry in `junto-live`, and the live plane's cross-machine clock sensitivity recorded in ADR 0034).

### Invite store

`<junto-home>/invites.toml`, mirroring `members.toml` and `keys.toml`: machine-local, never in the ledger, because the token is both secret and ephemeral.

Records `{ token_sha256, member_email, channel, expires_at, consumed_at }`. **Store the hash, not the token** — the founder only ever needs to *compare*, so a leaked store yields no usable invites. This is one hardening beyond Orca's shape. Consumed tokens are retained (marked) so replay is refused rather than silently re-granting.

## Revocation

A founder-authored `Park` targeting a `MemberAdded` entry retires the key that entry granted. This needs **no new entry kind** — park already means "this no longer stands", and ADR 0033 already treats verification as a projection fact.

One mechanism, two surfaces (decision 5):

| Act | Surface | Effect |
|---|---|---|
| **Revoke a member** (primary) | `junto revoke-member --member <email> --channel <name>` | parks **every** grant for that email — offboarding, or a compromised person |
| **Retire one device** | `junto retire-device --grant <entry-id>` | parks a single grant — a lost or decommissioned machine |

Revoking a member is the common case and must be one act, not N. Retiring a device exists because with multiple keys per member, losing one machine should not require revoking the person and re-enrolling everything else. `junto keys list --member <email>` names grants so a specific one can be retired; without it the per-device act has no usable handle.

**Semantics (decision 6):** a retired grant verifies entries stamped **at or before** the park's timestamp, and nothing after. Retiring a device therefore does not rewrite history; the entries it legitimately signed stay verified.

The harsher semantics — *distrust everything this key ever signed*, which is what a confirmed key compromise wants — is deliberately **not** in this design. It is a different act with different consequences and deserves its own decision rather than being conflated with retirement.

Only founder-authored parks count, matching the grant rule.

### Revoking a member stops their future contributions counting (decision 7)

Retiring keys alone is **not** sufficient for offboarding. Recognition today is party-set membership: `Ledger::project` marks an entry `unrecognized` iff its author's email is absent from the party, and the comment at `ledger.rs:290-292` states the rule is deliberately *"set-based, not temporal (`docs/adr/0017`)"*. Unverified entries are still **recognized**, so they still project — they carry standings, close gates, and appear in sessions and lineage. A member whose keys were all retired could therefore keep contributing entries that count, merely flagged.

So a member revocation carries a **cutoff timestamp**, and recognition becomes temporal *for revoked members only*: an entry from a revoked member is recognized iff it is stamped at or before the cutoff. Everything before the cutoff is untouched; nothing after it counts. This is the same shape as the key-retirement rule above, which is why the two compose into one concept — one revocation, one timestamp, governing both verification and recognition.

**This amends ADR 0017's set-based membership rule** and must be recorded as such rather than slipped in as a projection change. The amendment is narrow: membership stays set-based for everyone who has not been revoked.

### What revocation deliberately does NOT do

It does not remove the member from the party projection. That was considered and rejected on evidence: because recognition is party-set membership, dropping an email from the party marks **every** entry that author ever wrote as `unrecognized`, and `Ledger::project` feeds only recognized entries to standings, gates, gate executions, sessions, lineage, and genesis/name resolution. Removal would therefore erase a person's entire contribution history from the channel view — assertions lose their standing, ratifications they gave stop counting, sessions they ran disappear. A revoked member stays visible in the party, with no valid keys and no post-cutoff contributions, which is the honest record of what happened.

## Consumers

- **`crates/junto/src/live_ws.rs:83-92`** builds its handshake keyring from `view.party` public keys; it switches to the projected keyring so any enrolled device authenticates.
- **The handshake's failure message** learns to distinguish *your email is not in this channel's party* from *your email is, but this device's key is not enrolled* — and in the second case names the enrollment command. The gate stays hard; the fix is enrollment, not loosening it. The current message ("signature does not verify against the key on file") is what made this gap hard to diagnose in the first place.
- **`crates/junto/src/live_bridge.rs`** uses `keys::has_signing_key` to decide whether it may author as a remote watcher. Unchanged in behaviour, but worth re-reading once a member may legitimately have a key here *and* elsewhere.

## Consequences

- **Enrolling a key retroactively verifies that device's past entries.** If you have been writing from a second machine, those entries are `unverified` today and flip to verified on enrolment. Correct — the key genuinely was yours — but adding a key changes the standing of history, which is worth stating plainly.
- **Existing channels are unaffected.** A member with one key is a keyring of one; no migration, no re-signing, and every existing entry deserializes byte-identically.
- **The party stays human-readable.** Devices never appear in it.
- **A second data structure now derives from `MemberAdded` entries.** The party and the keyring must not drift apart; they are tested together.
- **Signing authority and party membership diverge.** After a revoke, a member is still in the party, with no valid keys and nothing counted after the cutoff. Every surface that reads one should be checked against the other — a UI listing "the party" now means "people who were admitted", not "people who can currently write".
- **Recognition is no longer purely set-based.** Decision 7 makes it temporal for revoked members, amending ADR 0017's stated rule. The amendment must be recorded in an ADR; it is narrow (membership stays set-based for everyone not revoked) but it is a change to a documented invariant, and `ledger.rs:290-292` currently asserts the opposite in a comment that must be corrected with it.

## Non-goals

- Retroactive distrust of a compromised key (see Revocation).
- Removing a member from the party projection — rejected on evidence, not deferred: recognition is party-set membership, so removal would erase that author's entire history from every projection.
- Any transport of private key material, by any path, ever.
- Device naming or a management UI beyond `junto keys list`. `invites.toml` and the keyring are inspectable; a device manager is a later product question.
- Delegation or certificate chains (decision 1's rejected alternative).
- Automating the out-of-band step. The founder confirming "yes, that code came from Dan" is the trust anchor by design.

## Testing

- **Kernel:** keyring unions multiple grants per email; an entry verifies against any non-retired grant; a non-founder-authored `MemberAdded` contributes no key; a retired grant stops verifying after the park's timestamp and still verifies before it; revoking a member retires every grant for that email in one act; a revoked member's post-cutoff entries are `unrecognized` while their pre-cutoff entries stay recognized AND keep their standings/gates/sessions/lineage contributions; an unrevoked member's recognition is unchanged (still set-based); party projection is byte-for-byte unchanged by the new keyring code, including for a revoked member (they remain in it).
- **Envelope:** round-trip both payloads; reject over-long input *before* parsing; reject an expired `expires_at`; accept one inside the skew allowance; reject a bad version.
- **Invite store:** single-use enforcement; replay of a consumed token refused; the token itself never written to disk (assert the file contains no preimage).
- **Flow:** end-to-end invite → enroll → add-member producing a `MemberAdded` whose key verifies a subsequent entry from the new device.
- **Live plane:** a second enrolled key authenticates; an unenrolled key is refused *with the diagnostic message that names enrollment*; a revoked member's key is refused.
