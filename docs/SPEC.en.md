# Feather DE (Пёрышко) — Specification

**[🇷🇺 Читать на русском](SPEC.md)** · **[← Back to README](../README.md)** · **[Roadmap](ROADMAP.en.md)**

This document sets the goals, budgets, architecture and roadmap. Every change must fit the
budgets in section 2. If it does not fit, it is a regression, even if everything works.

## 1. Goal

**The fastest and lightest Linux desktop environment that looks and behaves like the latest
macOS (Tahoe, Liquid Glass).** Light theme by default.

Principles, in order of priority:

1. **Responsiveness.** Input and a client's frame never wait for the compositor. The event loop
   never blocks on anything (vsync, disk, D-Bus, process spawning).
2. **Zero work when idle.** No timers at frame rate and no "just in case" redraws.
   Idle CPU is 0 %.
3. **Memory.** Buffers are created on demand and freed when no longer needed. No "just in case"
   indexes (icons are looked up one at a time).
4. **Recognisably macOS.** Metrics, colours, fonts, the behaviour of Dock / menu / Spotlight /
   Cmd+Tab. If this conflicts with 1–3, speed wins and the look finds another way.

## 2. Budgets (measured, not assumed)

| Metric | Budget | Baseline (RX 580, 4K, ×2) | How to measure |
|---|---|---|---|
| Compositor CPU when idle | 0 ticks in 5 s | 0 | `/proc/<pid>/stat`, fields 14+15 |
| Compositor work per frame (scene + GL) | ≤ 1 ms | 0.65 ms | `FEATHER_PROFILE=1` |
| Blocking on swap | ≤ 0.2 ms | 0.08 ms | `FEATHER_PROFILE=1` |
| Chromium animation FPS | = display refresh rate | 60/60 | `bench/anim.html` |
| Private memory (RssAnon), steady state | ≤ 60 MB | ~48 MB | `grep RssAnon /proc/<pid>/status` |
| Short peak during a screenshot | ≤ 60 MB + 2 × an RGBA frame, back within ≤ 2 s | — | RssAnon samples every 50 ms around the screenshot |
| Shell UI redraw (menu, Dock) | ≤ 4 ms CPU | — | `FEATHER_PROFILE=1`, "CPU buffers" |
| Cold start to the first frame | ≤ 300 ms | — | time to the first `[feather-profile]` |

A screenshot temporarily holds the captured frame and its PNG copy (at 2880×1800 each is
about 20 MB). Such a peak is acceptable as long as memory returns to the steady state.
Measurement of 2026-10-24, 2880×1800: 32.7 → peak 76.1 → 35.6 MiB in about 1.5 s. A peak
without a screenshot (menus, panels, settings) must still stay within 60 MB.

About 150 MB of RSS belongs to Mesa/LLVM. These are shared libraries common to all GL
processes and are not counted against the budget.

The baseline values do not confirm the budgets of the current version after all the changes.
After background rasterisation, font/CPU-panel optimisations and wiring up the Control Center
services, in a verified nested 4K ×2 session on an active Hyprland: RssAnon after Launchpad
34.46 MiB, observed peak 52.15 MiB (54.69 MB), idle 0 ticks/5 s, animations at ~60 Hz.
The current report: [CONTROL_CENTER.md](CONTROL_CENTER.md). Earlier comparisons:
[UI_CHECKS.md](UI_CHECKS.md); memory/worker stages — [MEMORY_CHECKS.md](MEMORY_CHECKS.md),
[RASTER_CHECKS.md](RASTER_CHECKS.md). Dock redraw with icons at 4K: 16–28 → 3.1–3.5 ms
(exact-size icons, UI_CHECKS). GL peaks above 1 ms and first CPU frames of panels above 4 ms
on an active host remain; acceptance of a real DRM session is still open.

## 3. Architecture

```
calloop EventLoop (single thread)
├─ ListeningSocketSource      — new clients
├─ Generic(Display)           — client requests, handled immediately
├─ WinitEventLoop             — host input; Redraw = "the previous frame was shown"
├─ Timer 1 s / at minute change — clock, RSS, notification expiry (once a minute when idle)
├─ appmenu channel            — ready menu snapshots from the D-Bus worker
├─ raster channel             — ready CPU buffers of icons/wallpapers/tiles from the worker
└─ idle-callback              — pointer focus → render_if_needed → flush_clients
```

| Module | Responsibility |
|---|---|
| `main.rs` | State, focus (`refresh_focus` is the single source of truth), shell actions, the Dock/Alt+Tab model, settings |
| `handlers.rs` | Wayland protocols: xdg-shell (+popups), decoration, activation, dmabuf v4, cursor-shape, selection |
| `input.rs` | Keyboard (filter, hotkeys, Alt+Tab), pointer, drag/resize, grabs |
| `pointer_constraints.rs` | Game relative pointer, lock/confine, safe release of the grab on focus change |
| `animation.rs` | Analytic springs of windows and panels; transforms and alpha, stops without a timer |
| `appmenu.rs` | KDE Wayland appmenu, asynchronous DBusMenu/Registrar in a separate thread, cancelling stale RPCs on focus change |
| `tray.rs` | StatusNotifierWatcher/host, tray icons and DBusMenu; worker, bounded queues, signals without polling |
| `controls.rs`, `controls/network.rs` | Snapshots and commands for NetworkManager Wi-Fi / PipeWire Pulse-compatible sound, events on workers, bounded queues |
| `control_panel.rs` | Control Center pages, network password, local slider preview, real states and vector icons |
| `raster.rs` | CPU worker: bounded queues, scale/theme snapshots, cancelling stale results; no EGL/Wayland objects |
| `render.rs` | Scene (front-to-back), damage tracking, frame pacing, frame callbacks, profiler |
| `window.rs` | Window model: geometry, SSD/CSD, mapped, maximize/fullscreen, 9-slice shadow, built-in windows |
| `sounds.rs` | Shared sleeping worker for system sounds/notifications: short PCM in a memfd, one time-limited player, no sound queue |
| `notifications.rs` | `org.freedesktop.Notifications` server on a worker: hint parsing, images ≤ 96 px, close/action signals; another daemon's name is never taken over |
| `api/` | `org.feather.Shell1` ([API.md](API.md)): zbus in the `feather-api` thread, requests enter the loop through a bounded channel with one-shot replies, window signals from `refresh_focus`; types and client — the `crates/feather-api` crate, the `featherctl` utility |
| `xwayland.rs` | Own X display: lock and sockets, xwayland-satellite on the first X client via `-listenfd`, restart and cleanup |
| `gtk_shell.rs`, `gtk_menu.rs` | `gtk_shell1` v1 (GTK menu address, global menu bar capability) and loading `org.gtk.Menus`/`org.gtk.Actions` into the shared menu tree |
| `spaces.rs` | Workspaces: window ownership, growing and removing spaces, going to a window's space, spring shift |
| `space_layout.rs` | Modes of each workspace (`Free`, `Tile` BSP, `Tape` Niri-style ribbon with GPU scrolling) and saving frames |
| `glass.rs` | Live optical glass on the GPU: Dual-Kawase blur `1/4 → 1/8 → 1/16` strictly within `damage ∩ panels`, SDF refraction and dispersion |
| `drop_shelf.rs` | Drag-and-drop shelf for `wl_data_device` (`ClientDnd` / `ServerDnd`), cursor shake, temporary images in `XDG_RUNTIME_DIR` |
| `menu_hud.rs` | HUD search over menu commands (`Super+/`): expanding `DBusMenu`/`org.gtk.Menus` and shell menus on demand, keyboard layout auto-correction |
| `pip.rs` | Picture in Picture (`Super+P`) and the ghost "click-through" mode (`Super+Alt+P`, grabbed with `Super`), snapping to corners |
| `mission.rs` | Mission Control: overview layout, space strip, clicks and dragging a window onto a space |
| `night.rs`, `brightness.rs` | Night mode (CRTC gamma, smooth transition on the one-second tick) and brightness (logind backlight or DDC/CI on a thread, last value) |
| `shell.rs` | Shell UI: menu bar, Dock, menus, Spotlight, Control Center, notifications, switcher |
| `theme.rs` | macOS metrics, light/dark palettes, drawing primitives, shadows, wallpapers |
| `ui_raster.rs` | tiny-skia AA mask, integer fills and separable bilinear glass; SSE2 span on x86_64 and scalar edges/other architectures |
| `cpu_profile.rs` | Optional FEATHER_PROFILE_UI: CPU stages and primitive/text costs; no timers or output when the flag is off |
| `text.rs` | Fonts (SF Pro → Inter → Noto via fontconfig), shared read-only mmap of TTC, rasterisation of needed glyphs and a bounded mask cache |
| `icons.rs` | File lookup/SVG/PNG on a worker; bounded cache of ready icons on the main thread, preview on eviction |
| `apps.rs` | Application registry (.desktop), window → application matching, procedural icons |
| `settings.rs` | `~/.config/feather-de/settings.conf`, atomic-save worker and XKB compilation |
| `system_settings.rs` | System settings pages, input sources, animations, scale, confirming DRM modes; CPU frame on the raster worker |
| `wallpaper.rs` | Built-in/own PNG/JPEG/WebP, Wayland picker worker, bounded decoder process; GPU texture without a permanent CPU copy |
| `system_preferences.rs` | Worker for XDG default applications and systemd-timedated (NTP/time zone/manual time), terminal launcher for Files |

### Frame pipeline

1. An event (input, client commit, timer) sets `needs_redraw`.
2. `render_if_needed` does nothing while the previous frame is "in flight" (`frame_pending`).
3. Dirty CPU buffers are redrawn, and **only the changed regions** are uploaded to the GPU.
4. `OutputDamageTracker` redraws only the changed screen regions; swap does not block (interval 0).
5. Frame callbacks go to clients right after submit: the client draws its next frame while ours
   is being shown. Only to windows this output actually drew (`shown_windows`); minimised ones,
   ones on other spaces and ones covered by a fullscreen window get them once a second.
   `wp_presentation` feedback is collected there too; in DRM it is answered with the flip time
   from the kernel.
6. `WinitEvent::Redraw` (host vblank) clears `frame_pending`. The safety net is 40 ms.
   In DRM each output has its own `pending`/`dirty`: a monitor does not wait for its neighbour's
   vblank. After a vblank the next frame is not built at once but at the `render_at` deadline —
   2 × the recent frame cost (≥ 4 ms and ≥ ¼ of the period) before the next vblank; not
   under VRR, not while locked; `FEATHER_RENDER_DEADLINE=0` turns it off.
7. A frame without damage does no swap, and callbacks are sent on a 16 ms timer (protection from a busy loop).
8. An active spring keeps `needs_redraw` until the next vblank. After reaching its target the
   spring snaps exactly to the final state; there is no separate animation timer.
   A minimised window is drawn to the end of its flight but is immediately excluded from focus/input.
   Tiles of the visible Launchpad page are prepared on a worker; neighbouring pages are not
   rasterised in advance, old pages are freed. A ready result triggers damage, not a spring restart.

### Background asset preparation

A single `feather-raster` takes up to 64 requests and returns up to 2 ready results through a
channel into calloop. The main thread only uses `try_send`, the worker sleeps on `recv`.
Wallpapers, blur, icon lookup/decoding (including procedural ones) and full Launchpad tiles
are prepared outside the event loop. Theme/scale are fixed for the duration of a job in a
thread-local context; a configuration change bumps the epoch, a page change/close bumps the tile ticket.
Results that no longer fit are discarded. Font tables are shared via Arc, glyph caches are separate.

The icon pixel cache is limited to 4 MiB and 128 entries; on eviction a ready 64 px preview
remains, not a repeat decode. After Launchpad closes, its buffers and background are removed;
on GNU, the worker returns the freed pages to the allocator once after a cleanup frame.
Allocator settings are not changed, there is no periodic trim/polling. GL stays on the main
thread. Comparative measurements and limits — [RASTER_CHECKS.md](RASTER_CHECKS.md).

Outlines of all characters are not expanded in advance: ttf-parser borrows tables from the
shared mmap, tiny-skia builds the AA mask of only the needed glyph. Each thread keeps its own
masks, up to 1 MiB/4096 entries at ordinary UI sizes. Files are opened only at startup.
fontdue remains solely the metrics reference in dev tests. Checks of savings, full RSS and
appearance — [MEMORY_CHECKS.md](MEMORY_CHECKS.md).

Panels use the same AA geometry, but solid colours are blended with integer premultiplied
source-over, and the bilinear glass reuses two intermediate rows. No full-size wallpaper cache
was added. Text clipping is linear, kerning uses cached glyph indices and the needed mmap table
without re-parsing the TTC. The static base of the Control Center is prepared on the raster
worker after the wallpapers: one bounded buffer, only the current theme/scale/geometry.
A notification is hidden while the CC is open. The main thread copies the base and draws live
labels/controls; if there is no base it falls back to ordinary rasterisation. Epoch/invalidation
are shared with the other assets. First-frame time comparison and pixel-by-pixel checks —
[UI_CHECKS.md](UI_CHECKS.md).

### Rules that must not be broken

- **Do not block the loop.** Processes are started with `spawn`; file I/O is allowed only at startup or on a user action.
- **Do not touch keyboard focus inside `KeyboardHandle::input`** — it deadlocks. Return a `KeyAction` and carry it out afterwards.
- **Focus changes only through `refresh_focus()`.** After a window stack change, `pointer_focus_dirty` is set.
- **Fullscreen buffers** are created lazily and freed (see the overlay). Damage from `render().draw` — only real regions.
- **CPU rasterisation** is allowed only for what has changed. Animations need GPU elements and transforms, not pixel redraws.
- **New protocols** are checked under `WAYLAND_DEBUG=1` before merging, at least with Chromium/Electron, Qt and GTK.

## 4. Protocols

Supported: wl_compositor, wl_subcompositor, wl_shm, xdg_shell (toplevel + popup, move/resize/
maximize/fullscreen/minimize), xdg_decoration (the client decides), xdg_activation_v1, wp_presentation,
linux-dmabuf v4 with feedback, wl_data_device, primary_selection, wp_viewporter,
wp_fractional_scale_v1, wp_cursor_shape_v1, xdg_output.
Also `org_kde_kwin_appmenu_manager` v2: a menu address is bound to a specific wl_surface.
The D-Bus worker sends snapshots through a bounded channel into calloop; the main thread
passes commands without waiting. A focus change clears the menu at once and cancels the old RPC.

Queue by value:

| Priority | Protocol | Why |
|---|---|---|
| P0 | `zwp_text_input_v3` + `zwp_input_method_v2` | IME, input in Chromium/GTK with `--enable-wayland-ime` |
| P0 | keyboard grab for `xdg_popup` | keyboard navigation of menus |
| P0 | `zwlr_layer_shell_v1` | third-party panels, notifications (mako), launchers |
| P1 | `ext_session_lock_v1`, `ext_idle_notify_v1`, `zwp_idle_inhibit_manager_v1` | screen lock, blanking, video without sleep |
| P1 | `zwlr_screencopy_v1` / `ext_image_copy_capture` + xdg-desktop-portal | screenshots, screen sharing (ScreenCast already runs through its own PipeWire stream, `src/pw_cast.rs`) |
| P1 | XWayland (xwayland-satellite) | X11 applications inside the DE, not in labwc |
| P2 | `wp_linux_drm_syncobj_v1` (explicit sync) | NVIDIA, lower latency |
| P2 | `wp_content_type_v1`, `wp_tearing_control_v1` | games, VRR |
| P2 | `wlr_data_control` | clipboard managers |

## 5. Roadmap

The current checklist with the status of each item is **[ROADMAP.en.md](ROADMAP.en.md)**; below
is the original description of the stages and acceptance criteria.

An item counts as done only when its acceptance criteria are met and the budgets in section 2 are not broken.

### Stage 1 — GPU "glass" and rounded corners
- **Blur under glass** (menu bar, Dock, menus, Spotlight, CC, notifications, Alt+Tab): dual-Kawase
  on the GPU, recomputed only when there is damage under a panel.
  *Done when* panels show blurred window content and the work per frame stays ≤ 1.5 ms.
- **Rounded corners of client windows** via a custom texture shader (as in niri); removes the
  square bottom corners of SSD windows. *Done when* corners match the title bar at any scale.
- **Shadows for CSD windows without their own** — already there; check GTK4 and libadwaita.

### Stage 2 — animations (macOS physics)
- Spring animations (no durations): window open/close, minimising into the Dock
  ("genie" or "scale"), Dock magnification, appearance of menus and Spotlight.
- GPU transforms only (`RescaleRenderElement`, alpha). CPU buffers are not redrawn.
- *Done when* animations run at the display refresh rate, idle is 0 % CPU, and input is not delayed.

### Stage 3 — macOS behaviour
- **Mission Control** (F3 / gesture): all windows laid out, workspaces (Spaces) on top.
- **Launchpad**: an application grid with search.
- **Cmd+` (Alt+`)**: switching windows within an application.
- **Global menu**: Qt/KDE through Wayland appmenu + DBusMenu are implemented, GTK-native
  `org.gtk.Menus`/`org.gtk.Actions` remains. Support depends on what a specific window exports:
  an installed Chromium exports DBusMenu, windows without an export keep the shell menu.
- **Notification Center**: an `org.freedesktop.Notifications` server, history, Do Not Disturb.
- **Control Center on real data**: NetworkManager (Wi-Fi), BlueZ, PipeWire (sound),
  brightness (logind `SetBrightness`), battery (UPower). Everything is asynchronous, with no loop blocking.
  Wi-Fi and sound are implemented, including Personal/SAE passwords and outputs. Enterprise/hidden/WEP
  use nm-connection-editor. BlueZ and brightness/battery are the next integrations;
  current limitations — [CONTROL_CENTER.md](CONTROL_CENTER.md).
- **Dock**: pinning and unpinning (context menu), dragging, launch bounce, an attention
  indicator via `xdg_activation`.
- Hot corners, Stage Manager (optional).

### Stage 4 — own DRM/KMS backend

**A first version exists** (`src/drm_backend.rs`, `src/cursor.rs`, `session/start-feather-drm.sh`):
libseat (logind) → udev (primary GPU) → GBM/EGL → `DrmOutputManager`, the first connected
monitor in its preferred mode, frames on vblank, libinput (tap and "natural" scrolling on a
touchpad), own cursor from xcursor on a hardware plane, Ctrl+Alt+F1…F12, emergency exit
Ctrl+Alt+Backspace, pause/resume on VT switch. Enabled with the `--drm` flag or automatically
when the compositor is not started inside another desktop.
Log: `~/.local/state/feather-de/drm.log`.

Not done yet: several monitors and hot-plugging, choice of mode/refresh rate, VRR,
explicit sync, XWayland (X11 programs will not start in a DRM session). Rendering to a deadline
before vblank is implemented (2026-10-30); DRM acceptance is open.

Original plan:
- Drop labwc: udev + libseat + libinput + DRM/GBM, direct scanout of fullscreen windows,
  VRR, several monitors, hot-plugging, explicit sync.
- *Done when* input → photon latency is lower than in nested mode and direct scanout
  works for video and games. This is the biggest remaining latency win: right now every frame
  passes through a second compositor.

### Stage 5 — ecosystem
- A "System Settings" application (theme, wallpapers, Dock, keyboard, displays, hotkeys).
- Custom wallpapers (images), dynamic day/night wallpapers.
- Hot reload of configuration without a restart.

## 6. Testing and profiling

- **Nested run:** `target/release/feather-de --windowed`. From a Feather session itself the
  test instance ends up nested inside it and gets software rendering.
  For honest measurements run it on the host (`WAYLAND_DISPLAY=wayland-0` under labwc).
- **Profiler:** `FEATHER_PROFILE=1` prints every 2 s frames/s, frames with changes,
  client commits, CPU rasterisation, scene+GL, the worst frame and swap time.
- **Visual tests without input emulation:** `FEATHER_DEBUG_SCREENSHOT=/tmp/x.png` saves the
  compositor's own frame after 2.5 s (not the host's screen — `grim` captures what is visible,
  and under a tiling WM it is easy to photograph someone else's window).
  `FEATHER_DEBUG_OPEN=launchpad` opens Launchpad at startup.
- **Input latency:** `FEATHER_PROFILE=1` prints "key → frame" — from the key press to sending
  the frame with the application's response to the host (without the host's own and the monitor's latency).
- **Protocol:** `WAYLAND_DEBUG=1 <client>` — checking focus (`wl_keyboard.enter`),
  decoration mode, configure.
- **Reference test:** `chromium --ozone-platform=wayland --app=file://…/bench/anim.html`
  prints FPS to the console (`--enable-logging=stderr`). It must equal the display refresh rate.
- **Regressions that already happened and must not return:**
  - focus on a window without a buffer (an invisible window captures clicks);
  - forced SSD on top of CSD ("a window inside a window");
  - a lost button release → the client stays in an implicit grab forever (dead clicks);
  - `xdg_toplevel.move` without `wl_pointer.leave` (dead Electron drag areas);
  - a blocking swap (a browser at ~35 FPS instead of 60);
  - dmabuf without feedback (Mesa clients fall back to software rendering);
  - `under_from_surface_tree` was given a relative point instead of a global one: the function
    subtracts `location` itself, so hit-testing shifted by the window coordinates and the top/left
    of windows could not be clicked. Check: `hyprctl dispatch movecursor` along a window edge, and in the
    client's `WAYLAND_DEBUG` `wl_pointer.enter` must arrive with y ≈ 0;
  - a 24 px host cursor at 4K: `XCURSOR_SIZE` is set in `start-feather-de.sh` according to the resolution;
  - a low-resolution buffer stretched over the screen (`size` without `src` in
    `MemoryRenderBufferRenderElement`): Smithay takes `src` = the on-screen size and reads beyond
    the texture — wallpapers showed only a corner with stripes.

## 7. Session and development

- Session: `session/feather-de.desktop` → `start-feather-de.sh` → labwc (kiosk) →
  `feather-session-inner.sh` → `feather-de --standalone` in a loop.
- `session/reload-binary.sh` rebuilds and restarts the compositor. **All windows will close**,
  including Claude Desktop, if you work from this same session.
- Settings: `~/.config/feather-de/settings.conf` (appearance, wallpaper, scale, dock_zoom,
  animations, keyboard_layouts, keyboard_shortcut, display, wallpaper_image, default_browser,
  default_terminal). The first UI version and the limits of its
  verification — [SYSTEM_SETTINGS.md](SYSTEM_SETTINGS.md).
- Scale: `--scale 1.5` or `FEATHER_SCALE`, otherwise automatic by resolution.
