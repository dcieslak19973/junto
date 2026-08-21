//! junto — the host/app library.
//!
//! Split from the `junto` binary (`src/main.rs`) so the host/web/live-plane
//! internals are reachable from `tests/` as an ordinary integration-test
//! dependency (`docs/superpowers/specs/2026-08-20-live-session-plane-design.md`
//! Task 7) without every test living as an inline `#[cfg(test)] mod` — the
//! live websocket test in particular needs a real `TcpListener` + a second
//! client-side crate (`tokio-tungstenite`) driving the actual `axum::serve`
//! router, which only a genuine external test binary can do. `main.rs` is the
//! CLI/process entry point over this same module tree; only the handful of
//! items it calls directly (`host`, `web`, `mcp`, `binding`, `init`,
//! `members`, `render`) are `pub`. Everything else keeps its original
//! intra-crate visibility (`pub(crate)`/private) — the module tree itself is
//! unchanged, only how it is packaged.

pub mod acp;
pub mod agent;
pub mod binding;
pub mod forge;
pub mod grader;
pub mod host;
pub mod init;
pub mod keys;
pub mod launch;
pub mod live_bridge;
pub mod live_plane;
pub mod live_ws;
pub mod mcp;
pub mod members;
pub mod outcome;
pub mod pending_lineage;
pub mod render;
pub mod verify;
pub mod web;
