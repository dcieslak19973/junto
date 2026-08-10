# Entries are signed over their canonical bytes; verification is a projection fact

Status: proposed (Dan, 2026-08-10) · builds on [`0004`](0004-any-member-may-author-any-entry.md), [`0008`](0008-canonical-entry-serialization-is-jcs-json.md), [`0009`](0009-git-refs-substrate-ndjson-per-author.md), [`0010`](0010-canonical-order-and-dedup-by-entry-id.md), [`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md), [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md), [`0021`](0021-member-codes-guard-agent-surfaces-only.md)

junto's identity story stops at the OS boundary. On the human surface the host *derives* the author from git config and checks membership only ([`0021`](0021-member-codes-guard-agent-surfaces-only.md)); on MCP the author is *claimed* and the member code is accident-proofing, not security ([`0012`](0012-mcp-over-http-is-the-first-write-surface.md), [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md)). That is defensible while the trust domain is one machine ([`0015`](0015-one-host-per-machine-serving-many-substrates.md)): forgery is out of scope because there is nobody to forge against.

**[`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md) sync ends that.** The moment a channel's record crosses a forge to a team, the author field inside an entry is an unauthenticated string:

- **Per-author refs partition writes; they do not authenticate them.** `refs/junto/<channel>/<author>` prevents *collisions*, not *impersonation* — anyone with push access to the repo can write into any author's ref namespace unless the forge enforces per-ref ACLs, which is not a capability we can assume ([`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md) already defers the whole `Capabilities` question).
- **Nothing inside the entry proves authorship after the fact.** The `ProvenanceRef` digest ([`0005`](0005-provenance-ref-uri-plus-digest.md)) gives tamper-evidence on *inputs*, not on *who wrote the entry*.
- This lands hardest exactly where junto is strongest: an `Approval` ([`0006`](0006-gate-engine-event-sourced.md)) is the entry whose author *is* the authority claim, and the autonomy envelope's no-self-widening guard ([`0026`](0026-routing-policy-resolves-to-auto-or-gate-the-autonomy-envelope.md)) is only as good as "this approval really came from the human."

## Decision — sign the canonical bytes; keep authority where it is

**1. An entry carries an optional detached signature over its own canonical bytes.**

`LedgerEntry` gains `signature: Option<Signature>` (`#[serde(skip_serializing_if = "Option::is_none")]`, the omitted-when-absent pattern [`0019`](0019-decision-frames-on-subject-entries.md) established for `frame`). The **preimage is the entry's canonical bytes with `signature` absent** — which is byte-identical to what [`0008`](0008-canonical-entry-serialization-is-jcs-json.md) produces today. `to_canonical_bytes` therefore keeps its meaning, and a `signing_bytes()` accessor names the preimage explicitly so no caller re-derives it.

This is the cheap part, and it is why this ADR is worth proposing now rather than at sync time: **the preimage already exists.** [`0008`](0008-canonical-entry-serialization-is-jcs-json.md) gives a deterministic, cross-platform, spec-defined byte string per entry; a signature scheme needs exactly that and nothing else. Signing is *additive* — it does not touch the ledger model, the fold rules, the gate engine, or the substrate.

**2. Signing is Ed25519 over a per-member keypair; the public key enters the record, the secret never does.**

The secret key lives machine-locally beside the member codes (`~/.junto`, [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md)) and, like them, is **never a ledger entry**. The **public** key is published *in* the record: `MemberAdded` carries the member's public key, so the party projection ([`0017`](0017-party-is-a-projection-membership-is-founder-granted.md)) becomes the keyring. The founder's own key is established by the `ChannelOpened` genesis entry ([`0016`](0016-channel-lifecycle-acts-are-ledger-entries.md)) — trust-on-first-use, anchored at the one entry that defines the channel.

Membership is already founder-granted and already projects from the record. Attaching a key to that grant adds no new distribution mechanism, no directory, no relay.

**3. Verification is a projection fact — never a drop, never a gate.**

`ChannelView` gains `unverified: HashSet<EntryId>`, sitting exactly beside `unrecognized` ([`0017`](0017-party-is-a-projection-membership-is-founder-granted.md)) and read the same way: an entry whose signature is absent, malformed, or does not verify against the author's projected public key **still projects** and is **surfaced**, never silently dropped. Legacy unsigned entries are `unverified` and keep working.

Crucially this **does not move authority into authorship**. [`0004`](0004-any-member-may-author-any-entry.md) stands unchanged: any Member may still author any entry kind, and ratification remains a Gate/Verifier concern. Signing answers "did this Member write this?", not "may they?" — verification, not prevention, which is [`0004`](0004-any-member-may-author-any-entry.md)'s own lab-notebook framing carried one step further.

**4. Signatures do not secure ref history; the fetch path must.**

A per-entry signature is tamper-evidence on *entries*. It says nothing about a ref that was **deleted or rolled back** by anyone with push access — the record can be truncated without forging anything. [`0011`](0011-sync-is-push-fetch-plus-convergent-union-merge.md)'s reconciliation already refuses to move backward (*keep ours* when we are ahead), so the local guarantee holds; what this ADR adds is that a fetched ref which is **neither equal, ahead, nor a true divergence** — i.e. it *lost* entries we have seen — is a reportable anomaly rather than a silent union. Union-merge makes the data recoverable; the anomaly should still surface.

## Scope / degradations

- **Optional, not required.** Unsigned entries remain valid; no flag day, no migration of existing refs. A "require signatures" posture is a future channel-level policy, not kernel behavior.
- **The MCP surface is where this actually bites.** There the author is claimed ([`0012`](0012-mcp-over-http-is-the-first-write-surface.md)) — an agent signing with its own key is a real upgrade over the member code. On the human surface it remains near-ceremonial until sync crosses a machine boundary, which is the honest reason this is *proposed* and not *urgent*.
- **Key rotation, revocation, and archival are deferred.** Buzz's NIP-IA handles retiring stale pubkeys; we have no equivalent and do not need one until a second machine exists. Recorded as the known gap so a later ADR inherits it rather than rediscovering it.
- **Delegation (owner-signs-agent-key) is deliberately not adopted.** Buzz's NIP-OA two-signature scheme scopes a grant from an owner key to an agent key. It is a good design and it is *out of scope here*: junto's answer to "what may an agent do" is the autonomy envelope ([`0026`](0026-routing-policy-resolves-to-auto-or-gate-the-autonomy-envelope.md)), which is policy in the record rather than conditions in a credential. Adding a second signature later is compatible with this one — an owner attestation would sign the same preimage machinery — but conflating the two now would smuggle authority back into authorship, undoing [`0004`](0004-any-member-may-author-any-entry.md).
- **No formal verification.** Buzz ships a Tamarin proof of its multi-tenant auth model; we do not, and a scheme this small (one signature, one preimage, keyring from the party projection) does not earn one yet.

## Consequences

- The `id` stays a random UUID ([`0010`](0010-canonical-order-and-dedup-by-entry-id.md)); this ADR does not make entries content-addressed. It does, however, make the *second* half of that eventual move free — a content-addressed id and a signature share the same preimage, so whichever lands first pays the cost for both.
- Dedup-by-id ([`0010`](0010-canonical-order-and-dedup-by-entry-id.md)) now has a sharper failure mode worth naming: two *different* entries reusing one id currently resolve first-in-canonical-order-wins silently, and a signature makes that detectable (the losing entry verifies too), even though the resolution rule is unchanged.
- Canonical order and the union-merge are untouched: signatures ride inside the entry JSON, so a union-merge still rewrites lines in canonical order without touching signed bytes, and the merge commit stays deterministic.
- One new dependency (an Ed25519 implementation) in the kernel, whose error surface folds into the existing `Error` without leaking a crypto type — the same discipline [`0008`](0008-canonical-entry-serialization-is-jcs-json.md) applied to the serializer.

## Prior art (clean-room inspiration only)

Block's **Buzz** (`block/buzz`) — every event carries a BIP-340 Schnorr signature the relay verifies; NIP-OA owner attestation, NIP-AA agent authentication, NIP-AE owner-readable agent memory, NIP-IA identity archival. Buzz solved cryptographic authorship and delegation and lists workflow approvals as unfinished; junto solved authority ([`0006`](0006-gate-engine-event-sourced.md), [`0019`](0019-decision-frames-on-subject-entries.md), [`0026`](0026-routing-policy-resolves-to-auto-or-gate-the-autonomy-envelope.md), [`0029`](0029-approved-gates-execute-via-an-app-level-reaction.md), [`0030`](0030-surface-approved-but-unexecuted-actionable-gates.md)) and punted on authorship. This ADR takes the half junto is missing without importing the half it already has. Also: JSON Web Signature's canonicalize-then-sign shape; git's own detached commit signatures (`gpgsig`), which likewise sign a canonical object form and leave policy to the reader; Sigstore/in-toto attestations for the signature-as-evidence-not-enforcement posture.
