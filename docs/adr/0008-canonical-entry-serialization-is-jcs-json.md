# A LedgerEntry's canonical form is JCS (RFC 8785) JSON

Status: accepted (Dan, 2026-06-09) · builds on [`0001`](0001-ledger-is-the-durable-record.md), [`0002`](0002-ledger-entries-are-immutable.md), [`0003`](0003-ledger-entry-content-model.md), [`0005`](0005-provenance-ref-uri-plus-digest.md)

A [`LedgerEntry`] serializes to a **canonical byte form** — **Canonical JSON per [JCS / RFC 8785](https://www.rfc-editor.org/rfc/rfc8785.html)**, UTF-8. The durable record lives in git refs (`refs/junto/*`) and will eventually be content-addressed, so the bytes must be **deterministic and cross-platform-stable**: the same entry written on Windows and macOS must produce identical bytes, or dedup / ordering / hashing silently break. The format lives in one place — `LedgerEntry::to_canonical_bytes` / `from_canonical_bytes` (the `serial` module) — so the future git-refs substrate never re-derives it.

JCS keeps plain JSON's virtues (readable, greppable, `git show`-diffable — the human+agent-navigable record) and adds determinism **by spec**: object keys sorted by code-unit order, all insignificant whitespace removed.

## Considered: plain compact `serde_json`

Rejected. `serde_json` *is* deterministic for our types today — but only because structs serialize in field-declaration order and the entry graph contains no maps. That makes determinism hostage to an implementation detail: **reordering a struct field would silently change every entry's bytes** (and, once we content-address, every hash). JCS sorts keys, so a harmless field reorder can't alter the record's identity, and the bytes are reproducible from a *spec* across languages. The cost — one small dependency — is worth that robustness for a durable, soon-to-be-hashed record.

## Considered: binary (CBOR / MessagePack) and other text formats

- **Binary** is smaller and also deterministic, but **opaque** — not readable via `git show`, not diffable. That cuts directly against junto's "navigable record" value, so it loses despite the size win.
- **TOML / YAML / RON** offer no canonical guarantee (YAML is whitespace-significant and footgun-laden; RON is Rust-only and not greppable by other tools). None is designed as a deterministic wire form.

## Consequences

- **The one JCS gotcha does not bite us.** RFC 8785's tricky rule is IEEE-754 float normalization; the entry graph contains **only integers** (`i64` timestamp, `u32` count), so it is unaffected. *If a future field needs a non-double-representable number, store it as a string* (per the RFC's own guidance) rather than a JSON number.
- **Validated newtypes re-validate on the way in.** `Uri` / `ContentDigest` ([`0005`](0005-provenance-ref-uri-plus-digest.md)) deserialize through their checking constructors (`#[serde(try_from = "String")]`), so a malformed value cannot enter the kernel through the canonical-bytes boundary.
- **Enums are externally tagged** (`{"Assertion":{…}}`) — the only serde repr that works for every variant shape incl. tuple variants (`ApprovalRequirement::Count(u32)`). Adding a kind later still deserializes old data (consistent with the closed-but-extensible set in [`0003`](0003-ledger-entry-content-model.md)).
- Compact JCS contains **no raw newline bytes**, which sidesteps the CRLF-vs-LF hazard CLAUDE.md flags for the cross-platform ledger; a CRLF inside a string field is JSON-escaped to `\r\n`, not emitted raw.
- The kernel's public `Error` carries only a `Serialization(String)` message, **not** a concrete serializer/parser type, so the error API stays independent of the record format.

## Prior art (clean-room inspiration only)

RFC 8785 (JCS); the `serde_json_canonicalizer` crate (MIT — the RFC-compliant successor to the unmaintained `serde_jcs`); JSON Web Signature canonicalization; Subresource Integrity and OCI `@sha256:` digests for the content-addressing this enables.
