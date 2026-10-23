//! The Dock Trash: empty/full state from inotify (no polling), opening it in Files,
//! emptying after a confirmation and dropping files onto it. File I/O runs on short threads.
use crate::{shell::DockTarget, FeatherCompositor};
use feather_ui::fileops;
use smithay::reexports::calloop::{
    channel,
    generic::Generic,
    Interest, Mode, PostAction,
};
use std::path::PathBuf;

pub enum Msg {
    State(bool),
    Emptied(usize),
    Trashed(usize),
}

impl FeatherCompositor {
    pub fn start_trash(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = channel::channel::<Msg>();
        self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                state.trash_message(msg);
            }
        })?;
        self.trash_tx = Some(tx);
        if let Some(watcher) = fileops::Watcher::new() {
            let fd = watcher.fd();
            self.trash_watcher = Some(watcher);
            // SAFETY: the watcher (and its fd) lives in the compositor for the whole session
            let source = Generic::new(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }, Interest::READ, Mode::Level);
            self.loop_handle.insert_source(source, |_, _, state| {
                if state.trash_watcher.as_ref().is_some_and(fileops::Watcher::drain) {
                    state.check_trash();
                }
                Ok(PostAction::Continue)
            })?;
        }
        self.check_trash();
        Ok(())
    }

    /// Re-reads "is anything in the trash" off the event loop (it also creates the folders).
    fn check_trash(&self) {
        let Some(tx) = self.trash_tx.clone() else { return };
        let _ = std::thread::Builder::new().name("feather-trash-state".into()).spawn(move || {
            let files = fileops::trash_files_dir();
            let _ = std::fs::create_dir_all(&files);
            let _ = std::fs::create_dir_all(files.with_file_name("info"));
            let _ = tx.send(Msg::State(!fileops::trash_is_empty()));
        });
    }

    fn trash_message(&mut self, msg: Msg) {
        match msg {
            Msg::State(full) => {
                // The folder may have been recreated: (re)attach the watch, a single syscall.
                if let Some(w) = self.trash_watcher.as_mut() {
                    w.watch(&fileops::trash_files_dir());
                }
                if full != self.trash_full {
                    self.trash_full = full;
                    self.dock_model_dirty = true;
                    self.needs_redraw = true;
                }
            }
            Msg::Emptied(failed) => {
                if failed > 0 {
                    self.shell.notify("Корзина очищена не полностью", format!("Не удалось удалить объектов: {failed}"));
                    self.needs_redraw = true;
                }
                self.check_trash();
            }
            Msg::Trashed(failed) => {
                if failed > 0 {
                    self.shell.notify("Не все объекты перемещены в Корзину", format!("Ошибок: {failed}"));
                    self.needs_redraw = true;
                }
            }
        }
    }

    pub fn open_trash(&mut self) {
        let path = fileops::trash_files_dir();
        let quoted = format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
        crate::apps::spawn_wayland_command(&format!("{} {quoted}", crate::apps::files_command()), &self.wayland_socket_name);
    }

    pub fn confirm_empty_trash(&mut self) {
        self.auth.confirm(crate::auth_dialog::Confirm {
            title: "Удалить объекты из Корзины безвозвратно?".into(),
            message: "Это действие нельзя отменить.".into(),
            ok: "Очистить Корзину".into(),
            action: crate::auth_dialog::ConfirmAction::EmptyTrash,
        });
        self.shell.close_menus();
        self.refresh_focus();
        self.needs_redraw = true;
    }

    pub fn empty_trash_now(&mut self) {
        let Some(tx) = self.trash_tx.clone() else { return };
        let _ = std::thread::Builder::new().name("feather-trash-empty".into()).spawn(move || {
            let _ = tx.send(Msg::Emptied(fileops::empty_trash()));
        });
    }

    /// Files dropped on the Dock Trash.
    pub fn trash_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let Some(tx) = self.trash_tx.clone() else { return };
        let trash = fileops::trash_files_dir();
        // Items already in the trash stay where they are.
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| !p.starts_with(&trash)).collect();
        let _ = std::thread::Builder::new().name("feather-trash".into()).spawn(move || {
            let _ = tx.send(Msg::Trashed(fileops::trash(&paths)));
        });
    }

    pub fn over_dock_trash(&self, x: f64, y: f64) -> bool {
        self.shell.dock_target_at(x, y).is_some_and(|(t, _)| t == DockTarget::Trash)
    }
}
