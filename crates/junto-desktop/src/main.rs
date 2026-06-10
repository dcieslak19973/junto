//! junto-desktop — the human surface shell (`docs/adr/0018`).
//!
//! A Tauri window over the host's server-rendered pages: one implementation
//! of every pixel, usable here and in a plain browser. The app **owns host
//! lifecycle**: on launch it probes the host and, when none answers locally,
//! spawns `junto serve` and waits for it — so a human never touches a
//! terminal to read or verify the record. On exit the host is **left
//! running**: it is the machine's singleton (`docs/adr/0015`), serving agent
//! sessions that must not die with a window.
//!
//! The host connection is a URL, not an assumption (`JUNTO_HOST_URL`,
//! default `http://127.0.0.1:1727/`): point it at an SSH port-forward and a
//! corporate dev box's host works unchanged — the VS Code Remote pattern,
//! which keeps `docs/adr/0012`'s localhost-only posture intact. The app only
//! auto-spawns a host for the *default* (local) URL; a custom URL means the
//! operator manages that host themselves.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The machine-local host (`docs/adr/0015`; 1727: the year Franklin founded
/// the Junto).
const DEFAULT_HOST_URL: &str = "http://127.0.0.1:1727/";
const LOCAL_HOST_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 1727);

fn host_url() -> String {
    std::env::var("JUNTO_HOST_URL").unwrap_or_else(|_| DEFAULT_HOST_URL.to_string())
}

fn local_host_is_up() -> bool {
    let addr = SocketAddr::from(LOCAL_HOST_ADDR);
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

/// Make sure the machine's host answers, spawning `junto serve` if it
/// doesn't. Best-effort: if the binary is missing or slow to come up, the
/// window still opens and the webview shows the connection failure — the
/// operator sees *something* rather than a silently dead app.
fn ensure_local_host() {
    if local_host_is_up() {
        return;
    }
    let mut command = std::process::Command::new("junto");
    command.arg("serve");
    // Detach on Windows so the host has no console window and outlives us.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.spawn() {
        Ok(_child) => {
            // Poll until the host answers (or give up and let the webview
            // show the failure). The child is intentionally not reaped: the
            // host outlives the app by design (docs/adr/0018).
            for _ in 0..50 {
                if local_host_is_up() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("junto-desktop: spawned `junto serve` but it did not come up on 1727");
        }
        Err(err) => {
            eprintln!(
                "junto-desktop: could not spawn `junto serve` ({err}); is junto on PATH? \
                 (cargo install --path crates/junto)"
            );
        }
    }
}

fn main() {
    let url = host_url();
    // Only auto-spawn for the default local URL: a custom URL (e.g. an SSH
    // tunnel) is the operator's host to manage.
    if url == DEFAULT_HOST_URL {
        ensure_local_host();
    }

    tauri::Builder::default()
        .setup(move |app| {
            let external = url.parse().expect("JUNTO_HOST_URL is not a valid URL");
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(external))
                .title("junto")
                .inner_size(1100.0, 800.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running junto-desktop");
}
