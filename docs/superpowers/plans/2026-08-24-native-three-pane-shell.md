# Native Three-Pane Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the native surface's stacked horizontal chrome with a three-vertical shell — collapsible left and right blades around a center that becomes a real `PaneGrid` with arbitrary 2D nesting.

**Architecture:** All decidable layout state moves into a new pure module `crates/junto-iced/src/shell.rs` (typed, clamped, serde-backed, fully unit-tested); `view()` becomes a thin projection over it. The chrome bands currently stacked above the workspace relocate into blade views. The custom shared-width Columns renderer is deleted in favour of `iced::widget::pane_grid::PaneGrid`, whose state the crate already stores but does not render.

**Tech Stack:** Rust 2024, Iced 0.14 (`advanced`, `canvas`, `markdown` features), serde + toml 0.9 (both already dependencies — no new crates).

**Spec:** [`docs/superpowers/specs/2026-08-24-native-three-pane-shell-design.md`](../specs/2026-08-24-native-three-pane-shell-design.md)

## Global Constraints

- Target crate is `crates/junto-iced` — **its own workspace**. Every cargo command needs `--manifest-path crates/junto-iced/Cargo.toml`; a bare `cargo test --workspace` from the repo root does not touch it.
- Edition 2024, `rust-version = "1.94"`.
- No `unwrap()` / `expect()` / `panic!` in library code; return `Option`/`Result`. Permitted in tests.
- Clippy runs with `-D warnings`; fix warnings rather than `#[allow(...)]`-ing them.
- No new dependencies. `serde` (derive), `serde_json`, and `toml = "0.9"` are already in `Cargo.toml`.
- Doc-comment every public item with a one-line summary; comment the *why*, not the *what*.
- Newtypes over bare `f32`/`String`; `enum`s over stringly-typed state ("make illegal states unrepresentable").
- Test module idiom in this crate: `#[cfg(test)] mod <topic>_tests { use super::…; }` with full-sentence test function names.
- Persistence must never prevent startup: a missing, partial, or corrupt state file yields defaults, never an error surface.
- Out of scope, do not touch: typography, colour, spacing, channel-entry rendering, panes holding anything other than a channel.

---

### Task 1: The pure shell-state module

**Files:**
- Create: `crates/junto-iced/src/shell.rs`
- Modify: `crates/junto-iced/src/main.rs:11` (add `mod shell;` beside `mod pointing;` / `mod popover;`)

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces: `shell::ShellState` with public fields `left_collapsed: bool`, `right_collapsed: bool`, `left_width: BladeWidth`, `right_width: BladeWidth`, `left_view: LeftView`, `right_view: RightView`, `left_split: NavSplit`; methods `toggle_left(&mut self)`, `toggle_right(&mut self)`; newtypes `BladeWidth` (`::new(f32) -> Self`, `::get(self) -> f32`, consts `MIN`/`MAX`/`DEFAULT`) and `NavSplit` (`::new(f32) -> Self`, `::get(self) -> f32`, consts `MIN`/`MAX`/`DEFAULT`); enums `LeftView::{Attention, Sessions}` and `RightView::{Artifacts, Lineage}`.

- [ ] **Step 1: Write the failing tests**

Create `crates/junto-iced/src/shell.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod clamp_tests {
    use super::{BladeWidth, NavSplit, ShellState};

    #[test]
    fn a_width_inside_the_usable_range_is_kept_as_typed() {
        assert_eq!(BladeWidth::new(300.0).get(), 300.0);
    }

    #[test]
    fn a_sliver_width_is_clamped_up_so_a_blade_cannot_be_stranded() {
        // The drag divider must never leave a blade at an unusable width.
        assert_eq!(BladeWidth::new(3.0).get(), BladeWidth::MIN);
    }

    #[test]
    fn an_oversized_width_is_clamped_down_so_the_center_survives() {
        assert_eq!(BladeWidth::new(9000.0).get(), BladeWidth::MAX);
    }

    #[test]
    fn a_nan_width_falls_back_to_the_default_rather_than_propagating() {
        // f32::clamp returns NaN for NaN input, which would poison layout.
        assert_eq!(BladeWidth::new(f32::NAN).get(), BladeWidth::DEFAULT);
    }

    #[test]
    fn the_nav_split_is_clamped_so_neither_half_of_the_left_blade_vanishes() {
        assert_eq!(NavSplit::new(0.0).get(), NavSplit::MIN);
        assert_eq!(NavSplit::new(1.0).get(), NavSplit::MAX);
    }

    #[test]
    fn toggling_a_blade_twice_returns_it_to_where_it_started() {
        let mut state = ShellState::default();
        let before = state.left_collapsed;
        state.toggle_left();
        assert_ne!(state.left_collapsed, before);
        state.toggle_left();
        assert_eq!(state.left_collapsed, before);
    }

    #[test]
    fn the_two_blades_toggle_independently() {
        // Regression guard: one shared flag would collapse both at once.
        let mut state = ShellState::default();
        state.toggle_left();
        assert!(state.left_collapsed);
        assert!(!state.right_collapsed);
    }

    #[test]
    fn both_blades_start_expanded_so_a_first_run_shows_the_whole_shell() {
        let state = ShellState::default();
        assert!(!state.left_collapsed);
        assert!(!state.right_collapsed);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml shell::`
Expected: FAIL — compile error, `cannot find type BladeWidth in this scope` (and `mod shell;` not yet declared).

- [ ] **Step 3: Write the minimal implementation**

Prepend to `crates/junto-iced/src/shell.rs`, above the test module:

```rust
//! Pure layout state for the three-pane shell — blade collapse, widths,
//! active views, and the left blade's internal split.
//!
//! Everything decidable about the shell lives here rather than in `view()`,
//! which Iced gives no way to unit-test. `view()` is a projection over this.

use serde::{Deserialize, Serialize};

/// Which switchable view the left blade is showing beneath the pinned nav.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftView {
    /// Cross-channel "needs you" items — the focus board.
    #[default]
    Attention,
    /// Agent sessions for the focused channel.
    Sessions,
}

/// Which switchable view the right blade is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightView {
    /// Diffs, logs, and charts attached to the channel.
    #[default]
    Artifacts,
    /// The diverge/converge DAG.
    Lineage,
}

/// A blade width in logical pixels, clamped to a range that keeps both the
/// blade and the center usable. Deserialization clamps too, so a hand-edited
/// or corrupt state file cannot strand a blade.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "f32", into = "f32")]
pub struct BladeWidth(f32);

impl BladeWidth {
    /// Narrow enough to be frugal, wide enough to read a channel name.
    pub const MIN: f32 = 180.0;
    /// Beyond this the center stops being the center.
    pub const MAX: f32 = 560.0;
    /// Comfortable for a channel list plus badges.
    pub const DEFAULT: f32 = 280.0;

    /// Clamp `px` into the usable range. A non-finite value (NaN from a
    /// degenerate drag, infinity from a corrupt file) yields the default
    /// rather than propagating into layout.
    pub fn new(px: f32) -> Self {
        if px.is_finite() {
            Self(px.clamp(Self::MIN, Self::MAX))
        } else {
            Self(Self::DEFAULT)
        }
    }

    /// The clamped width in logical pixels.
    pub fn get(self) -> f32 {
        self.0
    }
}

impl Default for BladeWidth {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl From<f32> for BladeWidth {
    fn from(px: f32) -> Self {
        Self::new(px)
    }
}

impl From<BladeWidth> for f32 {
    fn from(width: BladeWidth) -> Self {
        width.0
    }
}

/// The fraction of the left blade given to pinned navigation, the rest going
/// to the switchable view beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "f32", into = "f32")]
pub struct NavSplit(f32);

impl NavSplit {
    /// Below this the channel list stops being usable.
    pub const MIN: f32 = 0.2;
    /// Above this the switchable view stops being usable.
    pub const MAX: f32 = 0.8;
    /// Nav takes a little over half by default.
    pub const DEFAULT: f32 = 0.55;

    /// Clamp `fraction` into the usable range; non-finite yields the default.
    pub fn new(fraction: f32) -> Self {
        if fraction.is_finite() {
            Self(fraction.clamp(Self::MIN, Self::MAX))
        } else {
            Self(Self::DEFAULT)
        }
    }

    /// The clamped fraction.
    pub fn get(self) -> f32 {
        self.0
    }
}

impl Default for NavSplit {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl From<f32> for NavSplit {
    fn from(fraction: f32) -> Self {
        Self::new(fraction)
    }
}

impl From<NavSplit> for f32 {
    fn from(split: NavSplit) -> Self {
        split.0
    }
}

/// The whole shell's layout state — what persists across runs.
///
/// `#[serde(default)]` is what makes a partial file safe: a state file written
/// by an older build, or hand-truncated, fills its missing fields with
/// defaults instead of failing to parse.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellState {
    /// Whether the left blade is collapsed to its stub.
    pub left_collapsed: bool,
    /// Whether the right blade is collapsed to its stub.
    pub right_collapsed: bool,
    /// Left blade width when expanded.
    pub left_width: BladeWidth,
    /// Right blade width when expanded.
    pub right_width: BladeWidth,
    /// The left blade's active switchable view.
    pub left_view: LeftView,
    /// The right blade's active switchable view.
    pub right_view: RightView,
    /// Where the left blade divides pinned nav from its switchable view.
    pub left_split: NavSplit,
}

impl ShellState {
    /// Collapse the left blade if expanded, expand it if collapsed.
    pub fn toggle_left(&mut self) {
        self.left_collapsed = !self.left_collapsed;
    }

    /// Collapse the right blade if expanded, expand it if collapsed.
    pub fn toggle_right(&mut self) {
        self.right_collapsed = !self.right_collapsed;
    }
}
```

Then add the module declaration in `crates/junto-iced/src/main.rs`, beside the existing `mod pointing;` / `mod popover;` at line 11:

```rust
mod shell;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml shell::`
Expected: PASS — 8 tests.

- [ ] **Step 5: Check formatting and lints**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Expected: both clean. If clippy flags `mod shell` as unused, that resolves in Task 3 when `App` holds a `ShellState`; until then add nothing to silence it — run clippy again after Step 6 instead.

- [ ] **Step 6: Commit**

```bash
git add crates/junto-iced/src/shell.rs crates/junto-iced/src/main.rs
git commit -m "feat(iced): pure shell layout state with clamped newtypes"
```

---

### Task 2: Persist shell state to `<junto-home>/ui.toml`

**Files:**
- Modify: `crates/junto-iced/src/shell.rs` (add `load` / `save`)

**Interfaces:**
- Consumes: `ShellState` from Task 1.
- Produces: `shell::load(path: &Path) -> ShellState` (total — never fails) and `shell::save(path: &Path, state: &ShellState) -> std::io::Result<()>`. Both take an explicit path so they are testable without touching the real home directory; Task 3 supplies `junto_home()?.join("ui.toml")`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/junto-iced/src/shell.rs`:

```rust
#[cfg(test)]
mod persistence_tests {
    use super::{BladeWidth, LeftView, NavSplit, RightView, ShellState, load, save};

    /// A unique temp path per test — these run in parallel, so a shared
    /// filename would make them flaky.
    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("junto-iced-shell-{name}.toml"))
    }

    #[test]
    fn state_survives_a_save_and_load_round_trip() {
        let path = temp_path("round-trip");
        let mut written = ShellState::default();
        written.left_collapsed = true;
        written.left_view = LeftView::Sessions;
        written.right_view = RightView::Lineage;
        written.right_width = BladeWidth::new(400.0);

        save(&path, &written).expect("save should succeed to a temp dir");
        assert_eq!(load(&path), written);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_absent_file_yields_defaults_rather_than_an_error() {
        // First run on a new machine: no file exists yet.
        let path = temp_path("absent-file-that-is-never-created");
        assert_eq!(load(&path), ShellState::default());
    }

    #[test]
    fn a_corrupt_file_yields_defaults_so_the_app_still_starts() {
        // Layout state is a convenience; it must never be a reason the
        // application refuses to launch.
        let path = temp_path("corrupt");
        std::fs::write(&path, "this is not toml {{{").expect("write temp file");
        assert_eq!(load(&path), ShellState::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_partial_file_fills_only_its_missing_fields() {
        // A file written by an older build must not lose the fields it does
        // carry, nor fail on the ones it lacks.
        let path = temp_path("partial");
        std::fs::write(&path, "left_collapsed = true\n").expect("write temp file");
        let loaded = load(&path);
        assert!(loaded.left_collapsed);
        assert_eq!(loaded.right_width, BladeWidth::default());
        assert_eq!(loaded.left_view, LeftView::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_out_of_range_width_on_disk_is_clamped_on_load() {
        // Hand-edited or corrupt values must not strand a blade at 2px.
        let path = temp_path("out-of-range");
        std::fs::write(&path, "left_width = 2.0\n").expect("write temp file");
        assert_eq!(load(&path).left_width.get(), BladeWidth::MIN);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_out_of_range_nav_split_on_disk_is_clamped_on_load() {
        let path = temp_path("split-out-of-range");
        std::fs::write(&path, "left_split = 0.99\n").expect("write temp file");
        assert_eq!(load(&path).left_split.get(), NavSplit::MAX);
        let _ = std::fs::remove_file(&path);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml shell::persistence`
Expected: FAIL — `cannot find function load in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Append to `crates/junto-iced/src/shell.rs`, above the test modules:

```rust
use std::path::Path;

/// Read shell state from `path`.
///
/// Total by construction: a missing file, an unreadable file, an unparseable
/// file, or a partial one all yield defaults. Layout state is a convenience,
/// never a reason the application will not start — so there is deliberately
/// no error to surface and no `Result` for a caller to mishandle.
pub fn load(path: &Path) -> ShellState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write shell state to `path`, creating the parent directory if needed.
///
/// Returns the io error rather than swallowing it so a caller can log it, but
/// callers are expected to treat a failed save as non-fatal.
pub fn save(path: &Path, state: &ShellState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(state)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, text)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml shell::`
Expected: PASS — 14 tests (8 from Task 1, 6 new).

- [ ] **Step 5: Check formatting and lints**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add crates/junto-iced/src/shell.rs
git commit -m "feat(iced): persist shell layout to <junto-home>/ui.toml"
```

---

### Task 3: Render the three-vertical frame

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `struct App` (line 73), `App::new` (line 1005), `Message` enum (near line 808), `App::update` (line 1083), `App::view` (line 2627)

**Interfaces:**
- Consumes: `shell::{ShellState, LeftView, RightView, BladeWidth, NavSplit, load, save}`.
- Produces: `App.shell: ShellState`; `Message::{ToggleLeftBlade, ToggleRightBlade, LeftViewPicked(LeftView), RightViewPicked(RightView), LeftWidthDragged(f32), RightWidthDragged(f32), NavSplitDragged(f32)}`; helper fns `left_blade(&App) -> Element<'_, Message>`, `right_blade(&App) -> Element<'_, Message>`, `blade_stub(collapsed_side: Side, badge: Option<usize>) -> Element<'_, Message>`.

This task builds the frame with **placeholder blade contents** (a heading per view). Tasks 4 and 5 fill them by relocating real content. Splitting here is deliberate: a reviewer can reject the frame without rejecting the migrations.

- [ ] **Step 1: Add shell state to `App`**

In `struct App` (line 73), add a field after `focus`:

```rust
    /// Three-pane shell layout — collapse, widths, active blade views.
    /// Loaded at startup and written back on every change (`shell::save`).
    shell: shell::ShellState,
```

In `App::new` (line 1005), initialise it by loading from disk:

```rust
            shell: shell::load(&shell_state_path()),
```

And add this helper beside `junto_home()` (line 6446):

```rust
/// Where the shell's layout state lives — `<junto-home>/ui.toml`, alongside
/// the host's `keys.toml`. Falls back to a relative path when the home cannot
/// be resolved; `shell::load` treats an unreadable path as "use defaults".
fn shell_state_path() -> std::path::PathBuf {
    junto_home()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ui.toml")
}
```

- [ ] **Step 2: Add the messages**

In the `Message` enum (near line 808, beside `ClearHighlight`), add:

```rust
    /// Collapse or expand the left blade.
    ToggleLeftBlade,
    /// Collapse or expand the right blade.
    ToggleRightBlade,
    /// Switch the left blade's view beneath the pinned nav.
    LeftViewPicked(shell::LeftView),
    /// Switch the right blade's view.
    RightViewPicked(shell::RightView),
    /// A left blade width drag, in logical pixels.
    LeftWidthDragged(f32),
    /// A right blade width drag, in logical pixels.
    RightWidthDragged(f32),
    /// A drag of the left blade's nav/view divider, as a fraction.
    NavSplitDragged(f32),
```

- [ ] **Step 3: Handle them in `update`**

In `App::update` (line 1083), add arms that mutate state then persist. Persisting on every change keeps the model trivial — these are user-paced events, not a hot path:

```rust
            Message::ToggleLeftBlade => {
                self.shell.toggle_left();
                self.persist_shell();
                Task::none()
            }
            Message::ToggleRightBlade => {
                self.shell.toggle_right();
                self.persist_shell();
                Task::none()
            }
            Message::LeftViewPicked(view) => {
                self.shell.left_view = view;
                self.persist_shell();
                Task::none()
            }
            Message::RightViewPicked(view) => {
                self.shell.right_view = view;
                self.persist_shell();
                Task::none()
            }
            Message::LeftWidthDragged(px) => {
                self.shell.left_width = shell::BladeWidth::new(px);
                self.persist_shell();
                Task::none()
            }
            Message::RightWidthDragged(px) => {
                self.shell.right_width = shell::BladeWidth::new(px);
                self.persist_shell();
                Task::none()
            }
            Message::NavSplitDragged(fraction) => {
                self.shell.left_split = shell::NavSplit::new(fraction);
                self.persist_shell();
                Task::none()
            }
```

And add the helper to `impl App`:

```rust
    /// Write shell state to disk, ignoring failure. A layout preference that
    /// cannot be saved is a lost preference, not an error worth a surface.
    fn persist_shell(&self) {
        let _ = shell::save(&shell_state_path(), &self.shell);
    }
```

- [ ] **Step 4: Restructure `view()` into three columns**

In `App::view` (line 2627), leave the early-return admin branch untouched. Replace the final assembly so the workspace is wrapped in a `row!` of three regions. The center keeps whatever it renders today — this task changes only the surrounding structure:

```rust
        // The three-pane shell: collapsible blades either side of the channel
        // workspace. Each blade collapses to a stub rather than to zero so the
        // attention badge stays legible even when the blade is put away
        // (docs/attention.md — attention is the spine).
        let left: Element<Message> = if self.shell.left_collapsed {
            blade_stub(Side::Left, Some(self.focus_items.len()))
        } else {
            container(left_blade(self))
                .width(Length::Fixed(self.shell.left_width.get()))
                .into()
        };
        let right: Element<Message> = if self.shell.right_collapsed {
            blade_stub(Side::Right, None)
        } else {
            container(right_blade(self))
                .width(Length::Fixed(self.shell.right_width.get()))
                .into()
        };

        column![
            top_bar,
            row![left, container(center).width(Fill), right].spacing(0),
        ]
        .into()
```

Bind `top_bar` to the existing tab bar (line 2820) and `center` to the existing workspace element. Add the `Side` enum beside the blade helpers:

```rust
/// Which side of the shell a blade sits on — used only to point its stub's
/// chevron outward and to route the stub's click to the right message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}
```

- [ ] **Step 5: Write the blade helpers with placeholder contents**

Add near the other view helpers (after line 2820's tab bar):

```rust
/// The collapsed form of a blade: a narrow rail carrying a chevron to reopen
/// it and, on the left, the count of items wanting attention. Collapsing must
/// not be able to hide that count entirely.
fn blade_stub<'a>(side: Side, badge: Option<usize>) -> Element<'a, Message> {
    let (glyph, message) = match side {
        Side::Left => ("›", Message::ToggleLeftBlade),
        Side::Right => ("‹", Message::ToggleRightBlade),
    };
    let mut rail = column![button(text(glyph).size(13)).on_press(message).padding(4)]
        .spacing(6)
        .align_x(Center);
    if let Some(count) = badge.filter(|count| *count > 0) {
        rail = rail.push(text(count.to_string()).size(11).color(RED));
    }
    container(rail)
        .width(Length::Fixed(24.0))
        .height(Fill)
        .into()
}

/// The left blade: pinned channel navigation above a switchable
/// Attention/Sessions view. Nav is pinned rather than switchable so changing
/// channels never costs a round trip through a view switcher.
fn left_blade(app: &App) -> Element<'_, Message> {
    let switcher = row![
        button(text("attention").size(12))
            .on_press(Message::LeftViewPicked(shell::LeftView::Attention))
            .padding(4),
        button(text("sessions").size(12))
            .on_press(Message::LeftViewPicked(shell::LeftView::Sessions))
            .padding(4),
    ]
    .spacing(4);

    // Filled in by Task 4 (attention) and Task 5 (sessions).
    let body: Element<Message> = match app.shell.left_view {
        shell::LeftView::Attention => text("attention").size(12).into(),
        shell::LeftView::Sessions => text("sessions").size(12).into(),
    };

    column![
        // Pinned nav — replaced with the real channel list in Task 4.
        container(text("channels").size(12)).height(Length::FillPortion(
            (app.shell.left_split.get() * 100.0) as u16
        )),
        button(text("‹").size(13))
            .on_press(Message::ToggleLeftBlade)
            .padding(4),
        switcher,
        container(body).height(Length::FillPortion(
            ((1.0 - app.shell.left_split.get()) * 100.0) as u16
        )),
    ]
    .spacing(6)
    .padding(8)
    .into()
}

/// The right blade: a switchable Artifacts/Lineage view.
fn right_blade(app: &App) -> Element<'_, Message> {
    let switcher = row![
        button(text("artifacts").size(12))
            .on_press(Message::RightViewPicked(shell::RightView::Artifacts))
            .padding(4),
        button(text("lineage").size(12))
            .on_press(Message::RightViewPicked(shell::RightView::Lineage))
            .padding(4),
    ]
    .spacing(4);

    // Filled in by Task 5.
    let body: Element<Message> = match app.shell.right_view {
        shell::RightView::Artifacts => text("artifacts").size(12).into(),
        shell::RightView::Lineage => text("lineage").size(12).into(),
    };

    column![
        row![
            switcher,
            button(text("›").size(13))
                .on_press(Message::ToggleRightBlade)
                .padding(4)
        ]
        .spacing(6),
        body,
    ]
    .spacing(6)
    .padding(8)
    .into()
}
```

- [ ] **Step 6: Verify it compiles and runs**

Run: `cargo check --manifest-path crates/junto-iced/Cargo.toml`
Expected: clean.
Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml`
Expected: the window shows three columns; clicking each chevron collapses and reopens that blade; the left stub shows a red count when the focus board has items. Quit and relaunch — **the collapse state and active views are exactly as you left them** (this is the Task 2 persistence proving itself end to end).

- [ ] **Step 7: Check formatting, lints, and tests**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): three-vertical shell frame with collapsible blades"
```

---

### Task 4: Move navigation and attention into the left blade

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `App::view` (the `adder_row` block at 2627–2666, the focus board at 2728), `left_blade`

**Interfaces:**
- Consumes: `left_blade` from Task 3.
- Produces: `channel_nav(&App) -> Element<'_, Message>` and `attention_view(&App) -> Element<'_, Message>`, both called from `left_blade`.

- [ ] **Step 1: Extract the channel nav**

Move the `adder_row` construction (currently the first ~40 lines of `view()`, lines 2628–2666, including the `substrates.len() > 1` pick_list branch and the `new_channel_error` handling) into a new helper. It renders the channel list above the open/create controls:

```rust
/// Pinned navigation: the open channels, then the controls to open or create
/// one. Lives at the top of the left blade and never toggles away.
fn channel_nav(app: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(2);
    for name in &app.channel_names {
        list = list.push(
            button(text(name.as_str()).size(12))
                .on_press(Message::ChannelPicked(name.clone()))
                .padding(4)
                .width(Fill),
        );
    }
    column![scrollable(list).height(Fill), adder(app)]
        .spacing(6)
        .into()
}
```

Rename the existing `adder_row`/`adder` construction into `fn adder(app: &App) -> Element<'_, Message>`, returning the same element it builds today. Keep its `substrates.len() > 1` branch and `new_channel_error` handling verbatim — this is a move, not a rewrite.

- [ ] **Step 2: Extract the focus board as the attention view**

Move the focus board block (line 2728 onward, the "visible top banner of cross-channel needs-you" construction) into:

```rust
/// The cross-channel "needs you" items — the focus board, relocated out of the
/// permanent top banner into the left blade where it can be put away.
fn attention_view(app: &App) -> Element<'_, Message> {
    // Body moved verbatim from the former top-banner block; it becomes a
    // vertical list rather than a horizontal chip strip, since the blade is
    // tall and narrow rather than short and wide.
    let mut items = column![].spacing(4);
    for item in &app.focus_items {
        items = items.push(focus_chip(item));
    }
    scrollable(items).height(Fill).into()
}
```

Reuse the existing per-item chip rendering as `focus_chip`; extract it from the former banner loop rather than writing new markup.

- [ ] **Step 3: Wire them into `left_blade`**

Replace the two placeholders from Task 3:

```rust
    let body: Element<Message> = match app.shell.left_view {
        shell::LeftView::Attention => attention_view(app),
        shell::LeftView::Sessions => text("sessions").size(12).into(),
    };
```

and replace the pinned-nav placeholder `container(text("channels").size(12))` with `container(channel_nav(app))`.

- [ ] **Step 4: Delete the old bands from `view()`**

Remove the now-dead `adder` and focus-board blocks from `view()`'s vertical stack. The stack should be down to the tab bar plus the three-column row.

- [ ] **Step 5: Verify**

Run: `cargo check --manifest-path crates/junto-iced/Cargo.toml`
Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml`
Expected: the channel list and open/create controls appear in the left blade; the "attention" tab lists the same items the top banner used to; **no** focus banner or adder row remains above the workspace; clicking a channel still opens it.

- [ ] **Step 6: Check formatting, lints, and tests**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): relocate channel nav and focus board into the left blade"
```

---

### Task 5: Move lineage, artifacts, and sessions into the blades

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — the lineage ribbon block in `App::view` (the `ribbon` binding following line 2680), `left_blade`, `right_blade`

**Interfaces:**
- Consumes: `left_blade` / `right_blade` from Tasks 3–4.
- Produces: `lineage_view(&App) -> Element<'_, Message>`, `artifacts_view(&App) -> Element<'_, Message>`, `sessions_view(&App) -> Element<'_, Message>`.

- [ ] **Step 1: Move the lineage ribbon into the right blade**

Extract the `ribbon` construction (the `LineageCanvas::layout` / pinned-plus-scrollable block) into:

```rust
/// The whole lineage DAG, relocated from the always-visible top ribbon into
/// the right blade. Freed from the top band it no longer needs the 150px
/// scroll cap the ribbon imposed — it gets the blade's full height.
fn lineage_view(app: &App) -> Element<'_, Message> {
    match &app.lineage {
        Some(graph) => {
            let open: HashSet<String> = app
                .panes
                .iter()
                .map(|(_, pane)| pane.channel.clone())
                .collect();
            let canvas = LineageCanvas::layout(graph, &open);
            let content_h = canvas.height.max(60.0);
            scrollable(
                Canvas::new(canvas)
                    .width(Fill)
                    .height(Length::Fixed(content_h)),
            )
            .height(Fill)
            .into()
        }
        None => text("no lineage yet").size(12).color(MUTED).into(),
    }
}
```

Note the `open` set is now built from `app.panes` directly rather than `app.order` — Task 6 retires `order`, and this pre-empts that dependency.

- [ ] **Step 2: Write the artifacts and sessions views**

```rust
/// Artifacts attached to the focused channel — diffs, logs, charts. Rendered
/// from the focused pane's existing artifact state rather than a new fetch.
fn artifacts_view(app: &App) -> Element<'_, Message> {
    let Some(pane) = app.focus.and_then(|id| app.panes.get(id)) else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    let mut items = column![].spacing(4);
    for entry in pane.artifacts() {
        items = items.push(artifact_row(pane, entry));
    }
    scrollable(items).height(Fill).into()
}

/// Agent sessions for the focused channel.
fn sessions_view(app: &App) -> Element<'_, Message> {
    let Some(pane) = app.focus.and_then(|id| app.panes.get(id)) else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    let mut items = column![].spacing(4);
    for session in pane.sessions() {
        items = items.push(session_row(pane, session));
    }
    scrollable(items).height(Fill).into()
}
```

`pane.artifacts()` / `pane.sessions()` are accessors over the pane's already-fetched `view.json` data; add them to `impl Pane` returning iterators over the existing collections. `artifact_row` / `session_row` reuse the existing inline renderers (the `ToggleArtifact` / `ArtifactLoaded` card markup) rather than new markup.

- [ ] **Step 3: Wire the three views in**

In `left_blade`:

```rust
        shell::LeftView::Sessions => sessions_view(app),
```

In `right_blade`:

```rust
    let body: Element<Message> = match app.shell.right_view {
        shell::RightView::Artifacts => artifacts_view(app),
        shell::RightView::Lineage => lineage_view(app),
    };
```

- [ ] **Step 4: Delete the ribbon from `view()`**

Remove the `ribbon` binding and its slot in the vertical stack. `view()`'s stack is now the tab bar plus the three-column row, and nothing else.

- [ ] **Step 5: Verify**

Run: `cargo check --manifest-path crates/junto-iced/Cargo.toml`
Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml`
Expected: no lineage ribbon above the workspace; the right blade's "lineage" tab draws the DAG at full blade height; "artifacts" lists the focused channel's artifacts; the left blade's "sessions" tab lists its sessions. **The channel workspace now starts immediately below the tab bar** — this is the payoff the whole spec exists for.

- [ ] **Step 6: Check formatting, lints, and tests**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): relocate lineage, artifacts, and sessions into blades"
```

---

### Task 6: Adopt the real `PaneGrid` in the center

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `struct App` (line 73, remove `order`), `App::new` (1011), `update` (1061, 1069, 1076, 1262), the Columns renderer (2685, 2795), the pane card (3263), and the `order` use at 2243

**Interfaces:**
- Consumes: everything above.
- Produces: the center rendered by `iced::widget::pane_grid::PaneGrid`; new messages `Message::{PaneResized(pane_grid::ResizeEvent), PaneDragged(pane_grid::DragEvent), SplitPane(pane_grid::Axis)}`; `App.order` is **removed**.

This is the riskiest task and lands last on purpose: if it has to be abandoned, Tasks 1–5 still deliver the full shell.

- [ ] **Step 1: Add the pane messages**

```rust
    /// A divider drag between panes.
    PaneResized(pane_grid::ResizeEvent),
    /// A pane dragged to a new position.
    PaneDragged(pane_grid::DragEvent),
    /// Split the focused pane along `axis`.
    SplitPane(pane_grid::Axis),
```

- [ ] **Step 2: Handle them in `update`**

```rust
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                Task::none()
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                Task::none()
            }
            Message::PaneDragged(_) => Task::none(),
            Message::SplitPane(axis) => {
                let Some(focus) = self.focus else {
                    return Task::none();
                };
                if let Some((new_pane, _)) = self.panes.split(axis, focus, Pane::loading("")) {
                    self.focus = Some(new_pane);
                }
                Task::none()
            }
```

- [ ] **Step 3: Replace the Columns renderer with `PaneGrid`**

Replace the custom shared-width Columns construction (the `for id in &self.order` loop at 2795 and its surrounding container) with:

```rust
        // The channel workspace. `pane_grid::State` has always been the store;
        // this is the widget finally rendering it, which is what buys
        // arbitrary 2D nesting — split any pane on either axis, at any depth.
        let center = pane_grid::PaneGrid::new(&self.panes, |id, pane, _maximized| {
            pane_grid::Content::new(channel_pane(self, id, pane))
                .title_bar(pane_grid::TitleBar::new(text(pane.channel.as_str()).size(12)))
        })
        .on_resize(10, Message::PaneResized)
        .on_drag(Message::PaneDragged)
        .width(Fill)
        .height(Fill)
        .spacing(6);
```

Adapt the existing per-pane card function (line 3263, "One channel pane as a bordered column") into `channel_pane(&App, pane_grid::Pane, &Pane) -> Element<'_, Message>`, keeping its body verbatim and dropping only the fixed-width container the Columns layout required.

- [ ] **Step 4: Retire `order`**

Remove the `order: Vec<pane_grid::Pane>` field (line 79) and fix each use site:

| Line | Today | Becomes |
|---|---|---|
| 1011 | `order: vec![first]` | delete the field initialiser |
| 1061 | iterate `self.order` | iterate `self.panes.iter()` |
| 1069 | `self.focus.or_else(\|\| self.order.last().copied())` | `self.focus.or_else(\|\| self.panes.iter().next().map(\|(id, _)\| *id))` |
| 1076 | `self.order.push(new_pane)` | delete — `panes.split` places it |
| 1262 | `self.order.retain(...)` | delete — `panes.close` removes it |
| 2243 | iterate `self.order` | iterate `self.panes.iter()` |
| 2685, 2795 | build columns from `self.order` | deleted with the Columns renderer |

Where a use site needs a *stable* iteration order (rather than spatial order), sort `self.panes.iter()` by channel name at the call site and comment why — do not reintroduce a parallel `Vec`.

- [ ] **Step 5: Replace `+ pane` with axis-aware splitting**

Wherever the old `+ pane` control is rendered, offer both axes:

```rust
row![
    button(text("split →").size(12))
        .on_press(Message::SplitPane(pane_grid::Axis::Vertical))
        .padding(4),
    button(text("split ↓").size(12))
        .on_press(Message::SplitPane(pane_grid::Axis::Horizontal))
        .padding(4),
]
.spacing(4)
```

- [ ] **Step 6: Verify the quad**

Run: `cargo check --manifest-path crates/junto-iced/Cargo.toml`
Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml`
Expected: open a channel, `split →`, then `split ↓` on one of the halves — **a quad layout, nestable further**. Drag the dividers to resize; drag a pane's title bar to reorder. Both blades still collapse independently.

- [ ] **Step 7: Check formatting, lints, and tests**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml`
Expected: all clean. Fix any test that referenced `order`.

- [ ] **Step 8: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): render a real PaneGrid for arbitrary 2D pane nesting"
```

---

### Task 7: Blade keybindings

**Files:**
- Modify: `crates/junto-iced/src/main.rs` — `App::subscription` (line 2533, batch at 2624)

**Interfaces:**
- Consumes: `Message::{ToggleLeftBlade, ToggleRightBlade}` from Task 3.
- Produces: no new state — a keyboard subscription firing existing messages.

Cheap by construction: the toggles already exist for the chevrons, so a key press is a second trigger for a tested transition. If this fights anything, cut it — nothing depends on it.

- [ ] **Step 1: Add the keyboard subscription**

In `App::subscription`, before the final `Subscription::batch` (line 2624):

```rust
        // Zed's dock bindings, since that is the reference point. These add no
        // state: they fire the same messages the chevrons do.
        let keys = iced::keyboard::on_key_press(|key, modifiers| {
            if !(modifiers.command() || modifiers.control()) {
                return None;
            }
            match key.as_ref() {
                iced::keyboard::Key::Character("b") => Some(Message::ToggleLeftBlade),
                iced::keyboard::Key::Character("r") => Some(Message::ToggleRightBlade),
                _ => None,
            }
        });
```

Then include it in the batch:

```rust
        iced::Subscription::batch(
            streams
                .into_iter()
                .chain([tick, keys])
                .chain(countdown_tick),
        )
```

- [ ] **Step 2: Verify**

Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml`
Expected: `Ctrl+B` toggles the left blade, `Ctrl+R` the right (`Cmd` on macOS). Then click into the "new channel name…" text input and type "brr" — **the letters must appear in the field and the blades must not toggle**, since the bindings require a modifier.

- [ ] **Step 3: Check formatting, lints, and tests**

Run: `cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check`
Run: `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings`
Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml`
Expected: all clean.

- [ ] **Step 4: Commit**

```bash
git add crates/junto-iced/src/main.rs
git commit -m "feat(iced): ctrl/cmd+b and ctrl/cmd+r toggle the blades"
```

---

### Task 8: Put `junto-iced` into CI

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: nothing.
- Produces: an `iced` job.

The crate is absent from CI entirely while already carrying ~70 tests that have never run on a pull request. This task is what makes every preceding task's tests mean something.

- [ ] **Step 1: Read the existing `desktop` job**

Open `.github/workflows/ci.yml` and read the `desktop` job (around line 80) — it is the exact shape to mirror: `--manifest-path`, an `fmt --check` step, a `clippy … -D warnings` step, and its platform matrix and trigger conditions.

- [ ] **Step 2: Add the `iced` job**

Add a job mirroring `desktop`, following the repo's established cadence — Linux canary on pull requests, Windows/macOS on merges to main — so it stays inside the free tier:

```yaml
  iced:
    name: junto-iced
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: ${{ github.event_name == 'pull_request' && fromJSON('["ubuntu-latest"]') || fromJSON('["windows-latest", "macos-latest"]') }}
    steps:
      - uses: actions/checkout@v4
      - name: Format
        run: cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check
      - name: Clippy
        run: cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings
      - name: Test
        run: cargo test --manifest-path crates/junto-iced/Cargo.toml
```

Match the `desktop` job's own `matrix`/`if` idiom rather than copying the expression above verbatim if it differs — consistency with the existing file wins. Iced needs system libraries on Linux; if the Linux run fails to link, add the same `apt-get` step the `desktop` job uses for Tauri's GTK dependencies.

- [ ] **Step 3: Verify the workflow parses**

Run: `gh workflow view ci.yml` (or push the branch and confirm the job appears).
Expected: the `iced` job is listed.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: build, lint, and test junto-iced"
```

---

## Self-review notes

**Spec coverage.** §2 shape → Task 3. §3 migration map → Tasks 4–5. §4 PaneGrid → Task 6. §5 mechanics → Tasks 3 and 7. §6 persistence → Task 2. §7 testing + CI → Tasks 1, 2, and 8. §8 non-goals → no task, correctly. §9 open decisions → no task by design; the ADR superseding 0018 is Dan's call and is not implementable here.

**Known gap, deliberately left.** §5 specifies double-click-to-reset on dividers. `PaneGrid` supplies divider dragging in Task 6, but the *blade* width dividers in Task 3 are rendered as plain containers with no drag handle wired — `LeftWidthDragged` / `RightWidthDragged` / `NavSplitDragged` exist and are tested, but nothing emits them yet. Wiring blade drag handles (an `iced::widget::mouse_area` over a divider, translating cursor position into the message) is a small follow-up task to add once the frame is real on screen and the right handle geometry is obvious. It is called out here rather than faked with a placeholder step.
