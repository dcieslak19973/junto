# Design — Agent Personas

> Status: design, awaiting review. Topic: the human-surface UX for creating/configuring
> reusable agent **personas**, and wiring them into channel launches over ACP.
> Builds on the harness registry (PR #36, merged) and ADR 0024 (ACP is the harness protocol).

## 1. What a persona is (terminology)

A **Persona** is a new noun in the ubiquitous language: a **named, reusable, machine-local
configuration over a Harness**. It is the thing a human actually picks when starting work — the
Harness is the *engine* it runs on, not a peer.

- **Harness** = the engine/adapter junto can drive (Claude Code, OpenCode). One per adapter; a
  machine capability; shown on the settings page. Unchanged from the registry in
  `crates/junto/src/launch.rs`.
- **Persona** = a named config that *references* a harness plus: a role/system-prompt, an optional
  model, MCP servers, and (Claude-only) skills + plugin marketplaces. **One harness → many
  personas** (a `security-reviewer` and a `doc-writer` can both run on Claude with different config).
  That one-to-many is why config can't live on the harness — the harness is a singleton engine.
- **Orthogonal to Playbook.** Playbook = the *work type* (how work is verified/gated). Persona =
  *who/what does it*. Don't conflate.

Persona joins `docs/domain-model.md`. It is a config layer, not a competitor to `Member` — see §3.

**Scope decision (settled): full.** A v1 persona carries role + harness + model + MCP servers +
skills + marketplaces. Investigation (see §6) confirmed every field has a delivery path over ACP,
so there is no "stored-but-inert" compromise.

## 2. Data model — `~/.junto/personas.toml`

Machine-local, mirroring the `WorkspacesFile` / `HarnessSessionsFile` idiom in `launch.rs`
(serde structs, load → find → save). **Not ledger content** — config is a machine fact and does
not sync. The persona's *identity* lands in the ledger when it authors work (§3); its *config*
never does.

```toml
[[personas]]
slug = "security-reviewer"                  # kebab; stable id + member-email stem; immutable
name = "Security Reviewer"                  # display label
harness = "claude"                          # FK to the harness registry (claude | opencode)
email = "security-reviewer@junto.local"     # the persona's own agent Member identity (§3)
role = "You are a meticulous security reviewer. ..."   # system-prompt; optional
model = "claude-opus-4-8"                    # optional model override
mcp_servers = [                              # → ACP session/new mcpServers
  { name = "junto", url = "http://127.0.0.1:1727/mcp" },
]
skills = ["security-review", "diagnose"]     # Claude-only; harness-specific
marketplaces = ["github:owner/repo"]         # Claude-only; plugin marketplaces
```

Rust shape (in a new `crates/junto/src/persona.rs` module, keeping `launch.rs` focused):

```rust
struct PersonasFile { personas: Vec<Persona> }

struct Persona {
    slug: String,                 // newtype PersonaSlug if it earns its keep
    name: String,
    harness: String,              // harness id
    email: String,
    role: Option<String>,
    model: Option<String>,
    mcp_servers: Vec<McpServer>,  // { name, url } (+ later: command/env variants)
    skills: Vec<String>,
    marketplaces: Vec<String>,
}
```

Functions mirror the workspace store: `personas_path`, `load_personas`, `save_persona`,
`persona_by_slug`, `all_personas`, `delete_persona`.

**Stock personas.** On first read, if `personas.toml` is absent/empty, seed one stock persona per
registered harness (e.g. `Claude Code` → harness `claude`, empty config). Guarantees the launch
picker is never empty and gives users something to clone. Stock personas are ordinary rows once
written; the seed only fills an empty store.

## 3. Persona identity & one-agent-per-channel

**Each persona is its own agent `Member`** — `Member::agent(name, email)` with
`email = <slug>@junto.local`. When the persona authors work, the ledger shows the persona
(`security-reviewer@junto.local`), not the generic engine — provenance stays meaningful.

The one-agent-per-channel resolver generalizes. Today `channel_harness(party)` finds the harness
whose member email is in the Party. New: **`channel_persona(party)`** finds the persona whose
`email` is in the Party, and from `persona.harness` derives the harness to drive. The launch path:

1. If a persona is already established in the channel (its member in the Party) → reuse it; picker
   hidden.
2. Else the form's selected persona becomes the channel's persona; auto-grant **the persona's
   member** via the existing founder-grant path (`host.add_member`, same as the current harness
   auto-grant in `web.rs::launch_session`).

`harness_member()` (back-compat) and the current `channel_harness` stay until callers migrate;
`channel_persona` is the new primary resolver.

## 4. Human-surface UI

Server-rendered HTML, dark theme, no SPA — matches `render.rs`. New surfaces:

- **`/personas`** route + `personas_page` handler (`web.rs`) + `personas_html` (`render.rs`),
  mirroring `new_page`/`settings_page`. Sidebar link next to `⚙ settings` in `page_shell`.
  Lists personas (name · harness · one-line config summary) with create / edit / delete.
- **Create/edit form** — `POST /personas` (create), `POST /personas/<slug>` (edit),
  `POST /personas/<slug>/delete`. Fields: name, harness `<select>` (from `all_harnesses()`), role
  (textarea), model (text), MCP servers (repeatable rows), and — revealed **only when harness =
  claude** — skills + marketplaces (repeatable rows). `slug` derived from name on create, immutable
  after (it stems the member email).
- **Launch picker switches from harnesses to personas.** In `render.rs` the start-work
  `<select name="harness">` becomes `<select name="persona">` listing `all_personas()`, shown only
  when no persona is established (`channel_persona(party).is_none()`). `LaunchForm.harness` →
  `LaunchForm.persona` (slug).

## 5. Launch wiring (the persona → ACP turn)

`launch_session` (web.rs) resolves the persona (established or form-selected), auto-grants its
member, then `launch()` / `run_turn_acp()` thread the persona's config into the ACP exchange.
`run_turn_acp` in `acp.rs` currently sends `session/new` with `{cwd, mcpServers: []}` and
`session/prompt` with plain text. Changes:

- **MCP servers** → populate `session/new` `mcpServers` from `persona.mcp_servers`.
- **Role** → `session/new` `_meta.systemPrompt` (string replaces, or `{append}` to extend the
  `claude_code` preset — see §6). Replaces the earlier prompt-prepending idea.
- **Model** → `_meta.claudeCode.options.model` (the adapter spreads `_meta.claudeCode.options` into
  the SDK options) or the `ANTHROPIC_MODEL` env on the spawned adapter.
- **Skills + marketplaces** → set **`CLAUDE_CONFIG_DIR`** env on the adapter spawn to an
  **isolated per-persona config dir** (`~/.junto/personas/<slug>/claude-config/`) that junto
  populates with a `settings.json` (enabled plugins + marketplaces) and the persona's skills. The
  adapter hardcodes `settingSources: ["user", "project", "local"]`, and the "user" source is
  `CLAUDE_CONFIG_DIR`, so Claude Code loads exactly the persona's declared skills/marketplaces and
  nothing from the operator's real `~/.claude` (isolation decision, §1).

These `_meta.claudeCode.*` fields and the config-dir lever are **Claude-adapter-specific**. The
persona carries its harness id, so junto sends Claude-specific `_meta`/env only for `claude`
personas. OpenCode personas wire role/MCP/model through OpenCode's own ACP surface; skills/
marketplaces are Claude concepts and simply aren't offered for OpenCode personas (the form hides
those fields). `run_turn_acp`'s new persona parameter is therefore harness-aware.

## 6. Why full scope is wireable (investigation record)

Read `@agentclientprotocol/claude-agent-acp` v0.44.0 (`dist/acp-agent.js`, wrapping
`@anthropic-ai/claude-agent-sdk` 0.3.170). Findings, with line refs:

- `_meta.systemPrompt` overrides the system prompt — string or `{append, ...}` preset options
  (L1898–1914).
- `mcpServers` from `session/new` is forwarded to the SDK (L1872–1898).
- `_meta.claudeCode.options` is spread directly into SDK options (`...userProvidedOptions`, L1950)
  — arbitrary SDK options incl. `model`; env `ANTHROPIC_MODEL` / `CLAUDE_MODEL_CONFIG` also resolve
  the model (L1923, L2528).
- `settingSources: ["user", "project", "local"]` is always on (L1948); the **user** source is
  `CLAUDE_CONFIG_DIR` (env, default `~/.claude`, L12). → per-persona config dir delivers
  skills/marketplaces.

**To confirm during implementation:** the exact `settings.json` keys Claude Code reads to enable
plugins and register marketplaces (don't hand-wave them in code). Re-probe with a throwaway Node
script (handoff §6 pattern) if the schema is unclear.

## 7. Testing

Follow the existing `web.rs` test idiom (in-memory host, stubbed harness via `JUNTO_HARNESS_CMD`):

- `personas.toml` round-trip: save → load → find by slug; stock-seed on empty store.
- `channel_persona(party)` resolves the established persona and derives its harness; returns `None`
  when no persona member is present.
- `launch_session` with a `persona` form field auto-grants the **persona's** member (a founder-
  authored `MemberAdded` for `<slug>@junto.local`), reuses an established persona, and rejects a
  non-founder grant (same as the current harness tests).
- A persona-config → ACP mapping unit test: given a `Persona`, assert the `session/new` params
  (`mcpServers`, `_meta.systemPrompt`) and the resolved `CLAUDE_CONFIG_DIR` / model env for a
  `claude` persona; assert OpenCode personas omit the Claude-only fields.

Cross-platform: `CLAUDE_CONFIG_DIR` and `personas.toml` paths via `PathBuf::join`, never string
concatenation. Per-persona config-dir writes pin LF.

## 8. Deferred / out of scope (v1)

- **Shareable / repo-committed personas** (team-shared). v1 is machine-local; syncing personas
  drags in the "config has local paths" problem. Known later fork.
- **No secret vault.** Personas carry config, never credentials. Auth stays with the harness
  (ADR 0024). MCP servers that need auth rely on the harness's own auth, as today.
- **OpenCode skills/marketplaces.** Not offered (Claude concepts). OpenCode model/MCP/role only.
- **A persona ADR.** Likely warranted (new domain noun) — write it once the model is built and
  proven, not before (rule of three / don't frameworkize a single example prematurely).

## 9. Build sequence

1. `persona.rs`: `Persona` + `PersonasFile` + store fns + stock-seed (test-pinned).
2. `channel_persona` resolver + the per-persona member identity; keep `channel_harness` until
   callers migrate.
3. `/personas` page + create/edit/delete handlers + `personas_html` form (harness-aware fields).
4. Launch picker: harness `<select>` → persona `<select>`; `LaunchForm.harness` → `.persona`.
5. Wire `launch()` / `run_turn_acp()` to thread persona config (MCP + `_meta.systemPrompt` + model
   + per-persona `CLAUDE_CONFIG_DIR`), harness-aware.
6. Confirm the Claude `settings.json` plugin/marketplace keys; populate the per-persona config dir.
