# Browser View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the throwaway browser-blade spike into a real, switchable browser view in `crates/junto-iced`'s right blade — URL bar, back/forward/reload, mouse+keyboard injected over CDP with correct coordinate mapping, a viewport sized to the blade and display scale factor, an Orca-style persistent-but-reaped process lifecycle, and a no-Chromium error state.

**Architecture:** A bidirectional CDP driver (`iced::stream::channel` worker with a `Sender<cdp::Command>` handshake) drives an already-installed Chromium; frames come back as `Message::BrowserFrame`. Pure logic (`browser.rs`) and CDP protocol (`cdp.rs`) are unit-tested; a custom `screencast.rs` `Widget` renders frames and injects input; `main.rs` wires state, the view, and lifecycle. No new dependencies.

**Tech Stack:** Rust 2024, `iced` 0.14 (features already enabled: tokio, image, canvas, markdown, advanced), `tokio-tungstenite` 0.29, `serde_json`, `base64`. Chromium (Edge/Chrome) over the DevTools Protocol.

**Spec:** [`docs/superpowers/specs/2026-08-27-browser-view-design.md`](../specs/2026-08-27-browser-view-design.md) — read it first; this plan argues from it.

## Global Constraints

- `crates/junto-iced` is its **own cargo workspace**. Every cargo command MUST pass `--manifest-path crates/junto-iced/Cargo.toml`. A bare `cargo test` at the repo root does not touch it.
- **No `unwrap`, `expect`, or `panic!` in library code** (tests may use them). Use `let … else`, `?`, `.ok()`, `unwrap_or_default`.
- **Clippy runs with `-D warnings`; `fmt` is checked.** Run `cargo fmt --manifest-path crates/junto-iced/Cargo.toml` and `cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings` before considering any task done.
- **Newtypes over bare primitives; comment the *why*, not the *what*** (repo CLAUDE.md).
- **No new dependencies.** Reuse what `Cargo.toml` already has.
- **Windows and macOS are both first-class:** platform-specific code sits behind `#[cfg(target_os = …)]` with every arm real.
- **De-SPIKE only the browser code.** Do NOT relabel the crate: leave `Cargo.toml`'s crate header and `main.rs`'s top module doc comment (the pane-workspace spike framing, ADR 0018) untouched. Do NOT write an ADR. Do NOT build the three gestures.
- TDD: write the failing test, watch it fail, implement minimally, watch it pass, commit. Pure modules are strict TDD; `view()`/widget/driver are verified by `iced_test` structure assertions + a live smoke run (Task 15), since Iced cannot unit-test `view()`.

---

### Task 1: `browser.rs` — viewport, page point, URL, cursor mapping

**Files:**
- Create: `crates/junto-iced/src/browser.rs`
- Modify: `crates/junto-iced/src/main.rs` (add `mod browser;` beside `mod cdp;` at line 12)

**Interfaces:**
- Produces: `ViewportSize { width: u32, height: u32 }` + `ViewportSize::from_logical(f32, f32) -> Self`; `PagePoint { x: f32, y: f32 }`; `map_cursor(f32, f32, f32, f32) -> PagePoint`; `normalize_url(&str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/junto-iced/src/browser.rs` with only the module doc comment and this test module:

```rust
//! Pure, renderer-neutral browser-view logic: URL normalization, the emulated
//! viewport and its coordinate mapping, keyboard translation, and the
//! navigation model. No `iced`, no I/O — the decidable parts of the browser
//! view live here so they are unit-tested without a running app, exactly as
//! `pointing.rs` keeps the pointing gesture's logic testable in isolation.

#[cfg(test)]
mod viewport_url_tests {
    use super::*;

    #[test]
    fn logical_size_rounds_and_never_collapses_to_zero() {
        // The emulated CSS viewport is the widget's logical size; a degenerate
        // 0-height blade must still yield a layout-able viewport, not a 0 that
        // Chromium rejects.
        assert_eq!(ViewportSize::from_logical(519.6, 1399.4), ViewportSize { width: 520, height: 1399 });
        assert_eq!(ViewportSize::from_logical(0.0, 0.0), ViewportSize { width: 1, height: 1 });
    }

    #[test]
    fn a_cursor_maps_one_to_one_and_clamps_to_the_viewport() {
        // Viewport == widget logical size, so mapping is identity; a cursor on
        // the far edge must not produce an out-of-viewport CSS coordinate.
        assert_eq!(map_cursor(12.0, 34.0, 520.0, 1400.0), PagePoint { x: 12.0, y: 34.0 });
        assert_eq!(map_cursor(600.0, -5.0, 520.0, 1400.0), PagePoint { x: 520.0, y: 0.0 });
    }

    #[test]
    fn a_bare_host_gets_https_and_explicit_schemes_pass_through() {
        assert_eq!(normalize_url(" example.com "), Some("https://example.com".to_owned()));
        assert_eq!(normalize_url("http://x.test"), Some("http://x.test".to_owned()));
        assert_eq!(normalize_url("about:blank"), Some("about:blank".to_owned()));
        assert_eq!(normalize_url("data:text/html,hi"), Some("data:text/html,hi".to_owned()));
        assert_eq!(normalize_url("   "), None);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml browser::`
Expected: FAIL (types/functions not found).

- [ ] **Step 3: Implement**

Prepend, above the test module:

```rust
/// The emulated CSS viewport, in CSS pixels. Equal to the screencast widget's
/// logical size (see the widget in `screencast.rs`): making the emulated
/// viewport match the widget is what lets `map_cursor` be 1:1 and keeps the
/// frame crisp — the display scale factor rides separately as
/// `deviceScaleFactor`, so the frame renders at physical resolution.
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
```

Add `mod browser;` to `main.rs` at line 12 (keep the existing `mod cdp; mod pointing; mod popover; mod shell;` block alphabetical-ish; place `mod browser;` first).

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test --manifest-path crates/junto-iced/Cargo.toml browser::`
Expected: PASS (3 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --manifest-path crates/junto-iced/Cargo.toml
cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings
git add crates/junto-iced/src/browser.rs crates/junto-iced/src/main.rs
git commit -m "feat(iced): browser.rs viewport/url/cursor-mapping primitives"
```

---

### Task 2: `browser.rs` — keyboard model and CDP key mapping

**Files:**
- Modify: `crates/junto-iced/src/browser.rs`

**Interfaces:**
- Produces: `Key { Char(char), Named(NamedKey) }`; `NamedKey` (Enter/Tab/Backspace/Delete/Escape/Arrow{Up,Down,Left,Right}/Home/End/PageUp/PageDown); `Modifiers { alt, ctrl, meta, shift }` + `Modifiers::bits(self) -> i64`; `KeyPhase { Down, Up }`; `KeyEvent { kind: &'static str, key: String, code: &'static str, windows_virtual_key_code: i64, text: Option<String>, modifiers: i64 }`; `key_event(&Key, KeyPhase, Modifiers) -> Option<KeyEvent>`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing tests**

Append a test module to `browser.rs`:

```rust
#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn a_printable_char_is_one_char_event_on_the_down_phase_only() {
        // CDP inserts text with a single `char` event; the matching key-up for a
        // character is a no-op, so it must not double-inject.
        let down = key_event(&Key::Char('a'), KeyPhase::Down, Modifiers::default()).expect("down");
        assert_eq!(down.kind, "char");
        assert_eq!(down.text.as_deref(), Some("a"));
        assert!(key_event(&Key::Char('a'), KeyPhase::Up, Modifiers::default()).is_none());
    }

    #[test]
    fn a_named_key_carries_its_virtual_key_code_on_both_phases() {
        let down = key_event(&Key::Named(NamedKey::Enter), KeyPhase::Down, Modifiers::default()).expect("down");
        assert_eq!(down.kind, "keyDown");
        assert_eq!(down.key, "Enter");
        assert_eq!(down.windows_virtual_key_code, 13);
        let up = key_event(&Key::Named(NamedKey::Enter), KeyPhase::Up, Modifiers::default()).expect("up");
        assert_eq!(up.kind, "keyUp");
    }

    #[test]
    fn modifiers_fold_to_the_cdp_bitmask() {
        // CDP: Alt=1, Ctrl=2, Meta=4, Shift=8.
        let mods = Modifiers { alt: false, ctrl: true, meta: false, shift: true };
        assert_eq!(mods.bits(), 2 | 8);
        let ev = key_event(&Key::Named(NamedKey::ArrowLeft), KeyPhase::Down, mods).expect("down");
        assert_eq!(ev.modifiers, 10);
        assert_eq!(ev.windows_virtual_key_code, 37);
    }
}
```

- [ ] **Step 2: Run and watch it fail** — `cargo test --manifest-path crates/junto-iced/Cargo.toml browser::key_tests` → FAIL.

- [ ] **Step 3: Implement**

Append to `browser.rs` (above the test modules):

```rust
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
/// mapped to a wrong code, which would inject garbage into a signed page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter, Tab, Backspace, Delete, Escape,
    ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
    Home, End, PageUp, PageDown,
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
        (self.alt as i64) | ((self.ctrl as i64) << 1) | ((self.meta as i64) << 2) | ((self.shift as i64) << 3)
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
                kind: if phase == KeyPhase::Down { "keyDown" } else { "keyUp" },
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
```

- [ ] **Step 4: Run and watch it pass** — same test path → PASS.
- [ ] **Step 5: fmt + clippy + commit** (`feat(iced): browser.rs keyboard→CDP mapping`).

---

### Task 3: `browser.rs` — navigation model

**Files:**
- Modify: `crates/junto-iced/src/browser.rs`

**Interfaces:**
- Produces: `NavState { url: String, can_back: bool, can_forward: bool, loading: bool }` (derive `Clone, PartialEq, Eq, Default`); `nav_from_history(current_index: i64, urls: &[String]) -> NavState`.

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run and watch it fail.**
- [ ] **Step 3: Implement** (above the test modules):

```rust
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
    NavState {
        url: urls.get(index).cloned().unwrap_or_default(),
        can_back: current_index > 0,
        can_forward: index + 1 < urls.len(),
        loading: false,
    }
}
```

- [ ] **Step 4: Run and watch it pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(iced): browser.rs navigation model`).

---

### Task 4: `cdp.rs` — de-SPIKE, `DebugPort`, platform-real `find_chromium`

**Files:**
- Modify: `crates/junto-iced/src/cdp.rs`

**Interfaces:**
- Produces: `DebugPort(pub u16)`; `find_chromium() -> Option<PathBuf>` (unchanged signature, new body); `pick(Vec<PathBuf>, impl Fn(&Path) -> bool) -> Option<PathBuf>`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test** — append to `cdp.rs`'s existing `#[cfg(test)] mod parse_tests` a sibling module:

```rust
#[cfg(test)]
mod platform_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn pick_returns_the_first_existing_candidate() {
        let candidates = vec![PathBuf::from("/no/such/a"), PathBuf::from("/yes/b"), PathBuf::from("/yes/c")];
        let chosen = pick(candidates, |p| p.starts_with("/yes"));
        assert_eq!(chosen, Some(PathBuf::from("/yes/b")));
    }

    #[test]
    fn pick_is_none_when_nothing_exists() {
        assert_eq!(pick(vec![PathBuf::from("/no/a")], |_| false), None);
    }

    #[test]
    fn this_platform_offers_real_candidates() {
        // Every first-class platform must name real install paths; an empty list
        // would make the browser view permanently unavailable there.
        assert!(!chromium_candidates().is_empty());
    }
}
```

- [ ] **Step 2: Run and watch it fail** — `cargo test --manifest-path crates/junto-iced/Cargo.toml cdp::platform_tests` → FAIL.

- [ ] **Step 3: Implement** — replace the spike's `find_chromium` (lines 47–62) and rewrite the module doc header. Add the `DebugPort` newtype near the top (after the `use` lines):

```rust
/// The DevTools debugging port Chromium actually bound. A newtype because a
/// bare `u16` is easy to swap with any other port at a call site, and the
/// wrong one silently fails the WebSocket upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugPort(pub u16);
```

```rust
/// Candidate Chromium executables for this platform, most-preferred first.
/// Every first-class arm names real install paths (the task requires both
/// Windows and macOS be real); Linux is included as it is free to support.
fn chromium_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        vec![
            PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/microsoft-edge"),
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// The first candidate for which `exists` is true. Factored out so candidate
/// selection is testable without touching the filesystem.
fn pick(candidates: Vec<PathBuf>, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.into_iter().find(|path| exists(path))
}

/// Locate an installed Chromium. An explicit `JUNTO_BROWSER` path wins (the
/// user's escape hatch and the test seam); otherwise the first existing
/// platform candidate. `None` on a platform with no install found, which the
/// view turns into the no-browser error state.
pub fn find_chromium() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("JUNTO_BROWSER").map(PathBuf::from)
        && explicit.exists()
    {
        return Some(explicit);
    }
    pick(chromium_candidates(), |path| path.exists())
}
```

Rewrite the file's `//! SPIKE …` header (lines 1–23) into a real module doc: keep the launch-switch rationale verbatim (it is still load-bearing) but drop the "throwaway"/"spike answers a narrow question" framing and the "Deliberately NOT here: input injection…" paragraph (those are now built). Example opening: `//! Driving an already-installed Chromium over the DevTools Protocol — the browser view's transport. No engine embedded (finding f463944e); frames arrive as JPEG screencast frames and input/navigation go back as CDP commands.`

- [ ] **Step 4: Run and watch it pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(iced): platform-real find_chromium + DebugPort`).

---

### Task 5: `cdp.rs` — `Command`, request builders, parsers

**Files:**
- Modify: `crates/junto-iced/src/cdp.rs`

**Interfaces:**
- Produces:
  - `Command` enum: `Navigate(String)`, `Reload`, `Back`, `Forward`, `Mouse { kind: MouseKind, at: browser::PagePoint, button: MouseButton, modifiers: i64 }`, `Scroll { at: browser::PagePoint, dx: f32, dy: f32, modifiers: i64 }`, `Key(browser::KeyEvent)`, `SetViewport { size: browser::ViewportSize, scale: f32 }`, `Show`, `Hide`. Derive `Debug, Clone`.
  - `MouseKind { Pressed, Released, Moved }`, `MouseButton { Left, Right, Middle }` (derive `Debug, Clone, Copy, PartialEq, Eq`).
  - Request builders (all `-> String`): `enable_request(u64)`, `navigate_request(u64, &str)`, `reload_request(u64)`, `navigation_history_request(u64)`, `navigate_to_history_request(u64, i64)`, `set_device_metrics_request(u64, browser::ViewportSize, f32)`, `stop_screencast_request(u64)`, `mouse_request(u64, MouseKind, browser::PagePoint, MouseButton, i64)`, `wheel_request(u64, browser::PagePoint, f32, f32, i64)`, `key_request(u64, &browser::KeyEvent)`. (`start_screencast_request` and `frame_ack_request` already exist.)
  - `NavHistory { current_index: i64, entries: Vec<NavEntry> }`, `NavEntry { id: i64, url: String }`; `parse_navigation_history(&str) -> Option<NavHistory>`.
  - `PageSignal { Navigated, Loaded, LoadingStarted }`; `parse_page_signal(&str) -> Option<PageSignal>`.
  - Simplify `Frame` to `{ jpeg: Vec<u8> }` (device size is no longer used for mapping); `parse_screencast_frame(&str) -> Option<(Frame, i64)>` keeps returning the ack session id.
- Consumes: `browser::{PagePoint, ViewportSize, KeyEvent}`.

- [ ] **Step 1: Write the failing tests** — add to `parse_tests` (adapting the existing frame test to the simplified `Frame`):

```rust
    #[test]
    fn a_screencast_frame_yields_its_bytes_and_ack_session() {
        let msg = r#"{"method":"Page.screencastFrame","params":{"data":"aGVsbG8=","sessionId":7}}"#;
        let (frame, session) = parse_screencast_frame(msg).expect("should parse");
        assert_eq!(frame.jpeg, b"hello");
        assert_eq!(session, 7);
    }

    #[test]
    fn navigate_and_metrics_requests_carry_their_params() {
        assert!(navigate_request(3, "https://x.test").contains("Page.navigate"));
        assert!(navigate_request(3, "https://x.test").contains("https://x.test"));
        let m = set_device_metrics_request(4, browser::ViewportSize { width: 520, height: 1400 }, 1.5);
        assert!(m.contains("Emulation.setDeviceMetricsOverride"));
        assert!(m.contains("\"width\":520"));
        assert!(m.contains("\"deviceScaleFactor\":1.5"));
        assert!(m.contains("\"mobile\":false"));
    }

    #[test]
    fn a_mouse_press_names_its_button_and_click_count() {
        let req = mouse_request(5, MouseKind::Pressed, browser::PagePoint { x: 10.0, y: 20.0 }, MouseButton::Left, 0);
        assert!(req.contains("Input.dispatchMouseEvent"));
        assert!(req.contains("\"type\":\"mousePressed\""));
        assert!(req.contains("\"button\":\"left\""));
        assert!(req.contains("\"clickCount\":1"));
        // A move carries no button and clickCount 0.
        let mv = mouse_request(6, MouseKind::Moved, browser::PagePoint { x: 1.0, y: 2.0 }, MouseButton::Left, 0);
        assert!(mv.contains("\"type\":\"mouseMoved\""));
        assert!(mv.contains("\"button\":\"none\""));
    }

    #[test]
    fn a_key_request_serializes_the_mapped_event() {
        let ev = browser::key_event(&browser::Key::Named(browser::NamedKey::Enter), browser::KeyPhase::Down, browser::Modifiers::default()).expect("ev");
        let req = key_request(7, &ev);
        assert!(req.contains("Input.dispatchKeyEvent"));
        assert!(req.contains("\"windowsVirtualKeyCode\":13"));
    }

    #[test]
    fn a_history_reply_parses_into_entries_and_index() {
        let reply = r#"{"id":9,"result":{"currentIndex":1,"entries":[{"id":10,"url":"a"},{"id":11,"url":"b"}]}}"#;
        let h = parse_navigation_history(reply).expect("history");
        assert_eq!(h.current_index, 1);
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[1].url, "b");
        // A screencast frame is not a history reply.
        assert!(parse_navigation_history(r#"{"method":"Page.screencastFrame"}"#).is_none());
    }

    #[test]
    fn page_signals_are_recognized() {
        assert_eq!(parse_page_signal(r#"{"method":"Page.loadEventFired"}"#), Some(PageSignal::Loaded));
        assert_eq!(parse_page_signal(r#"{"method":"Page.frameStartedLoading"}"#), Some(PageSignal::LoadingStarted));
        assert!(parse_page_signal(r#"{"id":1,"result":{}}"#).is_none());
    }
```

Also update the existing `the_start_request_bounds_the_frame_size` test to keep passing (unchanged) and add `use super::*;` is already present.

- [ ] **Step 2: Run and watch it fail.**

- [ ] **Step 3: Implement.** Simplify `Frame` (lines 153–160) to:

```rust
/// One decoded screencast frame — just the JPEG bytes. The frame's device
/// dimensions are intentionally not carried: the emulated viewport equals the
/// widget, so input mapping needs no per-frame size (`browser::map_cursor`).
#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
}
```

Update `parse_screencast_frame` (lines 194–219) to drop the `metadata`/`device_*` extraction and return `Frame { jpeg }`. Add the enums and builders (model them on the existing `start_screencast_request`/`frame_ack_request` `serde_json::json!` style). Key ones:

```rust
/// A command the app sends into the driver; the driver turns each into one or
/// more CDP requests on the page socket.
#[derive(Debug, Clone)]
pub enum Command {
    Navigate(String),
    Reload,
    Back,
    Forward,
    Mouse { kind: MouseKind, at: browser::PagePoint, button: MouseButton, modifiers: i64 },
    Scroll { at: browser::PagePoint, dx: f32, dy: f32, modifiers: i64 },
    Key(browser::KeyEvent),
    SetViewport { size: browser::ViewportSize, scale: f32 },
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind { Pressed, Released, Moved }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton { Left, Right, Middle }

impl MouseKind {
    fn cdp_type(self) -> &'static str {
        match self { Self::Pressed => "mousePressed", Self::Released => "mouseReleased", Self::Moved => "mouseMoved" }
    }
}
impl MouseButton {
    fn cdp_name(self) -> &'static str {
        match self { Self::Left => "left", Self::Right => "right", Self::Middle => "middle" }
    }
}

pub fn enable_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.enable" }).to_string()
}
pub fn navigate_request(id: u64, url: &str) -> String {
    serde_json::json!({ "id": id, "method": "Page.navigate", "params": { "url": url } }).to_string()
}
pub fn reload_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.reload" }).to_string()
}
pub fn navigation_history_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.getNavigationHistory" }).to_string()
}
pub fn navigate_to_history_request(id: u64, entry_id: i64) -> String {
    serde_json::json!({ "id": id, "method": "Page.navigateToHistoryEntry", "params": { "entryId": entry_id } }).to_string()
}
pub fn set_device_metrics_request(id: u64, size: browser::ViewportSize, scale: f32) -> String {
    serde_json::json!({
        "id": id, "method": "Emulation.setDeviceMetricsOverride",
        "params": { "width": size.width, "height": size.height, "deviceScaleFactor": scale, "mobile": false }
    }).to_string()
}
pub fn stop_screencast_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.stopScreencast" }).to_string()
}
pub fn mouse_request(id: u64, kind: MouseKind, at: browser::PagePoint, button: MouseButton, modifiers: i64) -> String {
    // A move carries no button and a zero click-count; a press/release names
    // the button and one click.
    let click_count = if matches!(kind, MouseKind::Moved) { 0 } else { 1 };
    let button_name = if matches!(kind, MouseKind::Moved) { "none" } else { button.cdp_name() };
    serde_json::json!({
        "id": id, "method": "Input.dispatchMouseEvent",
        "params": { "type": kind.cdp_type(), "x": at.x, "y": at.y, "button": button_name, "clickCount": click_count, "modifiers": modifiers }
    }).to_string()
}
pub fn wheel_request(id: u64, at: browser::PagePoint, dx: f32, dy: f32, modifiers: i64) -> String {
    serde_json::json!({
        "id": id, "method": "Input.dispatchMouseEvent",
        "params": { "type": "mouseWheel", "x": at.x, "y": at.y, "deltaX": dx, "deltaY": dy, "modifiers": modifiers }
    }).to_string()
}
pub fn key_request(id: u64, ev: &browser::KeyEvent) -> String {
    serde_json::json!({
        "id": id, "method": "Input.dispatchKeyEvent",
        "params": {
            "type": ev.kind, "key": ev.key, "code": ev.code,
            "windowsVirtualKeyCode": ev.windows_virtual_key_code,
            "text": ev.text, "modifiers": ev.modifiers
        }
    }).to_string()
}

/// A `Page.getNavigationHistory` reply, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavHistory { pub current_index: i64, pub entries: Vec<NavEntry> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavEntry { pub id: i64, pub url: String }

/// Parse a navigation-history reply. `None` for any other message (the socket
/// carries frames, events, and other replies too).
pub fn parse_navigation_history(text: &str) -> Option<NavHistory> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    let result = msg.get("result")?;
    let current_index = result.get("currentIndex")?.as_i64()?;
    let entries = result.get("entries")?.as_array()?
        .iter()
        .filter_map(|e| Some(NavEntry { id: e.get("id")?.as_i64()?, url: e.get("url")?.as_str()?.to_owned() }))
        .collect();
    Some(NavHistory { current_index, entries })
}

/// The page-lifecycle events that mean "re-query the history / flip loading".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSignal { Navigated, Loaded, LoadingStarted }

pub fn parse_page_signal(text: &str) -> Option<PageSignal> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    match msg.get("method")?.as_str()? {
        "Page.frameNavigated" => Some(PageSignal::Navigated),
        "Page.loadEventFired" => Some(PageSignal::Loaded),
        "Page.frameStartedLoading" => Some(PageSignal::LoadingStarted),
        _ => None,
    }
}
```

- [ ] **Step 4: Run and watch it pass** (`cargo test --manifest-path crates/junto-iced/Cargo.toml cdp::`).
- [ ] **Step 5: fmt + clippy + commit** (`feat(iced): CDP command/request builders + nav parsers`).

---

### Task 6: `cdp.rs` — spawn/port signatures for the driver

**Files:**
- Modify: `crates/junto-iced/src/cdp.rs`

**Interfaces:**
- Changes: `spawn(exe: &Path, url: &str, initial: browser::ViewportSize) -> Result<Chromium, String>` (was `(exe, url, width: u32, height: u32)`); `Chromium.port: DebugPort`; `page_websocket_url(port: DebugPort) -> Result<String, String>`; `wait_for_port(...) -> Result<DebugPort, String>`.
- Consumes: `browser::ViewportSize`, `DebugPort`.

- [ ] **Step 1: Implement** — update `spawn` to take `initial: browser::ViewportSize`, using it for `--window-size={w},{h}` (the launch size; real layout comes from `SetViewport`). Wrap the parsed port as `DebugPort`. Thread `DebugPort` through `Chromium.port`, `wait_for_port`, and `page_websocket_url` (its `format!("http://127.0.0.1:{port}/…")` becomes `port.0`). Keep the `Drop` reaper and the launch switches unchanged.

- [ ] **Step 2: Verify it compiles** — `cargo build --manifest-path crates/junto-iced/Cargo.toml` (call sites in `main.rs`/`cdp_probe.rs` will be updated in Tasks 10/14; if `cdp_probe.rs` still exists and breaks the build, this task may be committed together with Task 14, or temporarily update the probe's call. Simplest: do Task 14's `cdp_probe.rs` deletion before this builds cleanly — see the note in Task 14).

- [ ] **Step 3: fmt + clippy + commit** (`refactor(iced): spawn takes ViewportSize; DebugPort through cdp`).

> Note: Tasks 6, 10, and 14 all touch the build graph across `main.rs`/`cdp_probe.rs`; if executing sequentially, do 14's `cdp_probe.rs` deletion first so the workspace builds with the new `spawn` signature. If parallelizing, one owner takes Tasks 6+10+14 together.

---

### Task 7: `shell.rs` — `RightView` + persisted URL

**Files:**
- Modify: `crates/junto-iced/src/shell.rs`

**Interfaces:**
- Produces: `RightView { Lineage, Browser }` (`#[default] Lineage`, kebab-case serde, derive `Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize`); `ShellState.right_view: RightView`; `ShellState.browser_url: Option<String>`.

- [ ] **Step 1: Write the failing tests** — extend `persistence_tests`:

```rust
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
```

- [ ] **Step 2: Run and watch it fail.**
- [ ] **Step 3: Implement** — add the enum near `BottomView`:

```rust
/// Which surface the right blade shows. Persisted like the other blade state
/// so the choice survives a restart (the task requires this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightView {
    #[default]
    Lineage,
    Browser,
}
```

Add to `ShellState` (both fields under the existing `#[serde(default)]`, so old files still parse):

```rust
    /// Which surface the right blade shows.
    pub right_view: RightView,
    /// The browser view's last-visited URL, so it reopens where you left off.
    /// `None` until the first navigation.
    pub browser_url: Option<String>,
```

Add both to the hand-written `Default::default()` (`right_view: RightView::Lineage, browser_url: None`).

- [ ] **Step 4: Run and watch it pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(iced): persist RightView + last browser URL`).

---

### Task 8: `screencast.rs` — the input-injecting widget

**Files:**
- Create: `crates/junto-iced/src/screencast.rs`
- Modify: `crates/junto-iced/src/main.rs` (add `mod screencast;`)

**Interfaces:**
- Produces: `screencast(frame: Option<&image::Handle>) -> Screencast<'_, Message>` builder + setters `.on_resize(Fn(iced::Size) -> Message)`, `.on_mouse(Fn(cdp::MouseKind, browser::PagePoint, cdp::MouseButton) -> Message)`, `.on_scroll(Fn(browser::PagePoint, f32, f32) -> Message)`, `.on_key(Fn(browser::Key, browser::KeyPhase, browser::Modifiers) -> Message)`. `From<Screencast> for Element`.
- Consumes: `browser::{PagePoint, Key, KeyPhase, Modifiers, map_cursor}`, `cdp::{MouseKind, MouseButton}`.

- [ ] **Step 1: Implement the widget** (model directly on `popover.rs`'s `Widget` impl shape; leaf widget, so `children`/`diff`/`operate`/`overlay` use defaults).

Key points to get right:
- Generic over `Renderer: iced::advanced::Renderer + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>`.
- `tag`/`state`: a `State { bounds: Rectangle, focused: bool, cursor: Option<Point> }` via `tree::Tag::of::<State>()` / `tree::State::new(State::default())`.
- `size`: `Size { width: Length::Fill, height: Length::Fill }`.
- `layout`: `layout::Node::new(limits.max())` (fill).
- `draw`: if `Some(handle)`, call `iced_widget::image::draw(renderer, layout, handle, None, iced::border::Radius::default(), iced::ContentFit::Fill, iced::advanced::image::FilterMethod::Linear, iced::Rotation::default(), 1.0, 1.0)`; else `renderer.fill_quad` a `SURFACE`-colored quad so the widget still occupies (and reports) the blade before the first frame. (`SURFACE` is defined in `main.rs`; pass it in as a `background: Color` field on `Screencast`, set by the caller, to keep the widget palette-agnostic.)
- `update(&mut self, tree, event, layout, cursor, _renderer, _clipboard, shell, _viewport)`:
  - `let state: &mut State = tree.state.downcast_mut();`
  - `let bounds = layout.bounds();` if `bounds != state.bounds { state.bounds = bounds; shell.publish((self.on_resize)(bounds.size())); }`
  - Compute `let local = cursor.position_in(bounds);` (Some when over).
  - `Event::Mouse(ButtonPressed(Left))`: if `local` is Some → `state.focused = true`; publish `on_mouse(Pressed, map_cursor(local.x, local.y, bounds.width, bounds.height), Left)`; `shell.capture_event()`. If `local` is None → `state.focused = false` (click elsewhere unfocuses the page).
  - `ButtonReleased(Left)` / right / middle press+release: same mapping (only when `local` is Some), publish `on_mouse(...)`; capture on press.
  - `CursorMoved` while `local` is Some: publish `on_mouse(Moved, map_cursor(...), Left)` and update `state.cursor`.
  - `WheelScrolled { delta }` while `local` is Some: convert `delta` to (dx, dy) pixels (`mouse::ScrollDelta::Pixels { x, y }` → as-is; `Lines { x, y }` → multiply by a line height, e.g. 40.0) and publish `on_scroll(map_cursor(...), dx, dy)`; capture.
  - `Event::Keyboard(KeyPressed { key, text, modifiers, .. })` when `state.focused`: `if let Some(k) = to_key(&key, text.as_deref()) { publish on_key(k, KeyPhase::Down, to_mods(modifiers)); shell.capture_event(); }`.
  - `Event::Keyboard(KeyReleased { key, modifiers, .. })` when `state.focused`: same with `KeyPhase::Up` (drops to `None` for `Char`, so releases of typed characters vanish — correct).
- `mouse_interaction`: return `mouse::Interaction::Idle` when over bounds (the page owns its own cursor semantics; a text-cursor would lie).

Add private helpers in `screencast.rs`:

```rust
fn to_key(key: &iced::keyboard::Key, text: Option<&str>) -> Option<browser::Key> {
    use iced::keyboard::key::Named as N;
    // Prefer the platform-resolved text for printables (respects shift/layout).
    if let Some(t) = text
        && let Some(c) = t.chars().next()
        && !c.is_control()
    {
        return Some(browser::Key::Char(c));
    }
    let named = match key {
        iced::keyboard::Key::Named(N::Enter) => browser::NamedKey::Enter,
        iced::keyboard::Key::Named(N::Tab) => browser::NamedKey::Tab,
        iced::keyboard::Key::Named(N::Backspace) => browser::NamedKey::Backspace,
        iced::keyboard::Key::Named(N::Delete) => browser::NamedKey::Delete,
        iced::keyboard::Key::Named(N::Escape) => browser::NamedKey::Escape,
        iced::keyboard::Key::Named(N::ArrowUp) => browser::NamedKey::ArrowUp,
        iced::keyboard::Key::Named(N::ArrowDown) => browser::NamedKey::ArrowDown,
        iced::keyboard::Key::Named(N::ArrowLeft) => browser::NamedKey::ArrowLeft,
        iced::keyboard::Key::Named(N::ArrowRight) => browser::NamedKey::ArrowRight,
        iced::keyboard::Key::Named(N::Home) => browser::NamedKey::Home,
        iced::keyboard::Key::Named(N::End) => browser::NamedKey::End,
        iced::keyboard::Key::Named(N::PageUp) => browser::NamedKey::PageUp,
        iced::keyboard::Key::Named(N::PageDown) => browser::NamedKey::PageDown,
        _ => return None,
    };
    Some(browser::Key::Named(named))
}

fn to_mods(m: iced::keyboard::Modifiers) -> browser::Modifiers {
    browser::Modifiers { alt: m.alt(), ctrl: m.control(), meta: m.logo(), shift: m.shift() }
}
```

Add `mod screencast;` to `main.rs`.

- [ ] **Step 2: Write an `iced_test` structure test** in `screencast.rs`:

```rust
#[cfg(test)]
mod tests {
    // A minimal harness message + a container so the simulator can lay the
    // widget out and confirm it fills its region (the property that lets it
    // report a real viewport size).
    #[test]
    fn the_widget_fills_its_region() {
        use iced::widget::container;
        use iced::{Element, Length};
        #[derive(Debug, Clone)]
        enum M { Resize(iced::Size), Mouse, Scroll, Key }
        let view: Element<'_, M> = container(
            super::screencast(None)
                .background(iced::Color::BLACK)
                .on_resize(M::Resize)
                .on_mouse(|_, _, _| M::Mouse)
                .on_scroll(|_, _, _| M::Scroll)
                .on_key(|_, _, _| M::Key),
        )
        .width(Length::Fixed(300.0))
        .height(Length::Fixed(200.0))
        .into();
        let mut ui = iced_test::simulator(view);
        // Laying it out must not panic and the tree must build.
        let _ = ui.into_target_bounds();
    }
}
```

(If `iced_test`'s exact simulator entry differs, mirror the pattern already used in `main.rs`'s `iced_test::simulator(app.view())` tests. The point is: it lays out without panicking. Deeper event simulation is left to the live smoke test, Task 15.)

- [ ] **Step 3: Run** — `cargo test --manifest-path crates/junto-iced/Cargo.toml screencast::` → PASS; `cargo clippy … -D warnings` clean.
- [ ] **Step 4: fmt + commit** (`feat(iced): screencast widget — frame draw + input injection`).

---

### Task 9: `main.rs` — App state and Message variants

**Files:**
- Modify: `crates/junto-iced/src/main.rs`

**Interfaces:**
- Produces: new `App` fields and `Message` variants used by Tasks 10–13.

- [ ] **Step 1: Replace the SPIKE `App` fields** (lines 256–271) with:

```rust
    /// The control end of the running browser driver — `None` until the driver
    /// sends `BrowserReady`, and after the browser exits.
    browser_cmd: Option<iced::futures::channel::mpsc::Sender<cdp::Command>>,
    /// The newest screencast frame, decoded once into an image handle. Only one
    /// is kept — the stream is change-driven; a backlog would only ever render
    /// as staleness.
    browser_frame: Option<iced::widget::image::Handle>,
    /// The current page's navigation state (url + back/forward + loading).
    browser_nav: browser::NavState,
    /// The URL-bar text — mirrors the page location, editable while typing.
    browser_url_input: String,
    /// A driver-reported error (no Chromium, spawn/connect failure, or exit);
    /// shown in place of the frame.
    browser_error: Option<String>,
    /// The display scale factor, queried from the window; the frame renders at
    /// `logical × scale` physical pixels so text is crisp on a HiDPI display.
    browser_scale: f32,
    /// The browser widget's last reported logical size — paired with `scale`
    /// to size the emulated viewport.
    browser_logical: Option<iced::Size>,
    /// Latched true the first time the Browser view is opened; keeps the driver
    /// subscription alive across view switches (Orca's tab persistence), so the
    /// page survives a peek at lineage. Cleared only when the browser exits.
    browser_ever_opened: bool,
    /// Salts the driver subscription id so a crashed/closed browser can be
    /// cleanly respawned with a fresh stream instance rather than a dead one.
    browser_generation: u64,
    /// The main window id, for querying the scale factor.
    window_id: Option<iced::window::Id>,
```

- [ ] **Step 2: Update `App::new`** (line 1769) — remove the spike initializers (`browser_on: true`, `browser_frame*`), add:

```rust
    browser_cmd: None,
    browser_frame: None,
    browser_nav: browser::NavState::default(),
    browser_url_input: shell_loaded.browser_url.clone().unwrap_or_default(),
    browser_error: None,
    browser_scale: 1.0,
    browser_logical: None,
    browser_ever_opened: matches!(shell_loaded.right_view, shell::RightView::Browser),
    browser_generation: 0,
    window_id: None,
```

(where `shell_loaded` is the `shell::load(...)` value; if `new()` currently inlines it, bind it to a local first.) Then batch the scale-factor query into `new()`'s returned `Task`:

```rust
    // Learn the window id and its scale factor once at startup; refreshed on
    // resize (subscription). Chained so scale_factor targets the real window.
    let scale = iced::window::latest().and_then(|maybe| match maybe {
        Some(id) => iced::window::scale_factor(id).map(move |s| Message::WindowScale(id, s)),
        None => Task::none(),
    });
    (app, Task::batch([/* existing startup tasks */, scale]))
```

- [ ] **Step 3: Edit the `Message` enum** — remove `ToggleBrowser` (line 1516) and the old `BrowserFrame(Vec<u8>, f32, f32)` (line 1520). Add:

```rust
    /// The window's scale factor arrived (startup / resize).
    WindowScale(iced::window::Id, f32),
    /// Switch the right blade between lineage and browser (persisted).
    SelectRightView(shell::RightView),
    /// The driver handed over its command channel.
    BrowserReady(iced::futures::channel::mpsc::Sender<cdp::Command>),
    /// A decoded screencast frame (JPEG bytes).
    BrowserFrame(Vec<u8>),
    /// The page's navigation state changed.
    BrowserNav(browser::NavState),
    /// The driver failed or found no Chromium.
    BrowserError(String),
    /// The browser process/socket ended.
    BrowserClosed,
    /// Re-open after an exit/error.
    BrowserReopen,
    /// URL-bar edits and submission.
    BrowserUrlInput(String),
    BrowserNavigate,
    BrowserBack,
    BrowserForward,
    BrowserReload,
    /// The browser widget's region was laid out at this logical size.
    BrowserResized(iced::Size),
    /// Mouse input from the browser widget, in page coordinates.
    BrowserMouse(cdp::MouseKind, browser::PagePoint, cdp::MouseButton),
    /// Wheel input from the browser widget, in page coordinates.
    BrowserScroll(browser::PagePoint, f32, f32),
    /// Keyboard input from the browser widget.
    BrowserKey(browser::Key, browser::KeyPhase, browser::Modifiers),
```

Confirm `Message` still derives `Clone` (it must — `mpsc::Sender` and all `browser`/`cdp` payloads are `Clone`). Add `Debug`/`Clone` derives to `browser::PagePoint` etc. already done (Task 1–3 derived them; ensure `PagePoint` derives `Clone, Copy` — it does).

- [ ] **Step 4: Verify it compiles** — the `update`/`view`/`subscription` still reference removed items; this task leaves them temporarily handled by a `todo!()`-free stub only if needed. **Better: land Tasks 9–13 as one reviewable unit** (they are the same file and interdependent). If executing task-by-task, keep Task 9 uncommitted until Task 13 compiles; if using subagent-driven-development, assign Tasks 9–13 to one agent.

> Note: Tasks 9–13 all mutate `main.rs` and only jointly compile. Treat them as one owner / one branchlet; the sub-steps below stay separate for review granularity, but the commit happens once the file compiles (end of Task 13), with `git commit` checkpoints where a compile is achievable.

---

### Task 10: `main.rs` — the bidirectional driver

**Files:**
- Modify: `crates/junto-iced/src/main.rs` (`browser_stream`, lines 8987–9065)

**Interfaces:**
- Produces: `fn browser_stream() -> impl iced::futures::Stream<Item = Message>` (no args).
- Consumes: `cdp::{find_chromium, spawn, page_websocket_url, Command, *_request, parse_*}`, `browser::{ViewportSize, nav_from_history}`.

- [ ] **Step 1: Rewrite `browser_stream`** as the driver. Structure (fill in the CDP request calls from Task 5):

```rust
/// Drives an already-installed Chromium and multiplexes its DevTools socket:
/// screencast frames out as `Message::BrowserFrame`, and app `cdp::Command`s in
/// over the channel handed back via `Message::BrowserReady`. The browser is
/// owned by this future — it is killed and its profile removed when the stream
/// ends (`cdp::Chromium`'s `Drop`), so it can never be orphaned.
fn browser_stream() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    iced::stream::channel::<Message>(8, move |mut output: mpsc::Sender<Message>| async move {
        let Some(exe) = cdp::find_chromium() else {
            let _ = output.send(Message::BrowserError(no_chromium_message())).await;
            return;
        };
        // A sane launch size; real layout is set by the first SetViewport.
        let initial = browser::ViewportSize { width: 1024, height: 768 };
        let browser = match cdp::spawn(&exe, "about:blank", initial) {
            Ok(b) => b,
            Err(e) => { let _ = output.send(Message::BrowserError(e)).await; return; }
        };
        let ws_url = match cdp::page_websocket_url(browser.port).await {
            Ok(u) => u,
            Err(e) => { let _ = output.send(Message::BrowserError(e)).await; return; }
        };
        let Ok((mut socket, _)) = tokio_tungstenite::connect_async(&ws_url).await else {
            let _ = output.send(Message::BrowserError("failed to connect to the browser".to_owned())).await;
            return;
        };

        // Command channel: the app drives the browser through this.
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<cdp::Command>(32);
        if output.send(Message::BrowserReady(cmd_tx)).await.is_err() { return; }

        let mut next_id = 1u64;
        let mut history: Option<cdp::NavHistory> = None;
        let mut loading = false;

        // Enable the Page domain up front; screencast is (re)started on Show.
        if socket.send(tungstenite::Message::Text(cdp::enable_request(next_id).into())).await.is_err() { return; }
        next_id += 1;

        loop {
            tokio::select! {
                cmd = cmd_rx.next() => {
                    let Some(cmd) = cmd else { return; }; // app dropped the sender
                    // Translate each command into one or more requests. Examples:
                    //   Navigate(url) -> navigate_request
                    //   SetViewport{size,scale} -> set_device_metrics_request (also forces a repaint)
                    //   Show -> start_screencast_request(next, size)   (a fresh frame)
                    //   Hide -> stop_screencast_request
                    //   Mouse/Scroll/Key -> mouse_request/wheel_request/key_request
                    //   Reload -> reload_request
                    //   Back/Forward -> navigate_to_history_request(history.entries[current±1].id) when history is Some
                    // Build with `next_id` (increment per request) and `socket.send`.
                    // On any send error, `return` (ends the stream → browser dies).
                    // (Full match arm written during implementation.)
                }
                wire = socket.next() => {
                    let Some(Ok(tungstenite::Message::Text(text))) = wire else {
                        // socket closed / errored
                        let _ = output.send(Message::BrowserClosed).await;
                        return;
                    };
                    if let Some((frame, session_id)) = cdp::parse_screencast_frame(&text) {
                        if output.send(Message::BrowserFrame(frame.jpeg)).await.is_err() { return; }
                        // Ack is mandatory flow control — skip it and Chromium sends one frame then goes quiet.
                        if socket.send(tungstenite::Message::Text(cdp::frame_ack_request(next_id, session_id).into())).await.is_err() { return; }
                        next_id += 1;
                    } else if let Some(signal) = cdp::parse_page_signal(&text) {
                        loading = matches!(signal, cdp::PageSignal::LoadingStarted);
                        // Re-query history on navigate/load so back/forward + url stay current.
                        if socket.send(tungstenite::Message::Text(cdp::navigation_history_request(next_id).into())).await.is_err() { return; }
                        next_id += 1;
                    } else if let Some(h) = cdp::parse_navigation_history(&text) {
                        let urls: Vec<String> = h.entries.iter().map(|e| e.url.clone()).collect();
                        let mut nav = browser::nav_from_history(h.current_index, &urls);
                        nav.loading = loading;
                        history = Some(h);
                        if output.send(Message::BrowserNav(nav)).await.is_err() { return; }
                    }
                }
            }
        }
    })
}
```

Implement the command match arm fully (the comment block). For `Show`, send `start_screencast_request(next_id, w, h)` using the last-known viewport (or `initial` if none yet) — restarting the screencast forces one fresh frame (the show/refocus repaint trigger). For `SetViewport`, keep a local `let mut viewport = initial;` updated on each `SetViewport`, and send `set_device_metrics_request` (the resize repaint trigger). Track a local `screencasting: bool` if useful.

- [ ] **Step 2: Add the `no_chromium_message` helper** near `browser_stream`:

```rust
/// The error text shown when no Chromium is installed — names what was looked
/// for so the user can act.
fn no_chromium_message() -> String {
    "No Chromium-based browser found. Install Microsoft Edge or Google Chrome, \
     or set the JUNTO_BROWSER environment variable to a Chromium executable."
        .to_owned()
}
```

- [ ] **Step 3: Compiles as part of the Task 9–13 unit** (verified at Task 13). Behavior verified in Task 15.

---

### Task 11: `main.rs` — update arms and helpers

**Files:**
- Modify: `crates/junto-iced/src/main.rs` (`update`, `persist_shell` area)

- [ ] **Step 1: Add two helpers on `App`:**

```rust
/// Send a command to the running browser, if any. A dropped/closed driver
/// makes this a no-op — the view will already be showing the exit state.
fn send_browser(&mut self, command: cdp::Command) {
    if let Some(tx) = self.browser_cmd.as_mut() {
        let _ = tx.try_send(command);
    }
}

/// Push the emulated viewport to the browser once both its size and the scale
/// factor are known. The frame renders at `logical × scale` physical pixels;
/// the CSS viewport equals the logical size, keeping input mapping 1:1.
fn sync_browser_viewport(&mut self) {
    if let (Some(size), Some(_)) = (self.browser_logical, self.browser_cmd.as_ref()) {
        let viewport = browser::ViewportSize::from_logical(size.width, size.height);
        self.send_browser(cdp::Command::SetViewport { size: viewport, scale: self.browser_scale });
    }
}
```

- [ ] **Step 2: Remove the spike `ToggleBrowser`/`BrowserFrame(3-arg)` arms** (lines 2270–2286) and add arms:

| Message | Behavior |
|---|---|
| `WindowScale(id, s)` | `self.window_id = Some(id); self.browser_scale = s; self.sync_browser_viewport(); Task::none()` |
| `SelectRightView(v)` | `self.shell.right_view = v; self.persist_shell();` if `v == Browser` `{ self.browser_ever_opened = true; self.send_browser(cdp::Command::Show); }` else `{ self.send_browser(cdp::Command::Hide); }` `Task::none()` |
| `BrowserReady(tx)` | `self.browser_cmd = Some(tx);` navigate to the persisted URL (or about:blank): `let url = self.shell.browser_url.clone().unwrap_or_else(|| "about:blank".into()); self.send_browser(cdp::Command::Navigate(url)); self.send_browser(cdp::Command::Show); self.sync_browser_viewport(); Task::none()` |
| `BrowserFrame(jpeg)` | `self.browser_frame = Some(iced::widget::image::Handle::from_bytes(jpeg)); Task::none()` |
| `BrowserNav(nav)` | `self.browser_url_input = nav.url.clone(); self.shell.browser_url = Some(nav.url.clone()); self.browser_nav = nav; self.persist_shell(); Task::none()` |
| `BrowserError(msg)` | `self.browser_error = Some(msg); Task::none()` |
| `BrowserClosed` | `self.browser_cmd = None; self.browser_frame = None; self.browser_generation += 1; self.browser_ever_opened = false; self.browser_error = Some("The browser exited. Reopen to start it again.".into()); Task::none()` |
| `BrowserReopen` | `self.browser_error = None; self.browser_ever_opened = true; Task::none()` |
| `BrowserUrlInput(s)` | `self.browser_url_input = s; Task::none()` |
| `BrowserNavigate` | `if let Some(url) = browser::normalize_url(&self.browser_url_input) { self.send_browser(cdp::Command::Navigate(url)); } Task::none()` |
| `BrowserBack` | `self.send_browser(cdp::Command::Back); Task::none()` |
| `BrowserForward` | `self.send_browser(cdp::Command::Forward); Task::none()` |
| `BrowserReload` | `self.send_browser(cdp::Command::Reload); Task::none()` |
| `BrowserResized(size)` | `self.browser_logical = Some(size); self.sync_browser_viewport(); Task::none()` |
| `BrowserMouse(kind, at, button)` | `let modifiers = to_cdp_mods(self.modifiers); self.send_browser(cdp::Command::Mouse { kind, at, button, modifiers }); Task::none()` |
| `BrowserScroll(at, dx, dy)` | `let modifiers = to_cdp_mods(self.modifiers); self.send_browser(cdp::Command::Scroll { at, dx, dy, modifiers }); Task::none()` |
| `BrowserKey(key, phase, mods)` | `if let Some(ev) = browser::key_event(&key, phase, mods) { self.send_browser(cdp::Command::Key(ev)); } Task::none()` |

Add a small free helper `fn to_cdp_mods(m: iced::keyboard::Modifiers) -> i64 { browser::Modifiers { alt: m.alt(), ctrl: m.control(), meta: m.logo(), shift: m.shift() }.bits() }` (or reuse `screencast::to_mods` made `pub(crate)`).

- [ ] **Step 3: Part of the Task 9–13 compile unit.**

---

### Task 12: `main.rs` — subscription

**Files:**
- Modify: `crates/junto-iced/src/main.rs` (`subscription`, lines 3958–3966; and the resize→scale refresh)

- [ ] **Step 1: Replace the spike `browser_sub`** (3958–3966):

```rust
    // The browser driver lives while the browser has ever been opened this
    // session (Orca-style tab persistence) — switching to lineage sends Hide,
    // not a teardown. The id is the generation only: navigations must not
    // change identity (that would respawn the browser), but a crash-driven
    // BrowserClosed bumps the generation so a reopen gets a fresh stream.
    let browser_sub = self
        .browser_ever_opened
        .then(|| iced::Subscription::run_with(self.browser_generation, |_: &u64| browser_stream()));
```

- [ ] **Step 2: Refresh the scale factor on resize.** In the existing `modifiers_sub`/`listen_with` (or a new `listen_with`), map `iced::Event::Window(iced::window::Event::Resized(_))` → a message that re-queries scale. Simplest: add a `Message::WindowResized` that returns `iced::window::scale_factor(id)` when `self.window_id` is `Some`. Add to `subscription()`:

```rust
    let resize_sub = iced::event::listen_with(|event, _status, id| match event {
        iced::Event::Window(iced::window::Event::Resized(_)) => Some(Message::WindowResized(id)),
        _ => None,
    });
```

and the arm `Message::WindowResized(id) => iced::window::scale_factor(id).map(move |s| Message::WindowScale(id, s))`. Add `WindowResized(iced::window::Id)` to `Message`. Chain `resize_sub` into the `Subscription::batch`.

- [ ] **Step 3: Part of the Task 9–13 compile unit.**

---

### Task 13: `main.rs` — the right-blade view

**Files:**
- Modify: `crates/junto-iced/src/main.rs` (`right_blade`, lines 4818–4877; add nav icons near lines 194–207)

**Interfaces:**
- Consumes: everything from Tasks 8–12.

- [ ] **Step 1: Add browser nav icon codepoints** near the other `ICON_*` consts (from Lucide 0.469.0's CSS — `arrow-left`, `arrow-right`; reuse `ICON_ROTATE_CW` for reload). Look them up in `assets/` Lucide CSS the same way the existing consts were sourced; name them `ICON_ARROW_LEFT`, `ICON_ARROW_RIGHT`.

- [ ] **Step 2: Rewrite `right_blade`** to switch on `app.shell.right_view`:

```rust
fn right_blade(app: &App) -> Element<'_, Message> {
    let content: Element<Message> = match app.shell.right_view {
        shell::RightView::Lineage => lineage_view(app),
        shell::RightView::Browser => browser_pane(app),
    };
    // A two-item segmented control replaces the spike's swap chevron.
    let switch = right_view_switch(app);
    let toggle = container(icon_button(
        ICON_CHEVRON_RIGHT,
        "collapse · ctrl+r",
        tooltip::Position::Top,
        Message::ToggleRightBlade,
    ))
    .id(iced::widget::Id::new("right-blade-toggle"));
    let footer = row![switch, Space::new().width(Fill), toggle];
    container(column![container(content).height(Fill), footer].spacing(SP))
        .padding(SP)
        .into()
}
```

- [ ] **Step 3: Add `right_view_switch`** — two ghost/segmented buttons (Lineage | Browser), the active one styled selected, each emitting `Message::SelectRightView(...)`. Mirror the styling of the deleted Artifacts/Lineage switcher or the existing chip styles.

- [ ] **Step 4: Add `browser_pane`:**

```rust
fn browser_pane(app: &App) -> Element<'_, Message> {
    if let Some(err) = &app.browser_error {
        return browser_error_view(err, app.browser_ever_opened == false);
    }
    let nav = row![
        icon_button_maybe(ICON_ARROW_LEFT, "back", app.browser_nav.can_back.then_some(Message::BrowserBack)),
        icon_button_maybe(ICON_ARROW_RIGHT, "forward", app.browser_nav.can_forward.then_some(Message::BrowserForward)),
        icon_button(ICON_ROTATE_CW, "reload", tooltip::Position::Bottom, Message::BrowserReload),
        text_input("enter a URL…", &app.browser_url_input)
            .on_input(Message::BrowserUrlInput)
            .on_submit(Message::BrowserNavigate)
            .width(Fill),
    ]
    .spacing(SP_TIGHT);

    let widget = screencast::screencast(app.browser_frame.as_ref())
        .background(SURFACE)
        .on_resize(Message::BrowserResized)
        .on_mouse(Message::BrowserMouse)
        .on_scroll(Message::BrowserScroll)
        .on_key(Message::BrowserKey);

    let body: Element<Message> = if app.browser_frame.is_none() {
        // Before the first frame: the widget (so it reports its size and drives
        // the first paint) under a centered hint.
        iced::widget::stack![
            widget,
            container(text("starting browser…").size(TEXT_META).color(MUTED)).center(Fill),
        ]
        .into()
    } else {
        widget.into()
    };

    column![nav, container(body).height(Fill)].spacing(SP).into()
}
```

Add `browser_error_view(msg, offer_reopen)`: a column with `icon(ICON_CIRCLE_ALERT)`, the message text (`MUTED`), and — when `offer_reopen` — a "reopen" button emitting `Message::BrowserReopen`. Add a small `icon_button_maybe(codepoint, tip, Option<Message>)` that renders a disabled (dimmed, no message) button when `None`, else a normal `icon_button` — mirror the pane title bar's "conditionally-disabled close" pattern referenced at `icon_button_raw`'s doc (lines 108–111).

- [ ] **Step 5: Compile the whole unit** — `cargo build --manifest-path crates/junto-iced/Cargo.toml`. Fix every error until it compiles.

- [ ] **Step 6: `iced_test` view assertions** — add to `main.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn the_browser_view_shows_its_url_bar_and_the_lineage_view_does_not() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        app.shell.right_view = shell::RightView::Browser;
        // A URL-bar text_input is present in the browser view; assert via a stable id.
        // (Give the text_input `.id("browser-url")` in browser_pane for this.)
        let mut ui = iced_test::simulator(app.view());
        ui.find(iced::widget::Id::new("browser-url")).expect("browser view shows the URL bar");

        app.shell.right_view = shell::RightView::Lineage;
        let mut ui2 = iced_test::simulator(app.view());
        assert!(ui2.find(iced::widget::Id::new("browser-url")).is_err(), "lineage view has no URL bar");
    }

    #[test]
    fn the_error_state_replaces_the_frame() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        app.shell.right_view = shell::RightView::Browser;
        app.browser_error = Some("No Chromium-based browser found.".into());
        let mut ui = iced_test::simulator(app.view());
        // The error text is present; the URL bar is not.
        assert!(ui.find(iced::widget::Id::new("browser-url")).is_err(), "error state hides the URL bar");
    }
```

(Add `.id(iced::widget::Id::new("browser-url"))` to the URL `text_input` so these assertions have a handle.)

- [ ] **Step 7: Run** — `cargo test --manifest-path crates/junto-iced/Cargo.toml` → all green. `cargo clippy … -D warnings` clean. `cargo fmt`.
- [ ] **Step 8: Commit** the Tasks 9–13 unit (`feat(iced): browser view — driver, state, nav row, view switch`).

---

### Task 14: Cleanup — delete the probe, de-SPIKE comments

**Files:**
- Delete: `crates/junto-iced/src/bin/cdp_probe.rs`
- Modify: `crates/junto-iced/Cargo.toml`, `crates/junto-iced/src/cdp.rs` (header, done in Task 4), `crates/junto-iced/src/main.rs` (`browser_stream` doc, `App` field docs — done in Tasks 9–10)

- [ ] **Step 1: Delete the probe** — `git rm crates/junto-iced/src/bin/cdp_probe.rs`. With one binary left, plain `cargo run` is unambiguous again; no `default-run` needed.

- [ ] **Step 2: Cargo.toml** — remove the "SPIKE ONLY (browser blade)" comment on `base64` (lines 43–46); keep the dependency with a plain "Decodes CDP screencast JPEG frames." comment. Leave the crate header (lines 1–4) and `[workspace]` untouched.

- [ ] **Step 3: Grep for stragglers** — `grep -rn "SPIKE" crates/junto-iced/src`. The only remaining SPIKE references must be the crate-level pane-workspace framing in `main.rs`'s top module doc (lines 1–10) — intentionally kept (ADR 0018 is Dan's call). Every browser-specific SPIKE tag must be gone.

- [ ] **Step 4: Build + fmt + clippy + test** all green (manifest-path). Commit (`chore(iced): delete cdp_probe; de-SPIKE the browser code`).

> If executing Tasks sequentially, do Step 1 (delete probe) before Task 6's `spawn` signature change lands, or the probe's `cdp::spawn(&exe, &url, 1200, 900)` call breaks the build. The clean order: 6 → delete probe → 10. If parallelizing, one owner takes 6+10+14.

---

### Task 15: Verification — full check + live smoke

**Files:** none (verification only).

- [ ] **Step 1: Full crate check**

```bash
cargo fmt --manifest-path crates/junto-iced/Cargo.toml --check
cargo clippy --manifest-path crates/junto-iced/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path crates/junto-iced/Cargo.toml
```
All must pass. (The root `cargo test --workspace` does NOT cover this crate — do not rely on it.)

- [ ] **Step 2: Live end-to-end driver smoke** (validates the assumptions unit tests cannot: `setDeviceMetricsOverride` + screencast coexist under `--headless=new`; input lands as CSS px of the emulated viewport). Write a temporary `#[ignore]`d integration test (or a scratch `tokio` main under `/tmp`, not committed) that uses `cdp` directly:
  1. `find_chromium()` → spawn `about:blank` → `page_websocket_url` → connect.
  2. `Page.enable`; `set_device_metrics_request(_, {400, 300}, 1.5)`; `start_screencast_request`.
  3. Navigate to a data: page with a full-viewport `<div id=box onclick="document.title='clicked@'+event.clientX+','+event.clientY">` and a text `<input id=f>`.
  4. Assert ≥1 `Page.screencastFrame` arrives (ack each).
  5. `mouse_request(Pressed@(50,60), Left)` + `Released`; then `Runtime.evaluate` `document.title` → assert it reads `clicked@50,60` (proves 1:1 CSS-px input mapping).
  6. Focus the input via a click, send `key_request` for chars `h,i`; `Runtime.evaluate` `document.getElementById('f').value` → assert `hi` (proves keyboard injection).
  7. `set_device_metrics_request(_, {600, 400}, 1.5)`; assert a fresh frame arrives (proves the resize repaint trigger).
  Run with `cargo test --manifest-path crates/junto-iced/Cargo.toml -- --ignored browser_smoke` on this Windows machine (Edge present). Record the outcome as evidence in the completion report. **Do not commit** the scratch/ignored test unless it is made robust and deterministic; note it is environment- and browser-dependent.

- [ ] **Step 3: Launch the app once** — `cargo run --manifest-path crates/junto-iced/Cargo.toml` (host need not be running — the browser view is independent of the junto host). Switch the right blade to Browser, confirm a frame renders, type a URL + Enter, click a link, use back/reload, resize the window, switch to Lineage and back (page persists). Because the composited GUI window cannot be screenshotted in this environment, report this as a manual/driver-backed check and state explicitly that pixel-level visual confirmation was via the driver round-trip (Step 2) plus the `iced_test` layout assertions, not a screenshot.

- [ ] **Step 4: Report** — summarize what was built, the test counts, the smoke results (frames/click-coords/keystroke/resize), and the one honest limit (no GUI screenshot). Then invoke `superpowers:finishing-a-development-branch`.

---

## Self-review

- **Spec coverage:** URL bar + back/forward/reload (Tasks 3, 5, 11, 13); mouse+keyboard injection with coordinate mapping (Tasks 1, 2, 8, 10); viewport sized to blade width + scale factor (Tasks 1, 8, 10, 11, 12); process lifecycle / no orphan / Orca persistence (Tasks 6, 10, 12 + `Chromium::drop` kept); no-Chromium error state (Tasks 4, 10, 13); persist view choice + URL (Task 7, 11); `#[cfg]` both arms real (Task 4); delete `cdp_probe` / `cargo run` ambiguity (Task 14); no ADR / no gestures / crate spike framing kept (Global Constraints, Task 14). All spec sections map to a task.
- **Type consistency:** `cdp::Command` (Task 5) is the same shape used by `send_browser` (Task 11) and the driver (Task 10). `browser::PagePoint`/`KeyEvent`/`ViewportSize` flow browser.rs → cdp.rs → screencast.rs → main.rs unchanged. `Message` variants added in Task 9 are consumed in 10–13. `screencast::screencast(...)` builder setters (Task 8) match the call in `browser_pane` (Task 13).
- **Placeholder scan:** no TBD/TODO; pure-module code is complete; the driver's command match arm is specified by an explicit per-variant list (Task 10 Step 1) rather than left vague; view helpers (`right_view_switch`, `browser_error_view`, `icon_button_maybe`) have named behavior. The one deliberately-not-inlined item is the exact Lucide codepoints (Task 13 Step 1), sourced the same way the existing `ICON_*` consts were — a lookup, not a design gap.
- **Lifecycle correctness:** subscription id is generation-only (navigations don't respawn); `ever_opened` latch + `Hide`/`Show` gives Orca persistence; `BrowserClosed` bumps generation for clean restart; `Chromium::drop` guarantees no orphan.
