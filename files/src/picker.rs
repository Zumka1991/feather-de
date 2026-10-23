//! Native open/save/directory mode, sharing Files' navigation and selection UI.
use super::*;
use feather_ui::picker::{path_bytes, Config, Mode, Selection};
pub(super) struct PickerState {
    pub config: Config,
    pub name: String,
    pub name_focus: bool,
    filter: usize,
    pending: Option<PathBuf>,
    overwrite: Option<PathBuf>,
}
impl App {
    pub fn set_picker(&mut self, config: Config) {
        let name = if config.name.is_empty() && config.mode == Mode::Save {
            "Без названия".into()
        } else {
            config.name.clone()
        };
        let filter = config.filter;
        let focus = config.mode == Mode::Save;
        self.picker = Some(PickerState {
            config,
            name,
            name_focus: focus,
            filter,
            pending: None,
            overwrite: None,
        });
        self.refilter();
        self.dirty = true;
    }
    pub fn window_title(&self) -> String {
        self.picker
            .as_ref()
            .map(|p| p.config.title.clone())
            .unwrap_or_else(|| format!("{} — Файлы", self.title()))
    }
    pub fn picker_height(&self) -> f32 {
        self.picker.as_ref().map_or(0.0, |p| {
            54.0 + if p.config.mode == Mode::Save {
                40.0
            } else {
                0.0
            } + if p.config.filters.is_empty() {
                0.0
            } else {
                30.0
            } + p.config.choices.len() as f32 * 30.0
        })
    }
    pub(super) fn picker_matches(&self, e: &Entry) -> bool {
        self.picker.as_ref().is_none_or(|p| {
            e.is_dir || (p.config.mode != Mode::Directory && p.config.matches(&e.path, p.filter))
        })
    }
    fn picker_result(&self, paths: Vec<PathBuf>) -> Vec<Effect> {
        if paths.is_empty() || paths.len() > 64 {
            return Vec::new();
        }
        let p = self.picker.as_ref().unwrap();
        vec![Effect::Picked(Selection {
            paths: paths.iter().map(|p| path_bytes(p)).collect(),
            filter: p.filter,
            choices: p
                .config
                .choices
                .iter()
                .map(|c| (c.id.clone(), c.selected.clone()))
                .collect(),
        })]
    }
    pub(super) fn picker_accept(&mut self) -> Vec<Effect> {
        let Some(p) = &self.picker else {
            return Vec::new();
        };
        if p.pending.is_some() || self.loading {
            return Vec::new();
        }
        match p.config.mode {
            Mode::Save => {
                let name = p.name.as_str();
                if name.trim().is_empty()
                    || matches!(name, "." | "..")
                    || name.contains('/')
                    || name.contains('\0')
                {
                    self.flash("Введите имя файла без символа /".into());
                    return Vec::new();
                }
                let path = self.dir.join(name);
                if p.overwrite.as_ref() == Some(&path) {
                    return self.picker_result(vec![path]);
                }
                self.picker.as_mut().unwrap().pending = Some(path.clone());
                vec![Effect::CheckSave(path)]
            }
            Mode::Directory => {
                let mut paths: Vec<_> = self
                    .selected_paths()
                    .into_iter()
                    .filter(|p| self.entries.iter().any(|e| e.path == *p && e.is_dir))
                    .take(if p.config.multiple { 65 } else { 1 })
                    .collect();
                if paths.is_empty() {
                    paths.push(self.dir.clone());
                }
                if paths.len() > 64 {
                    self.flash("Можно выбрать не более 64 папок".into());
                    return Vec::new();
                }
                self.picker_result(paths)
            }
            Mode::Open => {
                let selected = self.selected_paths();
                if selected.len() == 1
                    && self
                        .entries
                        .iter()
                        .any(|e| e.path == selected[0] && e.is_dir)
                {
                    return self.navigate(selected[0].clone(), true);
                }
                let multiple = p.config.multiple;
                let paths = selected
                    .into_iter()
                    .filter(|path| self.entries.iter().any(|e| e.path == *path && !e.is_dir))
                    .take(if multiple { 65 } else { 1 })
                    .collect::<Vec<_>>();
                if paths.len() > 64 {
                    self.flash("Можно выбрать не более 64 файлов".into());
                    return Vec::new();
                }
                self.picker_result(paths)
            }
        }
    }
    pub fn save_checked(
        &mut self,
        path: PathBuf,
        result: Result<Option<bool>, String>,
    ) -> Vec<Effect> {
        let Some(p) = self.picker.as_mut() else {
            return Vec::new();
        };
        if p.pending.as_ref() != Some(&path) {
            return Vec::new();
        }
        p.pending = None;
        self.dirty = true;
        match result {
            Err(e) => {
                self.flash(e);
                Vec::new()
            }
            Ok(Some(true)) => {
                p.name = "Без названия".into();
                self.navigate(path, true)
            }
            Ok(Some(false)) => {
                p.overwrite = Some(path);
                Vec::new()
            }
            Ok(None) => self.picker_result(vec![path]),
        }
    }
    pub(super) fn picker_next_filter(&mut self) {
        if let Some(p) = self.picker.as_mut() {
            p.filter = (p.filter + 1) % (p.config.filters.len() + 1);
        }
        self.scroll = 0.0;
        self.refilter();
        self.selected.clear();
    }
    pub(super) fn picker_next_choice(&mut self, i: usize) {
        if let Some(c) = self
            .picker
            .as_mut()
            .and_then(|p| p.config.choices.get_mut(i))
        {
            if c.options.is_empty() {
                c.selected = if c.selected == "true" {
                    "false"
                } else {
                    "true"
                }
                .into();
            } else {
                let next = c
                    .options
                    .iter()
                    .position(|(id, _)| *id == c.selected)
                    .map_or(0, |i| (i + 1) % c.options.len());
                c.selected = c.options[next].0.clone();
            }
        }
    }
    pub(super) fn picker_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let p = self.picker.as_ref()?;
        let top = self.h - self.picker_height();
        if y < top {
            return None;
        }
        if x < SIDEBAR_W {
            return Some(Hit::Toolbar);
        }
        if y >= self.h - 42.0 && y < self.h - 14.0 {
            if x >= self.w - 146.0 && x < self.w - 16.0 {
                return Some(if p.pending.is_none() {
                    Hit::PickerAccept
                } else {
                    Hit::Nothing
                });
            }
            if x >= self.w - 240.0 && x < self.w - 154.0 {
                return Some(Hit::PickerCancel);
            }
        }
        let mut row = top + 8.0;
        if p.config.mode == Mode::Save {
            if y >= row && y < row + 28.0 {
                return Some(Hit::PickerName);
            }
            row += 40.0;
        }
        if !p.config.filters.is_empty() {
            if y >= row && y < row + 24.0 {
                return Some(Hit::PickerFilter);
            }
            row += 30.0;
        }
        for i in 0..p.config.choices.len() {
            if y >= row && y < row + 24.0 {
                return Some(Hit::PickerChoice(i));
            }
            row += 30.0;
        }
        Some(Hit::Nothing)
    }
    pub(super) fn picker_key(
        &mut self,
        sym: smithay_client_toolkit::seat::keyboard::Keysym,
        text: Option<&str>,
        mods: Mods,
    ) -> Option<Vec<Effect>> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        let p = self.picker.as_mut()?;
        if self.search_focus {
            return None;
        }
        if sym == K::Escape {
            if p.overwrite.take().is_some() {
                return Some(Vec::new());
            }
            return Some(vec![Effect::Close]);
        }
        if p.config.mode == Mode::Save && p.name_focus {
            if sym == K::Return || sym == K::KP_Enter {
                return Some(self.picker_accept());
            }
            if mods.ctrl
                && matches!(
                    text.map(str::to_lowercase).as_deref().or(latin(sym)),
                    Some("a" | "ф")
                )
            {
                p.name.clear();
                p.overwrite = None;
                return Some(Vec::new());
            }
            if sym == K::BackSpace {
                p.name.pop();
                p.overwrite = None;
                p.pending = None;
                return Some(Vec::new());
            }
            if !mods.ctrl {
                if let Some(t) = text.filter(|t| !t.chars().any(|c| c.is_control() || c == '/')) {
                    if p.name.len() + t.len() <= 255 {
                        p.name.push_str(t);
                    }
                    p.overwrite = None;
                    p.pending = None;
                    return Some(Vec::new());
                }
            }
        }
        if matches!(sym, K::F2 | K::Delete)
            || mods.ctrl
                && matches!(
                    text.map(str::to_lowercase).as_deref().or(latin(sym)),
                    Some("x" | "ч" | "v" | "м" | "n" | "т")
                )
        {
            return Some(Vec::new());
        }
        if mods.ctrl
            && matches!(
                text.map(str::to_lowercase).as_deref().or(latin(sym)),
                Some("a" | "ф")
            )
            && !p.config.multiple
        {
            if !self.visible.is_empty() {
                self.select_only(self.cursor.unwrap_or(0).min(self.visible.len() - 1));
            }
            return Some(Vec::new());
        }
        None
    }
    pub(super) fn paint_picker(&mut self, pm: &mut PixmapMut) {
        let Some(p) = &self.picker else { return };
        let top = self.h - self.picker_height();
        let x = SIDEBAR_W + 16.0;
        let w = self.w - x - 16.0;
        theme::fill_rect(
            pm,
            SIDEBAR_W,
            top,
            self.w - SIDEBAR_W,
            self.picker_height(),
            colors::window(),
        );
        theme::fill_rect(
            pm,
            SIDEBAR_W,
            top,
            self.w - SIDEBAR_W,
            1.0,
            colors::separator(),
        );
        let mut row = top + 8.0;
        if p.config.mode == Mode::Save {
            theme::fill_rrect(pm, x, row, w, 28.0, 6.0, rgba(245, 245, 248, 255));
            theme::stroke_rrect(
                pm,
                x,
                row,
                w,
                28.0,
                6.0,
                1.0,
                if p.name_focus {
                    colors::accent()
                } else {
                    colors::separator()
                },
            );
            self.fonts.draw_text_clipped(
                pm,
                &p.name,
                x + 10.0,
                row + 8.0,
                w - 20.0,
                12.0,
                colors::text(),
                false,
            );
            row += 40.0;
        }
        if !p.config.filters.is_empty() {
            let label = p
                .config
                .filters
                .get(p.filter)
                .map(|f| f.name.as_str())
                .unwrap_or("Все файлы");
            theme::fill_rrect(pm, x, row, w, 24.0, 6.0, rgba(240, 240, 245, 255));
            self.fonts.draw_text_clipped(
                pm,
                &format!("Тип файлов: {label}  ▾"),
                x + 10.0,
                row + 6.0,
                w - 20.0,
                11.0,
                colors::text(),
                false,
            );
            row += 30.0;
        }
        for c in &p.config.choices {
            let value = c
                .options
                .iter()
                .find(|(id, _)| *id == c.selected)
                .map(|(_, label)| label.as_str())
                .unwrap_or(if c.selected == "true" {
                    "Вкл."
                } else {
                    "Выкл."
                });
            theme::fill_rrect(pm, x, row, w, 24.0, 6.0, rgba(240, 240, 245, 255));
            self.fonts.draw_text_clipped(
                pm,
                &format!("{}: {value}  ▾", c.label),
                x + 10.0,
                row + 6.0,
                w - 20.0,
                11.0,
                colors::text(),
                false,
            );
            row += 30.0;
        }
        let hint = if p.pending.is_some() {
            "Проверка файла…".into()
        } else if p.overwrite.is_some() {
            "Файл существует. Заменить?".into()
        } else {
            format!("Выбрано: {}", self.selected_paths().len())
        };
        self.fonts.draw_text_clipped(
            pm,
            &hint,
            x,
            self.h - 33.0,
            (w - 248.0).max(0.0),
            11.0,
            colors::secondary(),
            false,
        );
        for (bx, bw, label, accent) in [
            (self.w - 240.0, 86.0, "Отмена", false),
            (
                self.w - 146.0,
                130.0,
                if p.overwrite.is_some() {
                    "Заменить"
                } else {
                    &p.config.accept
                },
                true,
            ),
        ] {
            theme::fill_rrect(
                pm,
                bx,
                self.h - 42.0,
                bw,
                28.0,
                7.0,
                if accent {
                    colors::accent()
                } else {
                    rgba(235, 235, 241, 255)
                },
            );
            self.fonts.draw_text_centered(
                pm,
                label,
                bx + bw / 2.0,
                self.h - 34.0,
                12.0,
                if accent {
                    rgba(255, 255, 255, 255)
                } else {
                    colors::text()
                },
                accent,
            );
        }
    }
    pub fn debug_picker_choose(&mut self, path: &Path) -> Vec<Effect> {
        let Some(i) = (0..self.visible.len()).find(|&i| self.entry(i).path == path) else {
            return Vec::new();
        };
        let (x, y, w, h) = self.item_rect(i);
        self.press(BTN_LEFT, x + w / 2.0, y + h / 2.0, Mods::default());
        self.release(BTN_LEFT, x + w / 2.0, y + h / 2.0);
        self.press(BTN_LEFT, self.w - 80.0, self.h - 28.0, Mods::default())
    }
}
