//! SPIKE PROBE — does the CDP screencast pipeline actually deliver frames?
//!
//! Standalone on purpose: it answers the risky question (frames arrive, at what
//! rate, at what size) without dragging the Iced app in. Delete with the rest
//! of the spike.
//!
//! Run: `cargo run --manifest-path crates/junto-iced/Cargo.toml --bin cdp_probe`

#[path = "../cdp.rs"]
mod cdp;

use std::time::Instant;

// Match the crate's existing idiom (`main.rs:8889`): futures traits come from
// iced's re-export, and the socket's message type from tokio-tungstenite.
use iced::futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default to a self-contained animated page. Screencast is CHANGE-DRIVEN:
    // Chromium emits a frame when the page repaints, not on a clock, so a
    // static page delivers its first paint and then correctly goes silent.
    // Measuring throughput needs something that actually moves.
    const SPINNER: &str = "data:text/html,<style>body{margin:0;background:%23111}\
@keyframes s{to{transform:rotate(360deg)}}\
div{width:300px;height:300px;margin:80px auto;background:linear-gradient(45deg,%23f0f,%230ff);\
animation:s 1s linear infinite}</style><div></div>";
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| SPINNER.to_owned());
    const MEASURE_FOR: std::time::Duration = std::time::Duration::from_secs(5);

    let exe = cdp::find_chromium().ok_or("no Chromium found (looked for Edge and Chrome)")?;
    eprintln!("browser:  {}", exe.display());
    eprintln!("url:      {url}");

    let spawned_at = Instant::now();
    let browser = cdp::spawn(&exe, &url, 1200, 900)?;
    eprintln!("port:     {} (in {:?})", browser.port, spawned_at.elapsed());

    let ws_url = cdp::page_websocket_url(browser.port).await?;
    eprintln!("target:   {}\n", &ws_url[..ws_url.len().min(72)]);

    let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    socket
        .send(tungstenite::Message::Text(
            r#"{"id":1,"method":"Page.enable"}"#.into(),
        ))
        .await?;
    socket
        .send(tungstenite::Message::Text(
            cdp::start_screencast_request(2, 1200, 900).into(),
        ))
        .await?;

    let mut frames = 0usize;
    let mut total_bytes = 0usize;
    let mut next_id = 100u64;
    let mut first_frame_at: Option<Instant> = None;
    let mut seen = 0usize;
    let started = Instant::now();

    // Measure for a fixed window rather than chasing a frame count: on a
    // change-driven stream, "N frames" is a property of the page, not the pipe.
    while first_frame_at
        .map(|t| t.elapsed() < MEASURE_FOR)
        .unwrap_or(true)
    {
        // stderr is unbuffered; stdout block-buffers when piped, which hid the
        // whole first run. Diagnostics go to stderr for that reason alone.
        let next = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next()).await;
        let Ok(next) = next else {
            eprintln!("!! stream went quiet after {seen} messages, {frames} frames");
            break;
        };
        let Some(msg) = next else {
            return Err("socket closed before the frames arrived".into());
        };
        let tungstenite::Message::Text(text) = msg? else {
            continue;
        };
        seen += 1;
        if seen <= 6 {
            eprintln!("<< {}", &text[..text.len().min(220)]);
        }

        if let Some((frame, session_id)) = cdp::parse_screencast_frame(&text) {
            if first_frame_at.is_none() {
                first_frame_at = Some(Instant::now());
                println!(
                    "first frame after {:?}  ({}x{}, {} bytes)",
                    started.elapsed(),
                    frame.device_width,
                    frame.device_height,
                    frame.jpeg.len()
                );
            }
            frames += 1;
            total_bytes += frame.jpeg.len();

            // Unacknowledged frames stop the stream dead — this ack IS the flow
            // control, not a formality.
            socket
                .send(tungstenite::Message::Text(
                    cdp::frame_ack_request(next_id, session_id).into(),
                ))
                .await?;
            next_id += 1;
        }
    }

    let streaming = first_frame_at.map(|t| t.elapsed()).unwrap_or_default();
    eprintln!("\n--- {frames} frames ---");
    eprintln!("elapsed since first frame: {streaming:?}");
    if streaming.as_secs_f64() > 0.0 {
        eprintln!(
            "rate:  {:.1} fps",
            (frames - 1) as f64 / streaming.as_secs_f64()
        );
    }
    eprintln!("avg frame: {} bytes", total_bytes / frames.max(1));
    eprintln!("\nVERDICT: screencast pipeline delivers frames.");
    Ok(())
}
