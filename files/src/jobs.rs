//! Copy/move jobs: Finder-like progress cards at the bottom of the window and the sheet that
//! asks what to do with a name already taken in the target folder.
//! The copying itself runs on a worker (`fileops::paste_with`); this side only reads its
//! shared `Progress` while painting.

use super::*;
use feather_ui::fileops::{Conflict, Progress};
use std::sync::{atomic::Ordering, Arc};

/// Cards appear only for pastes still running after this long (no flicker for small ones)
const SHOW_AFTER: Duration = Duration::from_millis(300);
const CARD_H: f32 = 64.0;
const CARD_GAP: f32 = 8.0;

pub(super) struct Job {
    id: u64,
    progress: Arc<Progress>,
    cut: bool,
    count: usize,
    /// Name of the only item, for «Копирование «x» в «y»»
    first: String,
    dest: String,
    started: Instant,
}

pub(super) struct ConflictPrompt {
    job: u64,
    name: String,
    dest: String,
    is_dir: bool,
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// "около 40 с", "около 3 мин"
fn remaining(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("около {} с", (seconds.ceil() as u64).max(1))
    } else {
        format!("около {} мин", (seconds / 60.0).ceil() as u64)
    }
}

impl App {
    /// Starts copying (or moving) `paths` into `dir` on a worker; the card shows its progress
    pub(super) fn start_paste(&mut self, paths: Vec<PathBuf>, dir: PathBuf, cut: bool) -> Vec<Effect> {
        self.next_job += 1;
        let id = self.next_job;
        self.jobs.push(Job {
            id,
            progress: Arc::default(),
            cut,
            count: paths.len(),
            first: paths.first().map(|p| file_name(p)).unwrap_or_default(),
            dest: file_name(&dir),
            started: Instant::now(),
        });
        self.dirty = true;
        vec![Effect::Paste(id, paths, dir, cut)]
    }

    /// Preview renders: a job already on screen with `done` of `total` bytes copied
    #[cfg(test)]
    pub(super) fn debug_job(&mut self, paths: Vec<PathBuf>, dir: PathBuf, cut: bool, done: u64, total: u64, current: &Path) {
        self.start_paste(paths, dir, cut);
        let job = self.jobs.last_mut().unwrap();
        job.started = Instant::now() - Duration::from_secs(12);
        job.progress.total.store(total, Ordering::Relaxed);
        job.progress.done.store(done, Ordering::Relaxed);
        job.progress.debug_set_current(current);
    }

    pub fn job_progress(&self, id: u64) -> Option<Arc<Progress>> {
        self.jobs.iter().find(|j| j.id == id).map(|j| j.progress.clone())
    }

    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The worker copied another chunk: repaint while a card is on screen
    pub fn job_tick(&mut self) {
        if self.visible_jobs().next().is_some() {
            self.dirty = true;
        }
    }

    /// The worker waits for an answer about `target`
    pub fn job_conflict(&mut self, id: u64, target: PathBuf, is_dir: bool) {
        let dest = target.parent().map(file_name).unwrap_or_default();
        self.conflicts.push_back(ConflictPrompt { job: id, name: file_name(&target), dest, is_dir });
        self.dirty = true;
    }

    pub fn paste_done(&mut self, id: u64, failed: usize) -> Vec<Effect> {
        let Some(index) = self.jobs.iter().position(|j| j.id == id) else { return Vec::new() };
        let job = self.jobs.remove(index);
        self.conflicts.retain(|c| c.job != id);
        let what = if job.cut { "Перемещение" } else { "Копирование" };
        if job.progress.cancelled() {
            self.flash(format!("{what} остановлено"));
        } else if failed > 0 {
            self.flash(format!("{what}: не удалось для {failed}"));
        } else {
            self.flash(format!("{} {}", if job.cut { "Перемещено:" } else { "Скопировано:" }, objects(job.count)));
        }
        if self.close_after_jobs && self.jobs.is_empty() {
            return vec![Effect::Close];
        }
        self.reload()
    }

    /// Closing the window would kill the copy: ask first
    pub fn request_close(&mut self) -> Vec<Effect> {
        if self.jobs.is_empty() {
            return vec![Effect::Close];
        }
        self.menu = None;
        self.confirm = Some(Confirm::Close);
        self.dirty = true;
        Vec::new()
    }

    /// «Прервать и закрыть»: the window closes once every worker has cleaned up
    pub(super) fn cancel_all_and_close(&mut self) -> Vec<Effect> {
        self.close_after_jobs = true;
        let ids: Vec<u64> = self.jobs.iter().map(|j| j.id).collect();
        ids.into_iter().flat_map(|id| self.cancel_job(id)).collect()
    }

    pub(super) fn cancel_job(&mut self, id: u64) -> Vec<Effect> {
        let Some(job) = self.jobs.iter().find(|j| j.id == id) else { return Vec::new() };
        job.progress.cancel.store(true, Ordering::Relaxed);
        self.dirty = true;
        // A worker blocked on the sheet must wake up to see the cancel
        let waiting = self.conflicts.iter().any(|c| c.job == id);
        self.conflicts.retain(|c| c.job != id);
        if waiting { vec![Effect::Resolve(id, Conflict::Skip, true)] } else { Vec::new() }
    }

    fn resolve_conflict(&mut self, choice: Conflict) -> Vec<Effect> {
        let Some(prompt) = self.conflicts.pop_front() else { return Vec::new() };
        self.dirty = true;
        vec![Effect::Resolve(prompt.job, choice, self.conflict_all)]
    }

    fn visible_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|j| j.started.elapsed() >= SHOW_AFTER)
    }

    /// Cards stacked up from the bottom-right corner of the content area
    fn job_rects(&self) -> Vec<(u64, (f32, f32, f32, f32))> {
        let (cx, cy, cw, ch) = self.content();
        let w = (cw - 24.0).min(380.0);
        let mut y = cy + ch - 12.0;
        self.visible_jobs()
            .map(|job| {
                y -= CARD_H;
                let rect = (cx + cw - 12.0 - w, y, w, CARD_H);
                y -= CARD_GAP;
                (job.id, rect)
            })
            .collect()
    }

    fn job_cancel_rect((x, y, w, _): (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
        (x + w - 34.0, y + 10.0, 22.0, 22.0)
    }

    /// Sheet, Skip / Keep both / Replace, and the «для всех» checkbox
    #[allow(clippy::type_complexity)]
    fn conflict_rects(&self) -> ((f32, f32, f32, f32), [(f32, f32, f32, f32); 3], (f32, f32, f32, f32)) {
        let (w, h) = (440.0, 176.0);
        let x = SIDEBAR_W + ((self.w - SIDEBAR_W - w) / 2.0).max(8.0);
        let y = TOOLBAR_H + 12.0;
        let (bw, by) = (112.0, y + h - 46.0);
        let buttons = [0, 1, 2].map(|i| (x + w - 20.0 - bw * (3 - i) as f32 - 8.0 * (2 - i) as f32, by, bw, 30.0));
        ((x, y, w, h), buttons, (x + 20.0, y + 96.0, w - 40.0, 20.0))
    }

    /// Hit testing for the sheet (it takes every click) and the cards
    pub(super) fn jobs_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
        if !self.conflicts.is_empty() {
            let (_, [skip, keep, replace], all) = self.conflict_rects();
            return Some(if inside(replace) {
                Hit::ConflictReplace
            } else if inside(keep) {
                Hit::ConflictKeep
            } else if inside(skip) {
                Hit::ConflictSkip
            } else if inside(all) {
                Hit::ConflictAll
            } else {
                Hit::Nothing
            });
        }
        let (id, rect) = self.job_rects().into_iter().find(|&(_, r)| inside(r))?;
        Some(if inside(Self::job_cancel_rect(rect)) { Hit::JobCancel(id) } else { Hit::Nothing })
    }

    /// Clicks on the sheet or a card; `None` lets the click through
    pub(super) fn jobs_press(&mut self, hit: &Hit, button: u32) -> Option<Vec<Effect>> {
        let on_card = || self.job_rects().iter().any(|&(_, (x, y, w, h))| {
            let (px, py) = self.pointer;
            px >= x && px < x + w && py >= y && py < y + h
        });
        if self.conflicts.is_empty() && !matches!(hit, Hit::JobCancel(_)) && !on_card() {
            return None;
        }
        if button != BTN_LEFT {
            return Some(Vec::new());
        }
        Some(match hit {
            Hit::ConflictReplace => self.resolve_conflict(Conflict::Replace),
            Hit::ConflictKeep => self.resolve_conflict(Conflict::KeepBoth),
            Hit::ConflictSkip => self.resolve_conflict(Conflict::Skip),
            Hit::ConflictAll => {
                self.conflict_all = !self.conflict_all;
                self.dirty = true;
                Vec::new()
            }
            Hit::JobCancel(id) => self.cancel_job(*id),
            _ => Vec::new(),
        })
    }

    /// Return keeps both (nothing is lost), Escape skips the item
    pub(super) fn conflict_key(&mut self, sym: smithay_client_toolkit::seat::keyboard::Keysym) -> Option<Vec<Effect>> {
        use smithay_client_toolkit::seat::keyboard::Keysym as K;
        if self.conflicts.is_empty() {
            return None;
        }
        Some(match sym {
            K::Return | K::KP_Enter => self.resolve_conflict(Conflict::KeepBoth),
            K::Escape => self.resolve_conflict(Conflict::Skip),
            _ => Vec::new(),
        })
    }

    pub(super) fn paint_jobs(&mut self, pm: &mut PixmapMut) {
        let rects = self.job_rects();
        for (id, (x, y, w, h)) in rects {
            let Some(job) = self.jobs.iter().find(|j| j.id == id) else { continue };
            let (total, done) = (job.progress.total.load(Ordering::Relaxed), job.progress.done.load(Ordering::Relaxed));
            let cancelled = job.progress.cancelled();
            let verb = if job.cut { "Перемещение" } else { "Копирование" };
            let title = if job.count == 1 {
                format!("{verb} «{}» в «{}»", job.first, job.dest)
            } else {
                format!("{verb} {} в «{}»", objects_genitive(job.count), job.dest)
            };
            let elapsed = job.started.elapsed().as_secs_f64();
            let detail = if cancelled {
                "Остановка…".to_string()
            } else if total == 0 {
                "Подготовка…".to_string()
            } else {
                let mut text = format!("{} из {}", fsops::human_size(done), fsops::human_size(total));
                if elapsed > 1.0 && done > 0 && done < total {
                    let rate = done as f64 / elapsed;
                    text.push_str(&format!(" — осталось {}", remaining((total - done) as f64 / rate)));
                }
                text
            };
            let current = job.progress.current();
            let fraction = if total == 0 { 0.0 } else { (done as f32 / total as f32).clamp(0.0, 1.0) };
            let hovered = self.hover == Hit::JobCancel(id);

            for i in 1..=4 {
                let e = i as f32 * 2.0;
                theme::fill_rrect(pm, x - e, y - e + 4.0, w + 2.0 * e, h + 2.0 * e, 12.0 + e, rgba(0, 0, 0, 6));
            }
            // Opaque under the glass: file names must not show through the text
            theme::fill_rrect(pm, x, y, w, h, 12.0, colors::window());
            theme::fill_rrect(pm, x, y, w, h, 12.0, colors::glass());
            theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 11.5, 1.0, colors::separator());
            self.fonts.draw_text_clipped(pm, &title, x + 14.0, y + 10.0, w - 60.0, 13.0, colors::text(), true);
            let bar_w = w - 60.0;
            theme::fill_rrect(pm, x + 14.0, y + 32.0, bar_w, 6.0, 3.0, colors::control());
            if fraction > 0.0 {
                theme::fill_rrect(pm, x + 14.0, y + 32.0, (bar_w * fraction).max(6.0), 6.0, 3.0, colors::accent());
            }
            let line = if job.count > 1 && !current.is_empty() && !cancelled && total > 0 { format!("{detail} · {current}") } else { detail };
            self.fonts.draw_text_clipped(pm, &line, x + 14.0, y + 44.0, w - 28.0, 11.0, colors::secondary(), false);

            let (bx, by, bw, bh) = Self::job_cancel_rect((x, y, w, h));
            theme::fill_rrect(pm, bx, by, bw, bh, bw / 2.0, if hovered { colors::accent() } else { colors::control() });
            let ink = if hovered { rgba(255, 255, 255, 255) } else { colors::secondary() };
            let (mx, my) = (bx + bw / 2.0, by + bh / 2.0);
            theme::line(pm, mx - 4.0, my - 4.0, mx + 4.0, my + 4.0, 1.6, ink);
            theme::line(pm, mx - 4.0, my + 4.0, mx + 4.0, my - 4.0, 1.6, ink);
        }
    }

    pub(super) fn paint_conflict(&mut self, pm: &mut PixmapMut) {
        let Some(prompt) = self.conflicts.front() else { return };
        let (name, dest, is_dir) = (prompt.name.clone(), prompt.dest.clone(), prompt.is_dir);
        theme::fill_rect(pm, 0.0, 0.0, self.w, self.h, rgba(0, 0, 0, 40));
        let ((x, y, w, h), buttons, (ax, ay, _, _)) = self.conflict_rects();
        for i in 1..=6 {
            let e = i as f32 * 2.0;
            theme::fill_rrect(pm, x - e, y - e + 6.0, w + 2.0 * e, h + 2.0 * e, 14.0 + e, rgba(0, 0, 0, 7));
        }
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::window());
        theme::fill_rrect(pm, x, y, w, h, 14.0, colors::glass());
        theme::stroke_rrect(pm, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 13.5, 1.0, colors::separator());
        let title = format!("«{name}» уже существует");
        let body = if is_dir {
            format!("В папке «{dest}» уже есть папка с таким именем. Если заменить её, всё её содержимое будет удалено.")
        } else {
            format!("В папке «{dest}» уже есть объект с таким именем. Заменить его копируемым или оставить оба?")
        };
        self.fonts.draw_text_clipped(pm, &title, x + 20.0, y + 20.0, w - 40.0, 14.0, colors::text(), true);
        for (i, line) in self.fonts.wrap_two_lines(&body, w - 40.0, 12.0, false).iter().enumerate() {
            self.fonts.draw_text(pm, line, x + 20.0, y + 46.0 + i as f32 * 16.0, 12.0, colors::secondary(), false);
        }
        // «Применить ко всем» checkbox
        theme::fill_rrect(pm, ax, ay + 2.0, 16.0, 16.0, 4.0, if self.conflict_all { colors::accent() } else { colors::control() });
        if !self.conflict_all {
            theme::stroke_rrect(pm, ax + 0.5, ay + 2.5, 15.0, 15.0, 3.5, 1.0, colors::separator());
        }
        if self.conflict_all {
            theme::polyline(pm, &[(ax + 4.0, ay + 10.5), (ax + 7.0, ay + 13.5), (ax + 12.5, ay + 6.5)], 1.8, rgba(255, 255, 255, 255));
        }
        self.fonts.draw_text(pm, "Применить ко всем", ax + 24.0, ay + 3.0, 12.0, colors::text(), false);

        let labels = ["Пропустить", "Оставить оба", "Заменить"];
        for (i, (bx, by, bw, bh)) in buttons.into_iter().enumerate() {
            let primary = i == 1;
            theme::fill_rrect(pm, bx, by, bw, bh, bh / 2.0, if primary { colors::accent() } else { colors::control() });
            let ink = if primary { rgba(255, 255, 255, 255) } else { colors::text() };
            self.fonts.draw_text_centered(pm, labels[i], bx + bw / 2.0, by + 7.5, 13.0, ink, false);
        }
    }
}

/// "3 объектов" after «Копирование» (genitive plural like Finder's "3 items")
fn objects_genitive(n: usize) -> String {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => format!("{n} объекта"),
        _ => format!("{n} объектов"),
    }
}
