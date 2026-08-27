//! Pure, renderer-neutral browser-view logic: URL normalization, the emulated
//! viewport and its coordinate mapping, keyboard translation, and the
//! navigation model. No `iced`, no I/O — the decidable parts of the browser
//! view live here so they are unit-tested without a running app, exactly as
//! `pointing.rs` keeps the pointing gesture's logic testable in isolation.

/// The emulated CSS viewport, in CSS pixels. Equal to the screencast widget's
/// logical size (see `screencast.rs`): making the emulated viewport match the
/// widget is what lets `map_cursor` be 1:1 and keeps the frame crisp — the
/// display scale factor rides separately as `deviceScaleFactor`, so the frame
/// renders at physical resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportSize {
    pub width: u32,
    pub height: u32,
}

impl ViewportSize {
    /// A viewport can never be zero: Chromium refuses a 0-dimension metrics
    /// override, and a collapsed blade must degrade to a 1px viewport, not a
    /// crash.
    const MIN: u32 = 1;

    /// Round the widget's logical size to whole CSS pixels, flooring at `MIN`.
    pub fn from_logical(width: f32, height: f32) -> Self {
        Self {
            width: (width.round() as u32).max(Self::MIN),
            height: (height.round() as u32).max(Self::MIN),
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

/// Map a widget-local cursor position to a page point. Identity by
/// construction — the emulated viewport equals the widget's logical size — but
/// clamped into `[0, w] × [0, h]` so a cursor exactly on the far edge never
/// yields an out-of-viewport coordinate. This is the one seam a future
/// letterbox mode would change.
pub fn map_cursor(local_x: f32, local_y: f32, bounds_w: f32, bounds_h: f32) -> PagePoint {
    PagePoint {
        x: local_x.clamp(0.0, bounds_w),
        y: local_y.clamp(0.0, bounds_h),
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
    fn logical_size_rounds_and_never_collapses_to_zero() {
        // The emulated CSS viewport is the widget's logical size; a degenerate
        // 0-height blade must still yield a layout-able viewport, not a 0 that
        // Chromium rejects.
        assert_eq!(
            ViewportSize::from_logical(519.6, 1399.4),
            ViewportSize {
                width: 520,
                height: 1399
            }
        );
        assert_eq!(
            ViewportSize::from_logical(0.0, 0.0),
            ViewportSize {
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn a_cursor_maps_one_to_one_and_clamps_to_the_viewport() {
        // Viewport == widget logical size, so mapping is identity; a cursor on
        // the far edge must not produce an out-of-viewport CSS coordinate.
        assert_eq!(
            map_cursor(12.0, 34.0, 520.0, 1400.0),
            PagePoint { x: 12.0, y: 34.0 }
        );
        assert_eq!(
            map_cursor(600.0, -5.0, 520.0, 1400.0),
            PagePoint { x: 520.0, y: 0.0 }
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
        let down = key_event(&Key::Named(NamedKey::Enter), KeyPhase::Down, Modifiers::default())
            .expect("down");
        assert_eq!(down.kind, "keyDown");
        assert_eq!(down.key, "Enter");
        assert_eq!(down.windows_virtual_key_code, 13);
        let up =
            key_event(&Key::Named(NamedKey::Enter), KeyPhase::Up, Modifiers::default()).expect("up");
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
