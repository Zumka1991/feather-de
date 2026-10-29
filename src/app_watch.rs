//! Installed apps follow the .desktop folders: ~/.local/share/applications and
//! /usr/share/applications are watched with inotify (no polling), a burst of changes causes one
//! re-read on a short thread, and the new list replaces the registry only when it differs.
//! Launchpad, the Dock menu, the switcher, a launch bounce and drags keep registry indices, so
//! while one of them is open the new list waits and is retried once a second.
use crate::{apps::AppRegistry, FeatherCompositor};
use feather_ui::fileops;
use smithay::reexports::calloop::{
    channel,
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, Mode, PostAction,
};
use std::{path::PathBuf, time::Duration};

fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs
}

impl FeatherCompositor {
    pub fn start_app_watch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = channel::channel::<AppRegistry>();
        self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(registry) = event {
                state.apps_listed(registry);
            }
        })?;
        self.apps_tx = Some(tx);
        for dir in app_dirs() {
            // A fresh account has no ~/.local/share/applications yet, and a missing folder
            // cannot be watched: create it once at startup (the system one is left alone).
            if dir.starts_with(std::env::var_os("HOME").unwrap_or_default()) {
                let _ = std::fs::create_dir_all(&dir);
            }
            let Some(mut watcher) = fileops::Watcher::new() else { continue };
            watcher.watch(&dir);
            let fd = watcher.fd();
            let index = self.app_watchers.len();
            self.app_watchers.push(watcher);
            // SAFETY: the watcher (and its fd) lives in the compositor for the whole session
            let source = Generic::new(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }, Interest::READ, Mode::Level);
            self.loop_handle.insert_source(source, move |_, _, state| {
                if state.app_watchers.get(index).is_some_and(fileops::Watcher::drain) {
                    state.schedule_apps_reload();
                }
                Ok(PostAction::Continue)
            })?;
        }
        Ok(())
    }

    /// Installing a package writes many files: one re-read after the burst
    fn schedule_apps_reload(&mut self) {
        if self.apps_reload_armed {
            return;
        }
        self.apps_reload_armed = true;
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(300)), |_, _, state| {
            state.apps_reload_armed = false;
            state.reload_apps();
            TimeoutAction::Drop
        });
    }

    fn reload_apps(&mut self) {
        let Some(tx) = self.apps_tx.clone() else { return };
        let _ = std::thread::Builder::new().name("feather-apps".into()).spawn(move || {
            let _ = tx.send(AppRegistry::new());
        });
    }

    fn apps_listed(&mut self, registry: AppRegistry) {
        // A folder deleted and created again (the user's one on first install) needs a new watch
        for (watcher, dir) in self.app_watchers.iter_mut().zip(app_dirs()) {
            watcher.watch(&dir);
        }
        if registry.apps == self.app_registry.apps {
            self.apps_pending = None;
            return;
        }
        let waiting = self.apps_pending.is_some();
        self.apps_pending = Some(registry);
        if !self.apply_pending_apps() && !waiting {
            self.retry_pending_apps();
        }
    }

    fn retry_pending_apps(&mut self) {
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            if state.apps_pending.is_none() || state.apply_pending_apps() {
                TimeoutAction::Drop
            } else {
                TimeoutAction::ToDuration(Duration::from_secs(1))
            }
        });
    }

    pub fn apps_pending_for_check(&self) -> bool {
        self.apps_pending.is_some()
    }

    /// Nothing on screen holds an index into the current registry
    fn app_indices_free(&self) -> bool {
        let s = &self.shell;
        s.launchpad.is_none()
            && s.dock_menu.is_none()
            && s.open_menu.is_none()
            && s.switcher.is_none()
            && s.bounce.is_none()
            && s.pending_bounce.is_none()
            && !s.spotlight_open
            && self.icon_drag.is_none()
            && !s.desktop.launcher_drop_pending()
    }

    fn apply_pending_apps(&mut self) -> bool {
        if !self.app_indices_free() {
            return false;
        }
        let Some(registry) = self.apps_pending.take() else { return true };
        self.app_registry = registry;
        self.dock_model_dirty = true;
        self.shell.dock_dirty = true;
        self.shell.topbar_dirty = true;
        self.shell.switcher_dirty = true;
        for w in &mut self.windows {
            w.dirty = true;
        }
        self.needs_redraw = true;
        true
    }
}
