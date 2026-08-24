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
        let written = ShellState {
            left_collapsed: true,
            left_view: LeftView::Sessions,
            right_view: RightView::Lineage,
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

    #[test]
    fn save_creates_nested_directories_if_missing() {
        let base = std::env::temp_dir().join("junto-iced-shell-nested-dir");
        let path = base.join("subdir").join("ui.toml");
        let state = ShellState::default();

        save(&path, &state).expect("save should succeed and create directories");
        assert_eq!(load(&path), state);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&base);
    }
}
