//! Driving an already-installed Chromium over the DevTools Protocol — the
//! browser view's transport. No browser engine is embedded (finding
//! `f463944e`): frames arrive as JPEG screencast frames and navigation/input
//! go back as CDP commands, so the view is an ordinary Iced widget rather than
//! an OS surface parked over the window.
//!
//! The launch switches are lifted from wmux's `browser-helper`, which paid for
//! them empirically — each one is a day someone already lost:
//!
//! - `--remote-debugging-port=0` + reading `DevToolsActivePort`. Picking a port
//!   in Rust first races TCP `TIME_WAIT`: Chromium silently binds a different
//!   one and the caller's registry goes stale. Puppeteer and Playwright both
//!   discover the port this way instead.
//! - `--remote-allow-origins=*`. Chromium 109+ applies a CORS check to the
//!   DevTools endpoint; without this it answers HTTP but refuses the upgrade to
//!   WebSocket.
//! - The three occlusion switches. A window Chromium believes is hidden has its
//!   compositor paused, and **screencast stops emitting frames** — the failure
//!   looks like a hang, not an error.

use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use crate::browser;

/// The DevTools debugging port Chromium actually bound. A newtype because a
/// bare `u16` is easy to swap with any other port at a call site, and the wrong
/// one silently fails the WebSocket upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugPort(pub u16);

/// A spawned Chromium plus the debugging port it actually chose.
#[derive(Debug)]
pub struct Chromium {
    child: Child,
    pub port: DebugPort,
    /// Kept so the profile can be removed on drop.
    user_data_dir: PathBuf,
}

impl Drop for Chromium {
    fn drop(&mut self) {
        // The browser is owned by whoever holds this handle; dropping it must
        // leave no orphaned process behind, so kill + reap + clean the profile.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.user_data_dir);
    }
}

/// Candidate Chromium executables for this platform, most-preferred first.
/// Every first-class arm names real install paths (the browser view requires
/// both Windows and macOS be real); Linux is included as it is free to support.
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
/// platform candidate. `None` when nothing is found, which the view turns into
/// the no-browser error state.
pub fn find_chromium() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("JUNTO_BROWSER").map(PathBuf::from)
        && explicit.exists()
    {
        return Some(explicit);
    }
    pick(chromium_candidates(), |path| path.exists())
}

/// Spawn a headless Chromium on a throwaway profile and wait for it to publish
/// its debugging port. `initial` sizes the launch window; the real layout
/// viewport is set afterward via [`set_device_metrics_request`], so this is
/// only a sane starting size.
pub fn spawn(exe: &Path, url: &str, initial: browser::ViewportSize) -> Result<Chromium, String> {
    let user_data_dir = std::env::temp_dir().join(format!("junto-browser-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&user_data_dir);

    let child = ProcessCommand::new(exe)
        // `=new` is the modern headless mode: a real compositor, so screencast
        // actually paints. Old headless was a separate, thinner implementation.
        .arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg("--remote-allow-origins=*")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg(format!(
            "--window-size={},{}",
            initial.width, initial.height
        ))
        // Without a fresh profile's first-run suppression, Chromium can sit on
        // a welcome page instead of the requested URL.
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=CalculateNativeWinOcclusion")
        .arg("--disable-backgrounding-occluded-windows")
        .arg("--disable-renderer-backgrounding")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", exe.display()))?;

    let port = wait_for_port(&user_data_dir, Duration::from_secs(15))?;
    Ok(Chromium {
        child,
        port,
        user_data_dir,
    })
}

/// Chromium writes the port it bound to `<user-data-dir>/DevToolsActivePort`,
/// first line the port, second the browser-level WebSocket path.
fn wait_for_port(user_data_dir: &Path, timeout: Duration) -> Result<DebugPort, String> {
    let path = user_data_dir.join("DevToolsActivePort");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(first) = text.lines().next()
            && let Ok(port) = first.trim().parse::<u16>()
        {
            return Ok(DebugPort(port));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "timed out waiting for {} — the browser may have failed to start its debug endpoint",
        path.display()
    ))
}

/// Ask the HTTP endpoint for a *page* target's WebSocket URL. The browser-level
/// socket in `DevToolsActivePort` cannot drive `Page.*`, so it is not enough.
pub async fn page_websocket_url(port: DebugPort) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match try_page_websocket_url(port).await {
            Ok(url) => return Ok(url),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn try_page_websocket_url(port: DebugPort) -> Result<String, String> {
    let body: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{}/json/list", port.0))
        .await
        .map_err(|e| format!("querying /json/list: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parsing /json/list: {e}"))?;

    body.as_array()
        .and_then(|targets| {
            targets
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                .and_then(|t| t.get("webSocketDebuggerUrl"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| "no page target yet".to_owned())
}

/// A command the app sends into the driver; the driver turns each into one or
/// more CDP requests on the page socket.
#[derive(Debug, Clone)]
pub enum Command {
    Navigate(String),
    Reload,
    Back,
    Forward,
    Mouse {
        kind: MouseKind,
        at: browser::PagePoint,
        button: MouseButton,
        modifiers: i64,
    },
    Scroll {
        at: browser::PagePoint,
        dx: f32,
        dy: f32,
        modifiers: i64,
    },
    Key(browser::KeyEvent),
    SetViewport {
        size: browser::ViewportSize,
        scale: f32,
    },
    /// Resume painting: (re)start the screencast, which forces one fresh frame
    /// of the current page — the show/refocus repaint trigger.
    Show,
    /// Pause painting while the browser view is hidden; the page stays alive.
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Pressed,
    Released,
    Moved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseKind {
    fn cdp_type(self) -> &'static str {
        match self {
            Self::Pressed => "mousePressed",
            Self::Released => "mouseReleased",
            Self::Moved => "mouseMoved",
        }
    }
}

impl MouseButton {
    fn cdp_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }
}

/// One decoded screencast frame — just the JPEG bytes. The frame's device
/// dimensions are intentionally not carried: the emulated viewport equals the
/// widget, so input mapping needs no per-frame size ([`browser::map_cursor`]).
#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
}

/// `Page.enable` — required before any other `Page.*` command or event.
pub fn enable_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.enable" }).to_string()
}

/// The JSON for `Page.startScreencast`. `everyNthFrame: 1` asks for all of
/// them; quality is the latency/fidelity dial.
pub fn start_screencast_request(id: u64, max_width: u32, max_height: u32) -> String {
    serde_json::json!({
        "id": id,
        "method": "Page.startScreencast",
        "params": {
            "format": "jpeg",
            "quality": 70,
            "maxWidth": max_width,
            "maxHeight": max_height,
            "everyNthFrame": 1,
        }
    })
    .to_string()
}

/// `Page.stopScreencast` — used when the browser view is hidden so no frames
/// flow while the page idles alive.
pub fn stop_screencast_request(id: u64) -> String {
    serde_json::json!({ "id": id, "method": "Page.stopScreencast" }).to_string()
}

/// Every frame MUST be acknowledged. Chromium throttles the stream to the acks
/// it receives, so a client that forgets gets one frame and then silence —
/// which reads as a broken pipeline rather than as backpressure working.
pub fn frame_ack_request(id: u64, session_id: i64) -> String {
    serde_json::json!({
        "id": id,
        "method": "Page.screencastFrameAck",
        "params": { "sessionId": session_id }
    })
    .to_string()
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
    serde_json::json!({
        "id": id,
        "method": "Page.navigateToHistoryEntry",
        "params": { "entryId": entry_id }
    })
    .to_string()
}

/// `Emulation.setDeviceMetricsOverride` — sizes the emulated CSS viewport to
/// the blade's logical size and renders at `deviceScaleFactor` physical pixels.
/// Re-applying it (on resize) also forces a fresh screencast frame.
pub fn set_device_metrics_request(id: u64, size: browser::ViewportSize, scale: f32) -> String {
    serde_json::json!({
        "id": id,
        "method": "Emulation.setDeviceMetricsOverride",
        "params": {
            "width": size.width,
            "height": size.height,
            "deviceScaleFactor": scale,
            "mobile": false,
        }
    })
    .to_string()
}

/// `Input.dispatchMouseEvent`. A move carries no button and a zero click-count;
/// a press/release names the button and one click.
pub fn mouse_request(
    id: u64,
    kind: MouseKind,
    at: browser::PagePoint,
    button: MouseButton,
    modifiers: i64,
) -> String {
    let (button_name, click_count) = match kind {
        MouseKind::Moved => ("none", 0),
        _ => (button.cdp_name(), 1),
    };
    serde_json::json!({
        "id": id,
        "method": "Input.dispatchMouseEvent",
        "params": {
            "type": kind.cdp_type(),
            "x": at.x,
            "y": at.y,
            "button": button_name,
            "clickCount": click_count,
            "modifiers": modifiers,
        }
    })
    .to_string()
}

/// `Input.dispatchMouseEvent` of type `mouseWheel` — a scroll at a page point.
pub fn wheel_request(id: u64, at: browser::PagePoint, dx: f32, dy: f32, modifiers: i64) -> String {
    serde_json::json!({
        "id": id,
        "method": "Input.dispatchMouseEvent",
        "params": {
            "type": "mouseWheel",
            "x": at.x,
            "y": at.y,
            "deltaX": dx,
            "deltaY": dy,
            "modifiers": modifiers,
        }
    })
    .to_string()
}

/// `Input.dispatchKeyEvent` from a pre-mapped [`browser::KeyEvent`].
pub fn key_request(id: u64, ev: &browser::KeyEvent) -> String {
    serde_json::json!({
        "id": id,
        "method": "Input.dispatchKeyEvent",
        "params": {
            "type": ev.kind,
            "key": ev.key,
            "code": ev.code,
            "windowsVirtualKeyCode": ev.windows_virtual_key_code,
            "text": ev.text,
            "modifiers": ev.modifiers,
        }
    })
    .to_string()
}

/// Pull a `Page.screencastFrame` out of a CDP message, returning the frame and
/// the session id its ack must quote. Non-frame traffic (command replies, other
/// events) yields `None` rather than an error — the socket carries both.
pub fn parse_screencast_frame(text: &str) -> Option<(Frame, i64)> {
    use base64::Engine as _;

    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    if msg.get("method")?.as_str()? != "Page.screencastFrame" {
        return None;
    }
    let params = msg.get("params")?;
    let data = params.get("data")?.as_str()?;
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let session_id = params.get("sessionId")?.as_i64()?;
    Some((Frame { jpeg }, session_id))
}

/// A `Page.getNavigationHistory` reply, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavHistory {
    pub current_index: i64,
    pub entries: Vec<NavEntry>,
}

/// One history entry: its stable id (for `navigateToHistoryEntry`) and URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavEntry {
    pub id: i64,
    pub url: String,
}

/// Parse a navigation-history reply. `None` for any other message (the socket
/// carries frames, events, and other replies too).
pub fn parse_navigation_history(text: &str) -> Option<NavHistory> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    let result = msg.get("result")?;
    let current_index = result.get("currentIndex")?.as_i64()?;
    let entries = result
        .get("entries")?
        .as_array()?
        .iter()
        .filter_map(|e| {
            Some(NavEntry {
                id: e.get("id")?.as_i64()?,
                url: e.get("url")?.as_str()?.to_owned(),
            })
        })
        .collect();
    Some(NavHistory {
        current_index,
        entries,
    })
}

/// The page-lifecycle events that mean "re-query the history / flip loading".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSignal {
    Navigated,
    Loaded,
    LoadingStarted,
}

/// Classify a page-lifecycle event, or `None` for any other message.
pub fn parse_page_signal(text: &str) -> Option<PageSignal> {
    let msg: serde_json::Value = serde_json::from_str(text).ok()?;
    match msg.get("method")?.as_str()? {
        "Page.frameNavigated" => Some(PageSignal::Navigated),
        "Page.loadEventFired" => Some(PageSignal::Loaded),
        "Page.frameStartedLoading" => Some(PageSignal::LoadingStarted),
        _ => None,
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn a_screencast_frame_yields_its_bytes_and_ack_session() {
        // "hello" base64-encoded, standing in for JPEG bytes.
        let msg = r#"{"method":"Page.screencastFrame","params":{
            "data":"aGVsbG8=","sessionId":7,
            "metadata":{"deviceWidth":1200.0,"deviceHeight":900.0}}}"#;
        let (frame, session) = parse_screencast_frame(msg).expect("should parse");
        assert_eq!(frame.jpeg, b"hello");
        assert_eq!(session, 7);
    }

    #[test]
    fn a_command_reply_is_not_mistaken_for_a_frame() {
        // The same socket carries replies; treating one as a frame would push
        // garbage into the image widget.
        assert!(parse_screencast_frame(r#"{"id":1,"result":{}}"#).is_none());
    }

    #[test]
    fn an_unrelated_event_is_ignored() {
        assert!(parse_screencast_frame(r#"{"method":"Page.loadEventFired"}"#).is_none());
    }

    #[test]
    fn malformed_json_is_ignored_rather_than_panicking() {
        assert!(parse_screencast_frame("{not json").is_none());
    }

    #[test]
    fn the_ack_quotes_the_session_it_answers() {
        let ack = frame_ack_request(9, 7);
        assert!(ack.contains("screencastFrameAck"));
        assert!(ack.contains("\"sessionId\":7"));
    }

    #[test]
    fn the_start_request_bounds_the_frame_size() {
        let req = start_screencast_request(2, 800, 600);
        assert!(req.contains("startScreencast"));
        assert!(req.contains("\"maxWidth\":800"));
    }

    #[test]
    fn navigate_and_metrics_requests_carry_their_params() {
        assert!(navigate_request(3, "https://x.test").contains("Page.navigate"));
        assert!(navigate_request(3, "https://x.test").contains("https://x.test"));
        let m = set_device_metrics_request(
            4,
            browser::ViewportSize {
                width: 520,
                height: 1400,
            },
            1.5,
        );
        assert!(m.contains("Emulation.setDeviceMetricsOverride"));
        assert!(m.contains("\"width\":520"));
        assert!(m.contains("\"deviceScaleFactor\":1.5"));
        assert!(m.contains("\"mobile\":false"));
    }

    #[test]
    fn a_mouse_press_names_its_button_and_click_count() {
        let req = mouse_request(
            5,
            MouseKind::Pressed,
            browser::PagePoint { x: 10.0, y: 20.0 },
            MouseButton::Left,
            0,
        );
        assert!(req.contains("Input.dispatchMouseEvent"));
        assert!(req.contains("\"type\":\"mousePressed\""));
        assert!(req.contains("\"button\":\"left\""));
        assert!(req.contains("\"clickCount\":1"));
        // A move carries no button and clickCount 0.
        let mv = mouse_request(
            6,
            MouseKind::Moved,
            browser::PagePoint { x: 1.0, y: 2.0 },
            MouseButton::Left,
            0,
        );
        assert!(mv.contains("\"type\":\"mouseMoved\""));
        assert!(mv.contains("\"button\":\"none\""));
    }

    #[test]
    fn a_key_request_serializes_the_mapped_event() {
        let ev = browser::key_event(
            &browser::Key::Named(browser::NamedKey::Enter),
            browser::KeyPhase::Down,
            browser::Modifiers::default(),
        )
        .expect("ev");
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
        assert_eq!(
            parse_page_signal(r#"{"method":"Page.loadEventFired"}"#),
            Some(PageSignal::Loaded)
        );
        assert_eq!(
            parse_page_signal(r#"{"method":"Page.frameStartedLoading"}"#),
            Some(PageSignal::LoadingStarted)
        );
        assert!(parse_page_signal(r#"{"id":1,"result":{}}"#).is_none());
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;

    #[test]
    fn pick_returns_the_first_existing_candidate() {
        let candidates = vec![
            PathBuf::from("/no/such/a"),
            PathBuf::from("/yes/b"),
            PathBuf::from("/yes/c"),
        ];
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
