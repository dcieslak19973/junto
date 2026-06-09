# junto — agent working guide

> Guidance for AI agents working in this repo. Tags mirror the design docs: **✅ settled · 🔵 proposed (default — change if you disagree) · ⚠️ open / needs a decision.**

## What junto is (one paragraph)

junto is **one surface where people and agents take a piece of work to a verified, provenance-bound outcome** — over the tools you already use. The unit is a **Channel** (a unit of inquiry) running one loop: *deliberate → agent-augmented work → gate → verified record*. It is vendor-neutral by adapters, terminal-less for humans, workflow-general (coding is one Playbook of several), and its durable output is a re-runnable record, not prose. Read [`README.md`](README.md) then [`docs/junto.md`](docs/junto.md) (the vision spine) for the full story.

## Status: early implementation (scaffold up)

✅ The design corpus (`docs/`) is settled enough to build on; its language and the constraints below are load-bearing for the code. A Cargo workspace scaffold exists — `crates/junto-kernel` (lib) + `crates/junto` (bin) — but the domain model is **not** modelled yet (the ledger-entry content model is still open; see `docs/junto.md` item *b*).

**Workspace layout:**

```
crates/junto-kernel/   # the generic, playbook-agnostic core (lib) — no vendor names, no playbook logic
crates/junto/          # the host/app entry (bin) — terminal-less for humans; not a CLI UI
docs/                  # the design corpus (vision, domain model, architecture, pluggability, worked examples)
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
| Git access | `gix` (gitoxide, MIT/Apache) — see git note below | 🔵 ⚠️ verify |
| PTY capture | `portable-pty` (wezterm, MIT — wraps ConPTY on Windows, openpty on macOS) | 🔵 |

### ⚠️ Git library — verify before committing

The substrate is **push/fetch of `refs/junto/*` to a forge** (see `docs/architecture.md` §Substrate). The deciding question for the git library is whether it can do **authenticated push/fetch of custom refspecs, cross-platform**.

- **`gix` (gitoxide)** — pure Rust, MIT/Apache, no C toolchain (a real Windows win), zero licensing ambiguity. **Preferred *if* it supports authenticated custom-refspec push today.** Verify this first — gix's push of custom refspecs over https/ssh has historically lagged its fetch.
- **`git2` (libgit2)** — mature push/fetch. libgit2 is **GPLv2 *with a linking exception*** — linking it into an MIT binary is explicitly permitted and is *not* "incorporating source," so it does **not** violate junto's MIT constraint. This is the fallback if gix push isn't ready.

Do not record this as settled until the gix-push capability is checked.

## Hard constraints (do not violate)

1. ✅ **MIT, greenfield, no copyleft *source*.** Reuse *ideas/patterns* (git-refs-as-record, forge-as-hub sync, pty-capture-to-artifact) clean-room; never copy copyleft source. Linking a permissively- or linking-exception-licensed library is fine (see git note); vendoring GPL source is not.
2. ✅ **Terminal-less for humans.** Humans never see or drive a shell. **Agents do** run shells under the hood — their output is **captured as verifiable Artifacts (diffs, logs, charts), never rendered as scrollback.** This is why cross-platform PTY capture matters.
3. ✅ **Durable record = git refs** (`refs/junto/*`, partitioned by author). Append-only; **no CRDT** — concurrent writes interleave by `(ts, author)`. Dedicated refs, never working-tree files (no `git status` pollution). The record holds **decisions/intent + provenance + digests, not raw agent transcripts** (those live outside the repo).
4. ✅ **Vendor-neutral by adapters.** Every external dependency sits behind a swappable adapter; **no vendor name reaches the kernel** — branch on **capability flags, not vendor identity**.
5. ✅ **Kernel ↔ Playbook seam.** **No playbook-specific logic in the kernel.** The kernel is generic (Channel · Member/Party · Message · Artifact · Provenance · Agent Session · Gate engine · Ledger · Outcome · Event). A Playbook *supplies* its Lifecycle, **gate-routing function** (the single most playbook-specific thing), Verifier, offered tools/agents, and artifact renderers.

## Cross-platform rules (Win + Mac)

- **Never hardcode `/` or `\`.** Use `std::path::Path` / `PathBuf` everywhere. Git refs and ledger paths cross OSes.
- **PTY capture must work on both** — ConPTY (Windows) and openpty (macOS). Use `portable-pty`; don't reach for unix-only `nix`/`fork`/`exec` PTY paths.
- **Normalize line endings** for the git-refs ledger. The same record gets written on both platforms; pin LF (or normalize on read) so CRLF vs LF doesn't corrupt dedup/ordering.
- Prefer cross-platform crates; gate any unavoidable OS-specific code behind `#[cfg(...)]` with both arms implemented.

## Rust conventions

🔵 Defaults drawn from widely-used Rust CLAUDE.md practice — change if you disagree, but they're the safe baseline (Dan doesn't write Rust, so lean idiomatic).

- **Error handling:** `thiserror` for typed errors in **library** crates (the kernel + adapters); `anyhow` for **application/binary** crates (the CLI/daemon top level). Don't leak `anyhow::Error` out of library APIs.
- **No `unwrap()` / `expect()` / `panic!` in library code.** Return a `Result` instead. They're fine in tests, build scripts, and `main`-level setup. If a panic is truly unreachable, `expect("why this can't fail")` with a reason — never bare `unwrap()`.
- **No `unsafe` without a `// SAFETY:` comment** explaining the invariant being upheld. For this project, prefer not having any.
- **Document public items** with `///` doc comments — especially the kernel nouns and adapter traits, since those *are* the ubiquitous language.
- Prefer borrowing (`&str`, `&[T]`) over owned (`String`, `Vec<T>`) in function parameters.
- **Don't restate what the tools enforce.** `rustfmt` owns formatting and `clippy` owns lint style — don't add rules CLAUDE.md can't enforce that the linter already does. Fix clippy warnings rather than `#[allow(...)]`-ing them; if you must allow one, comment why.
- Adapter boundaries are **traits**; vendor implementations live in their own module/crate. A trait method branching on a vendor name is a bug — branch on a `Capabilities` value instead (constraint #4).

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
- [`docs/self-improving-harness.md`](docs/self-improving-harness.md) — the self-improvement Playbook (evals = the crux)
- `docs/worked-example-*.md` — three Playbooks walked end-to-end

When a code decision contradicts a doc, surface the conflict — don't silently diverge. The docs use ✅/🔵/⚠️ tags; respect what's settled vs still open.

## Working conventions

- This is a **Windows** machine, **PowerShell** shell — use PowerShell syntax (`$null`, `$env:VAR`, backtick line-continuation).
- 🔵 Prefix tooling commands with `rtk` per the user's global RTK convention (token-optimized output) — e.g. `rtk cargo build`, `rtk cargo test`, `rtk git status`.
- The "rule of three": **build concrete cases before extracting a framework.** Don't frameworkize an adapter or Playbook seam from a single example — build a few, then extract.
