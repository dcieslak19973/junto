# Design — Native Three-Pane Shell

> Status: design, awaiting review. Topic: replacing the native surface's stacked horizontal
> chrome with a **three-vertical shell** — a collapsible left blade, a center that becomes a real
> `PaneGrid` with arbitrary 2D nesting, and a collapsible right blade. Target is
> `crates/junto-iced` (the native spike), **not** the server-rendered pages redesigned in
> [`2026-06-14-human-surface-redesign-design.md`](2026-06-14-human-surface-redesign-design.md).
>
> Scope is **shell only**: the frame, blade mechanics, and layout persistence. Typography, colour,
> and channel-entry rendering are explicitly out (§8). Reference points named by Dan (2026-08-24):
> xum's three-column workspace, Orca's collapsible left/right blades, and herdr/Zed's
> arbitrary quad pane layout.

## 1. The problem

`crates/junto-iced/src/main.rs` renders its root `view()` (line 2627) as a stack of horizontal
bands sitting on top of the channel workspace:

```
[ tab bar — channels · settings · agents ]        line 2820
[ lineage ribbon — canvas, 48px pinned + 150px scroll ]
[ focus board — cross-channel "needs you" banner ] line 2728
[ adder row — open ▸ combo + new-channel input ]
[ the channel workspace ]
```

Roughly 250–300px of chrome is permanently above every channel, on every screen, whether or not
it is relevant to what you are doing. The channel — the actual unit of inquiry — gets what is
left. Nothing in that stack can be dismissed, so the cost is paid continuously.

The second problem is the workspace itself. Despite storing an `iced::widget::pane_grid::State`,
the crate does not render a `PaneGrid`. Line 74–76 is explicit:

> `pane_grid::State` is used purely as a keyed store of panes; rendering is the custom
> shared-width Columns layout, not a PaneGrid.

with `order: Vec<pane_grid::Pane>` (line 79) as a flat left-to-right list. The center is therefore
strictly **1D** — N equal-width columns, no nesting, no horizontal splits. A quad layout is not
expressible.

## 2. The shape

Three vertical regions. Both blades collapse; the center never does.

```
+----------------+---------------------------+----------------+
| CHANNELS       |  [tab bar stays on top]   | [Artif][Linea] |
|  o junto-dev !2|                           |                |
|  o ui-overhaul |  +----------+----------+  | diff main.rs   |
|  o auth-rework |  | channel  | channel  |  |  +142 -30      |
|                |  |    A     |    B     |  |                |
|  open ▸ [____] |  +----------+----+-----+  | log cargo test |
+===== drag =====+  | channel  | ch | ch  |  |  142 passed    |
| [Attn][Sessions]|  |    C     | D  |  E  |  |                |
|  ! gate    2h  |  +----------+----+-----+  |                |
+----------------+---------------------------+----------------+
   collapsible          real PaneGrid            collapsible
```

**Left blade** — navigation is *pinned* at the top and never toggles away, so switching channels
is always one click. Beneath a draggable divider, a switcher selects **Attention** or **Sessions**.
The pinned/switchable split exists because a pure switcher would hide the channel list whenever
you looked at Attention, costing you navigation to gain a panel.

**Center** — the channel workspace, now a real `PaneGrid` (§4).

**Right blade** — a switcher over **Artifacts** and **Lineage**.

## 3. What moves where

The overhaul is mostly relocation. Every band in §1 already has a natural home:

| Today | Becomes |
|---|---|
| lineage ribbon (~200px, always on) | right blade → **Lineage** view |
| focus board "needs you" banner (line 2728) | left blade → **Attention** view |
| adder row (`open ▸` combo + new channel) | pinned into the left blade's nav footer |
| sessions (today inside each pane) | left blade → **Sessions** view |
| artifacts (inline `ToggleArtifact`/`ArtifactLoaded`) | right blade → **Artifacts** view |
| tab bar (channels · settings · agents, line 2820) | unchanged, stays top-level |
| admin views (`AdminView::Settings`/`Agents`) | unchanged — still replace the workspace wholesale |

The measurable payoff: the center goes from roughly 55% of viewport height to ~90%, and with both
blades collapsed a single channel gets very nearly the whole window.

**Lineage placement is provisional.** Dan (2026-08-24): "I'm not sure about lineage, lets put it
in the right blade for now." Revisit once it has been lived with — the lineage DAG is
window-wide data, and a right blade scoped to the focused channel may prove the wrong home.

## 4. The center — adopting the real PaneGrid

Replace the custom shared-width Columns renderer with `iced::widget::pane_grid::PaneGrid`.

This is **deleting a custom renderer, not adding a dependency**: the state the widget needs is
already stored and maintained. `PaneGrid` supplies, as built-ins, everything the quad layout
requires — splitting on either axis at any depth, drag-to-resize dividers, drag-to-reorder panes,
and maximize/restore.

Consequences to plan for:

- `order: Vec<pane_grid::Pane>` (line 79) becomes redundant — `PaneGrid` owns spatial arrangement.
  Every use site (lines 1011, 1061, 1069, 1076, 1262, 2243, 2685, 2795) must be re-expressed
  against the split tree or against a purpose-specific ordering where iteration order genuinely
  matters.
- The equal-width guarantee of the Columns layout is lost by design; panes get the split tree's
  proportions instead. Per-pane styling (line 3263, "One channel pane as a bordered column") must
  be re-hosted inside `pane_grid::Content`.
- Splitting gains an axis. Today `+ pane` means "add a column"; it becomes "split the focused pane
  horizontally or vertically."

A pane still holds exactly one **channel**. Panes holding sessions or terminals — the thing that
makes herdr's quad compelling — is a distinct question and is out of scope (§8).

## 5. Blade mechanics

**Collapse targets a stub, not zero.** Each blade collapses to a ~24px rail carrying a chevron;
the left rail also carries an attention badge. Rationale: [`docs/attention.md`](../../attention.md)
makes attention the spine of the human surface, and this design puts Attention *inside* the left
blade. A collapse-to-zero would let the user hide the single signal the system exists to surface.
A badge on the stub keeps "3 things need you" legible at all times while still reclaiming the width.

**Click is the primary affordance.** The chevrons toggle their blade. This is what the design
optimises for.

**Two fixed keybindings**, as an accelerant only: `Ctrl/Cmd+B` toggles the left blade,
`Ctrl/Cmd+R` the right — Zed's dock conventions. They are cheap precisely because they add no new
state: the toggle `Message`s already exist for the chevrons, so a key press is a second trigger for
an existing transition and inherits its tests. This introduces the crate's first
`iced::keyboard` subscription, batched into the existing `subscription()` (line 2533).

Explicitly **not** built: rebinding UI, a keymap file, or a shortcut-discovery surface. Dan
(2026-08-24): "I'm not a huge keybinding user (though I know there are people passionate about
them)." If the bindings fight anything during implementation they are the first thing cut; nothing
else in this spec depends on them.

**Dividers.** Three draggable dividers: left blade width, right blade width, and the left blade's
internal nav/switcher split. Each clamps to a minimum so a blade cannot be stranded at an unusable
sliver, and double-click resets it to its default.

## 6. Persistence

A new `<junto-home>/ui.toml`, modelled on the existing `keys.toml` read path
(`junto_home()`, line 6477).

| Persisted | Deliberately not persisted |
|---|---|
| each blade collapsed / expanded | the `PaneGrid` split tree |
| each blade's width | which channels are open |
| active view per blade | scroll positions |
| the left blade's internal divider position | focused pane |

Pane-layout persistence is genuinely useful and genuinely larger — restoring a split tree means
also restoring each pane's channel binding, and deciding what happens when a persisted channel no
longer exists or is no longer visible to this member. It is named here as the obvious follow-on
(§9) rather than allowed to inflate the shell.

Failure behaviour is total: a missing file, an unparseable file, or a partial file all yield
built-in defaults rather than an error surface. Layout state is a convenience; it must never be a
reason the application will not start.

## 7. Testing

`view()` is not unit-testable in Iced, so the design pushes every decidable thing out of it into a
pure `src/shell.rs`.

| Tested — pure logic in `shell.rs` | Accepted as untested |
|---|---|
| collapse/expand transitions per blade | widget rendering |
| active-view selection per blade | drag gesture mechanics |
| divider clamping — minimum widths honoured | pixel layout |
| `ui.toml` serde round-trip | |
| defaults when the file is absent | |
| fallback when the file is corrupt or partial | |

`view()` then becomes a thin projection over tested state. Types carry the invariants per
CLAUDE.md — `BladeView` as an `enum`, blade widths as a clamped newtype, not loose `f32`/`String`.
Tests are written before the module, per the repo's TDD convention.

**CI: `junto-iced` must join it.** The crate is absent from `.github/workflows/ci.yml` — there is a
`desktop` job for `crates/junto-desktop` and no equivalent for iced — yet it already carries ~70
`#[test]`/`#[cfg(test)]` attributes across `main.rs` and `pointing.rs` that have never run on a
pull request. Add a job mirroring the `desktop` job exactly (fmt, clippy `-D warnings`, and test
via `--manifest-path crates/junto-iced/Cargo.toml`), following the established cadence: Linux
canary on PRs, Windows/macOS on merges to main. Without it, the persistence logic this spec
introduces is unverified on every change, exactly like the 70 tests already there.

## 8. Non-goals

- **Typography, colour, spacing, density.** The visual-language pass the 2026-06-14 spec called
  for never landed on the native surface. It is still owed, and it is not this spec.
- **Channel-entry rendering and information architecture.** What an entry card shows, how gates are
  presented, how provenance is displayed — untouched.
- **Panes holding anything but a channel.** Sessions- or terminal-in-a-pane is a separate design.
- **Keybinding customisation.** §5.
- **Pane-layout persistence.** §6, §9.

## 9. Open decisions

**ADR 0018 says this surface should not exist.**
[`0018`](../../adr/0018-human-surface-is-a-desktop-shell-over-the-host.md) is accepted and states
the human surface is a Tauri shell over the host's server-rendered pages, having explicitly
rejected native-widget UI (egui/iced) for *surface-forking* — "every feature built twice, page and
widget" — with the revisit trigger being "only if webview rendering itself becomes the bottleneck."
`crates/junto-iced` remains a self-labeled SPIKE in its own `Cargo.toml`.

Investing a shell overhaul here leaves the record asserting the opposite of what is being built.
Per the repo's convention, an ADR is never edited to change a decision — a **new ADR superseding
0018** is the instrument. That decision is Dan's and is deliberately not made by this spec.
It is cheap to write now and expensive to retrofit after the native surface has accumulated
another overhaul's worth of investment.

**Pane-layout persistence** (§6) — the named follow-on once the shell lands.

**Lineage's home** (§3) — provisional in the right blade, to be revisited from use.

## 10. Considered and rejected

- **Center holds one channel at a time**, left blade as the channel list (the purest xum/Orca
  reading). Rejected: it would retire multi-channel comparison, which the pane workspace exists to
  provide. Dan chose blades wrapping the pane grid.
- **Center as tabs** rather than splits. Rejected for the same reason — it trades side-by-side
  comparison for viewport economy the blades already recover.
- **Left blade as a pure three-way switcher** (Channels / Attention / Sessions). Rejected: it hides
  navigation to show a panel. Nav is pinned instead (§2).
- **Two-level grid** — columns of vertically stacked panes, keeping the custom renderer. Rejected:
  it delivers NxM but not arbitrary nesting, while still costing a renderer rewrite. Adopting the
  real `PaneGrid` is less code and more capability.
- **Collapse to zero width.** Rejected: it can hide the attention signal entirely (§5).
