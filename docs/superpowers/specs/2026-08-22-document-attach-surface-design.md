# Document attach surface, and the debts of PR #69

Design for the five-item plan handed off in junto entry `69e27b99`
(channel *Document attach surface 20260822*, `6cd0cbb8`), diverged from
*Multiplayer-first rethink 20260821* (`3c38ead9`). Baseline: `fd4fe89`,
596 tests green.

Decisions settled with Dan are recorded in the channel as `c9238d39`
(D1–D6, E1–E2); the permanence finding that shaped them is `2c0cd09f`.
This document is the design those entries decided, written down once so
the plan has something to be a plan *of*.

## Why this plan

ADR 0037 shipped `SubjectKind::{Repo, Document}` and a
`Host::attach_subject` that accepts any kind. Every production caller
derives `Repo` through `repo_subject_uri`, which bails on anything that
is not a git checkout (`crates/junto/src/web.rs:733`). So *"a channel
can be about a document"* is true in the kernel, tested, rendered by
four surfaces — and reachable by nobody. The PR #69 dogfood had to add
a temporary in-crate probe to exercise the case the whole plan existed
for.

`SubjectDetached` is worse than unreachable: it is defined, folded
order-insensitively by `Ledger::project_subjects`, rendered on four
surfaces, tested end to end, and **nothing can emit one**. An attached
Subject cannot be withdrawn, while the only attach surface is a
free-text path field. Of the five shipped limits this plan closes, that
is the only one that *corrupts* rather than *constrains*.

## Ground truth at `fd4fe89`

| # | Item | Verified |
|---|------|----------|
| 1 | No Document attach surface | `host.rs:970` takes any kind; only caller `web.rs:905-934` hard-derives Repo; `web.rs:733` bails on non-repos |
| 2 | No `detach_subject` | Absent; `host.rs:955` says so in its own doc comment |
| 3 | Scratch pin below the refusal | Refusal `web.rs:1134-1142`; pin `web.rs:1160-1168` |
| 4 | ADR 0038 overstates re-resolution | Paragraph intact, unamended |
| 5 | `remember_mount` doc describes pre-reorder order | `mounts.rs:88` vs. the explicit contradiction at `web.rs:917-924` |

Item 3's hoist is safe as described: `executable_mount` (`web.rs:1130`)
feeds *only* the refusal, and nothing between the refusal and the pin
reads either value. The hoist moves the refusal inside the pin's
`else`.

## The permanence hazard this design exists to avoid

The three hazards fixed in plan 1 were one class: **derived state
recomputed later and assumed stable**. The fourth is that class one
step earlier, and item 1 is its *vector* rather than its victim.

`Uri::new` (`crates/junto-kernel/src/provenance.rs:31-34`) rejects
exactly one value: the empty string. No scheme check, no
normalization. junto's house convention for a caller-supplied
reference is `ProvenanceParam` (`crates/junto/src/mcp.rs:74-79`) —
free-text `uri` plus optional `digest`, shape-validated only. That is
safe where it is used, because a `ProvenanceRef` is *decoration*: a
malformed provenance URI misleads a reader and nothing else.

It is not safe for a Subject, because a Subject's `uri` **is** its
identity (ADR 0037: exact-string comparison, no normalization) and
that comparison is what `attach_subject`'s idempotency guard keys on
(`host.rs:981`). A Document tool built to house convention accepts a
bare machine path, appends it permanently, and hands every teammate a
different Subject for the same document.

## Design

### D2 — the URI guard

The subtle point, and the reason this is a design decision rather than
a line of validation: **`D:/git/junto/docs/spec.md` parses as a URI
with scheme `d`.** "Require a scheme" admits precisely the Windows
machine path it was written to exclude, on one of two equal target
platforms.

`fn subject_uri(raw: &str) -> Result<Uri, McpError>`, in `mcp.rs`
beside `parse_provenance`. Rules, in order:

1. Reject a leading path separator — `/` or `\` — catching POSIX
   absolute paths and UNC shares (`\\server\share\spec.md`).
2. Reject when there is no `:`.
3. Reject when the text before the first `:` is shorter than two
   characters — the Windows drive letter.
4. Reject when that text is not an RFC 3986 §3.1 scheme:
   `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.

Stated positively: the surface accepts exactly what carries a real
scheme of two or more characters. Both separators are inspected
explicitly; CLAUDE.md's "never hardcode `/` or `\`" governs *building*
paths, and a guard that ignored one separator would be a guard that
works on one platform.

Refusals are `McpError::invalid_params` naming the rule and the fix, so
an agent self-corrects without a round trip.

**E2, a new known limit:** the guard refuses an SCP-style git remote
(`git@host:path`) — the text before the first colon is `git@host`,
which is not a scheme. Such a remote must be given as `ssh://`. The
new surface is therefore strictly stricter than `repo_subject_uri`,
which keeps recording `origin` verbatim. The asymmetry is deliberate:
0037's second known limit is that ssh and https spellings of one repo
are two Subjects, and refusing the ambiguous spelling at the only
surface that can still refuse is the narrow version of fixing it.

### D3 — digests

`digest: Option<String>`, caller-supplied, validated by
`ContentDigest::new` for `algorithm:value` shape only, exactly like
`ProvenanceParam`. **The host never computes one.**

`provenance.rs:64` already states digests are "not yet computed or
verified by the kernel". The only digest junto computes is
`store_artifact` (`launch.rs:1203`), over bytes it wrote itself in the
same process — which is why that one is sound. A digest computed by
reading a document the caller merely *names* is platform-dependent on
any text file: CRLF versus LF, which CLAUDE.md pins to LF precisely
because "the same record gets written on both platforms". It would
report false drift across a Windows/macOS pair and record the false
reading permanently.

### D4 — the kind-mismatch refusal

`attach_subject`'s guard (`host.rs:981`) becomes:

- same `uri`, same `kind` → return the existing attachment id, as today;
- same `uri`, different `kind` → `Err`, appending nothing;
- new `uri` → append.

`uri` stays the single identity per 0037. Two kinds for one identity is
incoherent, and `mounts.rs` keys the mount store on `uri` alone, so
admitting both would manufacture a genuinely ambiguous mount. Refusing
appends nothing, which is the safest available behaviour in an
append-only record. Zero live impact: one production caller, always
`Repo`.

### E1 — the tool exposes `kind`

`SubjectKindParam { Repo, Document }`, `#[serde(rename_all =
"lowercase")]`, mirroring `AuthorKind` (`mcp.rs:42-49`) with a
`From<SubjectKindParam> for SubjectKind`.

Mirrors the kernel instead of narrowing it: a Document-only tool is a
surface that must be widened later, and D4's refusal is only
expressible at the surface if the surface can name both kinds. It also
serves the multiplayer case — attaching a teammate's repo by origin URL
on a machine with no clone.

One consequence, kept explicit: attaching a Repo Subject with no local
mount **manufactures** the unmounted-Repo state item 3 exists to
survive. That is correct — `launch` still refuses, because there
genuinely is no checkout — and item 3 shipping alongside is what makes
it survivable on steer.

### Item 2 — `Host::detach_subject`

```rust
pub async fn detach_subject(
    &self,
    channel: &str,
    target: EntryId,
    author: Member,
    auth: WriteAuth<'_>,
) -> Result<EntryId>
```

Mirrors `attach_subject`: `resolve_for_write`, lock, `project_fresh`,
`check_write_auth`, then validate and append. Returns the
*detachment's* own id.

Validation distinguishes the two failure modes rather than collapsing
them, because `project_subjects` drops detached attachments and a bare
"not found" would be actively misleading on a retry:

- `target` in `view.subjects` → append `SubjectDetached { target }`;
- `target` is a `SubjectAttached` in `view.entries` but not live →
  "already detached";
- otherwise → "not a subject attachment in this channel".

**D5:** the machine-local Mount is left untouched.
`mount_with_capability` walks *subjects*, not mounts, so a mount whose
Subject is gone is already inert, and the durable ledger should not
drive machine-local deletions.

### Item 1 — the MCP tools

Two tools on `JuntoMcp`, following `diverge_channel`
(`mcp.rs:612-642`) exactly: `#[tool(description = …)]`,
`Parameters<Req>`, `WriteAuth::Agent(req.code.as_deref())`, domain
errors through `invalid`.

```rust
struct AttachSubjectRequest {
    channel: String, author: AuthorParam, code: Option<String>,
    kind: SubjectKindParam, uri: String, digest: Option<String>,
}
struct DetachSubjectRequest {
    channel: String, author: AuthorParam, code: Option<String>,
    target: String,
}
```

`target` accepts an id or an unambiguous 6+ char prefix through the
existing `resolve_target`, extended with a fourth `TargetKind::Subject`
whose `bears_kind` tests membership in `view.subjects`. That matches the
convention the server already advertises: "Verification targets accept
unambiguous id prefixes (6+ chars)."

`get_info`'s `instructions` string (`mcp.rs:1053-1075`) is the only
place enumerating the tool set; both tools are named there.

### Items 3–5

- **3.** Compute `scratch` first; `if scratch.is_dir()` take it,
  `else` run the refusal block and `session_workdir`. The comment at
  `web.rs:1143-1159` moves with the code and drops the claim that the
  refusal precedes the pin.
- **4.** ADR 0038's known-limit paragraph: the substantive limit
  survives — nothing records which mount a session actually ran in — but
  "every time it is called, on launch and again on every steer" becomes
  launch-only, naming the pin as the reason and `web.rs` as where it
  lives.
- **5.** `mounts.rs:88`'s doc comment: `remember_mount` is called
  *before* `attach_subject`, so the irreversible ledger append lands
  last. Point at `web.rs:917-924`, which already says so.

### D6 — ADR treatment

No new ADR. ADR 0037's Known-limits section is amended in place: two
limits are closed (no detach surface, no Document surface), one is
narrowed (the guard now refuses ambiguous spellings at the new
surface), and one is added (E2's SCP-style refusal). A known limit that
has stopped being true is not history, it is a false statement about
current code. Closing a limit 0037 itself anticipated is not a new
architectural decision, and repo precedent reserves a new ADR for
*narrowing another ADR's decision*, which this does not do.

ADR 0038's amendment is item 4 and is a factual correction, not a
decision change.

## Testing

Behavioural only; no test asserts on source text.

**Kernel/host** (`host.rs` tests):
- detach appends, returns its own id, and `project_subjects` drops the
  target;
- detaching an already-detached target errors and appends nothing;
- detaching a non-attachment entry errors;
- a non-member is refused, and a wrong member code is refused;
- attach with same uri + same kind returns the existing id (unchanged);
- attach with same uri + different kind errors and appends nothing.

**MCP** (`mcp.rs` tests, `init_repo`/`open` pattern):
- attaching a Document then `view_channel` shows it — the case PR #69
  could only reach through a temporary probe;
- `D:/git/junto/spec.md` is refused (the drive-letter case);
- `/home/dan/spec.md` and `\\server\share\spec.md` are refused;
- `spec.md` is refused;
- `https://…`, `file:///…`, `git+ssh://…`, `urn:isbn:…` are accepted;
- detach by 6+ char prefix succeeds; an ambiguous prefix errors;
- a caller-supplied digest round-trips; a malformed one is refused.

**Web** (`web.rs` tests) — the one behavioural bug fix:
- a session that already ran in scratch steers successfully even when
  an unmounted Repo Subject is present in the channel (fails at
  `fd4fe89`, passes after item 3);
- the existing refusal still fires for a session that did *not* run in
  scratch — the regression this hoist must not cause.

## Verification

`golden_canonical_form_is_byte_stable` must stay green untouched: no
kernel type changes, so no canonical bytes move. Pre-commit, in order,
stopping on first failure: `rtk cargo fmt --check`, `rtk cargo clippy
--workspace --all-targets -- -D warnings`, `rtk cargo test
--workspace`.

Merged through junto's own code-PR push-gate, not `gh pr create`.

## Non-goals

- A human web control for attaching a Document — depends on decisions
  the surface plan owns.
- Computing or verifying digests anywhere (D3).
- URI normalization or canonicalization; 0037's exact-string identity
  stands.
- Any Outcome-loop change. A Document cannot reach it: `capabilities`
  grants `Diff` only to a mounted Repo (`mounts.rs:174-177`) and the
  gate is keyed on `Capability::Diff`, so a document channel refuses
  outright. That is correct and out of scope.
