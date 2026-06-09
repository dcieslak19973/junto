# A ProvenanceRef is a URI plus an optional content digest

Status: accepted, working (Dan, 2026-06-09) · refines `provenance` from [`0003`](0003-ledger-entry-content-model.md)

An assertion's `provenance` is a list of **`ProvenanceRef { uri, digest: Option<ContentDigest> }`**. Each ref is a **URI** (*where* the input is — git object / **Artifact** / external dataset; aligned with **W3C PROV** IRIs) plus an **optional content digest captured at record time**.

## Why the digest

It makes the ref **drift-detectable / tamper-evident** — re-hash the target later and compare to catch *stale as-of data*, delivering re-runnability even when the URI points at a **mutable** target.

- **Optional:** omit when the URI is already content-addressed (a git oid is its own integrity) or the content isn't hashable.
- **Self-describing / algorithm-agile:** store `sha256:…` (algorithm embedded, multihash-style), not a bare hash — records are long-lived (retention) and algorithms get deprecated.

## Considered / kept open

- Kept as its **own type**, not a bare `String`, so it can later become a typed enum (`Artifact(ArtifactId) | GitObject(Oid) | Session(SessionId) | External { uri, digest }`) without churning the entry API — the digest naturally lives on the mutable (`External`) variant.
- ⚠️ This is still the *pointer*, **not** the full re-runnable provenance (command + commit + data-as-of + seed + env) — deferred until Artifacts exist. Whether junto also **archives** the referenced bytes (a content store) is a later Artifact-store question. Honest limit: re-fetch may be impossible for ephemeral sources, but the digest still records *what we saw*.

## Prior art (clean-room inspiration only)

Subresource Integrity; lockfile checksums; SLSA / in-toto; OCI `@sha256:` pinning; Nix fixed-output derivations.
