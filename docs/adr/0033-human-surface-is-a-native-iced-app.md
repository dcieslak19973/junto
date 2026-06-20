# The human surface is a native Iced application (supersedes 0018)

Status: accepted (Dan, 2026-06-20) — **supersedes [`0018`](0018-human-surface-is-a-desktop-shell-over-the-host.md)** · informed by [`docs/native-ui-toolkit-assessment.md`](../native-ui-toolkit-assessment.md) · validated by the spike on branch `spike/iced-native-surface` · ledger: `native-iced-surface` channel

[`0018`](0018-human-surface-is-a-desktop-shell-over-the-host.md) made the human surface a **Tauri shell over the host's server-rendered HTML pages** — one implementation of every pixel, usable in a browser too. Daily dogfooding exposed its ceiling: a webview reads as "a website in a window," and the interactions junto wants next (a tmux-style multi-channel pane workspace, an always-visible lineage/branch graph, smooth live streaming) are exactly what HTML-in-a-webview does worst. A native-Iced spike (built this session) validated that native is **fast, feasible, and reaches the look-and-feel bar** — so the primary surface moves native.

## Decision — the primary human surface is a native Rust GUI in **Iced**

- **Iced** is the toolkit. It is permissively licensed (MIT/Apache — satisfies hard-constraint #1), genuinely cross-platform (Windows/macOS/Linux), and **proven at commercial polish** by **Kraken Desktop** (which *sponsors* Iced) and **System76 COSMIC** (a whole desktop environment). **GPUI is ruled out** for now by a GPL-3.0 contamination in its default build (`ztracing`/`zlog`, [zed#55470](https://github.com/zed-industries/zed/issues/55470)); egui is permissive but less app-structured. Full rationale + the GPUI revisit trigger live in `docs/native-ui-toolkit-assessment.md`.
- **The web/served surface is kept — for mobile + remote, read-only check-in.** Dan's call: full work happens on the desktop; on a phone or a remote machine you only *check in* (read the focus board, ratify/approve, glance at a running session). The host's server-rendered pages cover that (a browser, or an SSH-tunnel — `0018`'s remote pattern). So native does **not** forfeit mobile/remote; native is **additive** (a desktop power-surface), not a lossy replacement.
- **The native↔host seam is a data API, not HTML.** The host gains a JSON + SSE read API the native app renders into its own widgets: `/channels/{name}/view.json` (structured channel projection), `/lineage.json` (the whole diverge/converge DAG with per-channel milestones), `/channels.json` (the picker), the existing SSE `/sessions/{id}/stream`, and the POST act/launch/steer routes. The host (ledger · substrate · MCP · sync) is **untouched** — only the presentation layer is swapped/added. This is why the swap is feasible: junto already separates data from view.

## Settled UI decisions (from the spike)

- **Layout:** even auto-width **columns** (one per open channel; no pane_grid), reflowing as channels open/close.
- **Lineage:** an **always-visible top branch graph** matching the web's time-axis model — log-scaled by age (newest right), each channel a track from first→last activity, diverge (mauve) / converge (green) connectors, **milestone dots with hover-text labels**, currently-open channels highlighted. The whole timeline is shown (no internal scroll).
- **Live work:** per-channel **session panes** — the SSE feed rendered natively (Markdown stripped to text) with **steer / interrupt** acts that POST back.
- **Design tokens:** the web's exact **Catppuccin Mocha** palette and its typography (`Inter, system-ui` → Segoe UI on Windows; bundle Inter for cross-platform later).

## Consequences

The desktop surface becomes a from-scratch native build to the **Kraken polish bar** (a design system + custom widgets — deliberate product work, not spike-patching); the spike (`spike/iced-native-surface`) is the working prototype it draws from. The web surface stays maintained as the mobile/remote read-only surface, so there are **two surfaces for two jobs** (desktop power-use vs. phone/remote check-in), not two copies of one. `0018`'s "one implementation of every pixel" goal is the thing given up; in exchange the desktop surface gets native speed, real panes, and a polished lineage graph the webview couldn't reach. Revisit only if the maintenance of two surfaces outweighs the native gains, or if the web surface alone proves sufficient.
