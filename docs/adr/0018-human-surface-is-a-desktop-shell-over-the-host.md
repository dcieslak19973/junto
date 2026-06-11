# The human surface is a desktop shell over the host (Tauri); remote = SSH-tunneled hosts

Status: accepted (Dan, 2026-06-10) · builds on [`0012`](0012-mcp-over-http-is-the-first-write-surface.md), [`0013`](0013-host-serves-the-read-surface-recall-via-hook.md), [`0015`](0015-one-host-per-machine-serving-many-substrates.md)

Constraint #2 has always said the human surface is a GUI; the host's web pages were "the first pixel." Decided: that GUI is a **desktop app — a Tauri shell over the existing host** — not a separate UI stack beside it.

- **Tauri.** Rust core (links the workspace's crates when it needs them), MIT-compatible, small binaries, Windows + macOS first-class with Linux nearly free — and the team already builds with it (wmux). Electron was rejected for dragging in a JS toolchain the project doesn't otherwise have; a native-widget UI (egui et al.) was rejected because it would *fork* the surface — every feature built twice, page and widget.
- **v1 wraps the host's server-rendered pages.** The app's webview loads the host's HTML (index, channel pages, verification forms, review queue as it lands). One implementation of every pixel, usable in the app *and* a plain browser; pages can be replaced by a richer frontend later, page by page, if the UI outgrows server rendering.
- **The app owns host lifecycle — the last human CLI touchpoint dies.** On launch it probes the host port; if no host answers, it spawns `junto serve` and waits for it to come up. On exit it **leaves the host running**: the host is the *machine's* singleton (0015), serving agent sessions that must not die with a window.
- **The host connection is a URL, not an assumption.** Default `http://127.0.0.1:1727/`. This is the entire remote story for now: a corporate dev box runs `junto serve` near the repos (Linux host — supported), and the app reaches it through an SSH port-forward, the VS Code Remote pattern. A tunneled connection looks like localhost on both ends, so **0012's localhost-only / no-auth posture is preserved** — no new protocol, no TLS story forced. App-managed SSH tunnels are a later slice; v1 only refuses to hardcode the URL.
- **The desktop crate is quarantined from the kernel workspace.** Tauri's dependency tree is enormous; putting it in the root workspace would slow every `cargo check --workspace` and CI run for kernel work. `crates/junto-desktop` is its own workspace (excluded from the root), with its own CI job on the app's target platforms.

## Considered

- **Electron** — rejected: a second language/toolchain, far heavier runtime, no reuse of the Rust core.
- **Native-widget UI (egui/iced)** — rejected for surface-forking (above); revisit only if webview rendering itself becomes the bottleneck.
- **Browser/PWA only (no app)** — rejected: nothing owns host lifecycle, so a human still touches a terminal to start `junto serve`, and there is no credible path to app-managed remote tunnels or OS integration (tray, notifications) later.
- **App embeds the host in-process** — rejected for v1: it duplicates the singleton (0015) the moment a CLI-started host also runs, and child-process supervision of the one real `junto serve` keeps a single implementation. Revisit if process management proves brittle.
