# Feather DE (Пёрышко) — Changelog

**[🇷🇺 Читать на русском](CHANGELOG.md)** · **[← Back to README](README.md)** · **[Roadmap](docs/ROADMAP.en.md)**

This is a condensed English history of every release, newest first: what users get in each
version. The full day-by-day development log, with implementation details and verification
notes, is kept in Russian in [CHANGELOG.md](CHANGELOG.md).

Installed systems get every release through `sudo apt update && sudo apt upgrade` (Ubuntu) or
`sudo pacman -Syu` (Arch). After an upgrade, log out and log in again.

## 0.26.2 — 2026-11-21

- **Multi-monitor across hybrid/dual GPUs (AMD + NVIDIA, [#3](https://github.com/Zumka1991/feather-de/issues/3)):**
  each secondary GPU now runs its own EGL/GLES renderer and native scanout swapchain instead of
  having the primary GPU render directly into foreign VRAM. Fixes both the `SIGABRT`
  (`amdgpu: The CS has been rejected (-12)`) crash on AMD-primary + NVIDIA-secondary laptops and
  the `NoSupportedRendererFormat` failure in the reverse order. Frames transfer via verified
  GPU DMA-BUF sampling (`GL_TEXTURE_EXTERNAL_OES` + native fence sync) with automatic fallback to
  damage-rect CPU copy (`FEATHER_MULTIGPU_COPY=auto|dma|cpu`).
- No new package dependencies.

## 0.26.1 — 2026-11-21

- **Bluetooth headphones & accessories stay paired across reboots:** Feather now registers a
  built-in `org.bluez.Agent1` pairing agent (`/org/feather/BluetoothAgent`) and keeps powered
  adapters in `Pairable = true` (`Bondable`) and `Connectable = true` modes so BlueZ stores
  persistent link keys (`Bonded = true`) on disk instead of an ephemeral session-only pair, and
  trusted headphones reconnect automatically when taken out of their case.
- No new package dependencies.

## 0.26.0 — 2026-11-20

- **Create folders in the file/directory picker:** a new "New Folder" button on the bottom-left
  of the dialog (also available via `Ctrl+Shift+N` and the right-click menu on empty space)
  opens a macOS Tahoe-style modal sheet, validates the name, and automatically navigates inside
  the newly created folder — handy when creating new projects in IDEs or saving files.
- **Smooth, stutter-free Preview (~20× faster zoomed panning):** panning zoomed images now uses
  a clipped 14-bit integer/SSE2 sampler, an incremental viewport shift buffer, and a cached
  window-corner AA mask (`114–129 ms` → `3–6 ms` per frame at 2880×1800 ×2), plus cursor-anchored
  zoom (`Ctrl + wheel`, double-click), 2D touchpad scrolling, and arrow-key panning.
- **DRM GPU selection & startup diagnostics:** `FEATHER_DRM_DEVICE` selects a specific DRM card
  (including `/dev/dri/by-path` symlinks), and startup stages are logged before driver calls.
- No new package dependencies.

## 0.25.2 — 2026-11-18

- **File picker in the dark theme:** the Cancel button, file-name field, type filter and
  choices no longer appear white-on-white; their background follows the theme.
- **Sorting in the dialog:** a "Sort" row in the bottom panel cycles name, date, size and
  kind plus direction; folders stay on top.
- No new package dependencies.

## 0.25.1 — 2026-11-16

- **Installation in English.** The one-line installer, the Arch installer, `setup-desktop` and the
  post-install hints print English; under a Russian locale (`LANG=ru_*`) they stay Russian.
- **Package descriptions** (`apt show feather-de`, pacman) and application names in menus are
  English by default (Russian under a Russian locale). The login-screen session is called
  "Feather (direct DRM/KMS output)".
- **`featherctl`** help and messages are in English (the commands are unchanged).
- No new package dependencies.

## 0.25.0 — 2026-11-16

- **English is now the default interface language** and comes first in the language lists
  (first-run wizard and System Settings). A language you have already chosen is not changed.
- **More complete translations** in Files, System Settings and the shell: "Cut", "Open With…",
  view and sort menus, dialogs, status messages ("Cut: 3 items", "Copying “a” to “b”"), sizes
  with the right decimal separator, wallpapers, keyboard and notification settings, network
  errors, the calendar. Checked menu items are translated too.
- Still Russian only: the `featherctl` command-line tool, the demo text of the text editor and
  log messages.

## 0.24.2 — 2026-11-15

- White icons in the menu bar on the dark theme: the menu bar is always dark with white text and
  icons whatever the wallpaper, instead of turning black under a light sky.

## 0.24.1 — 2026-11-15

- A more expressive Dock on the dark theme: the plates under Chrome, Firefox, Telegram and similar
  icons are darker than the Dock glass with a bright edge highlight, the icons are larger on
  them, square icons are rounded like the rest, and a soft shadow sits under the icons.

## 0.24.0 — 2026-11-15

- Complete silence in Do Not Disturb: a new "Urgent notifications" switch in System Settings →
  Notifications. Turn it off and no banner appears in Do Not Disturb, even an urgent one;
  everything quietly collects in the Notification Center. On by default (behaviour unchanged).

## 0.23.1 — 2026-11-15

- **Arch Linux: one command installs everything.** The `install.sh` one-liner now works on Arch
  and derivatives: it installs Feather, the app set (Firefox, LibreOffice, video, music,
  printing, codecs, Flatpak for the Store), interface locales, and on a system without a login
  screen — the Feather login screen. Feather only: `… | bash -s -- --minimal`.
- Konsole and Dolphin on a KDE system no longer merge with Feather's Terminal and Files.

## 0.23.0 — 2026-11-15

- Monitors on a second graphics card (a hybrid laptop with HDMI on the discrete card, a docking
  station) are detected and work as ordinary screens. Not yet tested on real two-GPU hardware;
  `FEATHER_SINGLE_GPU=1` restores the old behaviour.
- The "Russian — typewriter" keyboard layout (System Settings → Keyboard → Add).

## 0.22.0 — 2026-11-14

- Tray icons share one style: app icons in the menu bar are made monochrome to match the panel
  text, as in macOS; coloured marks (a red unread dot) are kept. Original colours can be
  restored in System Settings.
- The menu bar no longer turns light over a maximised light window.
- Steam game icons in the Dock instead of a generic icon.

## 0.21.0 — 2026-11-14

- **Animation speed:** a new slider in System Settings → Appearance, from 0.5× to 2×, speeds up
  or slows down all Feather animations together.
- The lock screen on Astra Linux accepts the correct password again (the `feather-lock` PAM
  stack no longer includes a non-existent `system-auth`).

## 0.20.8 — 2026-11-14

- Mouse wheel and touchpad scrolling in the modal pick lists of System Settings (time zone,
  autostart apps, default apps).
- Files: pick any application, or type your own command, in "Open With…".
- A smarter contrast algorithm for the menu bar and its menus over any wallpaper.

## 0.20.7 — 2026-11-14

- The Drop Shelf remembers its position.
- Moving a window to the Scratchpad with `Super + Shift + S` slides the Scratchpad out at once.
- Open Terminal windows follow a theme change live.
- A neutral bash prompt on a fresh Ubuntu.
- Files: undo with `Ctrl + Z`.
- Fullscreen for any app with `Cmd + F` (`Super + F`).
- The menu bar menu takes on the tint of the wallpaper-based bar colour.
- Fixes for Steam `DISPLAY`, `linux-drm-syncobj` and black Zen/Firefox windows (a community
  pull request).

## 0.20.6 — 2026-11-13

- A tidier Control Center: cards with a highlighted edge, round toggle pills, sliders with a
  knob, cover art on the "Now Playing" card; "Do Not Disturb" and "Dark Mode" labels are no
  longer cut off. Wi‑Fi, Bluetooth and Sound pages get a back button, arc signal strength,
  device-type icons with a battery and a checkmark on the active item.
- Menu bar: new speaker and Do Not Disturb icons; icons no longer shift when the minute changes.

## 0.20.5 — 2026-11-13

- Security: caller authorisation on the private and control D-Bus interfaces. Only the real
  portal backend can use `org.feather.ScreenCast1`, only `xdg-desktop-portal` can call the
  portal backend, and `org.feather.Shell1` rejects calls from sandboxes (Flatpak, Snap,
  `xdg-dbus-proxy`) and respects the screen-capture ban.

## 0.20.4 — 2026-11-13

- Flatpak and Snap app icons in the Feather shell (including apps installed from Flathub, such
  as Zen Browser).

## 0.20.3 — 2026-11-12

- **Flathub in the built-in Store:** one-click connection, a "System package / Flathub" choice
  for programs available from both, installing, launching, removing and updates.
- A built-in interactive "Feather Keyboard Shortcuts" cheat-sheet window.

## 0.20.2 — 2026-11-12

- A central glass HUD plate when switching workspaces: the number, page dots and the current
  layout mode.

## 0.20.1 — 2026-11-12

- Direct workspace switching with `Cmd + 1…9`; a missing space is created on the fly and empty
  trailing spaces collapse when you leave.
- Moving the active window to a space with `Cmd + Shift + 1…9`, following it.
- Screen Recording moved to `⌥⌘5`.

## 0.20.0 — 2026-11-12

- **Workspace modes (`Ctrl + Alt + M`):** each space works independently as free windows,
  automatic tiling with draggable dividers, or an infinite scrollable ribbon in the style of
  PaperWM / Niri.
- **Live Liquid Glass on the GPU:** panels, Dock, menus, Spotlight, Control Center and
  Notification Center refract and blur the live picture under them.
- **Drop Shelf:** a shelf for dragged files, images and text, summoned by shaking the cursor.
- **Menu command search (`Super + /`):** an instant palette of the active window's menu commands.
- **Picture in Picture and Ghost PiP** (`Super + P` / `Super + Alt + P`).
- **A per-app audio mixer:** volume, mute and output device for each program.

## 0.19.10 — 2026-11-11

- Full control of Scratchpad tiles: navigation, swapping, live divider resizing from the
  keyboard and mouse, 50/50 equalising and fullscreen for a tile.
- A fix for the resize cursor disappearing at window borders, including in DRM.

## 0.19.9 — 2026-11-11

- Session logs no longer grow without limit; the global menu of Qt apps no longer accumulates
  closed windows; scrolling dense lists on 4K in built-in apps can no longer hang.

## 0.19.8 — 2026-11-11

- A noticeably faster, smoother desktop: no idle render passes on captured-mouse motion in
  games, key presses and trackpad scrolling.
- Less memory and bandwidth on HiDPI/4K: notifications and Mission Control labels use compact
  buffers.

## 0.19.7 — 2026-11-11

- **Arch Linux:** our own pacman repository `[feather-de]` with updates via `sudo pacman -Syu`
  and one-command installation.

## 0.19.6 — 2026-11-10

- Downloading files from Codex Desktop and other Electron programs: the save dialog opens even
  if the program passed only a file name.

## 0.19.5 — 2026-11-10

- Windows are easier to resize: wider grab zones on edges and corners, the same behaviour for
  all windows including those with their own frames, min/max sizes respected.

## 0.19.4 — 2026-11-10

- Fixed a whole-desktop crash when hovering a Dock icon whose name is wider than the panel.
- Files, Preview, Text Editor, Monitor, Store, Notes and Calculator report only the changed
  part of the window: fewer stalls, especially in maximised 4K windows.

## 0.19.3 — 2026-11-10

- Screen Recording: fixed a green screen with artefacts in HEVC video (4K on AMD cards).
- Notifications can be closed with the cross over the Scratchpad too.

## 0.19.2 — 2026-11-09

- Screen Recording without freezes: the graphics card encodes the video (VAAPI H.264, HEVC for
  4K), correct colours (BT.709), microphone in sync, a keyframe every 2 s.

## 0.19.1 — 2026-11-09

- The app ring no longer reorders programs: each has a fixed place like in the Dock. Switching
  only by an explicit action: a click or Enter.

## 0.19.0 — 2026-11-09

- **The app ring:** a short press of Cmd (Super) lays out the open programs in a circle around
  the cursor. "Cmd, Cmd" goes back to the previous program; choose with the mouse, arrows, Tab,
  the wheel or the first letter. Can be turned off in System Settings → Keyboard.

## 0.18.0 — 2026-11-09

- GTK 3/4 apps without their own header (dialogs, simple windows) get the same Feather title
  bar with traffic lights as Qt and built-in windows, including the tiling menu on the green
  button. GTK snap apps get the macOS-style buttons too.

## 0.17.1 — 2026-11-09

- Caps Lock can be chosen as the language switch; it works like in macOS: a tap switches the
  language, holding turns capitals on.

## 0.17.0 — 2026-11-08

- Files: window buttons in the title bar and the folder sidebar as a separate card below.
- Wallpapers 2.0: a preview monitor with the real picture, fill/fit/stretch scaling,
  auto-change, all gradients as separate tiles.
- Five new wallpapers: Waves, Dunes, Ridges, Emerald, Graphite.

## 0.16.0 — 2026-11-08

- Ilya Birman's typographic keyboard layout on the right Alt.
- Flatpak and Snap browsers (such as Zen) can be the default browser.
- A clear notification if xwayland-satellite is missing.

## 0.15.1 — 2026-11-08

- A Terminal with a single tab has no tab bar; a second tab opens with a tidy "+" in the title bar.

## 0.15.0 — 2026-11-07

- macOS Tahoe-style Terminal tabs; a blue dot marks a background tab with new output.
- The Caffeine icon is a cup on a saucer, no longer confused with the battery.

## 0.14.0 — 2026-11-06

- Files open and extract RAR archives themselves (RAR4, RAR5, multi-part, password-protected).
- Files: extracting all archives and "Compress" work again in a real window.
- Chrome and other Chromium windows no longer leave a gap above the Dock when they open maximised.

## 0.13.1 — 2026-11-07

- The Scratchpad (`Super + S`) opens faster and no longer stutters if Alt+Tab follows at once.

## 0.13.0 — 2026-11-06

- **The Scratchpad:** a layer of windows above all spaces. `Super + S` shows it,
  `Super + Shift + S` moves the focused window there and back.

## 0.12.0 — 2026-11-06

- Window tiling to halves and quarters (edges, the green-button menu, the Window menu,
  `Ctrl + Super + arrows`).
- Windows reopen where they were and at their old size; the previous session returns at login.
- Spotlight searches open windows, settings sections, notes and home-folder files.

## 0.11.1 — 2026-11-03

- Larvis can change the Dock position through the desktop API.

## 0.11.0 — 2026-11-03

- `FEATHER_AUTOMATION=1` enables window screenshot, click, scroll, keyboard and text-paste
  methods of the desktop API for application automation.

## 0.10.0 — 2026-11-03

- **The `org.feather.Shell1` desktop API** on the D-Bus session bus (windows, spaces, Mission
  Control and Launchpad, apps, sound, brightness, night mode, dark theme, Do Not Disturb, media,
  screenshots, lock and sleep) and the `featherctl` tool. The voice assistant Larvis drives the
  desktop through it.

## 0.9.1 — 2026-11-02

- Clipboard: `ext-data-control` and `wlr-data-control` support (wl-copy/wl-paste, clipboard
  managers, images pasted into other Wayland programs).
- Files: text selection by dragging in rename, search and path fields.

## 0.9.0 — 2026-11-01

- A task manager, **System Monitor**, on `Ctrl + Alt + Del`: processes with CPU and memory,
  sorting, search, "Quit Process" and CPU/memory/disk/network graphs.
- Lock, screen blanking and sleep can be postponed for 2 or 5 hours.

## 0.8.1 — 2026-10-31

- Files show extraction progress with an "X of Y — about N min left" card and a cancel button.

## 0.8.0 — 2026-10-31

- Files extract all common archives (single gz, xz, bz2, zst, cab, arj, lzh, cpio, ace);
  Windows zips with Russian names extract correctly; an archive opened from another program is
  extracted next to it at once.

## 0.7.1 — 2026-10-31

- On the dark Dock, round and non-square icons (for example Chrome) no longer blend into the background.

## 0.7.0 — 2026-10-31

- Files see all disks and flash drives, not only mounted ones: a click mounts, ⏏ safely ejects.
- A "Now Playing" tile in Control Center: track, artist, ⏮ ⏯ ⏭.

## 0.6.3 — 2026-10-31

- Files extract RAR archives, including RAR5 and multi-part ones; encrypted RAR, zip and 7z ask
  for a password; zip with a password no longer hangs.

## 0.6.2 — 2026-10-31

- Autostart programs start after you enter your password, not on the login screen.
- If a program does not open, the reason stays in `~/.local/state/feather-de/apps.log`.

## 0.6.1 — 2026-10-30

- After logging out and in again, Telegram, MouseVPN and other single-instance programs start
  again: on logout Feather closes the programs it started.

## 0.6.0 — 2026-10-30

- **The Store:** a new app in the Dock and Launchpad with a featured selection, categories,
  search and program pages. Install and remove in one click.

## 0.5.0 — 2026-10-30

- Minimised windows, windows on other spaces and windows under a fullscreen game are no longer
  drawn needlessly at the display rate: a browser with a video in the Dock no longer loads the
  CPU and GPU.
- Dock icon magnification on hover is done by the GPU.

## 0.4.2 — 2026-10-30

- Launching Codex, Antigravity, Chromium and other Electron apps no longer blocks the screen.

## 0.4.1 — 2026-10-30

- The icon of a just-installed program appears in the Dock and Launchpad at once.
- Files: the context menu widens for long items, and a too-long name is ellipsised.

## 0.4.0 — 2026-10-30

- Files: "Open With…" in a file's context menu, listing the programs that can open that type.

## 0.3.2 — 2026-10-30

- Files: rename by a slow click on a selected name (like Finder) and Ctrl+C/X/V in the name,
  path and search fields, including in the Russian layout.

## 0.3.1 — 2026-10-30

- One-command installation; the `.deb` adds the repository itself; the `feather-desktop`
  metapackage with programs like Ubuntu Desktop; xwayland-satellite inside the package; running
  AppImage from Files.

## 0.3.0 — 2026-10-30

- In the APT repository: native Wayland Electron/Chromium, bicubic scaling of legacy programs,
  `.deb` from the browser, a compatibility check in the installer, an archiver in Files,
  Ctrl+letter in the Russian layout.

## 0.2.0 — 2026-10-29

- The first version in the APT repository `https://zumka1991.github.io/feather-de`.
  Repository signing key: `D5E6 C262 8719 EF1D 6068 5758 C7F9 44C1 485C 5FC4`.
