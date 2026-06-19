# Native UI toolkit assessment (for a possible non-webview human surface)

> Status: ⚠️ **open / deferred** — exploration, not a decision. Reopens [ADR 0018](adr/0018-human-surface-is-a-desktop-shell-over-the-host.md) (settled: the human surface is a Tauri shell over the host's server-rendered pages). Captured during a dogfood session where Dan questioned the webview surface and leaned toward a native Rust GUI. The decision is **parked**; this doc records what we found so a future revisit starts informed. Ledger: `junto-dev` `51df990b` (open question) + the licensing record below.

## Why this is even on the table

The webview surface (ADR 0018) bought the *entire* human surface fast and gave browser + SSH-remote access for free. The doubt: it has a "website in a window" ceiling, and the things Dan wants next — a **tmux-style multi-channel pane workspace**, drag-resize, keyboard nav, smooth live streaming — are exactly what iframes-in-a-webview do worst and a native toolkit does best.

What makes a swap *feasible*: junto already separates **data from view**. The host owns the ledger, substrate, MCP, sessions, and sync; the surface is just a view. A native GUI would talk to the host (its HTTP/MCP API or an in-process call) instead of rendering server HTML — so the spine is untouched; only the presentation layer changes.

What it *costs*: the whole surface is a from-scratch rebuild (channel pages, focus board, live feed, steer, acts, launch picker, agents config), and native almost certainly **forfeits ADR 0018's browser + remote access** (a native app can't be SSH-tunneled the way a served page can) — which decides whether native *replaces* the webview or has to *coexist* with it. **That replace-vs-coexist crux is deferred.**

## The constraint that governs the toolkit choice

junto **hard constraint #1**: MIT, greenfield, **no copyleft *source*.** Linking a permissively- or linking-exception-licensed library is fine; pulling copyleft (GPL/AGPL) **source or statically-linked object code** into junto's binary is not — it would force junto's binary under copyleft terms.

## Toolkit landscape (assessed 2026-06-19)

| Toolkit | Model | Cross-platform | License | Fit for junto |
|---|---|---|---|---|
| **egui** ([emilk/egui](https://github.com/emilk/egui)) | immediate-mode | Win/macOS/Linux + web (WASM) | **MIT OR Apache-2.0** ✅ | **Clean.** Most-used native Rust GUI; fastest to a feelable spike; dev-tool aesthetic. Best for *finding out*. |
| **Iced** ([iced-rs/iced](https://github.com/iced-rs/iced)) | retained, Elm-like | Win/macOS/Linux + web | **MIT** ✅ | **Clean.** Most credible "serious app" choice — powers System76's COSMIC desktop. More structure than egui. |
| **Slint** ([slint-ui/slint](https://github.com/slint-ui/slint)) | declarative `.slint` markup + Rust | Win/macOS/Linux + embedded + web | tri-license: **GPLv3 / commercial / royalty-free (conditions)** ⚠️ | Best-looking, but **not straightforwardly permissive** — licensing needs care for a clean-MIT project. |
| **GPUI** ([zed-industries/zed `crates/gpui`](https://github.com/zed-industries/zed/tree/main/crates/gpui)) | immediate-mode, GPU | Win (DX11)/macOS (Metal)/Linux — **parity reached with Zed 1.0, Apr 2026** | advertises **Apache-2.0** but **GPL-3.0 contaminated** ❌ (see below) | Appealing tech (premium native feel; Zed runs agents over **ACP**, junto's harness protocol) but **currently blocked** by licensing + framework churn (pre-1.0 standalone, thin docs). |
| **Dioxus** ([DioxusLabs/dioxus](https://github.com/DioxusLabs/dioxus)) | React-like | desktop target is **webview by default** | MIT/Apache | Doesn't get us off webviews yet (native renderer young). |

## ⚠️ The GPUI licensing trap (the reason GPUI is blocked, not just risky)

GPUI's crate manifest says `license = "Apache-2.0"`, **but a default build statically links GPL-3.0-or-later object code** — packages **`ztracing`** and **`zlog`**, pulled in transitively via `gpui → sum_tree → ztracing` ([zed#55470](https://github.com/zed-industries/zed/issues/55470)). Because copyleft attaches on **static linking alone** (the code doesn't have to *run* — and here it's a no-op shim that's inactive in default builds), any binary depending on `gpui` inherits GPL-3.0 source-availability + share-alike obligations. **This violates junto constraint #1.** longbridge's [gpui-component](https://github.com/longbridge/gpui-component) is itself Apache-2.0 but sits on top of GPUI, so it **inherits the same contamination**.

The contamination is reportedly **unintentional and trivially fixable upstream** — the proposed fix swaps `ztracing` for the standard `tracing` crate (already MIT/Apache) and drops the bad dependency (a ~2-file change). As of 2026-06-19 the issue appears **open/unresolved** with no maintainer response.

## 🔁 Revisit trigger

**Re-evaluate GPUI for junto when [zed#55470](https://github.com/zed-industries/zed/issues/55470) is resolved** (the `ztracing`/`zlog` GPL dependency removed from the default `gpui`/`sum_tree` build). Confirm empirically: a fresh `cargo tree -e features` / license scan (e.g. `cargo deny`) on a minimal `gpui` dependency shows **no GPL-3.0/AGPL** crates. If clean, GPUI moves from ❌ blocked to a viable candidate (it already cleared the cross-platform bar at Zed 1.0). Until then, **GPUI and anything built on it (incl. longbridge/gpui-component) is out** for a clean-MIT junto.

Suggested cadence: re-check whenever the native-surface question is picked back up, or every few months.

## Refined framing — read-only mobile splits the surface into two jobs

Dan's call (2026-06-19): **full work happens on desktop; on mobile he only wants to *check in*** — read the focus board, ratify/approve/park, glance at a running session. That split resolves most of the replace-vs-coexist tension:

- **Mobile + remote check-in = the served web surface.** Reading the focus board and acting on it from a phone browser is *exactly* what the host's server-rendered HTML already does (and the [focus board](attention.md) with inline acts *is* that surface, once built). So mobile is a reason to **keep** the webview/served surface, not abandon it. egui/Iced are poor mobile fits anyway (both desktop-first; mobile is experimental/DIY on either — see [iced #302](https://github.com/iced-rs/iced/issues/302)).
- **Desktop power-work = an optional native app.** Tmux panes, steering, the daily-driver "real app" feel — where a native toolkit would earn its keep, *if* the feel proves worth a second codebase.

**The reframe:** because the web surface stays (covering mobile + remote), a native desktop app **no longer forfeits browser/remote access** — those live on the web surface. So native becomes *additive* (a power-surface for two genuinely different jobs), not a lossy *replacement* of ADR 0018. The cost flips from "lose capabilities" to "maintain a second surface."

**The cheap de-risking step (independent of any native decision):** make the served surface **responsive** and **build the focus board**. That ships the mobile check-in Dan wants now, with zero native commitment — and lets him live on the web surface daily to find out whether the "I want native" itch justifies the second codebase. Build the native desktop app later (if at all), knowing exactly what it's for.

## Where this leaves the (deferred) decision

If junto ever goes native: **egui or Iced are the safe-by-construction choices** (both permissive, both genuinely cross-platform). **egui** is the right tool to *prototype* and decide empirically — the sharpest spike is the **tmux-pane workspace** (the thing the webview does worst). **GPUI** is the "watch this space" option, gated on the licensing fix above. None of this is committed: the replace-vs-coexist crux (whether junto gives up browser/remote access) is the gate on the whole thing.
