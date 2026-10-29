//! The installer window: what the package is, then installing it with progress, then the result.

use crate::package::{self, Installed, Package, Progress};
use feather_ui::appearance::colors;
use feather_ui::draw::*;
use feather_ui::i18n::tr;
use feather_ui::text::FontSystem;
use std::path::PathBuf;
use tiny_skia::{Color, PixmapMut};

pub const WIDTH: f32 = 560.0;
pub const HEIGHT: f32 = 360.0;
const TOOLBAR_H: f32 = 42.0;
const RADIUS: f32 = 12.0;
const PAD: f32 = 32.0;
const ICON: f32 = 96.0;
const TEXT_X: f32 = PAD + ICON + 24.0;
const BUTTON_H: f32 = 30.0;

pub enum Action {
    None,
    Redraw,
    Close,
    Minimize,
    DragWindow,
    /// Start apt (as root through pkexec)
    Install,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Close,
    Install,
    Open,
    Retry,
}

pub enum State {
    /// The file could not be read as a package
    Invalid(String),
    Ready { installed: Installed, note: Option<String> },
    Installing { fraction: f32, message: String },
    /// Installed; the program to offer "Открыть"
    Done { launcher: Option<(String, String)> },
    Failed(String),
}

pub struct App {
    pub fonts: FontSystem,
    pub dirty: bool,
    pub focused: bool,
    pub path: PathBuf,
    pub package: Package,
    pub state: State,
    hover: (f32, f32),
    pressed: Option<Button>,
}

impl App {
    pub fn new(path: PathBuf) -> Self {
        let (package, state) = match package::read(&path) {
            Ok(p) => {
                let installed = package::installed(&p);
                (p, State::Ready { installed, note: None })
            }
            Err(e) => (Package::default(), State::Invalid(e)),
        };
        Self { fonts: FontSystem::new(), dirty: true, focused: true, path, package, state, hover: (-1.0, -1.0), pressed: None }
    }

    /// A ready-made window for previews and tests, without reading a file
    pub fn with(package: Package, state: State) -> Self {
        Self { fonts: FontSystem::new(), dirty: true, focused: true, path: PathBuf::new(), package, state, hover: (-1.0, -1.0), pressed: None }
    }

    pub fn installing(&self) -> bool {
        matches!(self.state, State::Installing { .. })
    }

    pub fn start_install(&mut self) {
        self.state = State::Installing { fraction: 0.0, message: tr("Ожидание пароля администратора…").into_owned() };
        self.dirty = true;
    }

    pub fn progress(&mut self, progress: Progress) {
        self.state = match progress {
            Progress::Status(fraction, message) => {
                // apt reports per stage; the bar never runs backwards
                let fraction = match &self.state {
                    State::Installing { fraction: old, .. } => fraction.max(*old),
                    _ => fraction,
                };
                State::Installing { fraction, message }
            }
            Progress::Done => State::Done { launcher: package::launcher(&self.package.name) },
            Progress::Cancelled => State::Ready {
                installed: package::installed(&self.package),
                note: Some(tr("Установка отменена").into_owned()),
            },
            Progress::Failed(text) => State::Failed(text),
        };
        self.dirty = true;
    }

    /// The buttons at the bottom right, last first: (button, label, primary)
    fn buttons(&self) -> Vec<(Button, String, bool)> {
        let primary = |installed: &Installed| match installed {
            Installed::No => "Установить",
            Installed::Same => "Переустановить",
            Installed::Older(_) => "Обновить",
            Installed::Newer(_) => "Установить старую версию",
        };
        let t = |s: &str| tr(s).into_owned();
        match &self.state {
            State::Invalid(_) => vec![(Button::Close, t("Закрыть"), true)],
            State::Ready { installed, .. } => vec![(Button::Install, t(primary(installed)), true), (Button::Close, t("Отмена"), false)],
            State::Installing { .. } => Vec::new(),
            State::Done { launcher: Some(_) } => vec![(Button::Open, t("Открыть"), true), (Button::Close, t("Готово"), false)],
            State::Done { launcher: None } => vec![(Button::Close, t("Готово"), true)],
            State::Failed(_) => vec![(Button::Retry, t("Повторить"), true), (Button::Close, t("Закрыть"), false)],
        }
    }

    /// Button rectangles, right to left from the bottom-right corner
    fn button_rects(&mut self) -> Vec<(Button, String, bool, (f32, f32, f32, f32))> {
        let mut right = WIDTH - PAD + 8.0;
        let y = HEIGHT - 20.0 - BUTTON_H;
        let mut out = Vec::new();
        for (button, label, primary) in self.buttons() {
            let w = (self.fonts.measure_raw(&label, 13.0, primary) + 32.0).max(96.0);
            right -= w;
            out.push((button, label, primary, (right, y, w, BUTTON_H)));
            right -= 10.0;
        }
        out
    }

    fn button_at(&mut self, x: f32, y: f32) -> Option<Button> {
        self.button_rects()
            .into_iter()
            .find(|(_, _, _, (bx, by, bw, bh))| x >= *bx && x <= bx + bw && y >= *by && y <= by + bh)
            .map(|(b, ..)| b)
    }

    fn press(&mut self, button: Button) -> Action {
        match button {
            Button::Close => Action::Close,
            Button::Install | Button::Retry => Action::Install,
            Button::Open => {
                if let State::Done { launcher: Some((_, command)) } = &self.state {
                    package::launch(command);
                }
                Action::Close
            }
        }
    }

    pub fn handle_pointer_motion(&mut self, x: f32, y: f32) -> Action {
        let before = (self.button_at(self.hover.0, self.hover.1), traffic_hit(self.hover.0, self.hover.1).is_some());
        self.hover = (x, y);
        if before != (self.button_at(x, y), traffic_hit(x, y).is_some()) {
            self.dirty = true;
            return Action::Redraw;
        }
        Action::None
    }

    pub fn handle_press(&mut self, x: f32, y: f32) -> Action {
        match traffic_hit(x, y) {
            Some(Traffic::Close) if !self.installing() => return Action::Close,
            Some(Traffic::Minimize) => return Action::Minimize,
            Some(_) => return Action::None,
            None => {}
        }
        if let Some(button) = self.button_at(x, y) {
            self.pressed = Some(button);
            self.dirty = true;
            return Action::Redraw;
        }
        if y < TOOLBAR_H {
            return Action::DragWindow;
        }
        Action::None
    }

    pub fn handle_release(&mut self, x: f32, y: f32) -> Action {
        let Some(pressed) = self.pressed.take() else { return Action::None };
        self.dirty = true;
        if self.button_at(x, y) == Some(pressed) {
            return self.press(pressed);
        }
        Action::Redraw
    }

    /// Return presses the primary button, Escape the other one
    pub fn handle_key(&mut self, enter: bool, escape: bool) -> Action {
        let buttons = self.buttons();
        if enter {
            if let Some((b, ..)) = buttons.iter().find(|(_, _, primary)| *primary) {
                return self.press(*b);
            }
        }
        if escape && !self.installing() {
            return match buttons.iter().find(|(_, _, primary)| !*primary) {
                Some((b, ..)) => self.press(*b),
                None => Action::Close,
            };
        }
        Action::None
    }

    pub fn draw(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        let dark = feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark;
        pm.fill(Color::TRANSPARENT);
        let bg = if dark { rgba(30, 31, 36, 250) } else { rgba(246, 246, 249, 252) };
        fill_rrect(pm, 0.0, 0.0, WIDTH, HEIGHT, RADIUS, bg);
        self.draw_toolbar(pm);
        draw_package_icon(pm, PAD, TOOLBAR_H + 18.0, ICON, dark);

        let text = colors::text();
        let secondary = colors::secondary();
        let mut y = TOOLBAR_H + 20.0;
        let width = WIDTH - TEXT_X - PAD;
        match &self.state {
            State::Invalid(error) => {
                let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let name = self.fonts.ellipsize(&name, width, 20.0, true);
                self.fonts.draw_raw(pm, &name, TEXT_X, y, 20.0, text, true);
                y += 34.0;
                for line in wrap(&mut self.fonts, &tr(error), width, 13.0, 3) {
                    self.fonts.draw_raw(pm, &line, TEXT_X, y, 13.0, secondary, false);
                    y += 19.0;
                }
            }
            _ => y = self.draw_package_info(pm, y, width),
        }
        let _ = y;
        self.draw_footer(pm, dark);
        stroke_rrect(pm, 0.5, 0.5, WIDTH - 1.0, HEIGHT - 1.0, RADIUS, 1.0, colors::separator());
    }

    fn draw_toolbar(&mut self, pm: &mut PixmapMut) {
        let hovered = traffic_hit(self.hover.0, self.hover.1).is_some();
        let cy = TOOLBAR_H * 0.5;
        let installing = self.installing();
        for (kind, cx) in [(Traffic::Close, 20.0), (Traffic::Minimize, 40.0), (Traffic::Maximize, 60.0)] {
            // A fixed-size window has no zoom; closing waits for the installation
            let enabled = match kind {
                Traffic::Close => !installing,
                Traffic::Minimize => true,
                Traffic::Maximize => false,
            };
            let color = if !enabled || (!self.focused && !hovered) {
                colors::traffic_inactive()
            } else {
                match kind {
                    Traffic::Close => rgba(255, 95, 87, 255),
                    Traffic::Minimize => rgba(254, 188, 46, 255),
                    Traffic::Maximize => rgba(40, 200, 64, 255),
                }
            };
            fill_circle(pm, cx, cy, 6.0, color);
            if hovered && enabled {
                let ink = rgba(0, 0, 0, 140);
                match kind {
                    Traffic::Close => {
                        line(pm, cx - 2.5, cy - 2.5, cx + 2.5, cy + 2.5, 1.2, ink);
                        line(pm, cx + 2.5, cy - 2.5, cx - 2.5, cy + 2.5, 1.2, ink);
                    }
                    _ => line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.2, ink),
                }
            }
        }
        self.fonts.draw_text_centered(pm, "Установка программы", WIDTH * 0.5, cy - 8.0, 13.0, colors::secondary(), true);
    }

    fn draw_package_info(&mut self, pm: &mut PixmapMut, mut y: f32, width: f32) -> f32 {
        let (text, secondary, tertiary) = (colors::text(), colors::secondary(), colors::tertiary());
        let p = self.package.clone();
        let title = self.fonts.ellipsize(&p.name, width, 22.0, true);
        self.fonts.draw_raw(pm, &title, TEXT_X, y, 22.0, text, true);
        y += 32.0;
        let mut facts = vec![format!("{} {}", tr("Версия"), p.version)];
        if !p.arch.is_empty() && p.arch != "all" {
            facts.push(p.arch.clone());
        }
        if p.size_kib > 0 {
            facts.push(package::human_size(p.size_kib));
        }
        let facts = self.fonts.ellipsize(&facts.join("  ·  "), width, 13.0, false);
        self.fonts.draw_raw(pm, &facts, TEXT_X, y, 13.0, secondary, false);
        y += 26.0;
        for line in wrap(&mut self.fonts, &p.summary, width, 14.0, 2) {
            self.fonts.draw_raw(pm, &line, TEXT_X, y, 14.0, text, false);
            y += 20.0;
        }
        y += 6.0;
        // As much of the long description as fits above the status line
        let room = ((HEIGHT - 112.0 - y) / 18.0).floor().max(0.0) as usize;
        for line in wrap(&mut self.fonts, &p.details, width, 12.5, room) {
            self.fonts.draw_raw(pm, &line, TEXT_X, y, 12.5, secondary, false);
            y += 18.0;
        }
        if !p.maintainer.is_empty() {
            let maintainer = p.maintainer.split(" <").next().unwrap_or(&p.maintainer);
            let label = format!("{} {}", tr("Разработчик:"), maintainer);
            let label = self.fonts.ellipsize(&label, width, 12.0, false);
            self.fonts.draw_raw(pm, &label, TEXT_X, HEIGHT - 104.0, 12.0, tertiary, false);
        }
        y
    }

    /// The status line or progress bar on the left of the bottom bar, then the buttons
    fn draw_footer(&mut self, pm: &mut PixmapMut, dark: bool) {
        let sep_y = HEIGHT - 76.0;
        fill_rect(pm, 0.0, sep_y, WIDTH, 1.0, colors::separator());
        let status_w = WIDTH - 2.0 * PAD;
        let buttons_w: f32 = self.button_rects().iter().map(|(.., r)| r.2 + 10.0).sum();
        let note_w = (status_w - buttons_w - 12.0).max(80.0);
        let base_y = HEIGHT - 20.0 - BUTTON_H;
        let (green, red) = (rgba(48, 186, 92, 255), rgba(236, 72, 64, 255));
        match &self.state {
            State::Ready { installed, note } => {
                let (text, color) = match (note, installed) {
                    (Some(note), _) => (note.clone(), colors::secondary()),
                    (None, Installed::No) => (String::new(), colors::secondary()),
                    (None, Installed::Same) => (tr("Эта версия уже установлена").into_owned(), colors::secondary()),
                    (None, Installed::Older(v)) => (format!("{} {v}", tr("Будет обновлена версия")), colors::secondary()),
                    (None, Installed::Newer(v)) => (format!("{} {v}", tr("Установлена более новая версия")), rgba(214, 140, 20, 255)),
                };
                self.draw_note(pm, &text, note_w, base_y, color);
            }
            State::Installing { fraction, message } => {
                let (fraction, message) = (*fraction, message.clone());
                let label = if message.is_empty() { tr("Установка…").into_owned() } else { message };
                let label = self.fonts.ellipsize(&label, status_w, 12.5, false);
                self.fonts.draw_raw(pm, &label, PAD, base_y - 2.0, 12.5, colors::secondary(), false);
                let (bx, by, bw, bh) = (PAD, base_y + 20.0, status_w, 6.0);
                let track = if dark { rgba(255, 255, 255, 30) } else { rgba(0, 0, 0, 22) };
                fill_rrect(pm, bx, by, bw, bh, 3.0, track);
                if fraction > 0.0 {
                    fill_rrect(pm, bx, by, (bw * fraction).max(bh), bh, 3.0, colors::accent());
                }
            }
            State::Done { .. } => {
                let cy = base_y + BUTTON_H * 0.5;
                fill_circle(pm, PAD + 9.0, cy, 9.0, green);
                polyline(pm, &[(PAD + 5.0, cy), (PAD + 8.0, cy + 3.5), (PAD + 13.5, cy - 3.5)], 2.0, Color::WHITE);
                let label = format!("{} {}", self.package.name, tr("установлена"));
                let label = self.fonts.ellipsize(&label, note_w - 28.0, 13.0, true);
                self.fonts.draw_raw(pm, &label, PAD + 26.0, cy - 9.0, 13.0, colors::text(), true);
            }
            State::Failed(text) => {
                let text = tr(text).into_owned();
                let lines = wrap(&mut self.fonts, &text, note_w, 12.0, 3);
                let mut y = base_y + BUTTON_H * 0.5 - lines.len() as f32 * 8.5;
                for line in lines {
                    self.fonts.draw_raw(pm, &line, PAD, y, 12.0, red, false);
                    y += 17.0;
                }
            }
            State::Invalid(_) => {}
        }
        for (button, label, primary, (x, y, w, h)) in self.button_rects() {
            let hovered = self.hover.0 >= x && self.hover.0 <= x + w && self.hover.1 >= y && self.hover.1 <= y + h;
            let pressed = self.pressed == Some(button) && hovered;
            if primary {
                let accent = colors::accent();
                let fill = if pressed { shade(accent, 0.82) } else if hovered { shade(accent, 0.92) } else { accent };
                fill_rrect(pm, x, y, w, h, 8.0, fill);
                self.fonts.draw_text_centered(pm, &label, x + w * 0.5, y + 7.0, 13.0, Color::WHITE, true);
            } else {
                let fill = match (dark, pressed || hovered) {
                    (true, false) => rgba(255, 255, 255, 26),
                    (true, true) => rgba(255, 255, 255, 40),
                    (false, false) => rgba(0, 0, 0, 14),
                    (false, true) => rgba(0, 0, 0, 26),
                };
                fill_rrect(pm, x, y, w, h, 8.0, fill);
                self.fonts.draw_text_centered(pm, &label, x + w * 0.5, y + 7.0, 13.0, colors::text(), false);
            }
        }
    }

    fn draw_note(&mut self, pm: &mut PixmapMut, text: &str, width: f32, base_y: f32, color: Color) {
        if text.is_empty() {
            return;
        }
        let lines = wrap(&mut self.fonts, text, width, 12.5, 2);
        let mut y = base_y + BUTTON_H * 0.5 - lines.len() as f32 * 9.0;
        for line in lines {
            self.fonts.draw_raw(pm, &line, PAD, y, 12.5, color, false);
            y += 18.0;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Traffic {
    Close,
    Minimize,
    Maximize,
}

fn traffic_hit(x: f32, y: f32) -> Option<Traffic> {
    let cy = TOOLBAR_H * 0.5;
    [(Traffic::Close, 20.0), (Traffic::Minimize, 40.0), (Traffic::Maximize, 60.0)]
        .into_iter()
        .find(|(_, cx)| (x - cx).hypot(y - cy) <= 7.5)
        .map(|(t, _)| t)
}

fn shade(c: Color, k: f32) -> Color {
    Color::from_rgba(c.red() * k, c.green() * k, c.blue() * k, c.alpha()).unwrap_or(c)
}

/// Word wrap into at most `max_lines`; the last line gets an ellipsis when text remains.
/// '\n' starts a new paragraph.
pub fn wrap(fonts: &mut FontSystem, text: &str, width: f32, size: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut truncated = false;
    'paragraphs: for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if current.is_empty() || fonts.measure_raw(&candidate, size, false) <= width {
                current = candidate;
                continue;
            }
            lines.push(std::mem::take(&mut current));
            if lines.len() >= max_lines {
                truncated = true;
                break 'paragraphs;
            }
            current = word.to_string();
        }
        if !current.is_empty() {
            if lines.len() >= max_lines {
                truncated = true;
                break;
            }
            lines.push(current);
        }
    }
    if truncated {
        if let Some(last) = lines.last_mut() {
            *last = fonts.ellipsize(&format!("{last} …"), width, size, false);
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }
    // A single word wider than the line is cut, not left to overflow
    for line in &mut lines {
        if fonts.measure_raw(line, size, false) > width {
            *line = fonts.ellipsize(line, width, size, false);
        }
    }
    lines
}

/// A parcel on a blue tile, in the depth/gradient language of Feather's other app icons
fn draw_package_icon(pm: &mut PixmapMut, x: f32, y: f32, size: f32, dark: bool) {
    let s = size;
    fill_rrect(pm, x, y + s * 0.03, s, s, s * 0.225, rgba(0, 0, 0, if dark { 70 } else { 34 }));
    fill_rrect_gradient(pm, x, y, s, s, s * 0.225, rgba(110, 196, 255, 255), rgba(20, 110, 236, 255));
    // Box: lid, body, tape
    let (bx, bw) = (x + s * 0.2, s * 0.6);
    fill_rrect_gradient(pm, bx, y + s * 0.36, bw, s * 0.44, s * 0.05, rgba(250, 222, 170, 255), rgba(226, 178, 110, 255));
    fill_rrect_gradient(pm, bx - s * 0.03, y + s * 0.25, bw + s * 0.06, s * 0.15, s * 0.04, rgba(255, 236, 196, 255), rgba(240, 200, 138, 255));
    fill_rect(pm, x + s * 0.47, y + s * 0.25, s * 0.06, s * 0.55, rgba(200, 140, 70, 200));
    // Arrow into the box
    let (cx, top) = (x + s * 0.5, y + s * 0.08);
    line(pm, cx, top, cx, y + s * 0.2, s * 0.05, Color::WHITE);
    polyline(pm, &[(cx - s * 0.07, y + s * 0.14), (cx, y + s * 0.21), (cx + s * 0.07, y + s * 0.14)], s * 0.05, Color::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Package {
        Package {
            name: "mouse-vpn".into(),
            version: "0.2.3".into(),
            arch: "amd64".into(),
            summary: "Fast private MouseVPN client".into(),
            details: "Connects to the nearest server in one click, keeps the connection alive across sleep and network changes, and never logs traffic.\nWorks with WireGuard and OpenVPN servers.".into(),
            maintainer: "Mouse Team <team@mouse.example>".into(),
            size_kib: 23950,
        }
    }

    #[test]
    fn buttons_follow_the_state() {
        let mut app = App::with(sample(), State::Ready { installed: Installed::No, note: None });
        assert_eq!(app.buttons().iter().map(|b| b.0).collect::<Vec<_>>(), [Button::Install, Button::Close]);
        assert!(matches!(app.handle_key(true, false), Action::Install));
        app.start_install();
        assert!(app.buttons().is_empty());
        assert!(matches!(app.handle_key(false, true), Action::None), "Escape must not close during install");
        app.progress(Progress::Status(0.6, "Распаковывается".into()));
        app.progress(Progress::Status(0.3, "Настраивается".into()));
        assert!(matches!(app.state, State::Installing { fraction, .. } if (fraction - 0.6).abs() < 1e-6));
        app.progress(Progress::Failed("E: boom".into()));
        assert_eq!(app.buttons()[0].0, Button::Retry);
    }

    #[test]
    fn wrap_limits_lines_and_marks_the_cut() {
        let mut fonts = FontSystem::new();
        let lines = wrap(&mut fonts, &sample().details, 200.0, 12.5, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert!(lines.iter().all(|l| fonts.measure_raw(l, 12.5, false) <= 200.5));
    }

    /// Renders every state: FEATHER_INSTALLER_PREVIEW=/dir cargo test -p feather-installer -- --ignored
    #[test]
    #[ignore]
    fn preview() {
        let dir = std::env::var("FEATHER_INSTALLER_PREVIEW").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
        let states = [
            ("ready", State::Ready { installed: Installed::Older("0.2.1".into()), note: None }),
            ("installing", State::Installing { fraction: 0.55, message: "Распаковывается mouse-vpn (0.2.3) …".into() }),
            ("done", State::Done { launcher: Some(("MouseVPN".into(), "true".into())) }),
            ("failed", State::Failed("Не удалось получить блокировку /var/lib/dpkg/lock-frontend. Её удерживает процесс 1234 (apt-get)".into())),
        ];
        for dark in [false, true] {
            feather_ui::scale::set_appearance(if dark { feather_ui::scale::Appearance::Dark } else { feather_ui::scale::Appearance::Light });
            for (name, state) in states.iter().map(|(n, s)| (*n, clone_state(s))) {
                let mut app = App::with(sample(), state);
                let mut pixmap = tiny_skia::Pixmap::new(WIDTH as u32, HEIGHT as u32).unwrap();
                app.draw(&mut pixmap.as_mut());
                pixmap.save_png(format!("{dir}/installer-{name}-{}.png", if dark { "dark" } else { "light" })).unwrap();
            }
        }
    }

    fn clone_state(s: &State) -> State {
        match s {
            State::Invalid(e) => State::Invalid(e.clone()),
            State::Ready { installed, note } => State::Ready { installed: installed.clone(), note: note.clone() },
            State::Installing { fraction, message } => State::Installing { fraction: *fraction, message: message.clone() },
            State::Done { launcher } => State::Done { launcher: launcher.clone() },
            State::Failed(t) => State::Failed(t.clone()),
        }
    }
}
