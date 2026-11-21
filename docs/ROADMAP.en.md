# Feather DE (Пёрышко) Roadmap

**[🇷🇺 Читать на русском](ROADMAP.md)** · **[← Back to README](../README.md)** · **[Specification](SPEC.en.md)**

> The checklist is maintained in Russian ([ROADMAP.md](ROADMAP.md)); this is its English translation.
> Linked technical reports (`docs/*.md`) are in Russian.

A single checklist. After each task tick the item (`[x]`) and, if needed, add a link to the
report. An item counts as done only after verification and without breaking the budgets in
[SPEC.en.md](SPEC.en.md) §2; mark "verified by the user in DRM" separately.
Details of what is already done — [CHANGELOG.en.md](../CHANGELOG.en.md). What is left to check on a real
computer — [DRM_CHECKLIST.md](DRM_CHECKLIST.md).

## Done — the basic shell

- [x] Compositor on Smithay: nested mode and direct DRM/KMS (first monitor, libinput, cursor on a hardware plane)
- [x] Login-screen sessions: "Пёрышко DE" (labwc kiosk) and "Пёрышко (direct output)"
- [x] Menu bar: application menu, global Qt/KDE menu (DBusMenu), SNI tray, clock, input sources
- [x] Tray: fixed a D-Bus hang on an event burst while loading icons; the last snapshot is kept when an update fails — [TRAY.md](TRAY.md#исправление-зависания-2026-10-26)
- [x] Dock: pinning, dragging, magnification, launch bounce, minimised windows, "Files" permanently
- [x] Trash in the Dock: empty/full, emptying with confirmation, dragging, "Put Back" in Files — [TRASH.md](TRASH.md)
- [x] Launchpad with search (Feather apps first), Spotlight, Alt+Tab with the mouse
- [x] Control Center: Wi-Fi (NetworkManager), sound (PipeWire), theme, scale — [CONTROL_CENTER.md](CONTROL_CENTER.md)
  - [x] Sound: 4–6 outputs without scrolling, a scroll bar; automatic switch to connected headphones
- [x] Spring animations, glass and rounded corners
- [x] Desktop: icons, dragging, menu, soft label shadows
- [x] Tahoe-style System Settings: appearance, desktop and Dock, displays (modes of the first output), keyboard, mouse and trackpad, default apps, date and time — [SYSTEM_SETTINGS.md](SYSTEM_SETTINGS.md)
- [x] Wallpapers: a gallery and your own images — [WALLPAPERS.md](WALLPAPERS.md)
  - [x] Wallpapers 2.0: a preview monitor with the real picture and its properties, scaling (fill/fit/stretch), auto-change (login/30 min/hour/day, in order or random), separate gradient tiles
  - [x] Five new procedural wallpapers: "Waves", "Dunes", "Ridges", "Emerald", "Graphite" (`tools/wallpaper-gen`)
- [x] Own polkit agent with a password window in the compositor — [POLKIT.md](POLKIT.md)
- [x] XDG Desktop Portal FileChooser through Files
- [x] ScreenCast portal: our own PipeWire stream with a monitor (OBS, browsers, calls)
- [ ] ScreenCast: capturing a single window, a monitor picker, a sharing indicator in the menu bar
- [x] Apps: Files, Terminal, Notes — [FILES.md](FILES.md), [TERMINAL.md](TERMINAL.md), [NOTES.md](NOTES.md)
  - [x] Terminal: macOS Tahoe-style tab bar, an activity dot for a background tab
  - [x] Files: window buttons moved from the sidebar into the title bar, the sidebar is a separate card below
- [x] Hover highlight: settings, Control Center, password/confirmation windows
- [x] Polish of the menu bar and Control Center: cards with an edge, toggle pills, sliders with a knob, a single icon set, Wi‑Fi/Bluetooth/Sound pages — screenshots in `docs/images/cc-polish/`
- [x] "Show at login" for the "About This Computer" window

## Interface details

- [x] Dock: explicit "Keep in Dock / Remove from Dock" via right click, an unpinned app disappears after its last window, protection from accidental pinning — [DOCK_PINS.md](DOCK_PINS.md)
- [x] Dock: persistent lock for dragging into the Dock, out of the Dock and reordering via right click — [DOCK_PINS.md](DOCK_PINS.md)
- [x] Dock: "Files" keeps the chosen position instead of being forced to first place — [DOCK_PINS.md](DOCK_PINS.md)
- [x] Dock: centring of slightly rectangular icons (the terminal is no longer pressed to the top edge)
- [x] Dock: the launch bounce is always upward, including left and right placement — [DOCK_RESIZE_CHECKS.md](DOCK_RESIZE_CHECKS.md#направление-отскока-2026-10-26)
- [x] Dock: automatic proportional shrinking of a huge Dock to the screen size (at rest and with magnification on hover, at the bottom and at the side without running into the menu bar), as in macOS
- [x] Wallpapers: fixed `No such file or directory` after a rebuild inside a running session — [WALLPAPERS.md](WALLPAPERS.md)
- [x] Input fields — Ctrl+A, cursor anywhere, selection (`feather_ui::input::TextInput`) — [TEXT_INPUTS.md](TEXT_INPUTS.md)
  - [x] Launchpad search
  - [x] Spotlight
  - [x] System Settings fields (time zone, date and time)
  - [x] Wi-Fi password in Control Center
  - [x] Files: search and rename
  - [x] Notes: search
- [x] Files: entering a path (Ctrl+L / ⇧⌘G)
- [x] Files: "Get Info" (size, dates, permissions, kind)
- [x] Files: Quick Look on Space (images, text, cards for other files) — [QUICK_LOOK.md](QUICK_LOOK.md)
- [x] Files: Ctrl shortcuts in any layout; copying with progress, cancellation and a "Replace / Keep Both / Skip" question
- [x] Files: "Open With…" in a file's context menu (programs from .desktop by MIME type)
- [x] Files: extracting RAR via unar (RAR5, multi-part `.partN.rar`), clear errors — [FILES.md](FILES.md)
- [x] Files: password entry for encrypted archives (RAR, zip, 7z) — [FILES.md](FILES.md)
- [x] Files: disks and flash drives in "Places" — mounting on click via udisks, an "Eject" button, refresh on plug-in — [FILES.md](FILES.md)
- [x] Files: RAR with our own decoder (`rars`, a separate process): RAR4 and RAR5, solid, `.partN.rar` and `.rNN` volumes, encrypted data and names, password via stdin; fixed extraction of all archives in a real window (the `SIGCHLD` race) — [FILES.md](FILES.md)
- [ ] Files: do not pass a zip and 7z password in arguments (currently `--passphrase` for bsdtar and `-p` for unar) — [FILES.md](FILES.md)
- [x] Files: all common archives (single gz/xz/bz2/zst, cab, arj, lzh, cpio, ace), Windows zip with Russian names, extracting archives opened from other programs — [FILES.md](FILES.md)
- [x] Files: an extraction progress card with cancellation (like copying) — [FILES.md](FILES.md)
- [x] Files: "Sort By" name, kind, date and size — menu and column headers — [FILES.md](FILES.md)
- [x] Dark theme in Files and Notes, open windows refresh on a theme change — [CLIENT_APPEARANCE.md](CLIENT_APPEARANCE.md)

## Security and power

- [x] Lock screen (ext-session-lock, password via PAM), Super+L, lock on wake — [LOCK_POWER.md](LOCK_POWER.md)
- [x] Screen blanking and sleep on idle (idle-notify, idle-inhibit), logind; timeouts up to 5 hours — [LOCK_POWER.md](LOCK_POWER.md)
- [x] The `🪶` menu: "Sleep", "Restart…", "Shut Down…" (with a macOS-style modal confirmation), "Lock Screen" (`⌘L`) and power management via `org.freedesktop.login1.Manager`
- [x] System Settings → "Lock Screen & Energy Saver" — [LOCK_POWER.md](LOCK_POWER.md)
- [x] Sender authorisation on the D-Bus session bus: `org.feather.ScreenCast1` accepts calls only from our own `feather-de --file-portal` (`org.freedesktop.impl.portal.desktop.feather`), `org.freedesktop.impl.portal.*` — only from `xdg-desktop-portal` (`org.freedesktop.portal.Desktop`), and `org.feather.Shell1` blocks calls from isolated sandboxes (Flatpak, `xdg-dbus-proxy`, Snap) and respects the policy forbidding screen capture — [API.md](API.md)

The implementation was verified in nested mode and on a separate test bus.
Manual acceptance of real PAM, DPMS and suspend/resume in DRM is still up to the user;
see the [verification order](LOCK_POWER.md#проверки). The overall acceptance of the SPEC budgets stays open.

## Notifications

- [x] `org.freedesktop.Notifications` server, macOS-style banners, actions — [NOTIFICATIONS.md](NOTIFICATIONS.md)
- [x] Notification Center (history, groups by app), persistent Do Not Disturb — [NOTIFICATIONS.md](NOTIFICATIONS.md)
- [x] Per-app notification settings (System Settings → Notifications)
- [x] Notification sounds (`sound-file`, `sound-name`, `suppress-sound`, a toggle in settings)
- [x] Do Not Disturb schedule — [NOTIFICATIONS.md](NOTIFICATIONS.md)

## Compatibility

- [x] XWayland (running X11 programs) through xwayland-satellite on demand — [XWAYLAND.md](XWAYLAND.md)
- [x] Wine/XWayland: persistent HiDPI scale with a setting in Displays — [GAMING.md](GAMING.md)
- [x] Games: relative pointer, lock/confine, a single `wl_pointer.frame` (`POINTER_NORAW`), holding `x11_warp_lock` while rotating the camera with the right button in Wine/XWayland, releasing the grab, releasing buttons and clearing `forwarded_pressed_keys` / modifiers on focus change — [GAMING.md](GAMING.md)
- [x] Proton Dock: immediate title/app_id, separate games and filtering out an unnamed service window — [GAMING.md](GAMING.md)
- [x] XWayland resize with CSD: the outer edge over the window below, `Resizing`, the real size of the X11 client — [GAMING.md](GAMING.md)
- [x] Uniform resize of all windows: a 10+5 px (15 px) band, 24 px corners + 7 px inside the rounding, keeping the resize cursor when leaving the client surface, preloading corner cursors in DRM, client min/max, stopping at the menu bar — `FEATHER_DEBUG_RESIZE=1`, [WINDOWS.md](WINDOWS.md#ресайз-окон-srcwindowrs-srcinputrs-srchandlersrs)
- [x] Lineage II in DRM: scale/UI, right button and camera, resize and switching from the Dock after logging in again (verified by the user)
- [x] GTK global menu (`org.gtk.Menus` / `org.gtk.Actions`, `gtk_shell1`) — [APPMENU.md](APPMENU.md#gtk)
- [x] Uniform traffic lights: KDE server-decoration for GTK 3/4 without a HeaderBar (our title bar), `gtk-3.0/gtk.css` for snap — [DECORATIONS.md](DECORATIONS.md)
  - [ ] Chrome, Firefox, Electron and Telegram draw their own buttons (on the left); toggles are in the programs themselves — [DECORATIONS.md](DECORATIONS.md#кто-остаётся-со-своими-кнопками-и-почему)
- [x] Protocols for third-party panels and input methods: layer-shell, foreign-toplevel (wlr and ext), text-input, input-method, virtual-keyboard — [PROTOCOLS.md](PROTOCOLS.md)
  - [ ] Testing with real waybar, rofi, fcitx5 and ibus
- [x] Clipboard without window focus: `ext-data-control-v1` and `zwlr-data-control-v1` (wl-copy/wl-paste, clipboard managers, arboard; images no longer go to the X11 clipboard)

## Windows and workspaces

- [x] Workspaces (Spaces): Ctrl+Alt+←/→, direct switching `Super+1…9` and moving a window with `Super+Shift+1…9` creating spaces on the fly, a central HUD plate with the space number and mode when switching, animation — [WINDOWS.md](WINDOWS.md#рабочие-столы-srcspacesrs-шаг-1)
- [x] Workspace modes (`Free` / `Tile` / `Tape`): an independent mode for each space — free macOS Tahoe windows, automatic BSP tiling and a scrollable horizontal ribbon in the spirit of Niri (`Ctrl+Alt+M`, a pill in the menu bar and in Mission Control, Spotlight, the "Window" menu, `featherctl space-mode`, saved in `windows.conf`) — [WINDOWS.md](WINDOWS.md#режимы-рабочих-столов-srcspace_layoutrs), [API.md](API.md)
- [x] Mission Control: overview of windows and spaces, moving a window to a space — [WINDOWS.md](WINDOWS.md#mission-control-srcmissionrs-шаг-2)
- [x] Trackpad gestures: spaces, Mission Control and Launchpad (`GestureSwipe` / `GesturePinch` with 3–4 fingers; a physical trackpad in DRM is verified by the user)
- [x] Alt+Tab: MRU across spaces, the Alt+Shift conflict, mouse selection strictly on click (no capture on hover) and cancelling — [SWITCHER.md](SWITCHER.md)
- [x] Switching between windows of one application (Alt+`) — [WINDOWS.md](WINDOWS.md)
- [x] The app ring on a short press of Cmd (Super): mouse gesture, arrows, Tab, a letter, "Cmd, Cmd" — back; does not interfere with shortcuts — [RING.md](RING.md) (a physical keyboard in DRM — verified by the user)
- [x] Hot corners — [WINDOWS.md](WINDOWS.md#активные-углы-srchot_cornersrs)

## Monitors

- [ ] Several monitors, hot-plugging, arrangement and a primary monitor — [MONITORS.md](MONITORS.md)
  - [x] Implementation for outputs of one GPU, arrangement/primary settings, moving windows, a shared scale
  - [x] Virtual outputs: a real window, Wayland events, disconnect/return, locking all screens
  - [ ] Physical KMS/hotplug/VT and budgets on several monitors (there is no second monitor to test with)
  - [x] Monitors on a second graphics card (hybrid graphics, docking stations) — [MONITORS.md](MONITORS.md#несколько-видеокарт--2026-11-15--2026-11-21)
    - [x] Per-GPU EGL/GLES renderer and native scanout swapchain for secondary cards, verified GPU DMA-BUF copy (`GL_TEXTURE_EXTERNAL_OES` + native fence sync), and damage-rect CPU copy fallback (`FEATHER_MULTIGPU_COPY=auto|dma|cpu`) — fix for [#3](https://github.com/Zumka1991/feather-de/issues/3)
    - [ ] Testing on physical dual-GPU hardware (AMD + NVIDIA from [#3](https://github.com/Zumka1991/feather-de/issues/3))
    - [x] Manual DRM card selection (`FEATHER_DRM_DEVICE`) and startup stage diagnostics for [#3](https://github.com/Zumka1991/feather-de/issues/3)
- [x] VRR (`vrr_supported` / `use_vrr` / `FEATHER_VRR`), explicit sync (`wp_linux_drm_syncobj_v1`), direct scanout (`ALLOW_PRIMARY_PLANE_SCANOUT_ANY`)
- [x] Night mode (warm colours on a schedule, DRM gamma) — [DISPLAY_COLOR.md](DISPLAY_COLOR.md)
- [x] Brightness: logind/backlight and DDC/CI of external monitors — [DISPLAY_COLOR.md](DISPLAY_COLOR.md#яркость-srcbrightnessrs)

## System Settings — new pages

- [x] Sound: microphone choice, input level and mute; an inputs/outputs page — [MICROPHONE.md](MICROPHONE.md)
  The implementation was verified in nested mode; physical recording/mute and keeping the choice in DRM — manual acceptance.
- [x] Sound: alert and interface sounds — [SYSTEM_SOUNDS.md](SYSTEM_SOUNDS.md)
  The implementation, settings and nested checks are ready; the audible result in DRM — manual acceptance.
- [x] Network: saved Wi-Fi, Ethernet, VPN (NetworkManager) — [NETWORK.md](NETWORK.md)
- [x] Bluetooth (BlueZ): turning on, scanning, connecting — [BLUETOOTH.md](BLUETOOTH.md)
- [x] Bluetooth: "Forget" for any remembered device (including one with a lost pairing) and cancelling a hung connection — [BLUETOOTH.md](BLUETOOTH.md)
- [x] Bluetooth: persistent bonding (`Bonded = true`) across reboots (built-in `org.bluez.Agent1`, `Pairable = true`, and `Connectable = true`) — [BLUETOOTH.md](BLUETOOTH.md)
- [x] Keyboard: key repeat (delay, rate), Caps Lock
- [x] Keyboard: Caps Lock as a way in "Switch Language" (from forum feedback)
- [x] Keyboard: Ilya Birman's typographic layout on the right Alt (`misc:typo`) — from the first feedback
- [x] Default programs: Flatpak/Snap browsers are visible in the choice and open links without `XDG_DATA_DIRS` from profile.d
- [x] Flatpak and Snap program icons in the Dock, Launchpad, Spotlight, tray and on the desktop (`exports/share/icons`, `$XDG_DATA_DIRS`, an AppStream fallback)
- [x] X11: a notification at login if xwayland-satellite is missing (build from source)
- [x] Keyboard: custom shortcuts; key repeat in built-in fields (Spotlight, Settings) — [KEYBOARD.md](KEYBOARD.md)
  The implementation was verified in nested mode; the overall acceptance of frame time and DRM stays in the performance section.
- [ ] Keyboard: Fn/multimedia volume keys — [AUDIO_KEYS.md](AUDIO_KEYS.md)
  - [x] Implementation of ±5 %, mute, holding, an indicator and functional checks with a separate sound service
  - [x] Idle acceptance after the scenario: 0 ticks/5 s; host mouse input into the test window eliminated
  - [ ] A real Fn keyboard and the audible result in DRM (verified by the user)
- [x] Dock: icon size, auto-hide
- [x] Dock: position left/right
- [x] Appearance: accent and highlight colour, interface text size
- [x] Launching programs at login
- [x] About This Computer: processor, memory, graphics card, disks
- [x] Accessibility: large text, contrast, reduce transparency, sticky keys
- [x] Language & Region; translation of the interface into popular languages
- [x] Screenshots: folder and format
- [x] Privacy: program access to screen recording (portal)

## Own apps

- [x] Preview: images and PDF
- [x] Text editor
- [x] Calculator
- [x] App Store (`feather-store`): AppStream catalogue, installing and removing via PackageKit
- [x] Store: an "Updates" tab with a list from PackageKit and an "Update All" button
- [x] Store: Flathub (Flatpak) catalogue support — one-click connection, streaming AppStream XML parsing, a "System package / Flathub" source choice, installing, launching, removing and updates
- [ ] Store: Snap programs and screenshot viewing
- [x] "Screen Recording" (in the style of macOS `Screenshot.app` / `⌥⌘5`: a screenshot and a video recording of the whole screen, a window or a chosen area, a timer, microphone, click highlighting, a `[⏹ MM:SS]` indicator in the menu bar, H.264 `.mp4` via `ffmpeg` and a pure-Rust MJPEG `.avi` fallback)
  - [x] Recording without freezes: damage tracking, a frame from the GPU without copying on the main thread, hardware VAAPI H.264/HEVC, frames by the clock (sound in sync)

## System integrity

- [x] System sounds: alerts/BEL, screenshot, volume feedback and a shared notification player — [SYSTEM_SOUNDS.md](SYSTEM_SOUNDS.md)
  - [ ] The audible result and the real player in DRM (verified by the user)
- [x] Login screen and boot splash in the Feather style
- [x] First run: welcome and initial setup
- [x] Package and installation (release)
- [x] Arch Linux x86_64 test kit: a pacman package, an archive with a windowed demo,
  dependency checks and install/removal from the archive without sources — [notes](../packaging/README-TEST.md)
- [x] Arch Linux pacman repository `[feather-de]` on GitHub Pages: built in CI in an Arch container, signed with the APT key, one-command installation (`arch/install.sh`) and updates through `pacman -Syu`
  - [ ] The first release with the `arch` CI job and an installation check on a real Arch (SDDM/GDM)
  - [x] The shared `install.sh` command works on Arch too: hands control to `arch/install.sh`, which installs the app set, locales and the greetd login screen (verified in a VM on a bare Arch)
  - [x] Konsole and Dolphin on a KDE system do not merge with Feather's Terminal and Files
- [x] The `org.feather.Shell1` desktop API for Larvis and scripts (windows, spaces, apps, sound, screen, media, screenshots, lock, sleep) and the `featherctl` utility — [API.md](API.md)
- [x] A screenshot of a chosen window and mouse and keyboard input for application automation — [API.md](API.md)
- [x] Reading and changing the Dock position via the desktop API — [API.md](API.md)
  - [ ] Testing in a real DRM session: brightness, microphone, MPRIS, `Suspend` (verified by the user)

## Publishing

- [x] Detailed READMEs in Russian and English: features, apps, installation, architecture, measurements and real limitations
- [x] Public GitHub repository and project description — https://github.com/Zumka1991/feather-de

## Performance (budget acceptance)

- [x] Scene + GL ≤ 1 ms per frame on an active host (average 0.30–0.55 ms with `FEATHER_DEBUG_ANIMATIONS=1`)
- [x] First CPU frames of panels ≤ 4 ms (Spotlight 2.52 ms, window restore 1.70 ms, minimise 1.92 ms, menu 2.24 ms, Control Center 3.18 ms, Launchpad 0.69 ms)
- [x] A full frame of the settings window on a worker (5.86 ms warm / 9.83–13.07 ms first frame thanks to the shadow tile cache and `full_span`)
- [x] Memory peak when changing wallpapers ≤ 60 MB (`peak_sampled_anon_kib = 53008` KiB ≈ 51.77 MiB)
- [x] Total memory of the compositor and the wallpaper decoding helper ≤ 60 MB (`peak_combined_anon_kib = 58552` KiB ≈ 57.18 MiB, decoder `5720` KiB)
- [ ] Acceptance of all budgets in a real DRM session (verified by the user)
- [x] Hidden windows (minimised, other spaces, under a fullscreen one) get frame callbacks once a second: 61 → 1 commit/s for a minimised Chrome
- [x] Dock magnification on hover — GPU textures instead of bicubic scaling on the CPU
- [x] `wp_presentation` (vblank time from the kernel in DRM)
- [x] Per-monitor frames and building a frame to a deadline before vblank (`FEATHER_RENDER_DEADLINE=0` — the old way)
  - [ ] Acceptance in DRM: latency, no dropped frames, a second monitor (verified by the user)
- [x] RssAnon growth analysed: retention in glibc arenas, malloc settings give 50 → 30 MiB under load — [MEMORY_CHECKS.md](MEMORY_CHECKS.md#настройки-malloc--2026-11-06)
  - [ ] Confirm in a multi-hour DRM session (verified by the user)
- [x] Speed and stability audit of 2026-11-11: compositor rendering strictly on demand with no idle frames on input, separate compact buffers for notifications and Mission Control titles, a two-generation glyph cache, `FrameDiff::damage_rects`, partial `wl_shm` copying in Terminal, a single `pidfd` reaper and protection of modal focus / the lock screen

## Next stage — development in the macOS Tahoe style

- [x] Window tiling: screen edges, the green-button menu, Ctrl+Super+arrows, the "Window" menu — [WINDOWS.md](WINDOWS.md#тайлинг-srctilingrs)
- [x] Window memory: place and size of programs, restoring the previous session's programs at login — [WINDOWS.md](WINDOWS.md#память-окон-srcwindow_memoryrs)
- [x] A brightness slider in Control Center; a live battery indicator (UPower) — [LOCK_POWER.md](LOCK_POWER.md#батарея)
  - [ ] Testing on a real laptop (verified by the user)
- [x] Caffeine: a menu bar icon that keeps the computer from sleeping and blanking the screen — [LOCK_POWER.md](LOCK_POWER.md#кофеин)
  - [x] The icon is a cup on a saucer (outline / full with steam), not confused with the battery
- [x] Scratchpad: a layer of windows above the spaces on Super+S, tiles with resizable dividers and keyboard control (`Super+arrows`, `Super+Shift+arrows`, `Ctrl/Alt+Super+arrows`, `Super+0`, `Super+F`), moving with Super+Shift+S, remembered — [WINDOWS.md](WINDOWS.md#подложка-srcscratchrs)
  - [ ] Testing with Telegram and in DRM (verified by the user)
- [x] Clipboard history (Super+Shift+V) and an emoji panel (Super+.) — [SPOTLIGHT.md](SPOTLIGHT.md)
- [x] "Now Playing" (MPRIS2) in Control Center, media keys, bringing back the player window — [CONTROL_CENTER.md](CONTROL_CENTER.md)
- [x] A sound icon in the menu bar with a separate "Sound" panel; the Wi‑Fi icon only when an adapter is present — [CONTROL_CENTER.md](CONTROL_CENTER.md)
- [x] A month calendar in the Notification Center — [NOTIFICATIONS.md](NOTIFICATIONS.md)
- [x] A real "System Monitor" `feather-monitor` on Ctrl+Alt+Del: processes with CPU/memory on a background thread only while the window is open, "Quit Process", CPU/memory/disk/network graphs
- [ ] A "Force Quit…" window (`⌥⌘Esc`) with a list of programs that have windows
- [x] Smart Spotlight: settings sections, open windows, files and notes — [SPOTLIGHT.md](SPOTLIGHT.md)
- [x] Layout modes of each workspace (`Free windows`, `Auto-tiling`, `Infinite ribbon` with Niri GPU scrolling, `Ctrl+Alt+M`, pills in the menu bar and Mission Control, `featherctl space-mode`) — [WINDOWS.md](WINDOWS.md#режимы-рабочих-столов-srcspace_layoutrs)
- [x] Live optical glass (Liquid Glass) on the GPU: a 2-level Dual-Kawase blur over the damage of active panels (`1/4 → 1/8 → 1/16`), refraction of a rounded lens (SDF bevel), subtle dispersion and caustics under the menu bar, Dock, menus, Control Center, Spotlight, Notification Center, toasts, the HUD and the Drop Shelf (`FEATHER_GLASS=0` — fallback to the wallpaper; the lock screen never reads windows) — [WINDOWS.md](WINDOWS.md#живое-стекло-liquid-glass-на-gpu-srcglassrs)
- [x] The Drop Shelf at the level of the Wayland DnD protocol: summoned by shaking the cursor or at a side edge while dragging, `Super+D`, the "Window" menu, a pill in the menu bar and `featherctl shelf`; receiving and handing out files, images and text/URLs between spaces, the Scratchpad and fullscreen windows — [WINDOWS.md](WINDOWS.md#полка-перетаскивания-srcdrop_shelfrs)
- [x] Menu command search (Menu HUD, `Super+/`): an instant palette of the active window's `DBusMenu` / `org.gtk.Menus` commands and Feather menus with breadcrumbs, right-hand key badges, QWERTY ↔ ЙЦУКЕН layout auto-correction and `featherctl menu-hud` — [SPOTLIGHT.md](SPOTLIGHT.md#поиск-по-командам-меню--menu-hud-super--)
- [x] Ghost PiP (Picture in Picture `Super+P` and the "click-through" mode `Super+Alt+P` grabbed with `Super`, snapping to corners, moving between spaces, a menu bar icon and `featherctl window pip`) — [WINDOWS.md](WINDOWS.md#призрачный-pip--картинка-в-картинке-и-сквозь-курсор-srcpiprs)
- [x] A per-app audio mixer (SoundSource style) in the menu bar speaker panel, Control Center and "System Settings → Sound": individual volume, mute and output device choice for each program (`0 %` CPU when idle, `featherctl audio`) — [CONTROL_CENTER.md](CONTROL_CENTER.md#пооконный-аудио-микшер-soundsource-стиль--2026-11-12)
- [x] A complete illustrated user guide in Russian and English with screenshots of all modes and drawers (`docs/USER_GUIDE.md`, `docs/USER_GUIDE.en.md`) and updated main pages `README.md` / `README.ru.md`
- [x] A built-in interactive cheat-sheet window "Feather Keyboard Shortcuts" (`src/shortcuts_guide.rs`, `Super+Shift+/` and `Super+F1`, the `🪶` and `Help` menus, Launchpad and Spotlight): 6 categories, live search with layout auto-correction, live display of remapped keys and a quick jump to the settings — [KEYBOARD.md](KEYBOARD.md)

## The Larvis desktop

- [x] In a nested session, check separate windows and Firefox profiles, input on a
  hidden space and handing control over with Cmd + Shift + U.
- [x] The Larvis desktop is a Feather workspace, not a task: it is absent from the Dock, Alt+Tab, Mission Control
  and `ListWindows`; an icon in the menu bar next to the tray and Cmd + U switch there and back
  (`GetLarvisDesktop`, `ShowLarvisDesktop`, `featherctl larvis`).
- [x] The Larvis desktop among the spaces: Ctrl + Alt + ←/→, a tile in Mission Control, the `SpaceChanged` signal.
- [ ] Check a separate session, switching and manual mode on a real DRM output,
  including the budgets from SPEC.md.
