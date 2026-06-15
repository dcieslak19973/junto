# Design — Human-Surface Redesign

> Status: design, awaiting review. Topic: a ground-up redesign of junto's **human surface**
> (the server-rendered pages in `crates/junto/src/render.rs` + routes in `web.rs`) to give it a
> point of view. Scope is **redesign-first**: the look, language, and information architecture,
> buildable against today's flat-channel data. **Channel lineage** (diverge/converge) is the
> designed successor and gets its own ADR + spec (see §7).
>
> Mockups (the source of truth for the visual target):
> [`assets/2026-06-14-human-surface-redesign/overview.png`](assets/2026-06-14-human-surface-redesign/overview.png)
> · [`session-blade.png`](assets/2026-06-14-human-surface-redesign/session-blade.png)
> · live HTML alongside them (`mock-overview.html`, `mock-session-blade.html`).

## 1. The problem (why redesign)

The current human surface was built as **instrumentation by the people who understand the
internals**, so it exposes the internals. Reviewing the live pages with Dan (2026-06-14), four
complaints all landed — and all trace to one root cause: **no design point of view.**

- **Reads like a debug log** — `read-only projection`, `code-PR push-gate (verify loop)`, inline
  `docs/adr/0023/0024` citations, 8-char entry hashes on every line, walls of prose.
- **Flat / hard to scan** — uniform weight, tiny grey all-caps labels; nothing tells you where to
  look.
- **Generic, no personality** — a novel system wearing a stock dark-admin skin.
- **Wrong things prominent** — the "needs you" board (the attention router, the *actual point* per
  `docs/attention.md`) is a single faint line when empty, while cards/forms/warnings dominate.

The fix is not to patch each symptom; it is to **decide what the surface is for a human** and let
language, hierarchy, and layout follow. This spec is that decision.

## 2. Design principles (the point of view)

1. **Speak the human's language, not the ledger's.** No entry hashes, no `refs/junto/*`, no
   "projection", no ADR citations, no playbook jargon in the chrome. "A gate — Claude has been
   waiting 2h", not "Proposal [pending] @1718…". Internal ids are available on demand (a details
   disclosure, a copy affordance), never the default surface.
2. **A living workspace, not an admin panel.** Dense-but-legible, agent presence is felt (a pulsing
   "1 agent working now"), real typographic hierarchy. Reference register: Mux / a good IDE — power
   without clutter. (Chosen over a calm-editorial and a conversational/feed direction; see §8.)
3. **Attention is the spine.** What needs you is the first thing rendered and the loudest thing on
   the page, surfaced *where the eye already goes* — including as glyphs on the lineage strip — not
   as a list you scroll to.
4. **One thing has one home.** Navigation, machine config, and identity are separate concerns with
   separate places — not co-mingled rows in a "MACHINE" group.
5. **Scope before sprawl.** A **workspace** (a registered substrate/repo) scopes the working view;
   the global summary still spans all. (Dan preferred the scoped model over everything-at-once.)

## 3. Layout — the canonical shape

Three fixed regions, top to bottom (see `overview.png`):

```
┌─────────────────────────────────────────────────────────────┐
│ TOP BAR   j junto   [▣ junto-ledger ▾]   …   ● 1 working  D   │  identity + scope + live status
├─────────────────────────────────────────────────────────────┤
│ LINEAGE STRIP   (horizontal; scoped to the workspace)         │  orient: what exists, what's live,
│   junto-dev ●──────────────────────────────────────● now      │  what needs you (glyphs on tracks)
│   …other inquiries as tracks…                                 │
├─────────────────────────────────────────────────────────────┤
│ DETAIL PANE   (the selected inquiry — a blade stack)          │  work: cards → zoom into blades
└─────────────────────────────────────────────────────────────┘
```

There is **no left sidebar.** The lineage strip *is* the navigation, and it lives **on top, always**
— in both the overview and the zoomed states (a fixed home; the nav never relocates). This directly
retires the current `page_shell` left rail, whose contents and purpose Dan found uniformly wrong.

### 3.1 Top bar

- **Wordmark** (links home / clears selection).
- **Workspace switcher** — the registered substrate in focus (`▣ junto-ledger ▾`). Scopes the
  lineage strip and the detail pane. Replaces the current sidebar's per-substrate grouping.
- **Live status** — "1 agent working · N gates need you", with a pulsing presence dot.
- **Identity** — who you act as (`D` Dan Cieslak), opening a menu to **Agents** and **Settings**
  (which today are orphan nav rows). New-inquiry is a **+** affordance on the strip, not a nav row.

### 3.2 Lineage strip

A horizontal band scoped to the active workspace. Time runs left→right, newest on the right, with
a dashed **now** line. Status is carried *on the track*: a **live/needs-you** track glows (amber)
with its glyph at the now-cap; quiet tracks are muted; the selected track is highlighted.

**Bottom-pinned, windowed (validated in the prototype):** the workspace's **main-line** (its spine —
e.g. `junto-dev`) is **pinned at the bottom** of the strip, its label below the line. Side-quests
stack **upward** from it — newest nearest the spine, older higher. **Nothing ever renders below the
main-line.** Only a few tracks (~3) show by default; an **expander at the top** ("⌃ N older
side-quests — walk back") pages older history in, growing the strip taller. This is how the strip
**scales** without sprawling: recent work is always in view at the bottom; history is one expand
away upward. (Open: the ordering rule when several side-quests converge close together — §9.)

**Redesign-first rendering (this spec):** tracks are independent horizontal lines (one per open
channel in the workspace), labelled, with attention glyphs and the now line. There are **no
diverge/converge edges yet** — the data model has no channel lineage today (§7). The strip is built
*lineage-ready*: a track component, a time axis, selection → detail. When lineage lands, the same
strip gains branch/merge edges (and a "map" expansion for the full 2D DAG) with no IA change.

Clicking a track selects it and fills the detail pane below. The strip stays put.

### 3.3 Detail pane — the inquiry, two columns

Selecting a track fills the pane with that **inquiry** directly. There is **no separate "session"
zoom** — prototyping showed the inquiry view and the session view are the same content, so they
**merge**: for an inquiry with a live session, the inquiry view *is* the session view. Header
(name · status chips · **＋ Record a finding** · `converge → …`), a sub-line ("diverged from
junto-dev 8h ago · Claude Code"), a progress line, then **two columns**:

- **Left — control / conversation:** *Needs you* (the gate the session raised, inline, with
  approve/reject), *Turns* (recent agent activity), and the **steer** box (§4). For an idle inquiry
  this column instead offers *Start work* (and *Standing decisions*).
- **Right — artifact viewer:** the artifact the work produced. The artifact **list is the tab row**;
  it **defaults to the latest artifact the last turn generated** (e.g. `diff · render.rs`, tagged
  *last turn*), and **⤢ pop out** expands the selected artifact to a full overlay. Switching tabs
  swaps the viewer (diffs render inline; image artifacts like mockups show in-place and pop out).

The lineage strip stays above the pane, so "where am I" never depends on the pane. See
`session-blade.png` for the earlier zoomed form and the **interactive prototype**
(`PROTOTYPE-human-surface.html`) for the validated merged layout.

## 4. Inputs — steering vs. recording (two acts, two homes)

The current global bottom "steer / add a finding" bar conflates two different acts. They split:

- **Steer a session** is *contextual to one running agent*. It lives **inside the session blade**
  ("◗ Steer this session" → "Send to Claude"). There is no global steer bar.
- **Record a finding** is a *deliberate ledger act* (it carries a decision frame; `docs/adr/0019`).
  It is a header action on the inquiry (**＋ Record a finding**) that opens a compose flow, not a
  persistent catch-all input.

This also makes the page honest: you steer *an agent*, you record *to the channel* — never an
ambiguous box that might do either.

## 5. Language & content rules (applies to every string the human sees)

- Entry ids: hidden by default; available behind a disclosure / copy button. Never inline on a line.
- No `docs/adr/*` paths, no "projection", no `refs/junto/*`, no "push-gate (verify loop)". Describe
  the *thing* ("a gate", "a claim awaiting your call", "converged back to junto-dev").
- Time as human cues ("2h ago", "blocked 2h"), already present via `ago()` — keep and extend.
- Members shown by name + a quiet `(agent)`/avatar marker, not `Name <email>` raw.
- Standing decisions rendered as readable sentences with a small provenance footnote, not a hash
  prefix + wall of prose.

## 6. Code impact (orientation, not a plan)

- `crates/junto/src/render.rs` — the big one. Retire `page_shell`'s left rail; introduce
  `top_bar`, `lineage_strip`, and the `detail blade` renderers. `index_html` / `channel_html`
  collapse into "strip + selected detail". Keep the existing `ChannelView` projection as the data
  source — this is a *rendering* change, not a kernel change.
- `crates/junto/src/web.rs` — routes for workspace scoping and blade selection (query params or
  paths); the act endpoints (ratify/approve/etc.) stay.
- `host.rs` (`ChannelSummary`, `AttentionGroup`) — likely reused as-is; the strip needs per-track
  status (live/quiet/needs-you), which the summaries largely already carry.
- CSS — a real design system (type scale, color roles, the `--accent/--warn/--ok` palette in the
  mocks) replacing the current ad-hoc styles. Still server-rendered, minimal JS (blade zoom and the
  switcher are progressive-enhancement; `<details>`/links degrade).

## 7. Scope & sequencing — lineage is a separate effort

The mockups show **diverge/converge** (side-quests branching off a spine and merging back). Per
`docs/attention.md` these verbs are **settled in design but not built**: the kernel has channels
with no parent/child edges, and the lifecycle entry kinds ("diverged / closed / converged",
anticipated by `adr/0016`) do not exist. Therefore:

- **This spec (build now):** the redesign — look, language, IA, top bar, lineage strip *without
  edges*, blade stack, input split. Ships against today's data and fixes all four §1 complaints.
- **Follow-on (separate ADR + spec):** **channel lineage** — the kernel `diverge`/`converge` entry
  kinds + the lineage DAG, then the strip's branch/merge edges and the full-DAG **map** expansion
  (`assets/.../lineage-full-map.png` previews the eventual map). This is a data-model change and
  must be designed with divergence per `attention.md` §"Convergence", not bolted on.

Building the redesign first de-risks the look and delivers value without waiting on kernel work.

## 8. Alternatives considered

- **Calm command center** (Linear/Things — one centered column, restraint). Safest, ages well, but
  least personality; rejected as not enough of a *point of view* for a novel system.
- **Conversation / feed** (Franklin's-Junto, editorial serif, items as sentences). Most distinctive
  and on-thesis, but the riskiest and weakest for dense daily work. Held as a possible *voice*
  influence, not the base.
- **Living workspace** — chosen (§2.2).
- **Rail orientation:** everything-visible left rail and a workspace-switcher left rail were both
  mocked; the switcher won, then the left rail was dropped entirely in favor of **top strip always**
  (one fixed nav home; full width for detail blades).

## 9. Open questions

- **Strip overflow:** resolved by the bottom-pinned **windowed** strip (§3.2) — ~3 tracks + an
  expander. Remaining: the exact **ordering rule** when several side-quests converge close together
  (by last-activity? by diverge point?), and the **expand/collapse animation** (the strip changes
  height).
- **Empty / first-run states:** workspace with no channels; brand-new install with no substrate.
- **Selection persistence:** does the selected inquiry survive navigation / reload (query param vs.
  server session)?
- **Record-a-finding flow:** the compose UI for a decision frame (2–4 options) is referenced but
  not designed here — likely its own small design pass.
- **Responsive:** desktop-shell-framed today (`docs/adr/0018`); narrow widths out of scope for v1.
