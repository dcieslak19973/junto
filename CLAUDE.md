# junto — agent working guide

> Guidance for AI agents working in this repo. Tags mirror the design docs: **✅ settled · 🔵 proposed (default — change if you disagree) · ⚠️ open / needs a decision.**

## What junto is (one paragraph)

junto is **one surface where people and agents take a piece of work to a verified, provenance-bound outcome** — over the tools you already use. The unit is a **Channel** (a unit of inquiry) running one loop: *deliberate → agent-augmented work → gate → verified record*. It is vendor-neutral by adapters, terminal-less for humans, workflow-general (coding is one Playbook of several), and its durable output is a re-runnable record, not prose. Read [`README.md`](README.md) then [`docs/junto.md`](docs/junto.md) (the vision spine) for the full story.

## Status: early implementation (kernel spine built)

✅ The design corpus (`docs/`) is settled enough to build on; its language and the constraints below are load-bearing for the code. The first kernel slices exist and are test-pinned: the **Ledger** (immutable `LedgerEntry`s, event-sourced projection into standings + gate statuses), the **Gate engine**, the **canonical (JCS) record format**, the **git-refs substrate** (durable record in `refs/junto/*`), **sync** (push/fetch to any git remote with convergent union-merge reconciliation), the **MCP write surface** (`junto serve` — agents author entries over streamable HTTP), the **channel model** (repo-agnostic channels with minted globally-unique ids, opened explicitly via a `ChannelOpened` genesis entry; one **host per machine/user** serving every registered home substrate; `junto init` sets a project up), the **Party** (`docs/adr/0017`: the opener is the founding member; the founder grants membership via `MemberAdded` entries; **only members' entries project** — non-member entries surface as *unrecognized*; minting a member also mints a machine-local **member code** that the host's *agent-facing* write surfaces require — accident-proofing, not security; the human surface checks membership only, `docs/adr/0021`), and **Agent Sessions + Artifacts** (`docs/adr/0020`: `SessionStarted`/`SessionUpdated`/`ArtifactAttached` entries; state folds last-applicable-wins; artifact content stays out of the ledger — URI + digest only; artifact kind is a playbook-supplied string). Settled decisions live in `docs/adr/` (index: `docs/adr/README.md`) — read the relevant ADRs before touching the ledger, gates, serialization, substrate, sync, the MCP surface, channel identity/addressing, membership, or sessions. Not built yet: forge capability flags (Bitbucket fallback), playbooks, the real (GUI) human surface.

**Dogfooding (active):** junto records its own development decisions in its own ledger. Start the host with `cargo run -p junto -- serve` (singleton, port 1727, serving every substrate registered in `~/.junto/substrates.toml` — this repo is registered; `--repo <path>` serves one repo instead). The checked-in `.mcp.json` connects Claude Code to it, and a SessionStart hook injects the `junto-dev` channel brief into agent context (the recall bridge — `docs/adr/0013`); the checked-in `.junto.toml` is this checkout's **channel binding** (per-worktree additions go in the gitignored `.junto.local.toml`, read by `junto brief`). Humans read the index at `http://127.0.0.1:1727/` and the record at `/channels/junto-dev`. **Channels must be opened before use** (`open_channel` tool or `junto open` — no create-on-first-write, `docs/adr/0014`/`0016`); discover them with `list_channels`. Channel convention: **`junto-dev`** for build/design decisions.

**The consult/record convention (agents, follow this):** *consult before deciding, record after.* Before making or recommending a decision in territory the ledger covers, check the injected brief (or `view_channel`) — do **not** contradict a **ratified** entry or re-try a **parked** dead-end without surfacing it to Dan first. After a consequential decision, `record` it with real rationale + provenance (PR links, ADR paths); propose consequential *actions* through a gate. Dan ratifies/approves — agents author as themselves, never as their operator (identity is claimed, with a member-code check at the host's write surfaces — `docs/adr/0012`/`0017`). Writes require membership in the channel's Party plus your **member code** (`code` param); the session brief carries your code (from the gitignored `.junto.local.toml`), and Dan grants membership with `junto add-member`. Sync with `sync_channel` (remote `origin`) after recording.

**Workspace layout:**

```
crates/junto-kernel/        # the generic, playbook-agnostic core (lib) — no vendor names, no playbook logic
crates/junto-substrate-git/ # git-refs SubstrateProvider adapter (shells out to system git)
crates/junto/               # the host/app entry (bin) — terminal-less for humans; not a CLI UI
docs/                       # the design corpus (vision, domain model, architecture, pluggability, worked examples)
docs/adr/                   # one settled architectural decision per file
```

**Commands** (from repo root):

```powershell
rtk cargo check --workspace                                # fast compile validation — prefer this while iterating
rtk cargo test --workspace                                 # run all tests
rtk cargo clippy --workspace --all-targets -- -D warnings  # lint; warnings fail the build
rtk cargo fmt --check                                      # format check (CI); `cargo fmt` to fix
rtk cargo build --release                                  # only when you actually need the binary
```

**Pre-commit / "is it green?" — run in this order (cheap → expensive), stop on first failure:**

```powershell
rtk cargo fmt --check; if ($?) { rtk cargo clippy --workspace --all-targets -- -D warnings; if ($?) { rtk cargo test --workspace } }
```

Why `cargo check` over `cargo build`: it validates the code compiles without producing a binary, and is much faster. Only `build` when you need the runnable artifact.

## Tech stack

| Decision | Choice | Status |
|---|---|---|
| Language | **Rust** | ✅ (Dan) |
| Platforms | **Windows + macOS** (cross-platform; Linux likely free, not a target yet) | ✅ (Dan) |
| Edition | **2024** (resolver 3) | ✅ |
| MSRV | 1.94 (`rust-version` in workspace) | 🔵 |
| Layout | Cargo **workspace**; kernel crate + one crate (or module) per adapter boundary, so vendor code stays quarantined | ✅ (seam) / 🔵 (granularity) |
| Async runtime | `tokio` (MIT) | 🔵 |
| Git — substrate push/fetch | **System `git` CLI (shell out)** for `refs/junto/*` | 🔵 (recommended; see note) |
| PTY capture | `portable-pty` (wezterm, MIT — wraps ConPTY on Windows, openpty on macOS) | 🔵 |

### Git library — assessed 2026-06-08

The substrate is **`git push`/`fetch` of custom `refs/junto/*` over the standard git wire protocol** (see `docs/architecture.md` §Substrate) — **not** forge APIs. Because all targets (GitHub/GitLab/Bitbucket, cloud + DC) speak the same git protocol, the library choice is **forge-independent**; forge-specific concerns (PRs, CODEOWNERS) live in `ForgeAdapter`, not here.

- ❌ **`gix` (gitoxide) — ruled out for now.** Pure Rust / MIT, but **push is entirely unimplemented** (authoritative [crate-status.md](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md): `[ ] push`, `[ ] send-pack/receive-pack`, `[ ] refspec for push`; fetch is `[x]`). Keep as a candidate for fast pure-Rust **reads/fetch** later, and revisit for push when it lands.
- ✅ **System `git` CLI (shell out) — recommended primary.** Pushes custom refspecs natively and, crucially, **inherits the user's existing git auth** (credential managers, SSH, enterprise SSO/tokens, proxies) uniformly across all six targets — the real cross-forge pain point. Needs `git` on PATH (safe for junto's audience); fits "agents run shells under the hood".
- 🔵 **`git2` (libgit2) — in-process fallback** if we ever want to drop the system-git dependency. GPLv2 **with a linking exception** → linking into an MIT binary is permitted and is *not* "incorporating source" (does not violate the MIT constraint). Caveat: its credential callbacks can stumble on enterprise helpers/SSH.

⚠️ **Forge custom-ref support (substrate design, not a library question) — assessed 2026-06-08:**
- **GitHub & GitLab (cloud + self-hosted): ✅** accept arbitrary `refs/junto/*` (git allows any ref outside `refs/{heads,tags}`).
- **Bitbucket Cloud: ❌** pre-receive hooks reject custom namespaces; **no way to relax** (this is the exact wall `git-bug`'s `refs/bugs/*` hits).
- **Bitbucket Data Center: ⚠️** blocked by default; a server admin *can* relax pre-receive/ref restrictions.
- **Mitigation (fits the capability-flag design):** `SubstrateProvider` carries a `supports_arbitrary_refs` capability; when false, fall back to an allowed namespace like **`refs/heads/junto/*`** (works everywhere since it's `refs/heads/*`, at the cost of branch-list clutter / possible branch-protection interplay — the very pollution the dedicated-ref design wanted to avoid). **Verify empirically against a real Bitbucket instance before locking in.**

🔁 **Revisit trigger — re-check gix periodically.** gix is the *preferred* long-term substrate (pure Rust, no C toolchain, MIT/Apache) and is ruled out only for lack of push. **When [crate-status.md](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md) flips `push`, `send-pack/receive-pack`, and `gix-refspec for push` to `[x]`, re-run this assessment** — empirically push a custom `refs/junto/*` ref over authenticated https + ssh to GitHub/GitLab/Bitbucket (cloud + DC), and if it holds, switch the substrate from the `git` CLI to gix. Suggested cadence: every few months, or whenever the git library is otherwise touched. (Last assessed 2026-06-08.)

## Hard constraints (do not violate)

1. ✅ **MIT, greenfield, no copyleft *source*.** Reuse *ideas/patterns* (git-refs-as-record, forge-as-hub sync, pty-capture-to-artifact) clean-room; never copy copyleft source. Linking a permissively- or linking-exception-licensed library is fine (see git note); vendoring GPL source is not.
2. ✅ **Terminal-less for humans.** The point (Dan, 2026-06-09): humans don't *work* through a terminal agent harness (Claude Code, OpenCode, Gemini CLI, Goose…) — junto's human surface is a GUI (think Mux's UI). One-time CLI setup plumbing (`junto init` / `junto serve`) is acceptable, especially dogfood-era. **Agents do** run shells under the hood — their output is **captured as verifiable Artifacts (diffs, logs, charts), never rendered as scrollback.** This is why cross-platform PTY capture matters.
3. ✅ **Durable record = git refs** (`refs/junto/*`, partitioned by author). Append-only; **no CRDT** — concurrent writes interleave by `(ts, author)`. Dedicated refs, never working-tree files (no `git status` pollution). The record holds **decisions/intent + provenance + digests, not raw agent transcripts** (those live outside the repo).
4. ✅ **Vendor-neutral by adapters.** Every external dependency sits behind a swappable adapter; **no vendor name reaches the kernel** — branch on **capability flags, not vendor identity**.
5. ✅ **Kernel ↔ Playbook seam.** **No playbook-specific logic in the kernel.** The kernel is generic (Channel · Member/Party · Message · Artifact · Provenance · Agent Session · Gate engine · Ledger · Outcome · Event). A Playbook *supplies* its Lifecycle, **gate-routing function** (the single most playbook-specific thing), Verifier, offered tools/agents, and artifact renderers.

## Cross-platform rules (Win + Mac — first-class, tested on both)

junto targets **Windows and macOS equally** (Linux likely comes free). Windows is the one that surprises people, so assume a contributor on either OS and design for the harder case. **Set up CI with a `{windows, macos}` matrix early** — most of these pitfalls (casing, path separators, a platform-locked script) are cheap to catch on commit #3 and expensive later.

**Filesystem & git:**
- **Never hardcode `/` or `\`.** Use `std::path::Path` / `PathBuf` and `join`, never string concatenation. Git refs and ledger paths cross OSes.
- **Normalize line endings** for the git-refs ledger. The same record gets written on both platforms; pin LF (`.gitattributes` does this for the repo; do the same in code that writes refs) so CRLF vs LF doesn't corrupt dedup/ordering.
- **Windows filesystem is case-insensitive; Linux/CI is not.** `Foo.rs` and `foo.rs` collide on Windows but differ on Linux — a casing typo can build for you and break CI. Keep module/file names consistent.
- **Avoid relying on the executable bit or symlinks** — neither round-trips cleanly through Windows git (symlinks need Developer Mode). Don't commit artifacts that depend on them.

**Runtime (the genuinely hard, junto-specific ones):**
- **PTY capture: ConPTY (Windows) ≠ openpty (macOS).** Use `portable-pty`; never reach for unix-only `nix`/`fork`/`exec`. The *semantics* still differ (ConPTY does its own VT processing/screen rewrites), so **agent-output→artifact fidelity must be validated on both** — don't assume byte-identical capture. This is core to the terminal-less model; treat it as a design risk, not a detail.
- **Process control differs.** No real `SIGTERM` on Windows (it's job objects / `TerminateProcess`), and killing a process *tree* (an agent plus its children) is fiddlier. Wrap "spawn / time out / kill an Agent Session" in one cross-platform abstraction from the start; don't sprinkle `kill(2)` calls.
- **Windows locks open files** — you can't delete/rename a file another process holds open (Unix lets you). Expect "file in use" errors around git-worktree cleanup while an agent process is alive; sequence teardown accordingly.
- **Shell assumptions for agent-run commands.** An agent emitting `sh`/bash lines assumes a Unix shell; the Windows local backend is PowerShell/cmd. This is what the `ExecutionBackend` abstraction is for — **WSL is the Windows escape hatch** to give agents a Unix shell without porting every command.
- Prefer cross-platform crates; gate any unavoidable OS-specific code behind `#[cfg(...)]` with **both arms implemented**, not a Windows stub.

**Dev tooling & scripts:**
- **Write tooling in Rust, not shell.** A `.ps1` is Windows-only; a `.sh` is Unix-only. For anything beyond a one-liner, prefer a `cargo xtask` (a Rust dev-tool crate) so there's one implementation that runs everywhere.
- **Hooks are cross-platform by construction:** the auto-fmt/clippy hook (`.claude/settings.json`) is just bare `cargo fmt` / `cargo clippy` invocations — single tokens with no shell operators or scripts, so they run identically under cmd, PowerShell, bash, and zsh. Keep new hooks to that shape (one executable, no `&&`/`||`/`;` chaining, no interpreter-specific syntax). It runs from the session's project-root cwd.
- On **Windows**, add a Defender exclusion for the repo's `target/` dir — real-time AV scanning of build artifacts noticeably slows rebuilds.

## Rust conventions

🔵 Defaults drawn from widely-used Rust practice — change if you disagree, but they're the safe baseline (Dan doesn't write Rust, so lean **idiomatic, conventional, and boring**; favor the obvious solution over the clever one).

**Write Rust the way the ecosystem does.** Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and standard idioms (`Result`/`Option` with `?`, iterators, `From`/`Into`, the newtype pattern, RAII). Don't import patterns from other languages (no manual getters/setters, no inheritance-style hierarchies, no `Arc<Mutex<…>>` reached for before it's needed). When unsure how something is conventionally done, match `std` and the surrounding code.

### Correctness & safety

- **Error handling:** `thiserror` for typed errors in **library** crates (the kernel + adapters); `anyhow` for **application/binary** crates (the CLI/daemon top level). Don't leak `anyhow::Error` out of library APIs.
- **No `unwrap()` / `expect()` / `panic!` in library code.** Return a `Result` instead. They're fine in tests, build scripts, and `main`-level setup. If a panic is truly unreachable, `expect("why this can't fail")` with a reason — never bare `unwrap()`.
- **No `unsafe` without a `// SAFETY:` comment** explaining the invariant being upheld. For this project, prefer not having any.
- **Document public items** with `///` doc comments — especially the kernel nouns and adapter traits, since those *are* the ubiquitous language.
- Prefer borrowing (`&str`, `&[T]`) over owned (`String`, `Vec<T>`) in function parameters.
- **Don't restate what the tools enforce.** `rustfmt` owns formatting and `clippy` owns lint style — don't add rules CLAUDE.md can't enforce that the linter already does. Fix clippy warnings rather than `#[allow(...)]`-ing them; if you must allow one, comment why.
- Adapter boundaries are **traits**; vendor implementations live in their own module/crate. A trait method branching on a vendor name is a bug — branch on a `Capabilities` value instead (constraint #4).

### Readable & understandable code (a first-class goal)

This codebase is meant to be **navigable by both humans and agents** (it's the substrate for a system where agents are peers — see `docs/junto.md`). Clarity outranks cleverness; optimize for the next reader, not for line count.

- **Names carry the ubiquitous language.** Types, functions, and modules should read like `docs/domain-model.md` — a `Channel`, a `Gate`, a `LedgerEntry`. A reader who knows the domain should recognize the code. Spell names out; avoid cryptic abbreviations.
- **Make illegal states unrepresentable.** Reach for the type system: `enum`s over stringly-typed states, **newtypes** over bare `String`/`u64` (e.g. `ChannelId(Uuid)`, not `String`), `Option`/`Result` over sentinel values. Self-documenting types beat comments.
- **Small, single-purpose functions.** Prefer early returns and `?` over deep nesting; if a function needs section comments to be followed, split it. Keep modules organized by domain concept so the file tree tells the story.
- **Comment the *why*, not the *what*.** The code says what it does; comments explain intent, invariants, and non-obvious tradeoffs (especially around the gate engine, provenance, and sync). Doc-comment every public item with a one-line summary of its role.
- **Prefer clear standard combinators** (`map`, `and_then`, `filter`, `?`) where they read naturally — but break a long iterator chain into named steps before it becomes a puzzle. Readability is the tiebreaker, not brevity.
- **Match the surrounding code.** Consistency (naming, structure, error style) matters more than any individual preference; when editing, mirror the file you're in.
- **Simplest thing that works.** No premature abstraction or speculative generality (rule of three — build concrete cases first). A bit of duplication is cheaper than the wrong abstraction.

## Ubiquitous language — use these names in code

The full table is [`docs/domain-model.md`](docs/domain-model.md) — **read it before naming types.** The naming traps an agent *will* get wrong while coding:

- **Agent Session** — always qualified, never bare "session". Bare "session" is overloaded (terminal/login) and **Ace calls its *channels* "Sessions"** — the opposite layer from ours. One Channel → many Agent Sessions.
- **Channel** = one unit of inquiry. **Playbook** = the *type* stamped on a channel (code-PR / research / prod-troubleshooting / self-improvement). Not "Channel Kind".
- **Ledger** = one per channel, holding many **entries** (decisions/findings/claims, each provenance-bound with a verification state). The research "hypothesis ledger" is just a research channel's ledger.
- **Member** (human *or* agent — agents are first-class) · **Party** (the set of Members) · **Gate** (checkpoint a consequential action must pass) · **Artifact** (verifiable output, not scrollback) · **Provenance** (a *relation* on artifacts/entries, not a standalone entity) · **Outcome** (PR | memo | fix | promoted policy | parked).
- Adapter/boundary nouns: **SubstrateProvider · ForgeAdapter · AgentHarnessAdapter · ExecutionBackend · ChatConnector · Connector · MemoryProvider · InferenceEndpoint**. All declare **Capabilities**.

## Design docs (source of truth — read before large changes)

- [`docs/junto.md`](docs/junto.md) — vision spine (start here)
- [`docs/domain-model.md`](docs/domain-model.md) — the ubiquitous language (nouns & verbs)
- [`docs/architecture.md`](docs/architecture.md) — substrate, sync, governance, gotchas
- [`docs/pluggability.md`](docs/pluggability.md) — the vendor-neutral adapter boundaries
- [`docs/attention.md`](docs/attention.md) — the human surface as an **attention router**: the focus board, side-quests (forking), personal-optimum measurement — with the research citations
- [`docs/self-improving-harness.md`](docs/self-improving-harness.md) — the self-improvement Playbook (evals = the crux)
- `docs/worked-example-*.md` — three Playbooks walked end-to-end

When a code decision contradicts a doc, surface the conflict — don't silently diverge. The docs use ✅/🔵/⚠️ tags; respect what's settled vs still open.

## Working conventions

- This is a **Windows** machine, **PowerShell** shell — use PowerShell syntax (`$null`, `$env:VAR`, backtick line-continuation).
- 🔵 Prefix tooling commands with `rtk` per the user's global RTK convention (token-optimized output) — e.g. `rtk cargo build`, `rtk cargo test`, `rtk git status`.
- The "rule of three": **build concrete cases before extracting a framework.** Don't frameworkize an adapter or Playbook seam from a single example — build a few, then extract.
