# Human-Surface Redesign — Implementation Plan (1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the human surface's left-sidebar chrome with the redesigned **top bar + bottom-pinned, windowed lineage strip**, as pure render functions with unit tests, wired onto the index route.

**Architecture:** The human surface is server-rendered HTML in `crates/junto/src/render.rs` (pure `String`-returning functions) served by `crates/junto/src/web.rs` (axum handlers). This plan adds new render functions and a new design-system CSS constant, and re-points the index handler at them. No kernel changes; data comes from the existing `ChannelSummary` / `AttentionGroup` projections (`host.rs`). Lineage edges (diverge/converge) are **out of scope** (a later ADR + plan) — the strip renders flat recency-ordered tracks with the workspace's ambient channel pinned as the main-line.

**Tech Stack:** Rust 2024, axum, server-rendered HTML strings, inline SVG. Tests are `#[test]` string-assertion tests in `render.rs`'s `mod tests` (matching the existing idiom).

**Spec:** [`docs/superpowers/specs/2026-06-14-human-surface-redesign-design.md`](../specs/2026-06-14-human-surface-redesign-design.md). Visual target + interactive reference: [`PROTOTYPE-human-surface.html`](../specs/assets/2026-06-14-human-surface-redesign/PROTOTYPE-human-surface.html).

---

## Plan series (scope map)

- **Plan 1 (this):** design-system CSS, `top_bar`, the windowed `lineage_strip`, wired onto `/`. Ships a visibly-new index.
- **Plan 2:** the **detail pane** — `inquiry_view` (merged inquiry/session: control column + artifact viewer with tabs/pop-out), replacing `channel_html`'s body. Steer stays in the session; **Record a finding** as a deliberate act.
- **Plan 3:** **route scoping & language pass** — workspace switcher behavior (`?w=`), track selection (`?inquiry=`), retire `page_shell`'s rail everywhere (settings/agents/new adopt the top bar), and the plain-language cleanup (hide hashes, drop ADR/"projection" jargon, humanize standing decisions).

Each plan is independently green (`cargo test --workspace`).

---

## File structure

- `crates/junto/src/render.rs` — add `top_bar`, `lineage_strip`, the `LineageModel`/`Track` shaping helper, and a new `APP_CSS` constant. Keep existing functions until Plan 3 retires them. (This file is already large; the redesign functions form a cohesive new section — a future split into `render/` is reasonable but **not** in scope here.)
- `crates/junto/src/web.rs` — re-point `index_page` at the new renderers; add a `?w=` workspace query (defaulting to the first substrate).
- `crates/junto/src/host.rs` — read-only; reuse `ChannelSummary`, `overview()`, `substrate_paths()`.

---

## Task 1: `Track` + `LineageModel` shaping

Turn the flat `&[ChannelSummary]` (scoped to one workspace) into the strip's model: a **main-line** (the ambient channel — the one whose name equals the substrate's directory name, else the oldest/least-recent open channel) pinned at the bottom, and the rest as **branches** newest-first.

**Files:**
- Modify: `crates/junto/src/render.rs` (new section near the existing `focus_board`)
- Test: `crates/junto/src/render.rs` `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn lineage_model_pins_ambient_as_mainline_and_orders_branches_newest_first() {
    use std::path::PathBuf;
    let repo = PathBuf::from("/x/junto-ledger");
    let mk = |name: &str, secs: i64, gates: usize| ChannelSummary {
        id: ChannelId::new(),
        name: Some(name.to_string()),
        substrate: repo.clone(),
        entry_count: 1,
        last_activity: Some(Timestamp::from_millis(secs * 1000)),
        open_gates: gates,
        members: 1,
        latest: None,
        closed: false,
    };
    // ambient channel shares the repo's dir name; others are side tracks.
    let summaries = vec![mk("agent-ui", 30, 0), mk("junto-ledger", 10, 0), mk("ui-redesign", 90, 1)];
    let model = LineageModel::from_summaries(&summaries, &repo);
    assert_eq!(model.mainline.name.as_deref(), Some("junto-ledger"));
    // branches newest-first by last_activity
    let names: Vec<_> = model.branches.iter().map(|t| t.name.as_deref().unwrap()).collect();
    assert_eq!(names, vec!["ui-redesign", "agent-ui"]);
    assert!(model.branches[0].needs_you); // ui-redesign has an open gate
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p junto lineage_model_pins -- --nocapture`
Expected: FAIL — `LineageModel` / `Track` not found.

- [ ] **Step 3: Implement the model**

```rust
/// One track on the lineage strip — a channel rendered as a horizontal line.
/// (Diverge/converge edges arrive with the lineage ADR; here every track is
/// flat.)
pub struct Track {
    pub id: ChannelId,
    pub name: Option<String>,
    pub needs_you: bool,
    pub last_activity: Option<junto_kernel::Timestamp>,
}

/// The strip's model for one workspace: the ambient channel pinned as the
/// bottom main-line, the rest stacked above newest-first.
pub struct LineageModel {
    pub mainline: Track,
    pub branches: Vec<Track>,
}

impl LineageModel {
    /// Shape the scoped summaries. The **main-line** is the ambient channel
    /// (its name equals the substrate's directory name); absent that, the
    /// least-recently-active open channel (the de-facto trunk). The rest are
    /// branches, newest-first.
    pub fn from_summaries(summaries: &[ChannelSummary], substrate: &std::path::Path) -> Self {
        let track = |s: &ChannelSummary| Track {
            id: s.id,
            name: s.name.clone(),
            needs_you: s.open_gates > 0,
            last_activity: s.last_activity,
        };
        let dir = substrate
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let mut open: Vec<&ChannelSummary> = summaries.iter().filter(|s| !s.closed).collect();
        // newest-first
        open.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        let main_idx = open
            .iter()
            .position(|s| s.name == dir)
            .unwrap_or(open.len().saturating_sub(1)); // else the oldest
        let mainline = track(open[main_idx]);
        let branches = open
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != main_idx)
            .map(|(_, s)| track(s))
            .collect();
        LineageModel { mainline, branches }
    }
}
```

- [ ] **Step 4: Run it to confirm it passes**

Run: `cargo test -p junto lineage_model_pins`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/render.rs
git commit -m "render: LineageModel shapes scoped channels into a bottom-pinned strip"
```

---

## Task 2: `lineage_strip` — the windowed, bottom-pinned SVG

Render the model as inline SVG: main-line pinned at the bottom (bold, label below the line), branches stacked upward newest-nearest-spine, windowed to `WINDOW` with a walk-back expander when more exist, the selected track highlighted, a `now` line and time axis.

**Files:**
- Modify: `crates/junto/src/render.rs`
- Test: `crates/junto/src/render.rs` `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn lineage_strip_pins_mainline_windows_branches_and_marks_selection() {
    use std::path::PathBuf;
    let repo = PathBuf::from("/x/junto-ledger");
    let mk = |name: &str, secs: i64, gates: usize| ChannelSummary {
        id: ChannelId::new(), name: Some(name.into()), substrate: repo.clone(),
        entry_count: 1, last_activity: Some(Timestamp::from_millis(secs*1000)),
        open_gates: gates, members: 1, latest: None, closed: false,
    };
    // 1 ambient + 5 branches → 2 hidden behind the expander (WINDOW = 3)
    let s = vec![
        mk("junto-ledger",1,0), mk("ui-redesign",90,1), mk("acp",80,0),
        mk("terminology",70,0), mk("agent-ui",60,0), mk("ci",50,0),
    ];
    let model = LineageModel::from_summaries(&s, &repo);
    let sel = model.branches[0].id; // ui-redesign selected
    let html = lineage_strip(&model, Some(&sel));
    assert!(html.contains("junto-ledger")); // main-line label present
    assert!(html.contains("walk back")); // expander, since hidden > 0
    assert!(html.contains("ui-redesign"));
    // windowed: the 5th branch is hidden by default
    assert!(!html.contains(">ci<"));
    // selection highlight band is emitted
    assert!(html.contains("strip-sel"));
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p junto lineage_strip_pins`
Expected: FAIL — `lineage_strip` not found.

- [ ] **Step 3: Implement `lineage_strip`** (port the validated geometry from the prototype's `buildStrip`: `WINDOW = 3`, `ROW = 30`, spine pinned at the bottom, branches `yOf(i) = spineY - (i+1)*ROW`, expander row at top when `branches.len() > WINDOW`, selected-track highlight `<rect class="strip-sel">`, `now` line, time axis below the spine; main-line label drawn **below** the line. Flat lines only — no diverge/converge paths.)

```rust
const STRIP_WINDOW: usize = 3;
const STRIP_ROW: i32 = 30;

/// The bottom-pinned, windowed lineage strip (`docs/.../redesign` §3.2).
/// `selected` highlights a track. Flat tracks (no lineage edges yet).
pub fn lineage_strip(model: &LineageModel, selected: Option<&ChannelId>) -> String {
    // ... port buildStrip(): compute shown = branches[..WINDOW] unless an
    // ?expanded flag (Plan 3) — for now always the windowed view; emit the
    // expander label "⌃ N older — walk back" when branches.len() > WINDOW.
    // Spine at the bottom; label below the line via a <text y=spineY+22>.
    // Returns a complete <svg>…</svg> string.
    todo!("port from PROTOTYPE-human-surface.html buildStrip()")
}
```

> Implementer: translate `buildStrip` faithfully (it's ~50 lines of SVG string-building); keep class names `strip-sel`, `track`, `mainline`, `now` so the CSS in Task 4 targets them. The `todo!` is a placeholder for the port only — replace it; do not leave it.

- [ ] **Step 4: Run it to confirm it passes**

Run: `cargo test -p junto lineage_strip_pins`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/render.rs
git commit -m "render: lineage_strip — bottom-pinned, windowed, walk-back expander"
```

---

## Task 3: `top_bar` — wordmark · workspace switcher · live status · identity

**Files:**
- Modify: `crates/junto/src/render.rs`
- Test: `crates/junto/src/render.rs` `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn top_bar_shows_workspace_live_status_and_identity() {
    use std::path::PathBuf;
    let workspaces = vec![PathBuf::from("/x/junto-ledger"), PathBuf::from("/x/wmux")];
    let html = top_bar(&workspaces, &PathBuf::from("/x/junto-ledger"), 1, 2, Some("Dan Cieslak"));
    assert!(html.contains("junto-ledger"));      // active workspace
    assert!(html.contains("wmux"));              // switcher offers the other
    assert!(html.contains("1 agent"));           // live status
    assert!(html.contains("2 gate"));            // gate count
    assert!(html.contains("Dan Cieslak"));       // identity
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p junto top_bar_shows`
Expected: FAIL — `top_bar` not found.

- [ ] **Step 3: Implement `top_bar`**

```rust
/// The redesigned top bar: wordmark, workspace switcher (scopes the strip),
/// live status (agents working · gates awaiting you), identity.
pub fn top_bar(
    workspaces: &[std::path::PathBuf],
    active: &std::path::Path,
    agents_working: usize,
    open_gates: usize,
    identity: Option<&str>,
) -> String {
    let name = |p: &std::path::Path| {
        p.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    let mut menu = String::new();
    for w in workspaces {
        use std::fmt::Write as _;
        let _ = write!(menu, "<a href=\"/?w={}\">{}</a>",
            escape_html(&w.display().to_string()), escape_html(&name(w)));
    }
    let who = identity.map(escape_html).unwrap_or_default();
    format!(
        "<header class=\"topbar\"><a class=\"brand\" href=\"/\"><span class=\"logo\">j</span>junto</a>\
         <div class=\"ws\"><button class=\"ws-cur\">▣ {active}</button><div class=\"ws-menu\">{menu}</div></div>\
         <span class=\"spacer\"></span>\
         <span class=\"live\">{agents_working} agent{ap} working · {open_gates} gate{gp} need{gn} you</span>\
         <span class=\"who\"><span class=\"pa\">D</span>{who}</span></header>",
        active = escape_html(&name(active)),
        ap = if agents_working==1 {""} else {"s"},
        gp = if open_gates==1 {""} else {"s"},
        gn = if open_gates==1 {"s"} else {""},
    )
}
```

- [ ] **Step 4: Run it to confirm it passes**

Run: `cargo test -p junto top_bar_shows`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/junto/src/render.rs
git commit -m "render: top_bar — wordmark, workspace switcher, live status, identity"
```

---

## Task 4: `APP_CSS` design-system constant

The new dark design system (the palette/type from the prototype). Not unit-tested (CSS) — verified visually in Task 5. Keep the existing `CSS` constant until Plan 3 retires the old pages.

**Files:**
- Modify: `crates/junto/src/render.rs`

- [ ] **Step 1: Add the constant** (port the `:root` variables, `.topbar`, `.ws`, `.strip`/`svg .mainline`/`.track`/`.strip-sel`, `.now` rules from `PROTOTYPE-human-surface.html`'s `<style>`).

```rust
/// The redesigned human surface's stylesheet (`docs/.../redesign`). Ported
/// from the prototype; replaces `CSS` for the new pages.
const APP_CSS: &str = "/* ...ported design system... */";
```

- [ ] **Step 2: Compile**

Run: `cargo check -p junto`
Expected: clean (an unused-const warning is fine until Task 5 wires it).

- [ ] **Step 3: Commit**

```bash
git add crates/junto/src/render.rs
git commit -m "render: APP_CSS design-system constant (ported from the prototype)"
```

---

## Task 5: wire the new index (`new_index_html` + handler)

Compose `top_bar` + `lineage_strip` + the existing `focus_board` into a new index, scoped to one workspace, and point `index_page` at it.

**Files:**
- Modify: `crates/junto/src/render.rs` (add `new_index_html`)
- Modify: `crates/junto/src/web.rs` (`index_page`)
- Test: `crates/junto/src/render.rs` + `crates/junto/src/web.rs`

- [ ] **Step 1: Write the failing render test**

```rust
#[test]
fn new_index_html_frames_top_bar_and_strip() {
    use std::path::PathBuf;
    let repo = PathBuf::from("/x/junto-ledger");
    let s = vec![ChannelSummary{ id: ChannelId::new(), name: Some("junto-ledger".into()),
        substrate: repo.clone(), entry_count:1, last_activity: Some(Timestamp::from_millis(1000)),
        open_gates:0, members:1, latest:None, closed:false }];
    let model = LineageModel::from_summaries(&s, &repo);
    let html = new_index_html(&[repo.clone()], &repo, &model, &[], None);
    assert!(html.contains("class=\"topbar\""));
    assert!(html.contains("<svg"));        // the strip
    assert!(html.contains(APP_CSS_MARKER)); // uses the new stylesheet
}
```

(Add `pub const APP_CSS_MARKER: &str = "redesigned human surface";` as the first comment token inside `APP_CSS` so the test can assert the new CSS is in use without matching the whole sheet.)

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p junto new_index_html_frames`
Expected: FAIL — `new_index_html` not found.

- [ ] **Step 3: Implement `new_index_html`**

```rust
/// The redesigned index: top bar + lineage strip + the focus board, for one
/// workspace.
pub fn new_index_html(
    workspaces: &[std::path::PathBuf],
    active: &std::path::Path,
    model: &LineageModel,
    attention: &[AttentionGroup],
    identity: Option<&str>,
) -> String {
    let agents_working = 0; // sessions wiring lands in Plan 2
    let open_gates: usize = attention.iter().flat_map(|g| &g.items)
        .filter(|i| matches!(i.kind, crate::host::AttentionKind::Gate)).count();
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>junto</title><style>{APP_CSS}</style></head><body>{bar}{strip}\
         <main class=\"pane\">{board}</main></body></html>",
        bar = top_bar(workspaces, active, agents_working, open_gates, identity),
        strip = lineage_strip(model, None),
        board = focus_board(attention, "/"),
    )
}
```

- [ ] **Step 4: Run it to confirm it passes**

Run: `cargo test -p junto new_index_html_frames`
Expected: PASS.

- [ ] **Step 5: Re-point the handler** (`web.rs` `index_page`): pick the active workspace from `?w=` (default: first `substrate_paths()`), scope summaries to it, build the model, render `new_index_html`.

```rust
#[derive(Debug, serde::Deserialize)]
struct IndexQuery { #[serde(default)] w: String }

async fn index_page(
    State(host): State<Arc<Host>>,
    axum::extract::Query(q): axum::extract::Query<IndexQuery>,
) -> Response {
    let substrates = host.substrate_paths().unwrap_or_default();
    let active = if q.w.is_empty() {
        substrates.first().cloned()
    } else {
        Some(std::path::PathBuf::from(&q.w))
    };
    let Some(active) = active else {
        return Html(render::new_index_html(&[], std::path::Path::new(""),
            &render::LineageModel { mainline: render::Track::empty(), branches: vec![] }, &[], None))
            .into_response();
    };
    match host.overview().await {
        Ok((summaries, attention)) => {
            let scoped: Vec<_> = summaries.into_iter().filter(|s| s.substrate == active).collect();
            let model = render::LineageModel::from_summaries(&scoped, &active);
            let identity = crate::host::git_user(&active).ok();
            let who = identity.as_ref().map(|m| m.display_name.as_str());
            Html(render::new_index_html(&substrates, &active, &model, &attention, who)).into_response()
        }
        Err(err) => internal(format!("listing channels: {err}")),
    }
}
```

> Add `Track::empty()` (a placeholder main-line for the no-substrate case) in `render.rs`. The route still needs the `Query` import; everything else (`/new`, `/settings`, `/channels/*`) is untouched this plan.

- [ ] **Step 6: Run the suite + lint + fmt**

Run: `cargo fmt --check; cargo clippy -p junto --all-targets -- -D warnings; cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Visual check**

Run: `cargo run -p junto -- serve` and open `http://127.0.0.1:1727/`. Expect: top bar with the workspace switcher, the bottom-pinned windowed strip, the focus board below. Switch `?w=` to confirm scoping.

- [ ] **Step 8: Commit**

```bash
git add crates/junto/src/render.rs crates/junto/src/web.rs
git commit -m "web: redesigned index — top bar + lineage strip + focus board, scoped by workspace"
```

---

## Self-review notes

- **Spec coverage (Plan 1 slice):** top bar §3.1 ✓ (Task 3); lineage strip §3.2 bottom-pinned/windowed/walk-back ✓ (Tasks 1–2); workspace scoping §2.5/§3.1 ✓ (Task 5); design language/CSS §2 ✓ (Task 4). Detail pane §3.3, input split §4, language pass §5, retiring `page_shell` everywhere → **Plans 2–3** (intentional).
- **Lineage-edges caveat:** redesign-first renders flat tracks; the bold main-line is the ambient channel, not a real lineage spine. Diverge/converge edges + the full map are a separate ADR + plan (spec §7).
- **Type consistency:** `LineageModel{mainline,branches}`, `Track{id,name,needs_you,last_activity}`, `lineage_strip(&model, Option<&ChannelId>)`, `top_bar(workspaces, active, agents_working, open_gates, identity)`, `new_index_html(workspaces, active, model, attention, identity)` — used consistently across tasks.
- **Open:** `agents_working` is hard-wired to 0 until the sessions count is threaded in Plan 2; flagged in Task 5.
