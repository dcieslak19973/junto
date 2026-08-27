//! SPIKE — driving an already-installed Chromium over the DevTools Protocol.
//!
//! Throwaway. The question this answers is narrow: **can browser frames reach
//! an Iced widget, fast enough, without embedding a browser engine?** If the
//! answer is yes, the real build re-decides CEF-vs-external-Chromium on its own
//! merits (see junto-dev finding `89c8e707`); nothing here presumes that choice.
//!
//! Deliberately NOT here: input injection, the three gestures, artifacts,
//! anchors, the ledger. Pixels only.
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
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A spawned Chromium plus the debugging port it actually chose.
#[derive(Debug)]
pub struct Chromium {
    child: Child,
    pub port: u16,
    /// Kept so the profile can be removed on drop.
    user_data_dir: PathBuf,
}

impl Drop for Chromium {
    fn drop(&mut self) {
        // Best-effort teardown: a spike must not leave orphaned browsers behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.user_data_dir);
    }
}

/// Locate an installed Chromium. Edge ships on every Windows box and Chrome is
/// the common alternative; the spike does not care which, since both speak the
/// same protocol.
pub fn find_chromium() -> Option<PathBuf> {
    const CANDIDATES: &[&str] = &[
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .map(PathBuf::from)
}

/// Spawn a headless Chromium on a throwaway profile and wait for it to publish
/// its debugging port.
pub fn spawn(exe: &Path, url: &str, width: u32, height: u32) -> Result<Chromium, String> {
    let user_data_dir =
        std::env::temp_dir().join(format!("junto-browser-spike-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&user_data_dir);

    let child = Command::new(exe)
        // `=new` is the modern headless mode: a real compositor, so screencast
        // actually paints. Old headless was a separate, thinner implementation.
        .arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg("--remote-allow-origins=*")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg(format!("--window-size={width},{height}"))
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
fn wait_for_port(user_data_dir: &Path, timeout: Duration) -> Result<u16, String> {
    let path = user_data_dir.join("DevToolsActivePort");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(first) = text.lines().next()
            && let Ok(port) = first.trim().parse::<u16>()
        {
            return Ok(port);
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
pub async fn page_websocket_url(port: u16) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match try_page_websocket_url(port).await {
            Ok(url) => return Ok(url),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn try_page_websocket_url(port: u16) -> Result<String, String> {
    let body: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list"))
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

/// One decoded frame: JPEG bytes plus the metadata a later input-mapping slice
/// would need to translate a click back into page coordinates.
#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub device_width: f32,
    pub device_height: f32,
}

/// The JSON for `Page.startScreencast`. `everyNthFrame: 1` asks for all of
/// them; quality is the latency/fidelity dial the spike wants to measure.
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

/// Every frame MUST be acknowledged. Chromium throttles the stream to the
/// acks it receives, so a client that forgets gets one frame and then silence —
/// which reads as a broken pipeline rather than as backpressure working.
pub fn frame_ack_request(id: u64, session_id: i64) -> String {
    serde_json::json!({
        "id": id,
        "method": "Page.screencastFrameAck",
        "params": { "sessionId": session_id }
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
    let metadata = params.get("metadata")?;
    let device_width = metadata.get("deviceWidth")?.as_f64()? as f32;
    let device_height = metadata.get("deviceHeight")?.as_f64()? as f32;

    Some((
        Frame {
            jpeg,
            device_width,
            device_height,
        },
        session_id,
    ))
}

#[cfg(test)]
mod parse_tests {
    use super::{frame_ack_request, parse_screencast_frame, start_screencast_request};

    #[test]
    fn a_screencast_frame_yields_its_bytes_and_ack_session() {
        // "hello" base64-encoded, standing in for JPEG bytes.
        let msg = r#"{"method":"Page.screencastFrame","params":{
            "data":"aGVsbG8=","sessionId":7,
            "metadata":{"deviceWidth":1200.0,"deviceHeight":900.0}}}"#;
        let (frame, session) = parse_screencast_frame(msg).expect("should parse");
        assert_eq!(frame.jpeg, b"hello");
        assert_eq!(session, 7);
        assert_eq!(frame.device_width, 1200.0);
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
}
