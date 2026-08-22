# Subjects and Mounts: what a channel is about, durable; how this machine reaches it, not

Status: accepted (Dan, 2026-08-21) — **implemented** · realizes spec §1 of [`2026-08-21-multiplayer-first-rethink-design.md`](../superpowers/specs/2026-08-21-multiplayer-first-rethink-design.md); extends [`0023`](0023-launching-agent-sessions-oneshot-first-pty-next.md)'s Workspace

`domain-model.md:32` already protected a fact worth keeping: a Workspace is *"a machine fact, never ledger content (paths don't sync)"* — `D:\git\junto` here is `/home/dan/junto` there. But `0023`'s Workspace conflated two things that only happened to coincide in v1: *what a channel is about* and *how this machine reaches it*. Both were "the workspace," both were always a git repo, and the shortcut was named honestly in `0023` itself — *"the file stores a list of repos per channel so multi-repo inquiries are additive later; v1 reads exactly one, and it must be a git repo (diff capture leans on git)."* That parenthetical is the whole constraint: the `.git` requirement exists to serve `workspace_diff`, not because a channel about a Jira ticket or a design doc is somehow a lesser channel. A ticket is not a machine fact — every party member must know a channel is about `PROJ-412` — so it cannot live in a machine-local file the way a checkout path can.

## The split

| | **Subject** | **Mount** |
|---|---|---|
| What | what the channel is *about* | how *this machine* resolves it |
| Example | `git+https://github.com/dcieslak19973/junto.git` · `file:///notes/spec.md` | `D:\git\junto` · `D:\notes\spec.md` |
| Lives in | the ledger — durable, portable, synced | `~/.junto/mounts.toml` — machine-local, never synced |
| Cardinality | 0..N per channel | 0..1 per subject, per machine |

**A Subject is a portable URI, recorded in the ledger.** `SubjectAttached { kind: SubjectKind, uri: Uri, digest: Option<ContentDigest> }` folds append-only into `ChannelView::subjects` (`crates/junto-kernel/src/subject.rs`) — no mutation, no new sync semantics, the same shape as `ArtifactAttached`. The optional `digest` is the content hash captured *at attach time*, so later drift on that URI is detectable; a live repo has none (there is nothing single to hash).

**A Mount is machine-local config, keyed by Subject URI, and never enters the ledger.** `crates/junto/src/mounts.rs` replaces `0023`'s `~/.junto/workspaces.toml` with `~/.junto/mounts.toml` — a clean cutover, no shim, because it is regenerable machine config, not durable record. Unlike the Workspace store it replaces, a Mount carries **no `.git` requirement**: a Document subject mounts to any path and simply reports fewer capabilities than a Repo does. `Host::attach_subject` writes the Subject; `remember_mount` (called by the same launch-form flow, once a typed path resolves) writes the Mount — two calls into two stores, because they answer two different questions for two different audiences.

This is why paths stay out of the record: a Jira ticket has no path to leak, but even for a Subject that *is* a repo, "where Dan keeps it" and "where Priya keeps it" are legitimately different, permanent facts about two machines, not a fact about the channel.

## Capabilities are computed per executing host, never recorded

```
capabilities(subject, host) = kind_affordances(kind) ∩ provider_available(host) ∩ mount_present(host)
```

`crate::mounts::capabilities(subject, mount)` (`mounts.rs`) is a pure function, called at use time, never persisted. Reading is the floor — a URI alone is enough to fetch or open something, so every Subject reports `Capability::Read` with no Mount at all. Everything past that (`Watch`, `Anchor`, `Diff`, `Execute`, `Mutate`) requires a Mount, and `Diff`/`Execute` further require `SubjectKind::Repo` — a Document has no working tree to run in and no mechanical diff; its provenance is a content digest instead, weaker and shown as weaker rather than pretended away, in the spirit of `CodeAnchor`'s three-state honesty.

The reason this cannot be recorded is not caution, it is correctness: capabilities vary by machine (one host has the checkout and the credentials, another has neither), so writing "this channel's repo is Execute-capable" into the ledger would smuggle a machine fact into content every party member reads identically. Recomputing per call is cheap (a `BTreeSet` fold over an enum) and it resolves against the **executing host**, not the viewing human — the machine that runs an agent session is not necessarily the machine of the person who opened the channel, and a capability check that answered for the wrong host would be actively wrong, not merely stale.

## `SubjectKind` is closed; the providers behind it are not

`SubjectKind` (`crates/junto-kernel/src/subject.rs`) is a deliberately closed enum in the kernel — today `Repo` and `Document`. Adding a kind is a kernel change on purpose: each kind's capability profile (the table above) is kernel-visible, folded by every projection, and load-bearing for gates like the Outcome loop's `Diff` requirement below. A closed set here is the same call `0003` made for entry `kind` and `0016` extended for lifecycle acts — the alternative, an open string tag, would let a surface invent a kind the kernel has no capability story for.

The **providers** that reach these things — a forge API, a chat connector, a knowledge-base client — stay behind adapters and never appear in the kernel by vendor name (constraint #4). Today there is exactly one provider path in production: `SubjectKind::Repo` resolved through git-on-disk, the same substrate machinery this whole design already depends on. `SubjectKind::Document` resolves through nothing more than a filesystem path or a `file://` URI — there is no provider yet, because a plain file needs none.

## The rule-of-three deferral

No `SubjectProvider` trait exists, and none should yet. `CLAUDE.md`'s rule of three applies literally: two kinds are built (`Repo`, `Document`), and extracting a provider trait from two concrete cases plus imagined future ones (Slack, Jira, Confluence) is exactly the move the convention forbids — the shape of a two-case abstraction is a guess, not a generalization. The spec's own build sequence names this explicitly: *third* subject kind → extract `SubjectProvider`. Until then, `mounts.rs`'s `capabilities` function is the entire "provider" story, expressed as a match over two variants — honest about being provisional, cheap to keep that way.

## Known limits, recorded honestly

**A Subject's identity is its URI, compared as an exact string.** `Uri` (`crates/junto-kernel/src/provenance.rs`) derives `PartialEq` on its wrapped `String` with no normalization, and `Host::attach_subject`'s idempotency check (`view.subjects.iter().find(|(_, s)| s.uri == subject.uri)`) relies on that literal equality. `git@github.com:dcieslak19973/junto.git` and `https://github.com/dcieslak19973/junto.git` name the same repository to git, but they are **two different Subjects** to junto — attaching both leaves a channel with a duplicate entry in its subject list, and a Mount keyed to one will not resolve for the other. Normalizing remote URLs (stripping protocol/auth, canonicalizing `.git` suffixes, resolving `git@host:path` to the same key as `https://host/path`) is an open design question, not solved here — recorded as a real gap rather than smoothed over, because the failure mode (two Subjects, one repo, silently) is exactly the kind of thing that looks fine in every demo and wrong in the field.

**The Outcome loop requires `Capability::Diff`.** The push-gate grading loop (`0026`/`0029`) grades a mechanical before/after — a `git diff` of the workspace — which only a mounted `Repo` subject can ever produce (`crate::launch::diff_capable`, checked directly against `mounts::capabilities` before `mode=outcome` is allowed to launch at all, `crates/junto/src/web.rs`'s `launch_session`). A channel whose only Subject is a Document, or with no Subject at all, refuses `mode=outcome` outright rather than running a loop that can never be satisfied — the correct behavior, but it means **a research or planning channel with no diffable subject cannot use the Outcome loop as it exists today.** Such a channel is not lesser work; it needs a rubric path that grades something other than a diff (a document's content against a rubric, a decision against stated criteria) and that path does not yet exist. Building it is future work, not assumed by anything here.

## Considered

- **Keep Workspace as-is, add a `kind` field to it** — rejected: Workspace's whole shape (a path, keyed by channel, hard-required to be a `.git` directory) is the wrong home for a Subject with no Mount at all. A field cannot fix a store whose primary key and required content are both wrong for the case it needs to hold.
- **Extract `SubjectProvider` now, in anticipation of Slack/Jira/Confluence** — rejected (rule of three, above); the spec names this decision explicitly and it is not revisited here.
- **Normalize Subject URIs at attach time** — rejected for this change: URL normalization for git remotes specifically (ssh vs. https vs. bare host, trailing `.git`, case-folding) is its own small design problem with real edge cases (self-hosted forges, non-standard ports); bolting a guess onto this change risks getting the normalization wrong under the same ADR that introduces the noun. Left as the recorded, open limit above.
