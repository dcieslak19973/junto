//! Pure, renderer-neutral browser-view logic: URL normalization, the emulated
//! viewport and its coordinate mapping, keyboard translation, and the
//! navigation model. No `iced`, no I/O — the decidable parts of the browser
//! view live here so they are unit-tested without a running app, exactly as
//! `pointing.rs` keeps the pointing gesture's logic testable in isolation.

/// The minimum CSS width we emulate. Below a blade this wide, a real site laid
/// out at the blade's own ~360–560px would collapse to a cramped mobile view or
/// overflow; emulating at least this width makes it lay out as a desktop page,
/// and the frame is then scaled DOWN to fit the blade (`fit_width`). At or above
/// this width the page renders at the blade's own width (no zoom).
pub const DESKTOP_MIN_WIDTH: f32 = 1280.0;

/// The emulated CSS viewport, in CSS pixels — always at least
/// `DESKTOP_MIN_WIDTH` wide (see `fit_width`). The display scale factor rides
/// separately as `deviceScaleFactor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportSize {
    pub width: u32,
    pub height: u32,
}

/// The zoom ratio (page CSS pixels per widget logical pixel) for a blade
/// `logical_w` wide: 1.0 once the blade is at least `DESKTOP_MIN_WIDTH`, and
/// `DESKTOP_MIN_WIDTH / logical_w` (> 1) below it, so a narrow blade shows a
/// desktop page shrunk to fit. Shared by `fit_width` and `map_cursor` so the
/// viewport and the input mapping never disagree.
fn zoom_ratio(logical_w: f32) -> f32 {
    let w = logical_w.max(1.0);
    w.max(DESKTOP_MIN_WIDTH) / w
}

impl ViewportSize {
    /// A viewport can never be zero: Chromium refuses a 0-dimension metrics
    /// override, and a collapsed blade must degrade to a 1px viewport, not a
    /// crash.
    const MIN: u32 = 1;

    /// The emulated viewport for a blade of logical size `(w, h)`: the width is
    /// raised to at least `DESKTOP_MIN_WIDTH` so real sites lay out as desktop,
    /// and the height is scaled by the same zoom ratio so the frame's aspect
    /// matches the blade and fills it without letterboxing.
    pub fn fit_width(logical_w: f32, logical_h: f32) -> Self {
        let ratio = zoom_ratio(logical_w);
        Self {
            width: ((logical_w.max(1.0) * ratio).round() as u32).max(Self::MIN),
            height: ((logical_h.max(1.0) * ratio).round() as u32).max(Self::MIN),
        }
    }
}

/// A point in page (CSS-pixel) coordinates — what `Input.dispatchMouseEvent`
/// expects. Distinct from a widget-local `iced::Point` so the two coordinate
/// spaces can never be confused at a call site.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    pub x: f32,
    pub y: f32,
}

/// Map a widget-local cursor position to a page point, scaling by the same
/// `zoom_ratio` `fit_width` used so a click lands where it looks like it does,
/// and clamping into the CSS viewport so an edge cursor never lands outside it.
pub fn map_cursor(local_x: f32, local_y: f32, bounds_w: f32, bounds_h: f32) -> PagePoint {
    let ratio = zoom_ratio(bounds_w);
    let css_w = bounds_w.max(1.0) * ratio;
    let css_h = bounds_h.max(0.0) * ratio;
    PagePoint {
        x: (local_x * ratio).clamp(0.0, css_w),
        y: (local_y * ratio).clamp(0.0, css_h),
    }
}

/// Normalize a URL-bar entry: trim, reject empty, pass an explicit supported
/// scheme through unchanged, and give a bare host/path `https://`. Not a
/// validator — Chromium is the arbiter of a real URL; this only decides the
/// scheme so a user can type `example.com`.
pub fn normalize_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    const SCHEMES: [&str; 5] = ["http://", "https://", "about:", "data:", "file://"];
    if SCHEMES.iter().any(|scheme| trimmed.starts_with(scheme)) {
        Some(trimmed.to_owned())
    } else {
        Some(format!("https://{trimmed}"))
    }
}

/// A key event the screencast widget hands to the mapper — neutral, no `iced`
/// types, so the CDP translation is testable without a running app.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// A printable character already resolved for modifiers by the platform.
    Char(char),
    /// A non-text key we map to a DOM key + virtual-key code.
    Named(NamedKey),
}

/// The non-text keys v1 injects. Function/media/IME keys are deliberately
/// absent (`key_event` never sees them — the widget drops them) rather than
/// mapped to a wrong code, which would inject garbage into a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

/// The held modifier keys, folded to CDP's bitmask by `bits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

impl Modifiers {
    /// CDP's `modifiers` bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8.
    pub fn bits(self) -> i64 {
        (self.alt as i64)
            | ((self.ctrl as i64) << 1)
            | ((self.meta as i64) << 2)
            | ((self.shift as i64) << 3)
    }
}

/// Press or release — CDP needs both for a named key; a char needs only the
/// down phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
}

/// The `Input.dispatchKeyEvent` fields for one key event.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEvent {
    /// "char" | "keyDown" | "keyUp".
    pub kind: &'static str,
    pub key: String,
    /// DOM `code`; empty for a `char` event.
    pub code: &'static str,
    pub windows_virtual_key_code: i64,
    pub text: Option<String>,
    pub modifiers: i64,
}

/// Translate a neutral key event to CDP fields, or `None` for a key v1 does
/// not inject.
pub fn key_event(key: &Key, phase: KeyPhase, mods: Modifiers) -> Option<KeyEvent> {
    match key {
        Key::Char(c) => {
            // A character inserts text on the down phase; the up phase is a
            // no-op in CDP, so drop it rather than emit an empty second event.
            if phase == KeyPhase::Up {
                return None;
            }
            let text = c.to_string();
            Some(KeyEvent {
                kind: "char",
                key: text.clone(),
                code: "",
                windows_virtual_key_code: 0,
                text: Some(text),
                modifiers: mods.bits(),
            })
        }
        Key::Named(named) => {
            let (key, code, vk) = named_fields(*named);
            Some(KeyEvent {
                kind: if phase == KeyPhase::Down {
                    "keyDown"
                } else {
                    "keyUp"
                },
                key: key.to_owned(),
                code,
                windows_virtual_key_code: vk,
                text: None,
                modifiers: mods.bits(),
            })
        }
    }
}

/// DOM `key`, DOM `code`, and Windows virtual-key code for a named key.
fn named_fields(named: NamedKey) -> (&'static str, &'static str, i64) {
    match named {
        NamedKey::Enter => ("Enter", "Enter", 13),
        NamedKey::Tab => ("Tab", "Tab", 9),
        NamedKey::Backspace => ("Backspace", "Backspace", 8),
        NamedKey::Delete => ("Delete", "Delete", 46),
        NamedKey::Escape => ("Escape", "Escape", 27),
        NamedKey::ArrowUp => ("ArrowUp", "ArrowUp", 38),
        NamedKey::ArrowDown => ("ArrowDown", "ArrowDown", 40),
        NamedKey::ArrowLeft => ("ArrowLeft", "ArrowLeft", 37),
        NamedKey::ArrowRight => ("ArrowRight", "ArrowRight", 39),
        NamedKey::Home => ("Home", "Home", 36),
        NamedKey::End => ("End", "End", 35),
        NamedKey::PageUp => ("PageUp", "PageUp", 33),
        NamedKey::PageDown => ("PageDown", "PageDown", 34),
    }
}

/// The navigation state the driver publishes and the view renders: current
/// URL, whether back/forward are possible, and whether a load is in flight.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NavState {
    pub url: String,
    pub can_back: bool,
    pub can_forward: bool,
    pub loading: bool,
}

/// Derive `NavState` from a `Page.getNavigationHistory` reply's `currentIndex`
/// and the ordered entry URLs. Pure, so it is tested without a socket; an
/// out-of-range index yields an empty state rather than indexing past the end.
pub fn nav_from_history(current_index: i64, urls: &[String]) -> NavState {
    let index = current_index.max(0) as usize;
    // A current index past the entries is not a real history (defensive against
    // a malformed reply); degrade to the empty state rather than reporting a
    // back button that leads nowhere.
    let Some(url) = urls.get(index).cloned() else {
        return NavState::default();
    };
    NavState {
        url,
        can_back: current_index > 0,
        can_forward: index + 1 < urls.len(),
        loading: false,
    }
}

#[cfg(test)]
mod viewport_url_tests {
    use super::*;

    #[test]
    fn fit_width_gives_a_desktop_viewport_below_the_threshold_and_1to1_above() {
        // A narrow blade renders a desktop-width page scaled to fit; the height
        // scales by the same ratio so the frame fills without letterboxing.
        assert_eq!(
            ViewportSize::fit_width(400.0, 900.0),
            ViewportSize {
                width: 1280,
                height: 2880
            }
        );
        // A blade wider than the threshold renders at its own width (no zoom).
        assert_eq!(
            ViewportSize::fit_width(1400.0, 900.0),
            ViewportSize {
                width: 1400,
                height: 900
            }
        );
        // Degenerate sizes never collapse to a zero Chromium would reject.
        let degenerate = ViewportSize::fit_width(0.0, 0.0);
        assert!(degenerate.width >= 1 && degenerate.height >= 1);
    }

    #[test]
    fn a_cursor_scales_by_the_zoom_ratio_and_clamps_to_the_viewport() {
        // In a 400px blade the page is 1280 CSS px wide, so the horizontal
        // midpoint (200) maps to 640 — the midpoint of the page.
        assert_eq!(
            map_cursor(200.0, 100.0, 400.0, 900.0),
            PagePoint { x: 640.0, y: 320.0 }
        );
        // A cursor past the edge clamps into the CSS viewport, never outside it.
        assert_eq!(
            map_cursor(500.0, -5.0, 400.0, 900.0),
            PagePoint { x: 1280.0, y: 0.0 }
        );
        // At or above the threshold the mapping is 1:1.
        assert_eq!(
            map_cursor(12.0, 34.0, 1400.0, 900.0),
            PagePoint { x: 12.0, y: 34.0 }
        );
    }

    #[test]
    fn a_bare_host_gets_https_and_explicit_schemes_pass_through() {
        assert_eq!(
            normalize_url(" example.com "),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            normalize_url("http://x.test"),
            Some("http://x.test".to_owned())
        );
        assert_eq!(normalize_url("about:blank"), Some("about:blank".to_owned()));
        assert_eq!(
            normalize_url("data:text/html,hi"),
            Some("data:text/html,hi".to_owned())
        );
        assert_eq!(normalize_url("   "), None);
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn a_printable_char_is_one_char_event_on_the_down_phase_only() {
        // CDP inserts text with a single `char` event; the matching key-up for
        // a character is a no-op, so it must not double-inject.
        let down = key_event(&Key::Char('a'), KeyPhase::Down, Modifiers::default()).expect("down");
        assert_eq!(down.kind, "char");
        assert_eq!(down.text.as_deref(), Some("a"));
        assert!(key_event(&Key::Char('a'), KeyPhase::Up, Modifiers::default()).is_none());
    }

    #[test]
    fn a_named_key_carries_its_virtual_key_code_on_both_phases() {
        let down = key_event(
            &Key::Named(NamedKey::Enter),
            KeyPhase::Down,
            Modifiers::default(),
        )
        .expect("down");
        assert_eq!(down.kind, "keyDown");
        assert_eq!(down.key, "Enter");
        assert_eq!(down.windows_virtual_key_code, 13);
        let up = key_event(
            &Key::Named(NamedKey::Enter),
            KeyPhase::Up,
            Modifiers::default(),
        )
        .expect("up");
        assert_eq!(up.kind, "keyUp");
    }

    #[test]
    fn modifiers_fold_to_the_cdp_bitmask() {
        // CDP: Alt=1, Ctrl=2, Meta=4, Shift=8.
        let mods = Modifiers {
            alt: false,
            ctrl: true,
            meta: false,
            shift: true,
        };
        assert_eq!(mods.bits(), 2 | 8);
        let ev = key_event(&Key::Named(NamedKey::ArrowLeft), KeyPhase::Down, mods).expect("down");
        assert_eq!(ev.modifiers, 10);
        assert_eq!(ev.windows_virtual_key_code, 37);
    }
}

#[cfg(test)]
mod nav_tests {
    use super::*;

    #[test]
    fn history_in_the_middle_can_go_both_ways() {
        let urls = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let nav = nav_from_history(1, &urls);
        assert_eq!(nav.url, "b");
        assert!(nav.can_back);
        assert!(nav.can_forward);
    }

    #[test]
    fn the_first_entry_cannot_go_back_and_the_last_cannot_go_forward() {
        let urls = vec!["a".to_owned(), "b".to_owned()];
        assert!(!nav_from_history(0, &urls).can_back);
        assert!(!nav_from_history(1, &urls).can_forward);
    }

    #[test]
    fn an_out_of_range_index_degrades_to_empty_rather_than_panicking() {
        assert_eq!(nav_from_history(9, &[]), NavState::default());
    }
}
