//! Native open/save/directory mode, sharing Files' navigation and selection UI.
use super::*;
use feather_ui::picker::{path_bytes, Config, Mode, Selection};

pub(super) struct NewFolderDialog {
    pub input: feather_ui::input::TextInput,
    pub error: Option<String>,
}

pub(super) struct PickerState {
    pub config: Config,
    pub name: String,
    pub name_focus: bool,
    filter: usize,
    pending: Option<PathBuf>,
    overwrite: Option<PathBuf>,
    new_folder_w: f32,
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
        let new_folder_w = self.measure_new_folder_button_w();
        self.picker = Some(PickerState {
            config,
            name,
            name_focus: focus,
            filter,
            pending: None,
            overwrite: None,
            new_folder_w,
        });
        self.refilter();
        self.dirty = true;
    }
    fn measure_new_folder_button_w(&mut self) -> f32 {
        let text_w = self.fonts.measure_text("Новая папка", 12.0, false);
        let max_w = (self.w - 240.0 - (SIDEBAR_W + 16.0) - 8.0).max(36.0);
        (text_w + 15.0 + 26.0).clamp(104.0_f32.min(max_w), max_w)
    }
    pub(super) fn picker_new_folder_rect(&self) -> (f32, f32, f32, f32) {
        let x = SIDEBAR_W + 16.0;
        let max_w = (self.w - 240.0 - x - 8.0).max(36.0);
        let bw = self
            .picker
            .as_ref()
            .map_or(126.0, |p| p.new_folder_w)
            .min(max_w);
        (x, self.h - 42.0, bw, 28.0)
    }
    pub(super) fn open_new_folder_dialog(&mut self) {
        let base = feather_ui::i18n::tr_static("Новая папка");
        let suggested = fsops::unique_name(&self.dir, base)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| base.to_string());
        let mut input = feather_ui::input::TextInput::new(&suggested);
        input.select_all();
        self.new_folder_dialog = Some(NewFolderDialog { input, error: None });
        self.menu = None;
        self.search_focus = false;
        if let Some(p) = self.picker.as_mut() {
            p.name_focus = false;
        }
        self.dirty = true;
    }
    pub(super) fn cancel_new_folder_dialog(&mut self) {
        self.new_folder_dialog = None;
        if let Some(p) = self.picker.as_mut() {
            if p.config.mode == Mode::Save {
                p.name_focus = true;
            }
        }
        self.dirty = true;
    }
    pub(super) fn submit_new_folder_dialog(&mut self) -> Vec<Effect> {
        let Some(dlg) = self.new_folder_dialog.as_mut() else {
            return Vec::new();
        };
        let name = dlg.input.text().trim().to_string();
        if name.is_empty() {
            return Vec::new();
        }
        if matches!(name.as_str(), "." | "..")
            || name.contains('/')
            || name.contains('\0')
            || name.len() > 255
        {
            dlg.error = Some("Введите имя папки без символа /".into());
            self.dirty = true;
            return Vec::new();
        }
        let target = self.dir.join(&name);
        if std::fs::symlink_metadata(&target).is_ok() {
            dlg.error = Some(format!("«{name}» уже существует"));
            self.dirty = true;
            return Vec::new();
        }
        match std::fs::create_dir(&target) {
            Ok(()) => {
                self.new_folder_dialog = None;
                if let Some(p) = self.picker.as_mut() {
                    p.overwrite = None;
                    p.pending = None;
                    if p.config.mode == Mode::Save {
                        p.name_focus = true;
                    }
                }
                self.flash(format!("Создана папка «{name}»"));
                self.navigate(target, true)
            }
            Err(err) => {
                dlg.error = Some(match err.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        "Нет прав на запись в эту папку".into()
                    }
                    std::io::ErrorKind::AlreadyExists => format!("«{name}» уже существует"),
                    _ => format!("Не удалось создать папку: {err}"),
                });
                self.dirty = true;
                Vec::new()
            }
        }
    }
    pub(super) fn new_folder_rects(
        &self,
    ) -> (
        (f32, f32, f32, f32),
        (f32, f32, f32, f32),
        (f32, f32, f32, f32),
        (f32, f32, f32, f32),
    ) {
        let w = (self.w - SIDEBAR_W - 24.0).clamp(280.0, 392.0);
        let h = 176.0;
        let x = SIDEBAR_W + ((self.w - SIDEBAR_W - w) / 2.0).max(8.0);
        let y = TOOLBAR_H + 16.0;
        let bw = 112.0;
        (
            (x, y, w, h),
            (x + 20.0, y + 72.0, w - 40.0, 30.0),
            (x + w - 20.0 - bw * 2.0 - 10.0, y + h - 46.0, bw, 30.0),
            (x + w - 20.0 - bw, y + h - 46.0, bw, 30.0),
        )
    }
    pub(super) fn open_picker_menu(&mut self, x: f32, y: f32) {
        self.menu_at = (x, y);
        let item = |s: &str, a: MenuAction| (s.to_string(), Some(a));
        let sep = || (String::new(), None);
        let mut items = vec![
            item("Новая папка", MenuAction::NewFolder),
            sep(),
            item(
                if self.show_hidden {
                    "✓ Скрытые файлы"
                } else {
                    "Скрытые файлы"
                },
                MenuAction::ToggleHidden,
            ),
            item(
                if self.view == View::Icons {
                    "✓ Значки"
                } else {
                    "Значки"
                },
                MenuAction::ShowIcons,
            ),
            item(
                if self.view == View::List {
                    "✓ Список"
                } else {
                    "Список"
                },
                MenuAction::ShowList,
            ),
            sep(),
            ("Упорядочить".into(), None),
        ];
        for by in fsops::SortBy::ALL {
            let label = if self.sort.by == by {
                format!("✓ {}", by.menu_label())
            } else {
                by.menu_label().to_string()
            };
            items.push((label, Some(MenuAction::SortBy(by))));
        }
        self.set_menu(x, y, items);
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
            + 30.0
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
    pub(super) fn picker_next_sort(&mut self) {
        const ORDER: [(fsops::SortBy, bool); 8] = [
            (fsops::SortBy::Name, false),
            (fsops::SortBy::Name, true),
            (fsops::SortBy::Modified, true),
            (fsops::SortBy::Modified, false),
            (fsops::SortBy::Size, true),
            (fsops::SortBy::Size, false),
            (fsops::SortBy::Kind, false),
            (fsops::SortBy::Kind, true),
        ];
        let current = (self.sort.by, self.sort.descending);
        let next = ORDER
            .iter()
            .position(|&s| s == current)
            .map_or(ORDER[0], |i| ORDER[(i + 1) % ORDER.len()]);
        self.apply_sort(fsops::Sort {
            by: next.0,
            descending: next.1,
        });
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
            let (nx, _, nw, _) = self.picker_new_folder_rect();
            if x >= nx && x < nx + nw {
                return Some(Hit::PickerNewFolder);
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
        if y >= row && y < row + 24.0 {
            return Some(Hit::PickerSort);
        }
        Some(Hit::Nothing)
    }
    pub(super) fn picker_key(
        &mut self,
        sym: smithay_client_toolkit::seat::keyboard::Keysym,
        letter: Option<char>,
        text: Option<&str>,
        mods: Mods,
    ) -> Option<Vec<Effect>> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        if self.picker.is_none() || self.search_focus {
            return None;
        }
        if sym == K::Escape {
            if self.menu.take().is_some() {
                return Some(Vec::new());
            }
            if self.picker.as_mut().unwrap().overwrite.take().is_some() {
                return Some(Vec::new());
            }
            return Some(vec![Effect::Close]);
        }
        if mods.ctrl && mods.shift && letter == Some('n') {
            self.open_new_folder_dialog();
            return Some(Vec::new());
        }
        let p = self.picker.as_mut()?;
        if p.config.mode == Mode::Save && p.name_focus {
            if sym == K::Return || sym == K::KP_Enter {
                return Some(self.picker_accept());
            }
            if mods.ctrl
                && letter == Some('a')
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
                && matches!(letter, Some('x' | 'v' | 'n'))
        {
            return Some(Vec::new());
        }
        if mods.ctrl
            && letter == Some('a')
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
        if self.picker.is_none() {
            return;
        }
        let new_folder_w = self.measure_new_folder_button_w();
        if let Some(p) = self.picker.as_mut() {
            p.new_folder_w = new_folder_w;
        }
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
            theme::fill_rrect(pm, x, row, w, 28.0, 6.0, colors::control());
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
            let fill = if self.hover == Hit::PickerFilter {
                colors::selected_gray()
            } else {
                colors::control()
            };
            theme::fill_rrect(pm, x, row, w, 24.0, 6.0, fill);
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
        for (i, c) in p.config.choices.iter().enumerate() {
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
            let fill = if self.hover == Hit::PickerChoice(i) {
                colors::selected_gray()
            } else {
                colors::control()
            };
            theme::fill_rrect(pm, x, row, w, 24.0, 6.0, fill);
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
        let arrow = if self.sort.descending { "↓" } else { "↑" };
        let sort_fill = if self.hover == Hit::PickerSort {
            colors::selected_gray()
        } else {
            colors::control()
        };
        theme::fill_rrect(pm, x, row, w, 24.0, 6.0, sort_fill);
        self.fonts.draw_text_clipped(
            pm,
            &format!("Сортировка: {} {arrow}  ▾", self.sort.by.menu_label()),
            x + 10.0,
            row + 6.0,
            w - 20.0,
            11.0,
            colors::text(),
            false,
        );
        // «Новая папка» button on the left of the action bar, like macOS Open/Save panels
        let (nx, ny, nw, nh) = self.picker_new_folder_rect();
        let nf_fill = if self.hover == Hit::PickerNewFolder {
            colors::selected_gray()
        } else {
            colors::control()
        };
        theme::fill_rrect(pm, nx, ny, nw, nh, 7.0, nf_fill);
        draw_new_folder_glyph(pm, nx + 10.0, ny + 7.0, 14.0, colors::text());
        if nw > 46.0 {
            self.fonts.draw_text_clipped(
                pm,
                "Новая папка",
                nx + 29.0,
                ny + 8.0,
                (nw - 37.0).max(0.0),
                12.0,
                colors::text(),
                false,
            );
        }
        let hint = if p.pending.is_some() {
            "Проверка файла…".into()
        } else if p.overwrite.is_some() {
            "Файл существует. Заменить?".into()
        } else {
            format!("Выбрано: {}", self.selected_paths().len())
        };
        let hint_x = nx + nw + 12.0;
        let hint_w = (self.w - 240.0 - 10.0 - hint_x).max(0.0);
        if hint_w > 24.0 {
            self.fonts.draw_text_clipped(
                pm,
                &hint,
                hint_x,
                self.h - 33.0,
                hint_w,
                11.0,
                if p.overwrite.is_some() {
                    rgba(255, 59, 48, 255)
                } else {
                    colors::secondary()
                },
                false,
            );
        }
        for (bx, bw, label, accent, hovered) in [
            (
                self.w - 240.0,
                86.0,
                "Отмена",
                false,
                self.hover == Hit::PickerCancel,
            ),
            (
                self.w - 146.0,
                130.0,
                if p.overwrite.is_some() {
                    "Заменить"
                } else {
                    &p.config.accept
                },
                true,
                self.hover == Hit::PickerAccept && p.pending.is_none(),
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
                } else if hovered {
                    colors::selected_gray()
                } else {
                    colors::control()
                },
            );
            if accent && hovered {
                theme::fill_rrect(pm, bx, self.h - 42.0, bw, 28.0, 7.0, rgba(255, 255, 255, 28));
            }
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
    pub(super) fn paint_new_folder(&mut self, pm: &mut PixmapMut) {
        let Some(dlg) = self.new_folder_dialog.as_ref() else {
            return;
        };
        let input = dlg.input.clone();
        let error = dlg.error.clone();
        theme::fill_rect(pm, 0.0, 0.0, self.w, self.h, rgba(0, 0, 0, 44));
        let ((x, y, w, h), (fx, fy, fw, fh), (cx, cy, cw, ch), (ax, ay, aw, ah)) =
            self.new_folder_rects();
        for i in 1..=6 {
            let e = i as f32 * 2.0;
            theme::fill_rrect(
                pm,
                x - e,
                y - e + 6.0,
                w + 2.0 * e,
                h + 2.0 * e,
                14.0 + e,
                rgba(0, 0, 0, 7),
            );
        }
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::glass());
        theme::stroke_rrect(
            pm,
            x + 0.5,
            y + 0.5,
            w - 1.0,
            h - 1.0,
            13.5,
            1.0,
            colors::separator(),
        );
        self.icons
            .draw(pm, &mut self.fonts, &Icon::Folder(None), x + 20.0, y + 18.0, 38.0);
        self.fonts.draw_text_clipped(
            pm,
            "Новая папка",
            x + 68.0,
            y + 20.0,
            w - 88.0,
            14.5,
            colors::text(),
            true,
        );
        let subtitle = if let Some(err) = error.as_deref() {
            err.to_owned()
        } else {
            let shell_w = self.fonts.measure_text("Имя новой папки в «»", 12.0, false);
            let room = (w - 88.0 - shell_w).max(24.0);
            let dir_name = self.fonts.ellipsize(&self.title(), room, 12.0, false);
            format!("Имя новой папки в «{dir_name}»")
        };
        let sub_ink = if error.is_some() {
            rgba(255, 59, 48, 255)
        } else {
            colors::secondary()
        };
        self.fonts.draw_text_clipped(
            pm,
            &subtitle,
            x + 68.0,
            y + 41.0,
            w - 88.0,
            12.0,
            sub_ink,
            false,
        );
        let a = colors::accent();
        let ring = tiny_skia::Color::from_rgba(a.red(), a.green(), a.blue(), 110.0 / 255.0)
            .unwrap_or(a);
        theme::fill_rrect(pm, fx, fy, fw, fh, 7.0, colors::window());
        theme::stroke_rrect(pm, fx - 1.0, fy - 1.0, fw + 2.0, fh + 2.0, 8.0, 2.5, ring);
        theme::stroke_rrect(pm, fx, fy, fw, fh, 7.0, 0.6, colors::separator());
        if input.is_empty() {
            self.fonts.draw_text_clipped(
                pm,
                "Имя папки",
                fx + 10.0,
                fy + 8.0,
                fw - 20.0,
                13.0,
                colors::tertiary(),
                false,
            );
        }
        input.draw(
            pm,
            &mut self.fonts,
            fx + 10.0,
            fy + 8.0,
            fw - 20.0,
            13.0,
            false,
            colors::text(),
            true,
            colors::accent(),
        );
        let cancel_fill = if self.hover == Hit::NewFolderCancel {
            colors::selected_gray()
        } else {
            colors::control()
        };
        theme::fill_rrect(pm, cx, cy, cw, ch, ch / 2.0, cancel_fill);
        self.fonts.draw_text_centered(
            pm,
            "Отмена",
            cx + cw / 2.0,
            cy + 7.5,
            13.0,
            colors::text(),
            false,
        );
        let empty = input.text().trim().is_empty();
        let ok_col = if empty {
            tiny_skia::Color::from_rgba(a.red(), a.green(), a.blue(), 0.45).unwrap_or(a)
        } else {
            a
        };
        theme::fill_rrect(pm, ax, ay, aw, ah, ah / 2.0, ok_col);
        if !empty && self.hover == Hit::NewFolderOk {
            theme::fill_rrect(pm, ax, ay, aw, ah, ah / 2.0, rgba(255, 255, 255, 36));
        }
        self.fonts.draw_text_centered(
            pm,
            "Создать",
            ax + aw / 2.0,
            ay + 7.5,
            13.0,
            rgba(255, 255, 255, 255),
            false,
        );
    }
    pub fn debug_picker_choose(&mut self, path: &Path) -> Vec<Effect> {
        if self.picker.as_ref().is_some_and(|p| p.config.mode == Mode::Directory) && self.dir == path {
            return self.press(BTN_LEFT, self.w - 80.0, self.h - 28.0, Mods::default());
        }
        let Some(i) = (0..self.visible.len()).find(|&i| self.entry(i).path == path) else {
            return Vec::new();
        };
        let (x, y, w, h) = self.item_rect(i);
        self.press(BTN_LEFT, x + w / 2.0, y + h / 2.0, Mods::default());
        self.release(BTN_LEFT, x + w / 2.0, y + h / 2.0);
        self.press(BTN_LEFT, self.w - 80.0, self.h - 28.0, Mods::default())
    }
}

/// Crisp Tahoe-style folder outline with a centered "+" for the picker's New Folder button
fn draw_new_folder_glyph(
    pm: &mut PixmapMut,
    x: f32,
    y: f32,
    size: f32,
    color: tiny_skia::Color,
) {
    let w = (size * 0.09).max(1.1);
    let p = |fx: f32, fy: f32| (x + fx * size, y + fy * size);
    theme::polyline(
        pm,
        &[
            p(0.06, 0.86),
            p(0.06, 0.16),
            p(0.40, 0.16),
            p(0.50, 0.28),
            p(0.94, 0.28),
            p(0.94, 0.86),
            p(0.06, 0.86),
        ],
        w,
        color,
    );
    theme::line(
        pm,
        p(0.35, 0.57).0,
        p(0.35, 0.57).1,
        p(0.65, 0.57).0,
        p(0.65, 0.57).1,
        w,
        color,
    );
    theme::line(
        pm,
        p(0.50, 0.42).0,
        p(0.50, 0.42).1,
        p(0.50, 0.72).0,
        p(0.50, 0.72).1,
        w,
        color,
    );
}
