# Design — Browser View in the Right Blade

> Status: design, awaiting review. Topic: turning the throwaway browser-blade **spike**
> (commit `bfe41db`) in `crates/junto-iced` into a real, switchable **browser view** in the
> right blade — URL bar, back/forward/reload, mouse + keyboard injected back over CDP with
> correct coordinate mapping, a viewport sized to the blade and the display scale factor,
> a persistent-but-reaped process lifecycle modelled on Orca's embedded browser, and a
> visible error state when no Chromium is installed.
>
> Ledger context: findings [`89c8e707`] (feasibility from reading Orca + wmux) and
> [`f463944e`] (the spike result — ran it, 200ms first frame then 59.7fps at ~11KB/frame,
> change-driven, ack-gated). Sibling spec:
> [`2026-08-24-native-three-pane-shell-design.md`](2026-08-24-native-three-pane-shell-design.md).
>
> **Explicitly out of scope**, and deliberately not decided here: the three gestures (Grab /
> Annotate / Draw), which need an anchor vocabulary that is undecided
> ([`2026-08-23-universal-pointing-design.md`](2026-08-23-universal-pointing-design.md) is
> proposed-not-decided; DocAnchor is a future slice in
> [`2026-08-21-multiplayer-first-rethink-design.md`](2026-08-21-multiplayer-first-rethink-design.md));
> and any ADR superseding [`0018`](../../adr/0018-human-surface-is-a-desktop-shell-over-the-host.md),
> which still records native-widget UI as rejected and `junto-iced` as a spike. This design
> de-SPIKEs **only** the browser code; the crate's own spike framing stays as Dan's call (§10).

## 1. The problem

The spike proved feasibility and is scaffolding, not a feature. Concretely, in
`crates/junto-iced`:

- `src/cdp.rs` — headed `//! SPIKE`. One-way only: it spawns Chromium, starts a screencast,
  and parses frames out. There is no path *in* — no navigation, no input, no resize.
- `src/main.rs::browser_stream` (line 9000) — spawns a browser hardcoded to
  `https://example.com` at a hardcoded `520x1400`, streams frames, acks them. No commands.
- `src/main.rs` `App` fields (lines 256–271) `browser_on` / `browser_frame` /
  `browser_frames_seen` / `browser_frame_size` — all tagged SPIKE, deliberately kept out of
  `ShellState` so the spike would not persist itself.
- `right_blade` (line 4818) — a chevron toggles `browser_on`; the frame is drawn with an
  on-screen `"SPIKE · browser · N frames"` overlay.
- `src/bin/cdp_probe.rs` — a throwaway probe binary, whose existence makes plain `cargo run`
  ambiguous.

The spike also flagged the two things it left unsolved: the viewport is
"blade-shaped, not desktop-shaped" but still hardcoded (`browser_stream` comment,
lines 9009–9016), and the frame size "must be sized to the blade's PHYSICAL pixels" using
the display scale factor (`App::browser_frame_size` doc, lines 267–271).

## 2. What the spike settled — do not re-litigate

From `bfe41db` and the ran-it finding `f463944e`:

1. A live browser renders in the right blade as an ordinary Iced widget (a texture in Iced's
   own layout tree), so it clips to the blade and composites **under** popovers/drawers — no
   airspace problem, no embedded engine.
2. The engine choice is settled for this build: drive an already-installed Chromium
   (Edge/Chrome) over CDP `Page.startScreencast`. **Not** cef-rs — `89c8e707`'s cef fallback
   is unneeded now that the external-Chromium path is proven. No new heavyweight dependency.
3. Screencast is **change-driven**, not clocked: a static page paints a few frames and goes
   silent, so an idle browser costs ~0 CPU — but first paint, resize, and refocus each need
   an explicit repaint trigger (§6) or the blade shows nothing.
4. `Page.screencastFrameAck` is **mandatory flow control**: skip an ack and Chromium sends
   exactly one frame then goes quiet, indistinguishable from a dead socket.
5. The wmux launch-switch list holds verbatim: `--remote-debugging-port=0` + reading
   `DevToolsActivePort`, `--remote-allow-origins=*`, and the three occlusion switches.

All of §2 is preserved as-is (the switch list, the ack discipline, the `Chromium`+`Drop`
reaper). This design changes what surrounds them.

## 3. Architecture — one new idea

The spike is one-way (frames out). A real browser needs commands *in* (navigate, click, type,
resize). The one architectural change is a **bidirectional driver**: the screencast stream
gains a command channel.

This reuses a pattern already in this crate — `live_ws_stream` (line 9294) is an
`iced::stream::channel` worker that owns a socket and multiplexes I/O. The browser driver does
the same shape:

```
browser_stream = iced::stream::channel(N, |output| async move {
    let (cmd_tx, cmd_rx) = futures::channel::mpsc::channel::<cdp::Command>(N);
    // ...spawn Chromium, connect the page socket, Page.enable...
    output.send(Message::BrowserReady(cmd_tx)).await;   // hand the app the control end
    loop {
        select! {
            cmd  = cmd_rx.next()   => forward cmd → CDP request(s) on the socket
            wire = socket.next()   => frame → BrowserFrame + ack; nav event → BrowserNav; closed → BrowserClosed
        }
    }
    // browser dropped here → killed + profile removed (cdp::Chromium::drop)
})
```

The app stores the `Sender<cdp::Command>` from `BrowserReady` and drives the browser through
it. This is the standard iced worker handshake (the crate's `annotate_tx` path does the same
thing for the live socket).

### Module layout — pure logic vs. adapter

Following the crate's existing split (pure, iced-free `pointing.rs`; custom `Widget`
`popover.rs`):

| File | Responsibility |
|---|---|
| `src/cdp.rs` (de-SPIKE) | Process lifecycle (`Chromium` + `Drop`, kept), `find_chromium` restructured into real `#[cfg]` arms (§8), the `cdp::Command` enum, CDP **request builders** and **event parsers**. `DebugPort` newtype. |
| `src/browser.rs` (**new**, pure, no `iced`) | URL normalization, `ViewportSize`/`PagePoint` newtypes, cursor→page mapping, CDP key-field mapping from a neutral `KeyStroke`, and the `NavState` model. Unit-tested standalone like `pointing.rs`. |
| `src/screencast.rs` (**new** custom `Widget`) | Fills the blade, draws the newest frame via `iced_widget::image::draw`, reports its logical size on change, and translates mouse/keyboard/scroll into page-space messages. |
| `src/main.rs` | Rewrite `browser_stream` into the driver; replace SPIKE `App` fields with real browser state; add the nav row; make the right blade a `RightView` switch. |
| `src/shell.rs` | `RightView` enum + `browser_url` added to `ShellState` (§7). |
| `src/bin/cdp_probe.rs` | **Deleted** (§8). |

## 4. Viewport sizing and coordinate mapping — the crux

The spike's own note: a desktop-shaped viewport shrunk into a ~520px blade renders unreadably,
and the frame must be sized to the blade's physical pixels via the display scale factor.

**Decision: emulate a CSS viewport equal to the widget's logical size, with
`deviceScaleFactor` = the display scale.** One decision buys two properties:

- **Crisp on HiDPI.** With `Emulation.setDeviceMetricsOverride { width: W, height: H,
  deviceScaleFactor: S }`, Chromium lays the page out at `W×H` CSS px and renders the
  screencast frame at `W·S × H·S` physical px. The `W×H`-logical widget draws that physical
  frame → a native-resolution downscale, not an upscale-blur. On Dan's 150% display, `S = 1.5`.
- **Correct input mapping by construction.** `Input.dispatchMouseEvent` takes coordinates in
  CSS px within the emulated viewport. Because the emulated viewport `W×H` equals the widget's
  logical size, a widget-local cursor point maps to page coordinates **1:1** — no letterbox
  arithmetic, no scale term in the mapping. The scale factor's *only* job is frame resolution.

`W`/`H` come from the widget's actual laid-out logical size (`screencast.rs` reports it, §5),
not a constant. `S` comes from `iced::window::scale_factor(id) -> Task<f32>` (a real 0.14 API,
`iced_runtime::window::scale_factor`), refreshed on `window::Event::Resized`. This retires the
`520x1400` constant and the `520`/`1400` args to `cdp::spawn`.

`browser::map_cursor(local_x, local_y, bounds_w, bounds_h) -> PagePoint` is the seam: today it
is identity + a clamp into `[0,W]×[0,H]` (a cursor exactly on the edge must not produce an
out-of-viewport coordinate). It exists as a named, tested function so that if a future letterbox
mode is ever wanted, the mapping has one home.

## 5. `screencast.rs` — the input-injecting widget

A custom `Widget` (precedent: `popover.rs`), because it needs three things `image` + `mouse_area`
cannot jointly give: its own laid-out bounds (to size the viewport), 1:1 local coordinates for
press/release (iced's `mouse_area::on_press` carries no position), and keyboard capture while
focused. Bounds `Renderer: iced::advanced::Renderer + image::Renderer<Handle = image::Handle>`.

**Tree state** (`Widget::tag`/`state`): `{ bounds: Rectangle, focused: bool, cursor: Option<Point> }`.

In `Widget::update(event, layout, cursor, shell, …)`:

- **Resize.** If `layout.bounds()` differs from stored `bounds`, `shell.publish(on_resize(size))`
  with the new **logical** size. This is what drives `SetViewport` and therefore the first
  paint (§6).
- **Mouse move.** `cursor.position_in(bounds)` → `browser::map_cursor` → `on_move(PagePoint)`;
  stored as `cursor` for press/release, which have no position of their own.
- **Press/release/scroll** (left/right/middle + wheel), when over bounds: publish
  `on_mouse(kind, PagePoint)` / `on_scroll(PagePoint, delta)` and `capture_event()`.
- **Focus.** A left press inside sets `focused = true`; a left press outside sets
  `focused = false`. (Clicking the URL `text_input` is outside the widget, so it un-focuses the
  page — the two input sinks never both consume a keystroke.)
- **Keyboard.** When `focused` and the event is `Event::Keyboard(KeyPressed/KeyReleased)`, build
  a neutral `browser::KeyStroke { text, named, modifiers }`, publish `on_key(KeyStroke)`, and
  `capture_event()`. Capturing is load-bearing: `iced::keyboard::listen()` (the `Ctrl/Cmd+B|R`
  blade shortcuts, `subscription()` line 3895) only ever receives events the UI left
  `Status::Ignored`, so a focused page swallows the shortcut cleanly rather than toggling a
  blade mid-type.

`Widget::draw` calls `iced_widget::image::draw(renderer, layout, handle, None, radius 0,
ContentFit::Fill, FilterMethod::Linear, …)` when a frame exists (aspect matches bounds, so
`Fill` neither stretches nor letterboxes), else fills `SURFACE` so the widget still occupies —
and therefore reports the size of — the blade before the first frame. `request_redraw()` on
each new frame handle.

**Keyboard scope (a stated contract, not a stub).** v1 injects: all printable characters via
`Input.dispatchKeyEvent { type: "char", text }`, plus a mapped control set —
Enter, Tab, Backspace, Delete, Escape, Arrows, Home, End, PageUp/PageDown — via
`keyDown`/`keyUp` carrying the correct `key`/`code`/`windowsVirtualKeyCode`, with `modifiers`
folded from the held modifier bitmask. This is enough to type URLs, search, and fill page forms.
Function/media/IME keys are explicitly not mapped in v1; `browser::key_stroke` returns `None` for
them and the widget drops them rather than injecting garbage.

## 6. Navigation and repaint triggers

`cdp::Command` (app → driver):

```
enum Command {
    Navigate(String),                 // already normalized by browser::normalize_url
    Reload, Back, Forward,
    Mouse(MouseInput), Key(KeyInput), Scroll { at: PagePoint, dx: f32, dy: f32 },
    SetViewport { size: ViewportSize, scale: f32 },
    Show,                             // resume: startScreencast (forces one fresh frame)
    Hide,                             // pause: stopScreencast (page stays alive)
}
```

Driver → app (`Message`): `BrowserReady(Sender<Command>)`, `BrowserFrame(Vec<u8>)` (jpeg only —
device size no longer needed for mapping), `BrowserNav(NavState)`, `BrowserError(String)`,
`BrowserClosed`.

- **URL bar.** A `text_input` bound to `browser_url_input`; Enter or the Go button →
  `browser::normalize_url` (adds `https://` when scheme-less; passes `about:`/`data:`/`file:`
  through; rejects empty) → `Command::Navigate`.
- **Back / Forward / Reload.** `Reload` → `Page.reload`. `Back`/`Forward` → the driver issues
  `Page.getNavigationHistory`, computes `currentIndex ± 1`, and issues
  `Page.navigateToHistoryEntry { entryId }`. The driver owns history; the app only enables the
  buttons from the last `NavState`.
- **`NavState { url, can_back, can_forward, loading }`.** On `Page.frameNavigated` (main frame)
  and `Page.loadEventFired`, the driver queries `Page.getNavigationHistory` and derives
  `url = entries[currentIndex].url`, `can_back = currentIndex > 0`,
  `can_forward = currentIndex < entries.len() - 1`; `loading` flips true on navigate, false on
  load. Emitted as `BrowserNav`.

**Repaint triggers** (§2.3):
- *First paint* — the `Navigate` load paints; belt-and-suspenders, `Show` restarts the
  screencast which forces one fresh frame of current state.
- *Resize* — `SetViewport` re-applies `setDeviceMetricsOverride`, which relayouts and emits a
  frame.
- *Refocus* — selecting the Browser view sends `Show` (stop→start screencast → one fresh frame);
  a `window` regaining focus does the same.

`screencastFrameAck` stays mandatory on every frame (§2.4).

## 7. Right-blade view switch and persistence

The right blade becomes a switch over `RightView { Lineage, Browser }` (reintroducing the
switcher shape the three-pane spec's §2 originally carried for Artifacts/Lineage). A small
two-item segmented control in the blade footer, `Message::SelectRightView(RightView)`.

`ShellState` (`src/shell.rs`) gains, both under the existing `#[serde(default)]` so old
`ui.toml` files still load:

```
pub right_view: RightView,           // #[default] Lineage; kebab-case serde like BottomView
pub browser_url: Option<String>,     // last-navigated URL, so the browser reopens where you left off
```

`right_view` is the view choice the task requires persisted "the same way the other blade state
persists". `browser_url` is the small addition Dan approved: the in-memory value is updated on
each `BrowserNav`, and rides `persist_shell()` (already called on every blade change) so it is
flushed at least on every view switch. On startup, if `right_view == Browser` the driver spawns
and the app navigates to `browser_url` (else `about:blank`).

The SPIKE `App` fields (256–271) are replaced by real state: `browser_cmd:
Option<Sender<Command>>`, `browser_frame: Option<image::Handle>`, `browser_nav: NavState`,
`browser_url_input: String`, `browser_error: Option<String>`, `browser_scale: f32`, and
`browser_generation: u64` (§9). The `frames_seen`/`frame_size` debug counters are dropped.

## 8. Process lifecycle, platform, and the probe binary

**Lifecycle — persistent within the session, reaped on exit, modelled on Orca.** Orca's embedded
browser is a tab surface (`orca tab list/create/close/switch`, `goto`/`back`/`reload`): a tab
stays alive when you look away and is torn down only by explicit close. Mirroring that:

- The driver subscription is gated on a latch `browser_ever_opened` (set true the first time the
  Browser view is selected; it does not clear on switch-away). So the browser survives switching
  to Lineage and back — page, scroll, and history intact.
- Switching to Lineage sends `Command::Hide` (`stopScreencast`) so no frames flow while hidden;
  switching back sends `Command::Show`. Change-driven + stopped screencast ⇒ ~0 idle CPU. Idle
  cost is one background process (~150MB RAM) for the app's lifetime, which is the Orca-tab
  tradeoff.
- **No orphan, ever.** The `cdp::Chromium` is owned by the driver future. On app exit the runtime
  drops the subscription → `Drop` kills the child, `wait()`s it, and removes the temp profile.
  Because library code has no `unwrap`/`expect`/`panic` (a repo constraint), the only teardown
  path is `Drop`, so there is no panic path that skips it.
- **Crash recovery.** The subscription id is `("browser", browser_generation)`. If the driver
  emits `BrowserClosed` (socket died / browser crashed), the app increments
  `browser_generation` and shows a reopen affordance; re-selecting Browser latches a fresh
  generation → iced starts a new driver instance. A stable id alone would leave a finished
  subscription un-restarted.

**Platform — real `#[cfg]` arms.** `find_chromium` is restructured from one flat list into
`#[cfg(target_os = "windows")]`, `#[cfg(target_os = "macos")]`, and `#[cfg(target_os = "linux")]`
arms, each a real candidate list (Windows: Edge + Chrome under Program Files; macOS: the
`/Applications/*.app/Contents/MacOS/*` paths already present; Linux: `google-chrome`,
`chromium`, `microsoft-edge`), plus a `JUNTO_BROWSER` env override honored on every platform as
the escape hatch and the test seam. A non-matching OS returns `None` → the error state (below).
macOS stays code-real though unrun on hardware, per the task.

**`cdp_probe`.** Deleted with the rest of the spike. This also resolves the `cargo run`
ambiguity the second binary introduced — one binary remains, so no `default-run` is needed. The
`base64` dependency stays (it decodes frame bytes) but loses its "SPIKE ONLY" comment.

## 9. Error and empty states

- **No Chromium** (or spawn/connect failure): the driver emits `BrowserError(msg)` and returns;
  the app stores it and the Browser view renders, in place of the screencast, a
  `ICON_CIRCLE_ALERT` heading, the message, the list of paths checked, and an install hint
  ("Install Microsoft Edge or Google Chrome, or set `JUNTO_BROWSER`"). Required by the task — no
  silent blank.
- **Before first frame:** the widget draws `SURFACE`; the view stacks a centered muted
  "starting browser…" label over it, cleared on the first `BrowserFrame`.
- **After `BrowserClosed`:** a "browser exited — reopen" state with a reopen button (§8).

## 10. Non-goals

- **The three gestures** (Grab / Annotate / Draw page element). They need an anchor landing
  place in junto's vocabulary that is genuinely undecided (universal-pointing is
  proposed-not-decided; DocAnchor is a future slice). Stop at the browser view.
- **No ADR.** [`0018`](../../adr/0018-human-surface-is-a-desktop-shell-over-the-host.md) still
  records native-widget UI as rejected and `junto-iced` as a spike, which contradicts this whole
  surface — that is Dan's call, and no ADR superseding it is written here. This design de-SPIKEs
  only the browser-specific code and leaves the crate's own spike framing (its `Cargo.toml`
  header, `main.rs`'s top doc comment) untouched.
- **Multi-tab.** One browser, one page. Orca has tabs; v1 does not.
- **Downloads, devtools, cookies/profile persistence, `wss`/proxy config, ad-blocking.** None.
  The profile is a throwaway temp dir, as in the spike.
- **Full VK keyboard coverage.** §5 states the covered set; the rest is dropped, not faked.

## 11. Testing

`view()` is not unit-testable in Iced, so decidable logic lives in the pure `browser.rs` and in
`cdp.rs` request/parse functions.

| Tested — pure logic | How |
|---|---|
| `normalize_url` (scheme-add, `about:`/`data:`/`file:` passthrough, empty rejected) | `browser.rs` unit tests |
| `ViewportSize::from_logical` (round, clamp ≥ 1) and `map_cursor` (identity + edge clamp) | `browser.rs` unit tests |
| `KeyStroke` → CDP fields (printable → `char`+text; Enter/Backspace/arrows → VK codes; unmapped → `None`) | `browser.rs` unit tests |
| `NavState` derivation from a `getNavigationHistory` reply (url, can_back/forward) | `browser.rs` + `cdp.rs` parse test |
| CDP request builders (navigate / reload / navigateToHistoryEntry / dispatchMouse / dispatchKey / setDeviceMetricsOverride / stopScreencast / ack) | `cdp.rs` builder tests, extending the existing `parse_tests` |
| frame + nav-event parsers | `cdp.rs` (the existing `parse_screencast_frame` tests stay) |

| View-level — `iced_test` simulator | |
|---|---|
| Browser view shows the URL bar + nav row; error state shows the alert + message; the two are mutually exclusive | assert widget ids present/absent |
| `RightView` switch flips Lineage ↔ Browser and persists (`ShellState` round-trip) | `shell.rs` + simulator |

| Accepted as untested here | Verified live instead |
|---|---|
| the composited GUI window (no screenshot harness in this environment) | the end-to-end driver smoke below |
| pixel layout, drag mechanics | — |

**Live end-to-end smoke (run once, reported as evidence — not committed to the default suite).**
Drive the real `cdp` driver against the installed Edge: spawn, `setDeviceMetricsOverride`,
`Page.navigate` to a `data:` page, assert `BrowserFrame`s arrive; inject a mouse click and a
keystroke, then `Runtime.evaluate` to read the DOM back and confirm the input landed; change the
viewport and confirm a fresh frame at the new size. This is what validates the load-bearing
assumptions (`setDeviceMetricsOverride` + screencast coexist under `--headless=new`; input is CSS
px of the emulated viewport) that unit tests cannot. The task's own note stands: the fully
composited window cannot be screenshotted in this environment, so visual confirmation is the
driver round-trip plus the layout assertions, not a picture.

Per the sibling spec's §7 note, `junto-iced` should be in CI; if it still is not, the plan adds
a job mirroring the `desktop` job (`fmt`, `clippy -D warnings`, `test --manifest-path
crates/junto-iced/Cargo.toml`). All new code obeys the crate constraints: no `unwrap`/`expect`/
`panic` in library code, newtypes over bare primitives (`DebugPort`, `ViewportSize`, `PagePoint`,
`RightView`), comments explaining *why*.

## 12. Considered and rejected

- **cef-rs / embedded engine.** Rejected: `f463944e` proved the external-Chromium path works
  with zero heavyweight deps; cef adds CMake/Ninja + ~150MB + an API surface for no gain here.
- **Ephemeral process** (kill on switch to Lineage, respawn on return). Rejected in favor of
  persistent-paused (§8): respawning loses the page/scroll every time you glance at lineage, and
  Orca — the reference Dan named — keeps its browser tab alive. Persistent has the same no-orphan
  guarantee; its only cost is idle RAM.
- **`image` + `mouse_area` instead of a custom widget.** Rejected: `mouse_area::on_press` carries
  no position, and neither primitive exposes the widget's own bounds (needed to size the viewport
  to the blade) or captures keyboard while focused. The custom widget is the smaller correct
  thing.
- **Viewport at `deviceScaleFactor: 1`, logical resolution.** Rejected: text is soft on a HiDPI
  display, and the spike explicitly named scale factor "the crux". Matching CSS viewport to
  widget logical size while carrying the real scale gives both crispness and 1:1 input mapping.
- **Re-applying `setDeviceMetricsOverride` as the refocus repaint trigger.** Considered; using
  `stop`→`start` screencast for Show/refocus is more reliable at forcing exactly one fresh frame
  of the *current* state, and `setDeviceMetricsOverride` is reserved for genuine resizes.
- **Persisting `browser_url` outside `ShellState`.** Rejected: `ShellState` is already "what
  persists across runs" (it holds the bottom-drawer choice, itself not pure geometry), so the URL
  belongs beside `right_view`, not in a second file.
