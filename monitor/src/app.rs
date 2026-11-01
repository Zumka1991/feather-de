//! Task manager window: the process list with «Снять задачу» and live CPU, memory, disk and
//! network graphs. Everything is painted from the last snapshot; nothing here reads /proc.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use feather_ui::appearance::colors;
use feather_ui::draw::{fill_circle, fill_path, fill_rect, fill_rrect, line, polyline, rgba, stroke_circle, stroke_rrect};
use feather_ui::i18n::{tr, Lang};
use feather_ui::input::{Edit, TextInput};
use feather_ui::text::FontSystem;
use tiny_skia::{Color, PathBuilder, PixmapMut};

use crate::sample::{self, EndResult, Proc, Snapshot};

pub const TOOLBAR_H: f32 = 52.0;
const HEADER_H: f32 = 40.0;
const ROW_H: f32 = 24.0;
const FOOTER_H: f32 = 48.0;
const RADIUS: f32 = 12.0;
const SIDEBAR_W: f32 = 228.0;
const SEG_X: f32 = 84.0;
const SEG_W: f32 = 320.0;
const SEARCH_W: f32 = 200.0;
const HISTORY: usize = 60;
/// How long a process may take to exit after SIGTERM before it is killed
const GRACE: Duration = Duration::from_secs(3);

/// Ending these takes the session, sound or windows with it, so a confirmation comes first
const CRITICAL: [&str; 10] = [
    "feather-de", "feather-session", "systemd", "dbus-daemon", "dbus-broker", "pipewire",
    "wireplumber", "pipewire-pulse", "Xwayland", "xwayland-satellite",
];

mod key {
    pub const UP: u32 = 0xff52;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const HOME: u32 = 0xff50;
    pub const END: u32 = 0xff57;
    pub const DELETE: u32 = 0xffff;
    pub const ESCAPE: u32 = 0xff1b;
    pub const RETURN: u32 = 0xff0d;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const TAB: u32 = 0xff09;
    pub const LEFT_TAB: u32 = 0xfe20;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Name,
    Cpu,
    Mem,
    Pid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resource {
    Cpu,
    Memory,
    Disk,
    Network,
}

impl Resource {
    const ALL: [Self; 4] = [Self::Cpu, Self::Memory, Self::Disk, Self::Network];
    fn title(self) -> &'static str {
        match self {
            Self::Cpu => "ЦП",
            Self::Memory => "Память",
            Self::Disk => "Диск",
            Self::Network => "Сеть",
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Cpu => rgba(10, 132, 255, 255),
            Self::Memory => rgba(191, 90, 242, 255),
            Self::Disk => rgba(48, 199, 88, 255),
            Self::Network => rgba(255, 149, 0, 255),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    Minimize,
    ToggleMaximize,
    Drag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Traffic {
    Close,
    Minimize,
    Maximize,
}

fn hit_traffic(x: f32, y: f32) -> Option<Traffic> {
    let cy = TOOLBAR_H * 0.5;
    [(Traffic::Close, 20.0), (Traffic::Minimize, 40.0), (Traffic::Maximize, 60.0)]
        .into_iter()
        .find(|(_, cx)| (x - cx).hypot(y - cy) <= 7.5)
        .map(|(t, _)| t)
}

fn inside(x: f32, y: f32, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

fn decimal(v: f64, digits: usize) -> String {
    let s = format!("{v:.digits$}");
    if matches!(feather_ui::scale::lang(), Lang::En | Lang::Zh) { s } else { s.replace('.', ",") }
}

pub fn bytes(b: u64) -> String {
    let b = b as f64;
    if b < 1024.0 * 1024.0 {
        tr(&format!("{} КБ", (b / 1024.0).round())).into_owned()
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        tr(&format!("{} МБ", decimal(b / 1048576.0, 1))).into_owned()
    } else {
        tr(&format!("{} ГБ", decimal(b / 1073741824.0, 1))).into_owned()
    }
}

pub fn rate(b: f64) -> String {
    if b < 1024.0 * 1024.0 {
        tr(&format!("{} КБ/с", decimal(b / 1024.0, 0))).into_owned()
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        tr(&format!("{} МБ/с", decimal(b / 1048576.0, 1))).into_owned()
    } else {
        tr(&format!("{} ГБ/с", decimal(b / 1073741824.0, 1))).into_owned()
    }
}

fn percent(v: f32) -> String {
    format!("{} %", decimal(v as f64, if v < 10.0 { 1 } else { 0 }))
}

fn uptime(s: u64) -> String {
    format!("{}:{:02}:{:02}:{:02}", s / 86400, s / 3600 % 24, s / 60 % 60, s % 60)
}

pub struct App {
    pub fonts: FontSystem,
    pub width: f32,
    pub height: f32,
    pub focused: bool,
    pub maximized: bool,
    pub dirty: bool,
    pub tab: Tab,
    pub shift: bool,
    snap: Snapshot,
    rows: Vec<usize>,
    sort: Column,
    descending: bool,
    selected: Option<i32>,
    scroll: f32,
    search: TextInput,
    search_focused: bool,
    all_users: bool,
    hover: (f32, f32),
    resource: Resource,
    cpu_hist: VecDeque<f32>,
    mem_hist: VecDeque<f32>,
    disk_hist: VecDeque<(f64, f64)>,
    net_hist: VecDeque<(f64, f64)>,
    status: Option<String>,
    confirm: Option<(i32, u64, String)>,
    terminating: Vec<(i32, u64, Instant)>,
}

impl App {
    pub fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            width: 860.0,
            height: 600.0,
            focused: true,
            maximized: false,
            dirty: true,
            tab: Tab::Processes,
            shift: false,
            snap: Snapshot::default(),
            rows: Vec::new(),
            sort: Column::Cpu,
            descending: true,
            selected: None,
            scroll: 0.0,
            search: TextInput::new("").with_limit(64),
            search_focused: false,
            all_users: false,
            hover: (-1.0, -1.0),
            resource: Resource::Cpu,
            cpu_hist: VecDeque::new(),
            mem_hist: VecDeque::new(),
            disk_hist: VecDeque::new(),
            net_hist: VecDeque::new(),
            status: None,
            confirm: None,
            terminating: Vec::new(),
        }
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        let (w, h) = ((width as f32).max(640.0), (height as f32).max(420.0));
        if (w - self.width).abs() > 0.5 || (h - self.height).abs() > 0.5 {
            self.width = w;
            self.height = h;
            self.clamp_scroll();
            self.dirty = true;
        }
    }

    pub fn update(&mut self, snap: Snapshot) {
        fn push<T>(q: &mut VecDeque<T>, v: T) {
            if q.len() == HISTORY {
                q.pop_front();
            }
            q.push_back(v);
        }
        // The first snapshot has no rates yet: start the graphs from the second one
        if !self.snap.procs.is_empty() {
            push(&mut self.cpu_hist, snap.cpu);
            push(&mut self.mem_hist, snap.mem_used as f32 / snap.mem_total.max(1) as f32);
            push(&mut self.disk_hist, (snap.disk_read, snap.disk_write));
            push(&mut self.net_hist, (snap.net_rx, snap.net_tx));
        }
        let now = Instant::now();
        self.terminating.retain(|&(pid, start, at)| {
            let alive = snap.procs.iter().any(|p| p.pid == pid && p.start == start);
            if alive && now.duration_since(at) >= GRACE {
                sample::force_if_alive(pid, start);
                return false;
            }
            alive
        });
        self.snap = snap;
        self.rebuild();
        self.dirty = true;
    }

    fn rebuild(&mut self) {
        let q = self.search.text().trim().to_lowercase();
        let procs = &self.snap.procs;
        self.rows = (0..procs.len())
            .filter(|&i| {
                let p = &procs[i];
                (self.all_users || p.own)
                    && (q.is_empty()
                        || p.name.to_lowercase().contains(&q)
                        || p.cmd.to_lowercase().contains(&q)
                        || p.pid.to_string().starts_with(&q))
            })
            .collect();
        let (sort, desc) = (self.sort, self.descending);
        self.rows.sort_by(|&a, &b| {
            let (a, b) = (&procs[a], &procs[b]);
            let ord = match sort {
                Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Column::Cpu => a.cpu.total_cmp(&b.cpu).then(a.mem.cmp(&b.mem)),
                Column::Mem => a.mem.cmp(&b.mem),
                Column::Pid => a.pid.cmp(&b.pid),
            };
            let ord = if desc { ord.reverse() } else { ord };
            ord.then(a.pid.cmp(&b.pid))
        });
        self.clamp_scroll();
    }

    fn proc(&self, pid: i32) -> Option<&Proc> {
        self.snap.procs.iter().find(|p| p.pid == pid)
    }

    fn list_top(&self) -> f32 {
        TOOLBAR_H + HEADER_H
    }

    fn list_h(&self) -> f32 {
        (self.height - FOOTER_H - self.list_top()).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        let max = (self.rows.len() as f32 * ROW_H - self.list_h()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn selected_row(&self) -> Option<usize> {
        let pid = self.selected?;
        self.rows.iter().position(|&i| self.snap.procs[i].pid == pid)
    }

    fn select_row(&mut self, row: usize) {
        let Some(&i) = self.rows.get(row) else { return };
        self.selected = Some(self.snap.procs[i].pid);
        self.confirm = None;
        let top = row as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H > self.scroll + self.list_h() {
            self.scroll = top + ROW_H - self.list_h();
        }
        self.clamp_scroll();
        self.dirty = true;
    }

    /// «Снять задачу»: SIGTERM, then SIGKILL after `GRACE`; `force` kills at once
    fn end_selected(&mut self, force: bool) {
        let Some(p) = self.selected.and_then(|pid| self.proc(pid)).cloned() else { return };
        if CRITICAL.contains(&p.name.as_str()) && self.confirm.as_ref().map(|c| c.0) != Some(p.pid) {
            self.confirm = Some((p.pid, p.start, p.name.clone()));
            self.status = None;
            self.dirty = true;
            return;
        }
        self.confirm = None;
        self.status = match sample::end(p.pid, force) {
            EndResult::Sent => {
                if !force {
                    self.terminating.push((p.pid, p.start, Instant::now()));
                }
                None
            }
            EndResult::Gone => None,
            EndResult::Denied => Some(format!("Нет прав завершить «{}»", p.name)),
        };
        self.dirty = true;
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.search_focused = false;
            self.dirty = true;
        }
    }

    pub fn handle_key(&mut self, sym: u32, utf8: Option<&str>, ctrl: bool) -> Action {
        if ctrl && matches!(sym, 0x71 | 0x51 | 0x77 | 0x57) {
            return Action::Close;
        }
        if (ctrl && sym == key::TAB) || sym == key::LEFT_TAB || (ctrl && matches!(sym, 0x31 | 0x32)) {
            self.set_tab(match (sym, self.tab) {
                (0x31, _) => Tab::Processes,
                (0x32, _) => Tab::Performance,
                (_, Tab::Processes) => Tab::Performance,
                _ => Tab::Processes,
            });
            return Action::None;
        }
        if self.tab == Tab::Performance {
            let i = Resource::ALL.iter().position(|r| *r == self.resource).unwrap_or(0);
            let next = match sym {
                key::UP => i.saturating_sub(1),
                key::DOWN => (i + 1).min(Resource::ALL.len() - 1),
                _ => return Action::None,
            };
            self.resource = Resource::ALL[next];
            self.dirty = true;
            return Action::None;
        }
        if let Some(confirm) = self.confirm.clone() {
            match sym {
                key::RETURN | key::KP_ENTER | key::DELETE => {
                    self.selected = Some(confirm.0);
                    self.end_selected(self.shift);
                    return Action::None;
                }
                key::ESCAPE => {
                    self.confirm = None;
                    self.dirty = true;
                    return Action::None;
                }
                _ => {}
            }
        }
        let page = (self.list_h() / ROW_H).floor().max(1.0) as usize;
        let last = self.rows.len().saturating_sub(1);
        let current = self.selected_row();
        let target = match sym {
            key::UP => Some(current.map_or(0, |r| r.saturating_sub(1))),
            key::DOWN => Some(current.map_or(0, |r| (r + 1).min(last))),
            key::PAGE_UP => Some(current.map_or(0, |r| r.saturating_sub(page))),
            key::PAGE_DOWN => Some(current.map_or(0, |r| (r + page).min(last))),
            key::HOME if self.search.is_empty() => Some(0),
            key::END if self.search.is_empty() => Some(last),
            _ => None,
        };
        if let Some(row) = target {
            self.select_row(row);
            return Action::None;
        }
        match sym {
            key::DELETE => self.end_selected(self.shift),
            key::ESCAPE if !self.search.is_empty() => {
                self.search.clear();
                self.rebuild();
                self.dirty = true;
            }
            key::ESCAPE => {
                self.search_focused = false;
                self.status = None;
                self.dirty = true;
            }
            key::RETURN | key::KP_ENTER => {
                if let Some(&i) = self.rows.first().filter(|_| self.selected_row().is_none()) {
                    self.selected = Some(self.snap.procs[i].pid);
                    self.dirty = true;
                }
            }
            _ => match self.search.key(sym, utf8, ctrl, self.shift) {
                Edit::Changed => {
                    self.search_focused = true;
                    self.scroll = 0.0;
                    self.rebuild();
                    self.dirty = true;
                }
                Edit::Moved => self.dirty = true,
                Edit::Ignored => {}
            },
        }
        Action::None
    }

    /// Repaints only when the pointer moves onto another hoverable element, not on every pixel
    pub fn handle_motion(&mut self, x: f32, y: f32) {
        let changed = self.hover_key(x, y) != self.hover_key(self.hover.0, self.hover.1);
        self.hover = (x, y);
        if changed {
            self.dirty = true;
        }
    }

    fn hover_key(&self, x: f32, y: f32) -> i64 {
        if x < 0.0 || y < 0.0 {
            return -1;
        }
        if hit_traffic(x, y).is_some() {
            return 1;
        }
        if y < TOOLBAR_H {
            return 0;
        }
        let (w, h) = (self.width, self.height);
        match self.tab {
            Tab::Performance => (0..Resource::ALL.len())
                .find(|&i| inside(x, y, 12.0, TOOLBAR_H + 12.0 + i as f32 * 74.0, SIDEBAR_W - 24.0, 68.0))
                .map_or(0, |i| 10 + i as i64),
            Tab::Processes if y >= h - FOOTER_H => {
                let by = h - FOOTER_H + 11.0;
                [(w - 16.0 - 130.0, 130.0), (w - 16.0 - 110.0, 110.0), (w - 16.0 - 110.0 - 8.0 - 90.0, 90.0)]
                    .iter()
                    .position(|&(bx, bw)| inside(x, y, bx, by, bw, 26.0))
                    .map_or(0, |i| 20 + i as i64)
            }
            Tab::Processes if y < self.list_top() => 30 + self.column_at(x) as i64,
            Tab::Processes => 100 + ((y - self.list_top() + self.scroll) / ROW_H).floor() as i64,
        }
    }

    pub fn handle_scroll(&mut self, dy: f32) {
        if self.tab == Tab::Processes && dy != 0.0 {
            self.scroll += dy;
            self.clamp_scroll();
            self.dirty = true;
        }
    }

    pub fn handle_click(&mut self, x: f32, y: f32) -> Action {
        match hit_traffic(x, y) {
            Some(Traffic::Close) => return Action::Close,
            Some(Traffic::Minimize) => return Action::Minimize,
            Some(Traffic::Maximize) => return Action::ToggleMaximize,
            None => {}
        }
        let (w, h) = (self.width, self.height);
        if y < TOOLBAR_H {
            if inside(x, y, SEG_X, 13.0, SEG_W, 26.0) {
                self.set_tab(if x < SEG_X + SEG_W * 0.5 { Tab::Processes } else { Tab::Performance });
                return Action::None;
            }
            if self.tab == Tab::Processes && inside(x, y, w - 16.0 - SEARCH_W, 12.0, SEARCH_W, 28.0) {
                self.search_focused = true;
                let extend = self.shift;
                self.search.click(&mut self.fonts, x - (w - 16.0 - SEARCH_W + 30.0), SEARCH_W - 54.0, 13.0, false, extend);
                if !self.search.is_empty() && x > w - 16.0 - 26.0 {
                    self.search.clear();
                    self.rebuild();
                }
                self.dirty = true;
                return Action::None;
            }
            return Action::Drag;
        }
        self.search_focused = false;
        self.dirty = true;
        match self.tab {
            Tab::Performance => {
                for (i, r) in Resource::ALL.into_iter().enumerate() {
                    let cy = TOOLBAR_H + 12.0 + i as f32 * 74.0;
                    if inside(x, y, 12.0, cy, SIDEBAR_W - 24.0, 68.0) {
                        self.resource = r;
                    }
                }
            }
            Tab::Processes => {
                if y >= h - FOOTER_H {
                    self.footer_click(x, y);
                } else if y < self.list_top() {
                    let column = self.column_at(x);
                    if self.sort == column {
                        self.descending = !self.descending;
                    } else {
                        self.sort = column;
                        self.descending = column != Column::Name && column != Column::Pid;
                    }
                    self.rebuild();
                } else {
                    let row = ((y - self.list_top() + self.scroll) / ROW_H).floor() as usize;
                    if row < self.rows.len() {
                        self.select_row(row);
                    } else {
                        self.selected = None;
                        self.confirm = None;
                    }
                }
            }
        }
        Action::None
    }

    fn footer_click(&mut self, x: f32, y: f32) {
        let (w, by) = (self.width, self.height - FOOTER_H + 11.0);
        if self.confirm.is_some() {
            if inside(x, y, w - 16.0 - 110.0, by, 110.0, 26.0) {
                let pid = self.confirm.as_ref().map(|c| c.0);
                self.selected = pid;
                self.end_selected(self.shift);
            } else if inside(x, y, w - 16.0 - 110.0 - 8.0 - 90.0, by, 90.0, 26.0) {
                self.confirm = None;
            }
            return;
        }
        if inside(x, y, w - 16.0 - 130.0, by, 130.0, 26.0) {
            self.end_selected(self.shift);
        } else if inside(x, y, 12.0, by, 190.0, 26.0) {
            self.all_users = !self.all_users;
            self.rebuild();
        }
    }

    /// Right edges of the numeric columns: ЦП, Память, PID
    fn columns(&self) -> (f32, f32, f32) {
        let pid = self.width - 16.0;
        let mem = pid - 80.0;
        let cpu = mem - 110.0;
        (cpu, mem, pid)
    }

    fn column_at(&self, x: f32) -> Column {
        let (cpu, mem, _) = self.columns();
        if x < cpu - 84.0 {
            Column::Name
        } else if x < cpu + 8.0 {
            Column::Cpu
        } else if x < mem + 8.0 {
            Column::Mem
        } else {
            Column::Pid
        }
    }

    pub fn draw(&mut self, pm: &mut PixmapMut) {
        self.dirty = false;
        let (w, h) = (self.width, self.height);
        pm.fill(Color::TRANSPARENT);
        let r = if self.maximized { 0.0 } else { RADIUS };
        fill_rrect(pm, 0.0, 0.0, w, h, r, colors::window());
        fill_rrect(pm, 0.0, 0.0, w, TOOLBAR_H + r, r, colors::sidebar());
        fill_rect(pm, 0.0, TOOLBAR_H, w, r, colors::window());
        line(pm, 0.0, TOOLBAR_H, w, TOOLBAR_H, 1.0, colors::separator());
        self.draw_toolbar(pm);
        match self.tab {
            Tab::Processes => self.draw_processes(pm),
            Tab::Performance => self.draw_performance(pm),
        }
        if !self.maximized {
            stroke_rrect(pm, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS, 1.0, colors::separator());
        }
    }

    fn draw_toolbar(&mut self, pm: &mut PixmapMut) {
        let hovered = hit_traffic(self.hover.0, self.hover.1).is_some();
        let cy = TOOLBAR_H * 0.5;
        for (t, cx) in [(Traffic::Close, 20.0), (Traffic::Minimize, 40.0), (Traffic::Maximize, 60.0)] {
            let color = if !self.focused && !hovered {
                colors::traffic_inactive()
            } else {
                match t {
                    Traffic::Close => rgba(255, 95, 87, 255),
                    Traffic::Minimize => rgba(254, 188, 46, 255),
                    Traffic::Maximize => rgba(40, 200, 64, 255),
                }
            };
            fill_circle(pm, cx, cy, 6.0, color);
            if hovered {
                let ink = rgba(0, 0, 0, 140);
                match t {
                    Traffic::Close => {
                        line(pm, cx - 2.5, cy - 2.5, cx + 2.5, cy + 2.5, 1.2, ink);
                        line(pm, cx + 2.5, cy - 2.5, cx - 2.5, cy + 2.5, 1.2, ink);
                    }
                    Traffic::Minimize => line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.2, ink),
                    Traffic::Maximize => {
                        line(pm, cx - 3.0, cy, cx + 3.0, cy, 1.2, ink);
                        line(pm, cx, cy - 3.0, cx, cy + 3.0, 1.2, ink);
                    }
                }
            }
        }

        fill_rrect(pm, SEG_X, 13.0, SEG_W, 26.0, 7.0, colors::control());
        let half = SEG_W * 0.5;
        for (i, (tab, label)) in [(Tab::Processes, "Процессы"), (Tab::Performance, "Производительность")].into_iter().enumerate() {
            let x = SEG_X + i as f32 * half;
            let active = self.tab == tab;
            if active {
                let pill = if feather_ui::scale::appearance() == feather_ui::scale::Appearance::Dark {
                    rgba(255, 255, 255, 40)
                } else {
                    rgba(255, 255, 255, 240)
                };
                fill_rrect(pm, x + 2.0, 15.0, half - 4.0, 22.0, 5.5, pill);
            }
            let color = if active { colors::text() } else { colors::secondary() };
            self.fonts.draw_text_centered(pm, label, x + half * 0.5, 18.0, 13.0, color, active);
        }

        if self.tab == Tab::Processes {
            let (sx, sy) = (self.width - 16.0 - SEARCH_W, 12.0);
            fill_rrect(pm, sx, sy, SEARCH_W, 28.0, 8.0, colors::control());
            if self.search_focused {
                stroke_rrect(pm, sx - 1.0, sy - 1.0, SEARCH_W + 2.0, 30.0, 9.0, 2.0, colors::accent());
            }
            stroke_circle(pm, sx + 15.0, sy + 13.0, 4.5, 1.4, colors::secondary());
            line(pm, sx + 18.5, sy + 16.5, sx + 21.5, sy + 19.5, 1.4, colors::secondary());
            if self.search.is_empty() && !self.search_focused {
                self.fonts.draw_text(pm, "Поиск процесса", sx + 30.0, sy + 6.0, 13.0, colors::tertiary(), false);
            } else {
                let search = self.search.clone();
                search.draw(pm, &mut self.fonts, sx + 30.0, sy + 6.0, SEARCH_W - 54.0, 13.0, false, colors::text(), self.search_focused, colors::accent());
            }
            if !self.search.is_empty() {
                let (cx, cy) = (sx + SEARCH_W - 13.0, sy + 14.0);
                fill_circle(pm, cx, cy, 6.5, colors::tertiary());
                line(pm, cx - 2.2, cy - 2.2, cx + 2.2, cy + 2.2, 1.3, colors::window());
                line(pm, cx + 2.2, cy - 2.2, cx - 2.2, cy + 2.2, 1.3, colors::window());
            }
        }
    }

    fn draw_processes(&mut self, pm: &mut PixmapMut) {
        let w = self.width;
        let (cpu_x, mem_x, pid_x) = self.columns();
        let top = TOOLBAR_H;
        let snap_cpu = self.snap.cpu;
        let mem_share = 100.0 * self.snap.mem_used as f32 / self.snap.mem_total.max(1) as f32;
        let list_top = self.list_top();
        let list_h = self.list_h();
        let first = (self.scroll / ROW_H).floor() as usize;
        let count = (list_h / ROW_H).ceil() as usize + 1;
        let selected = self.selected;
        let focused = self.focused;
        let hover_row = (self.hover.1 >= list_top && self.hover.1 < list_top + list_h)
            .then(|| ((self.hover.1 - list_top + self.scroll) / ROW_H).floor() as usize);
        let mem_total = self.snap.mem_total.max(1) as f32;
        // Rows go straight into the frame; the header and footer painted afterwards cover
        // the half-visible rows at both ends
        {
            let sp = &mut *pm;
            for row in first..(first + count).min(self.rows.len()) {
                let p = &self.snap.procs[self.rows[row]];
                let y = list_top + row as f32 * ROW_H - self.scroll;
                if y >= list_top + list_h {
                    break;
                }
                let is_selected = selected == Some(p.pid);
                if is_selected {
                    fill_rect(sp, 0.0, y, w, ROW_H, if focused { colors::accent() } else { colors::inactive_selection() });
                } else {
                    if row % 2 == 1 {
                        fill_rect(sp, 0.0, y, w, ROW_H, colors::stripe());
                    }
                    if hover_row == Some(row) {
                        fill_rect(sp, 0.0, y, w, ROW_H, colors::hover());
                    }
                    // Windows-style heat: the busier, the warmer the cell
                    let heat = |v: f32| rgba(255, 184, 0, (v.clamp(0.0, 1.0) * 110.0) as u8);
                    if p.cpu >= 0.5 {
                        fill_rect(sp, cpu_x - 84.0, y, 92.0, ROW_H, heat(p.cpu / 50.0));
                    }
                    if p.mem as f32 / mem_total >= 0.01 {
                        fill_rect(sp, cpu_x + 8.0, y, mem_x - cpu_x, ROW_H, heat(p.mem as f32 / mem_total * 5.0));
                    }
                }
                let ink = if is_selected && focused { rgba(255, 255, 255, 255) } else { colors::text() };
                let dim = if is_selected && focused { rgba(255, 255, 255, 210) } else { colors::secondary() };
                let dying = self.terminating.iter().any(|t| t.0 == p.pid);
                let name = self.fonts.ellipsize(&p.name, cpu_x - 84.0 - 16.0 - 8.0, 13.0, false);
                let nw = self.fonts.draw_text(sp, &name, 16.0, y + 4.0, 13.0, ink, false);
                if dying {
                    self.fonts.draw_text(sp, "завершается…", 16.0 + nw + 8.0, y + 5.0, 11.0, dim, false);
                }
                self.fonts.draw_text_right(sp, &percent(p.cpu), cpu_x, y + 4.0, 13.0, ink, false);
                self.fonts.draw_text_right(sp, &bytes(p.mem), mem_x, y + 4.0, 13.0, ink, false);
                self.fonts.draw_text_right(sp, &p.pid.to_string(), pid_x, y + 4.0, 13.0, dim, false);
            }
        }
        fill_rect(pm, 0.0, top, w, HEADER_H, colors::window());
        let hover_col = (self.hover.1 >= top && self.hover.1 < self.list_top()).then(|| self.column_at(self.hover.0));
        if let Some(col) = hover_col {
            let (x0, x1) = match col {
                Column::Name => (0.0, cpu_x - 84.0),
                Column::Cpu => (cpu_x - 84.0, cpu_x + 8.0),
                Column::Mem => (cpu_x + 8.0, mem_x + 8.0),
                Column::Pid => (mem_x + 8.0, w),
            };
            fill_rect(pm, x0, top, x1 - x0, HEADER_H, colors::hover());
        }
        let columns: [(Column, &str, String, f32); 4] = [
            (Column::Name, "Имя", String::new(), 16.0),
            (Column::Cpu, "ЦП", percent(snap_cpu), cpu_x),
            (Column::Mem, "Память", percent(mem_share), mem_x),
            (Column::Pid, "PID", String::new(), pid_x),
        ];
        for (col, title, total, x) in columns {
            let active = self.sort == col;
            let color = if active { colors::text() } else { colors::secondary() };
            let (tw, ty) = if total.is_empty() {
                (self.fonts.measure_text(title, 12.0, active), top + 13.0)
            } else {
                self.fonts.draw_text_right(pm, &total, x, top + 5.0, 15.0, colors::text(), true);
                (self.fonts.measure_text(title, 12.0, active), top + 22.0)
            };
            let tx = if col == Column::Name { x } else { x - tw };
            self.fonts.draw_text(pm, title, tx, ty, 12.0, color, active);
            if active {
                // Sort direction chevron next to the column title
                let (ax, ay) = if col == Column::Name { (x + tw + 8.0, ty + 8.0) } else { (x - tw - 9.0, ty + 8.0) };
                let d = if self.descending { 1.0 } else { -1.0 };
                polyline(pm, &[(ax - 3.0, ay - 1.5 * d), (ax, ay + 1.5 * d), (ax + 3.0, ay - 1.5 * d)], 1.3, color);
            }
        }
        line(pm, 0.0, self.list_top(), w, self.list_top(), 1.0, colors::separator());
        if self.rows.is_empty() && !self.snap.procs.is_empty() {
            self.fonts.draw_text_centered(pm, "Нет подходящих процессов", w * 0.5, list_top + 40.0, 13.0, colors::secondary(), false);
        }
        // Scroll indicator
        let content = self.rows.len() as f32 * ROW_H;
        if content > list_h && list_h > 0.0 {
            let bar_h = (list_h * list_h / content).max(24.0);
            let bar_y = list_top + (list_h - bar_h) * self.scroll / (content - list_h);
            fill_rrect(pm, w - 7.0, bar_y + 2.0, 4.0, bar_h - 4.0, 2.0, colors::selected_gray());
        }
        self.draw_footer(pm);
    }

    fn button(&mut self, pm: &mut PixmapMut, x: f32, y: f32, w: f32, label: &str, fill: Color, ink: Color) {
        let hovered = inside(self.hover.0, self.hover.1, x, y, w, 26.0);
        fill_rrect(pm, x, y, w, 26.0, 7.0, fill);
        if hovered {
            fill_rrect(pm, x, y, w, 26.0, 7.0, colors::hover());
        }
        self.fonts.draw_text_centered(pm, label, x + w * 0.5, y + 5.0, 13.0, ink, false);
    }

    fn draw_footer(&mut self, pm: &mut PixmapMut) {
        let (w, h) = (self.width, self.height);
        let top = h - FOOTER_H;
        fill_rect(pm, 0.0, top, w, ROW_H + 1.0, colors::window());
        line(pm, 0.0, top, w, top, 1.0, colors::separator());
        let by = top + 11.0;
        if let Some((_, _, name)) = self.confirm.clone() {
            let text = if name == "feather-de" {
                "Это сам Пёрышко: все окна закроются и сеанс завершится.".to_string()
            } else {
                format!("Без «{name}» могут перестать работать окна, звук или сеанс.")
            };
            fill_circle(pm, 24.0, top + FOOTER_H * 0.5, 8.0, rgba(255, 159, 10, 255));
            self.fonts.draw_text_centered(pm, "!", 24.0, top + 15.0, 12.0, rgba(255, 255, 255, 255), true);
            let room = w - 16.0 - 110.0 - 8.0 - 90.0 - 16.0 - 40.0;
            let text = self.fonts.ellipsize(&tr(&text), room, 13.0, false);
            self.fonts.draw_text(pm, &text, 40.0, top + 15.0, 13.0, colors::text(), false);
            self.button(pm, w - 16.0 - 110.0 - 8.0 - 90.0, by, 90.0, "Отмена", colors::control(), colors::text());
            self.button(pm, w - 16.0 - 110.0, by, 110.0, "Завершить", rgba(255, 59, 48, 255), rgba(255, 255, 255, 255));
            return;
        }
        let (kx, ky) = (16.0, by + 5.0);
        if self.all_users {
            fill_rrect(pm, kx, ky, 16.0, 16.0, 4.0, colors::accent());
            polyline(pm, &[(kx + 4.0, ky + 8.5), (kx + 7.0, ky + 11.5), (kx + 12.5, ky + 4.5)], 1.8, rgba(255, 255, 255, 255));
        } else {
            fill_rrect(pm, kx, ky, 16.0, 16.0, 4.0, colors::window());
            stroke_rrect(pm, kx, ky, 16.0, 16.0, 4.0, 1.0, colors::sidebar_edge());
        }
        self.fonts.draw_text(pm, "Все пользователи", kx + 24.0, ky + 1.0, 13.0, colors::text(), false);
        let status = match &self.status {
            Some(s) => s.clone(),
            None => format!("Процессов: {}", self.rows.len()),
        };
        let color = if self.status.is_some() { rgba(255, 69, 58, 255) } else { colors::secondary() };
        let room = w - 16.0 - 130.0 - 16.0 - 220.0;
        let status = self.fonts.ellipsize(&tr(&status), room, 12.0, false);
        self.fonts.draw_text(pm, &status, 220.0, ky + 1.0, 12.0, color, false);
        let enabled = self.selected.and_then(|p| self.proc(p)).is_some();
        let label = if self.shift { "Завершить сразу" } else { "Снять задачу" };
        if enabled {
            self.button(pm, w - 16.0 - 130.0, by, 130.0, label, colors::accent(), rgba(255, 255, 255, 255));
        } else {
            fill_rrect(pm, w - 16.0 - 130.0, by, 130.0, 26.0, 7.0, colors::control());
            self.fonts.draw_text_centered(pm, label, w - 16.0 - 65.0, by + 5.0, 13.0, colors::tertiary(), false);
        }
    }

    fn summary(&self, r: Resource) -> String {
        let s = &self.snap;
        match r {
            Resource::Cpu => percent(s.cpu),
            Resource::Memory => format!(
                "{} / {} ({})",
                bytes(s.mem_used),
                bytes(s.mem_total),
                percent(100.0 * s.mem_used as f32 / s.mem_total.max(1) as f32)
            ),
            Resource::Disk => format!("{} / {}", rate(s.disk_read), rate(s.disk_write)),
            Resource::Network => format!("↓ {}  ↑ {}", rate(s.net_rx), rate(s.net_tx)),
        }
    }

    /// Values of a graph as fractions of its height; the second series for disk/network
    fn series(&self, r: Resource) -> (Vec<f32>, Option<Vec<f32>>, f64) {
        match r {
            Resource::Cpu => (self.cpu_hist.iter().map(|v| v / 100.0).collect(), None, 100.0),
            Resource::Memory => (self.mem_hist.iter().copied().collect(), None, 1.0),
            Resource::Disk | Resource::Network => {
                let hist = if r == Resource::Disk { &self.disk_hist } else { &self.net_hist };
                // Round the scale up to a 1-2-5 step so the axis label stays readable
                let peak = hist.iter().map(|(a, b)| a.max(*b)).fold(0.0, f64::max).max(64.0 * 1024.0);
                let mut scale = 1024.0;
                while scale < peak {
                    let d = scale.log10().fract();
                    scale *= if d < 0.29 { 2.0 } else if d < 0.69 { 2.5 } else { 2.0 };
                }
                (
                    hist.iter().map(|(a, _)| (*a / scale) as f32).collect(),
                    Some(hist.iter().map(|(_, b)| (*b / scale) as f32).collect()),
                    scale,
                )
            }
        }
    }

    fn graph(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, values: &[f32], color: Color, fill: bool) {
        if values.is_empty() {
            return;
        }
        let step = w / (HISTORY - 1) as f32;
        let x0 = x + w - (values.len() - 1) as f32 * step;
        let points: Vec<(f32, f32)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| (x0 + i as f32 * step, y + h - v.clamp(0.0, 1.0) * h))
            .collect();
        if fill && points.len() > 1 {
            let mut pb = PathBuilder::new();
            pb.move_to(points[0].0, y + h);
            for (px, py) in &points {
                pb.line_to(*px, *py);
            }
            pb.line_to(x + w, y + h);
            pb.close();
            if let Some(path) = pb.finish() {
                let mut c = color;
                c.set_alpha(0.18);
                fill_path(pm, &path, c);
            }
        }
        polyline(pm, &points, 1.5, color);
    }

    fn draw_performance(&mut self, pm: &mut PixmapMut) {
        let (w, h) = (self.width, self.height);
        let r = if self.maximized { 0.0 } else { RADIUS };
        fill_rect(pm, 0.0, TOOLBAR_H, SIDEBAR_W, h - TOOLBAR_H - r, colors::sidebar());
        fill_rrect(pm, 0.0, h - 2.0 * r, SIDEBAR_W, 2.0 * r, r, colors::sidebar());
        fill_rect(pm, SIDEBAR_W - r, h - 2.0 * r, r, 2.0 * r, colors::sidebar());
        line(pm, SIDEBAR_W, TOOLBAR_H, SIDEBAR_W, h, 1.0, colors::sidebar_edge());

        for (i, res) in Resource::ALL.into_iter().enumerate() {
            let (cx, cy, cw) = (12.0, TOOLBAR_H + 12.0 + i as f32 * 74.0, SIDEBAR_W - 24.0);
            if self.resource == res {
                fill_rrect(pm, cx, cy, cw, 68.0, 10.0, colors::selected_gray());
            } else if inside(self.hover.0, self.hover.1, cx, cy, cw, 68.0) {
                fill_rrect(pm, cx, cy, cw, 68.0, 10.0, colors::hover());
            }
            let (gx, gy, gw, gh) = (cx + 8.0, cy + 10.0, 58.0, 48.0);
            fill_rect(pm, gx, gy, gw, gh, colors::window());
            let (a, b, _) = self.series(res);
            Self::graph(pm, gx, gy, gw, gh, &a, res.color(), true);
            if let Some(b) = b {
                Self::graph(pm, gx, gy, gw, gh, &b, res.color(), false);
            }
            let mut edge = res.color();
            edge.set_alpha(0.6);
            stroke_rrect(pm, gx, gy, gw, gh, 0.0, 1.0, edge);
            self.fonts.draw_text(pm, res.title(), gx + gw + 10.0, cy + 14.0, 14.0, colors::text(), true);
            let summary = self.summary(res);
            let summary = self.fonts.ellipsize(&tr(&summary), cw - gw - 26.0, 11.0, false);
            self.fonts.draw_text(pm, &summary, gx + gw + 10.0, cy + 36.0, 11.0, colors::secondary(), false);
        }

        let res = self.resource;
        let (px, pw) = (SIDEBAR_W + 24.0, w - SIDEBAR_W - 48.0);
        let ty = TOOLBAR_H + 16.0;
        self.fonts.draw_text(pm, res.title(), px, ty, 24.0, colors::text(), true);
        let s = self.snap.clone();
        let subtitle = match res {
            Resource::Cpu => s.cpu_model.clone(),
            Resource::Memory => bytes(s.mem_total),
            _ => String::new(),
        };
        let subtitle = self.fonts.ellipsize(&subtitle, pw - 140.0, 13.0, false);
        self.fonts.draw_text_right(pm, &subtitle, px + pw, ty + 10.0, 13.0, colors::secondary(), false);

        let (a, b, scale) = self.series(res);
        let gy = ty + 64.0;
        let gh = (h - gy - 170.0).max(120.0);
        let top_label = match res {
            Resource::Cpu | Resource::Memory => "100 %".to_string(),
            _ => rate(scale),
        };
        let left_label = match res {
            Resource::Cpu => "Загрузка за 60 секунд",
            Resource::Memory => "Использование памяти",
            Resource::Disk => "Скорость чтения и записи",
            Resource::Network => "Скорость приёма и передачи",
        };
        self.fonts.draw_text(pm, left_label, px, gy - 20.0, 12.0, colors::secondary(), false);
        self.fonts.draw_text_right(pm, &top_label, px + pw, gy - 20.0, 12.0, colors::secondary(), false);
        let color = res.color();
        let mut tint = color;
        tint.set_alpha(0.05);
        fill_rect(pm, px, gy, pw, gh, tint);
        let mut grid = color;
        grid.set_alpha(0.16);
        for i in 1..10 {
            let x = px + pw * i as f32 / 10.0;
            line(pm, x, gy, x, gy + gh, 1.0, grid);
        }
        for i in 1..4 {
            let y = gy + gh * i as f32 / 4.0;
            line(pm, px, y, px + pw, y, 1.0, grid);
        }
        Self::graph(pm, px, gy, pw, gh, &a, color, true);
        if let Some(b) = &b {
            let mut second = color;
            second.set_alpha(0.55);
            Self::graph(pm, px, gy, pw, gh, b, second, false);
        }
        let mut edge = color;
        edge.set_alpha(0.7);
        stroke_rrect(pm, px, gy, pw, gh, 0.0, 1.0, edge);
        self.fonts.draw_text(pm, "60 секунд", px, gy + gh + 6.0, 11.0, colors::tertiary(), false);
        self.fonts.draw_text_right(pm, "0", px + pw, gy + gh + 6.0, 11.0, colors::tertiary(), false);

        let own = s.procs.len();
        let stats: Vec<(&str, String)> = match res {
            Resource::Cpu => vec![
                ("Загрузка", percent(s.cpu)),
                ("Процессов", own.to_string()),
                ("Потоков", s.threads.to_string()),
                ("Логических ядер", s.cores.to_string()),
                ("Время работы", uptime(s.uptime)),
            ],
            Resource::Memory => vec![
                ("Используется", bytes(s.mem_used)),
                ("Доступно", bytes(s.mem_total.saturating_sub(s.mem_used))),
                ("Кэш", bytes(s.mem_cached)),
                ("Всего", bytes(s.mem_total)),
                ("Подкачка", if s.swap_total == 0 { tr("нет").into_owned() } else { format!("{} / {}", bytes(s.swap_used), bytes(s.swap_total)) }),
            ],
            Resource::Disk => vec![("Чтение", rate(s.disk_read)), ("Запись", rate(s.disk_write))],
            Resource::Network => vec![("Приём", rate(s.net_rx)), ("Передача", rate(s.net_tx))],
        };
        let sy = gy + gh + 34.0;
        let col_w = (pw / 3.0).max(150.0);
        for (i, (label, value)) in stats.iter().enumerate() {
            let x = px + (i % 3) as f32 * col_w;
            let y = sy + (i / 3) as f32 * 52.0;
            if (label == &"Чтение" || label == &"Приём") && b.is_some() {
                fill_rrect(pm, x, y + 4.0, 10.0, 3.0, 1.5, color);
            } else if b.is_some() {
                let mut c = color;
                c.set_alpha(0.55);
                fill_rrect(pm, x, y + 4.0, 10.0, 3.0, 1.5, c);
            }
            let lx = if b.is_some() { x + 16.0 } else { x };
            self.fonts.draw_text(pm, label, lx, y, 12.0, colors::secondary(), false);
            self.fonts.draw_text(pm, value, x, y + 18.0, 18.0, colors::text(), true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, name: &str, cpu: f32, mem: u64, own: bool) -> Proc {
        Proc { pid, name: name.into(), cmd: format!("/usr/bin/{name} --flag"), cpu, mem, threads: 1, own, start: pid as u64 }
    }

    fn app() -> App {
        let mut app = App::new();
        app.update(Snapshot {
            procs: vec![
                proc(10, "firefox", 12.0, 900 << 20, true),
                proc(11, "konsole", 0.5, 80 << 20, true),
                proc(12, "feather-de", 3.0, 120 << 20, true),
                proc(1, "systemd", 0.0, 10 << 20, false),
            ],
            mem_total: 16 << 30,
            mem_used: 4 << 30,
            ..Default::default()
        });
        app
    }

    fn pids(app: &App) -> Vec<i32> {
        app.rows.iter().map(|&i| app.snap.procs[i].pid).collect()
    }

    #[test]
    fn sorts_filters_and_shows_other_users_on_request() {
        let mut app = app();
        assert_eq!(pids(&app), [10, 12, 11]);
        app.handle_click(app.columns().1 - 20.0, TOOLBAR_H + 10.0);
        assert_eq!(pids(&app), [10, 12, 11]);
        app.handle_click(20.0, TOOLBAR_H + 10.0);
        assert_eq!(pids(&app), [12, 10, 11]);
        app.all_users = true;
        app.rebuild();
        assert_eq!(pids(&app), [12, 10, 11, 1]);
        for ch in "kon".chars() {
            app.handle_key(ch as u32, Some(&ch.to_string()), false);
        }
        assert_eq!(pids(&app), [11]);
        app.handle_key(key::ESCAPE, None, false);
        assert_eq!(pids(&app).len(), 4);
    }

    #[test]
    fn keyboard_selection_and_critical_confirmation() {
        let mut app = app();
        app.handle_key(key::DOWN, None, false);
        assert_eq!(app.selected, Some(10));
        app.handle_key(key::DOWN, None, false);
        assert_eq!(app.selected, Some(12));
        // feather-de asks first; Escape keeps it alive and sends nothing
        app.handle_key(key::DELETE, None, false);
        assert_eq!(app.confirm.as_ref().map(|c| c.0), Some(12));
        app.handle_key(key::ESCAPE, None, false);
        assert!(app.confirm.is_none() && app.terminating.is_empty());
        app.handle_key(key::END, None, false);
        assert_eq!(app.selected, Some(11));
        app.handle_key(key::HOME, None, false);
        assert_eq!(app.selected, Some(10));
    }

    #[test]
    fn formats_sizes_rates_and_uptime() {
        feather_ui::scale::set_lang(Lang::Ru);
        assert_eq!(bytes(512 << 10), "512 КБ");
        assert_eq!(bytes(1536 << 20), "1,5 ГБ");
        assert_eq!(rate(3.0 * 1048576.0), "3,0 МБ/с");
        assert_eq!(percent(4.25), "4,2 %");
        assert_eq!(uptime(90061), "1:01:01:01");
    }

    #[test]
    fn paints_both_tabs() {
        let mut app = app();
        app.update(Snapshot { cpu: 40.0, disk_read: 5e6, net_rx: 2e5, mem_total: 16 << 30, mem_used: 5 << 30, procs: app.snap.procs.clone(), ..Default::default() });
        let mut pixmap = tiny_skia::Pixmap::new(860, 600).unwrap();
        app.draw(&mut pixmap.as_mut());
        app.set_tab(Tab::Performance);
        for r in Resource::ALL {
            app.resource = r;
            app.draw(&mut pixmap.as_mut());
        }
    }

    /// `FEATHER_MONITOR_PNG=<dir> cargo test -p feather-monitor -- --ignored` paints both tabs
    /// from live /proc data at 2.5× (the user's scale) for a visual check and frame timings
    #[test]
    #[ignore]
    fn preview_png() {
        let dir = std::env::var("FEATHER_MONITOR_PNG").expect("FEATHER_MONITOR_PNG");
        let mut sampler = sample::Sampler::new();
        let mut app = App::new();
        for _ in 0..8 {
            app.update(sampler.sample());
            std::thread::sleep(Duration::from_millis(250));
        }
        app.selected = app.rows.get(1).map(|&i| app.snap.procs[i].pid);
        feather_ui::scale::set_ui_scale(2.5);
        for dark in [false, true] {
            feather_ui::scale::set_appearance(if dark { feather_ui::scale::Appearance::Dark } else { feather_ui::scale::Appearance::Light });
            for tab in [Tab::Processes, Tab::Performance] {
                app.tab = tab;
                let mut pixmap = tiny_skia::Pixmap::new(2150, 1500).unwrap();
                let t = Instant::now();
                app.draw(&mut pixmap.as_mut());
                eprintln!("{tab:?} dark={dark}: {:.2} мс", t.elapsed().as_secs_f64() * 1e3);
                pixmap.save_png(format!("{dir}/{tab:?}-{}.png", if dark { "dark" } else { "light" })).unwrap();
            }
        }
    }
}