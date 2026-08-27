//! Pure layout state for the three-pane shell — blade collapse, widths,
//! and the bottom drawer.
//!
//! Everything decidable about the shell lives here rather than in `view()`,
//! which Iced gives no way to unit-test. `view()` is a projection over this.

use serde::{Deserialize, Serialize};

/// Which panel the bottom drawer is showing, or `None` when it is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BottomView {
    #[default]
    Attention,
    Sessions,
}

/// Which surface the right blade shows. Persisted like the other blade state
/// so the choice survives a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightView {
    #[default]
    Lineage,
    Browser,
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
    /// The ceiling a blade can be dragged to. Generous because the right blade
    /// can hold the browser, which wants real width — at ~1200px a desktop page
    /// renders essentially 1:1 (`browser::DESKTOP_MIN_WIDTH`). Dragging a blade
    /// this wide is the user's call; the center shrinks but is never gone.
    pub const MAX: f32 = 1200.0;
    /// Comfortable for a channel list plus badges.
    pub const DEFAULT: f32 = 280.0;

    /// The left blade holds the channel list; this is comfortable for a
    /// name plus a badge without stealing width from the center.
    pub const LEFT_DEFAULT: f32 = 280.0;
    /// The right blade holds the lineage DAG, whose canvas derives its
    /// track region as `width - 24 - LABEL_W` (`LABEL_W` = 150px): 280px
    /// would leave it ~90px of track, barely enough to draw a
    /// diverge/converge connector. 520px gives it ~330px, matching the
    /// reference three-pane layout.
    pub const RIGHT_DEFAULT: f32 = 520.0;

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

/// The whole shell's layout state — what persists across runs.
///
/// `#[serde(default)]` is what makes a partial file safe: a state file written
/// by an older build, or hand-truncated, fills its missing fields with
/// defaults instead of failing to parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Which panel the bottom drawer is showing; `None` when it is closed.
    pub bottom: Option<BottomView>,
    /// Which surface the right blade shows.
    pub right_view: RightView,
    /// The browser view's last-visited URL, so it reopens where you left off.
    /// `None` until the first navigation.
    pub browser_url: Option<String>,
}

impl Default for ShellState {
    /// Hand-written rather than derived: the two blades have different
    /// per-side defaults (`BladeWidth::LEFT_DEFAULT`/`RIGHT_DEFAULT`), which
    /// a derived `Default` cannot express — it would defer to
    /// `BladeWidth`'s own single `Default` impl for both fields. This is
    /// also what `#[serde(default)]` calls to fill missing fields in a
    /// partial file, so it is what makes per-side defaults survive a
    /// partial `ui.toml` too.
    fn default() -> Self {
        Self {
            left_collapsed: false,
            right_collapsed: false,
            left_width: BladeWidth::new(BladeWidth::LEFT_DEFAULT),
            right_width: BladeWidth::new(BladeWidth::RIGHT_DEFAULT),
            bottom: None,
            right_view: RightView::Lineage,
            browser_url: None,
        }
    }
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

    /// Toggle the bottom drawer: selecting the currently open view closes
    /// it; selecting the other switches to it.
    pub fn toggle_bottom(&mut self, view: BottomView) {
        self.bottom = if self.bottom == Some(view) {
            None
        } else {
            Some(view)
        };
    }
}

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

#[cfg(test)]
mod clamp_tests {
    use super::{BladeWidth, BottomView, ShellState};

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

    #[test]
    fn the_left_and_right_blades_have_different_defaults_by_design() {
        // Regression guard: a future change that collapses the two sides
        // back to one shared default must fail this loudly, since the right
        // blade's lineage canvas depends on the wider default for a usable
        // track region (see `BladeWidth::RIGHT_DEFAULT`).
        let state = ShellState::default();
        assert_eq!(state.left_width.get(), BladeWidth::LEFT_DEFAULT);
        assert_eq!(state.right_width.get(), BladeWidth::RIGHT_DEFAULT);
        assert_ne!(state.left_width, state.right_width);
    }

    #[test]
    fn toggling_the_bottom_drawer_to_the_same_view_closes_it() {
        let mut state = ShellState::default();
        assert_eq!(state.bottom, None);
        state.toggle_bottom(BottomView::Attention);
        assert_eq!(state.bottom, Some(BottomView::Attention));
        state.toggle_bottom(BottomView::Attention);
        assert_eq!(state.bottom, None);
    }

    #[test]
    fn toggling_the_bottom_drawer_to_a_different_view_switches_without_closing() {
        let mut state = ShellState::default();
        state.toggle_bottom(BottomView::Attention);
        state.toggle_bottom(BottomView::Sessions);
        assert_eq!(state.bottom, Some(BottomView::Sessions));
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::{BladeWidth, RightView, ShellState, load, save};

    /// A unique temp path per test — these run in parallel, so a shared
    /// filename would make them flaky.
    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("junto-iced-shell-{name}.toml"))
    }

    #[test]
    fn state_survives_a_save_and_load_round_trip() {
        let path = temp_path("round-trip");
        let written = ShellState {
            left_collapsed: true,
            right_width: BladeWidth::new(400.0),
            ..Default::default()
        };

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
        assert_eq!(loaded.left_width.get(), BladeWidth::LEFT_DEFAULT);
        assert_eq!(loaded.right_width.get(), BladeWidth::RIGHT_DEFAULT);
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
    fn save_creates_nested_directories_if_missing() {
        let base = std::env::temp_dir().join("junto-iced-shell-nested-dir");
        let path = base.join("subdir").join("ui.toml");
        // Cleanup runs FIRST and unconditionally, so a previous run that
        // panicked mid-test (leaving the directory behind) cannot make
        // this run pass vacuously by finding its own leftover state
        // already on disk before `save` ever ran.
        let _ = std::fs::remove_dir_all(&base);
        // Differs from `ShellState::default()` in at least one field: `load`
        // is total and returns the default on ANY failure (missing file,
        // unreadable, unparseable), so asserting the round trip against the
        // default would also pass if `save` silently wrote nothing at all —
        // it is the only coverage of `save`'s `create_dir_all` branch, so it
        // must be able to fail.
        let state = ShellState {
            left_collapsed: true,
            right_width: BladeWidth::new(400.0),
            ..Default::default()
        };

        save(&path, &state).expect("save should succeed and create directories");
        assert_eq!(load(&path), state);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn stale_left_view_left_split_and_right_view_keys_from_an_older_build_still_load_to_defaults() {
        // `left_view`/`left_split` existed before the left blade lost its
        // switchable Attention/Sessions view; `right_view` existed before
        // the right blade lost its switchable Artifacts/Lineage view for
        // the same underlying reason — the blade's Artifacts view read the
        // exact same per-pane expansion cache the channel-entry cards
        // already render every artifact through, so it was a duplicate
        // toggle over existing state, not a distinct view worth keeping.
        // An old `ui.toml` still carrying any of these keys — the real
        // `ui.toml` this build inherits carries `right_view = "lineage"`
        // today — must not block startup: serde ignores unknown fields by
        // default, and `#[serde(default)]` fills what is now missing — a
        // stale or corrupt file is never a reason to refuse to launch.
        let path = temp_path("stale-left-view-split-and-right-view-keys");
        std::fs::write(
            &path,
            "left_collapsed = true\nleft_view = \"sessions\"\nleft_split = 0.99\nright_view = \"lineage\"\n",
        )
        .expect("write temp file");

        let loaded = load(&path);
        assert_eq!(
            loaded,
            ShellState {
                left_collapsed: true,
                ..ShellState::default()
            },
            "unknown stale keys must be ignored and known fields must still load"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn right_view_and_browser_url_round_trip() {
        let mut state = ShellState::default();
        assert_eq!(state.right_view, RightView::Lineage);
        state.right_view = RightView::Browser;
        state.browser_url = Some("https://example.com".to_owned());
        let toml = toml::to_string_pretty(&state).expect("serialize");
        let back: ShellState = toml::from_str(&toml).expect("deserialize");
        assert_eq!(back, state);
    }

    #[test]
    fn an_old_file_without_right_view_defaults_to_lineage() {
        // A ui.toml written before the browser view must still load.
        let back: ShellState = toml::from_str("left_collapsed = false").expect("partial");
        assert_eq!(back.right_view, RightView::Lineage);
        assert_eq!(back.browser_url, None);
    }
}
