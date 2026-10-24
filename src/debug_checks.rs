//! Opt-in end-to-end checks. Events are injected into this compositor only.
use crate::{
    shell::{DockMenuAction, DockTarget, ShellClickAction, R},
    FeatherCompositor,
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::{cell::Cell, rc::Rc, time::Duration};

fn at(
    state: &FeatherCompositor,
    ms: u64,
    mut f: impl FnMut(&mut FeatherCompositor) + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_millis(ms)),
        move |_, _, state| {
            f(state);
            TimeoutAction::Drop
        },
    )?;
    Ok(())
}

fn tap(st:&mut FeatherCompositor,key:u32){st.debug_keyboard_key(key,true);st.debug_keyboard_key(key,false);}
fn chord(st:&mut FeatherCompositor,modifier:u32,key:u32){st.debug_keyboard_key(modifier,true);tap(st,key);st.debug_keyboard_key(modifier,false);}

/// Fake socket helper for the nested polkit check: accepts only "feather".
fn polkit_fake_helper(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let Ok(clone) = stream.try_clone() else { continue };
            let mut reader = BufReader::new(clone);
            let mut w = stream;
            let mut line = String::new();
            for _ in 0..2 {
                line.clear();
                let _ = reader.read_line(&mut line);
            }
            let _ = writeln!(w, "PAM_PROMPT_ECHO_OFF Password: ");
            line.clear();
            let _ = reader.read_line(&mut line);
            let ok = line.trim_end() == "feather";
            // PAM's failure delay, so the "checking" state is visible.
            std::thread::sleep(Duration::from_millis(if ok { 200 } else { 700 }));
            let _ = writeln!(w, "{}", if ok { "SUCCESS" } else { "FAILURE" });
        }
    });
    Ok(())
}

pub fn install(state: &mut FeatherCompositor) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("FEATHER_DEBUG_LAUNCHER").is_some() {
        // Drag an app out of Launchpad onto the bare desktop: a launcher file appears there
        at(state, 1500, |s| {
            let app = s.app_registry.apps.iter().position(|a| matches!(a.action, crate::apps::AppAction::Exec(_)) && a.id != crate::apps::AppRegistry::PERMANENT).unwrap();
            let entry = s.app_registry.apps[app].clone();
            s.pointer_location = (120.0, 220.0).into();
            s.begin_icon_press(crate::DragSource::Launchpad { app }, crate::shell::AppIcon::of(&entry), entry.name.clone(), crate::shell::ShellClickAction::None);
            for (x, y) in [(135.0, 235.0), (150.0, 260.0)] {
                s.pointer_location = (x, y).into();
                s.update_icon_drag();
            }
            assert!(s.icon_drag.as_ref().unwrap().active);
            s.finish_icon_press();
            eprintln!("[feather-launcher-check] dropped {} on the desktop", entry.name);
        })?;
        at(state, 3200, |s| {
            let item = s.shell.desktop.items.iter().find(|i| i.launcher.is_some()).expect("launcher listed");
            let l = item.launcher.as_ref().unwrap();
            assert!(l.executable && item.name.ends_with(".desktop"));
            eprintln!("[feather-launcher-check] listed «{}» → {}: ok", l.name, crate::desktop::launcher_command(&l.exec));
        })?;
        at(state, 3400, |s| {
            // Over a window the drag becomes a Wayland drag of a launcher file
            let app = s.app_registry.apps.iter().position(|a| a.id == "firefox").unwrap_or(1);
            let entry = s.app_registry.apps[app].clone();
            let w = s.windows.iter().rev().find(|w| w.is_visible()).expect("a window to drag over");
            let over = ((w.x + w.width / 2) as f64, (w.y + 40) as f64);
            s.pointer_location = (over.0 - 20.0, over.1).into();
            s.begin_icon_press(crate::DragSource::Launchpad { app }, crate::shell::AppIcon::of(&entry), entry.name.clone(), crate::shell::ShellClickAction::None);
            s.pointer_location = over.into();
            s.update_icon_drag();
            assert!(s.icon_drag.is_none(), "handed over to the Wayland drag");
            assert!(s.shell.ghost.is_some(), "the icon stays under the pointer");
            let offered = s.shell.desktop.dragged_paths();
            assert!(offered.len() == 1 && offered[0].exists() && offered[0].extension().is_some_and(|e| e == "desktop"));
            // No program took it; back on the bare desktop it becomes a launcher there
            s.desktop_dnd_dropped();
            s.pointer_location = (150.0, 520.0).into();
            s.desktop_dnd_cancelled();
            assert!(s.shell.ghost.is_none());
            eprintln!("[feather-launcher-check] offered {} to programs, dropped back on the desktop", offered[0].display());
        })?;
        at(state, 5200, |s| {
            let n = s.shell.desktop.items.iter().filter(|i| i.launcher.is_some()).count();
            assert_eq!(n, 2, "the second launcher arrived");
            eprintln!("[feather-launcher-check] second launcher listed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_MISSION").is_some() {
        // Mission Control without host input: spread, drag a window onto «+», pick a desktop
        at(state, 1200, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
        })?;
        at(state, 2200, |s| {
            s.open_mission();
            let m = s.mission.as_ref().unwrap();
            let shown = s.windows.iter().filter(|w| w.is_visible()).count();
            assert_eq!(m.targets.len(), shown, "every window of the desktop is in the overview");
            assert!(s.surface_under_pointer(10.0, 300.0).is_none(), "clients get no pointer");
            eprintln!("[feather-mission-check] opened with {shown} windows");
        })?;
        at(state, 3000, |s| {
            let (id, r) = s.mission.as_ref().unwrap().targets[0];
            let add = crate::mission::strip(s.spaces.count, s.shell.screen_w as f32).into_iter().find(|(h, _)| *h == crate::mission::Hit::Add).unwrap().1;
            let (x, y) = ((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.5) as f64);
            s.mission_button(true, x, y);
            s.mission_motion(x + 30.0, y - 30.0);
            let (ax, ay) = ((add.x + add.w * 0.5) as f64, (add.y + add.h * 0.5) as f64);
            s.mission_motion(ax, ay);
            s.mission_button(false, ax, ay);
            let w = s.windows.iter().find(|w| w.id == id).unwrap();
            assert_eq!((w.space, s.spaces.count), (1, 2), "dropped on «+»: a new desktop");
            assert!(s.mission.as_ref().unwrap().targets.iter().all(|(t, _)| *t != id), "left the overview");
            assert!(s.mission_open());
            eprintln!("[feather-mission-check] dragged a window to a new desktop");
        })?;
        at(state, 3600, |s| {
            // × of the new desktop: its window comes back to the first one
            let tile = crate::mission::strip(s.spaces.count, s.shell.screen_w as f32)[1].1;
            s.mission_motion((tile.x + tile.w * 0.5) as f64, (tile.y + tile.h * 0.5) as f64);
            let (x, y) = ((tile.x + 4.0) as f64, (tile.y + 4.0) as f64);
            s.mission_motion(x, y);
            s.mission_button(true, x, y);
            s.mission_button(false, x, y);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 1), "desktop removed");
            assert!(s.windows.iter().filter(|w| w.mapped && !w.minimized).all(|w| w.space == 0 && w.is_visible()));
            assert_eq!(s.mission.as_ref().unwrap().targets.len(), s.windows.iter().filter(|w| w.is_visible()).count(), "its window is back in the overview");
            eprintln!("[feather-mission-check] removed a desktop with ×");
        })?;
        at(state, 4000, |s| {
            let dock = (s.shell.screen_w as f64 * 0.5, s.shell.screen_h as f64 - 30.0);
            assert!(s.shell.dock_contains(dock.0, dock.1));
            s.close_mission(None);
            assert!(!s.mission_open());
        })?;
        at(state, 4800, |s| {
            assert!(s.mission.is_none(), "closed after the spring");
            eprintln!("[feather-mission-check] closed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SPACES").is_some() {
        // Desktops without host input: carry a window right, come back, let the Dock
        // bring its desktop forward, carry it home and watch the empty desktop go away.
        at(state, 1200, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::SystemSettings);
        })?;
        at(state, 1800, |s| {
            let id = s.windows.iter().rev().find(|w| w.is_visible()).map(|w| w.id).unwrap();
            s.space_step(1, true);
            assert_eq!((s.spaces.current, s.spaces.count), (1, 2));
            let w = s.windows.iter().find(|w| w.id == id).unwrap();
            assert!(w.space == 1 && w.is_visible(), "carried window is in front");
            assert!(s.windows.iter().filter(|w| w.id != id && w.mapped).all(|w| !w.is_visible()), "others hidden");
            eprintln!("[feather-spaces-check] carried window to a new desktop");
        })?;
        at(state, 2600, |s| {
            s.space_step(-1, false);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 2), "a desktop with a window stays");
            let w = s.windows.iter().find(|w| w.space == 1).unwrap();
            assert!(!w.is_visible() && s.switcher_items().iter().any(|i| i.key == s.app_key(w)), "hidden but in Alt+Tab");
            let key = s.app_key(w);
            s.activate_app(key);
            assert_eq!(s.spaces.current, 1, "activating its app goes to its desktop");
            eprintln!("[feather-spaces-check] Alt+Tab/Dock bring the window's desktop forward");
        })?;
        at(state, 3400, |s| {
            s.space_step(-1, true);
            assert_eq!((s.spaces.current, s.spaces.count), (0, 1), "the emptied desktop went away");
            assert!(s.windows.iter().filter(|w| w.mapped && !w.minimized).all(|w| w.is_visible()));
            assert!(s.space_step_noop_left(), "nothing left of the first desktop");
            eprintln!("[feather-spaces-check] carried home, empty desktop removed: ok");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_EXTERNAL_LOCK_CHECK").is_some(){
        if state.backend.is_drm() || !state.debug_input_only {return Err("external lock check requires isolated windowed input".into());}
        at(state,2500,|st|{
            assert!(st.lock.external());
            st.debug_pointer_motion(100.0,100.0);st.debug_pointer_button(0x110,true);
            assert!(st.pressed_buttons.contains(&0x110));
            st.debug_pointer_motion(130.0,140.0);st.debug_pointer_button(0x110,false);
            assert!(st.pressed_buttons.is_empty());
            eprintln!("[feather-lock-check] external client receives motion while button held; releases tracked");
        })?;
    }

    if std::env::var_os("FEATHER_DEBUG_LOCK_CHECK").is_some(){
        if state.backend.is_drm() || !state.debug_input_only {return Err("lock check requires isolated windowed input".into());}
        at(state,2000,|st|{
            assert!(!st.lock.active);assert!(!st.windows.is_empty());
            chord(st,125,38); // Super+L through the ordinary keyboard filter
            assert!(st.lock.active);assert!(st.keyboard.current_focus().is_none());
            let layout=st.active_layout_index;chord(st,56,42);assert_ne!(st.active_layout_index,layout);
            chord(st,56,42);assert_eq!(st.active_layout_index,layout);
            assert!(st.surface_under_pointer(200.0,200.0).is_none());
            tap(st,1);chord(st,56,15);tap(st,99);chord(st,125,18);
            assert!(st.lock.active && st.shell.switcher.is_none());
            st.debug_pointer_motion(100.0,100.0);st.debug_pointer_button(0x110,true);st.debug_pointer_button(0x110,false);
            assert!(st.active_drag.is_none());assert!(st.keyboard.current_focus().is_none());
            eprintln!("[feather-lock-check] Super+L captures keyboard/pointer, Escape/Alt+Tab/Print/Super+E cannot escape");
        })?;
        at(state,3000,|st|st.debug_power_check())?;
    }

    if std::env::var_os("FEATHER_DEBUG_DOCK_RESTORE").is_some() {
        assert!(state.dock_locked, "saved Dock lock was not restored");
        let files = state.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap();
        assert_eq!(state.pinned_indices().last(), Some(&files), "saved Files position was not restored");
        at(state, 1000, move |s| {
            let target = DockTarget::App(files);
            let (x, y, size) = s.shell.dock_slot_global(target).unwrap();
            s.open_dock_menu_for(target, R::new(x, y, size, size));
            eprintln!("[feather-dock-restore-check] saved lock and Files position restored");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_DOCK_PINS").is_some() {
        assert!(std::env::var("XDG_CONFIG_HOME").unwrap_or_default().starts_with("/tmp/"));
        fn point(s: &mut FeatherCompositor, target: DockTarget) -> (f64, f64) {
            let items = s.build_dock_items();
            s.shell.set_dock_items(items);
            s.shell.ensure_dock_rendered(&mut s.fonts);
            let (x, y, size) = s.shell.dock_slot_global(target).expect("Dock icon missing");
            ((x + size * 0.5) as f64, (y + size * 0.5) as f64)
        }
        fn menu(s: &mut FeatherCompositor, target: DockTarget, action: DockMenuAction) {
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            // Hover can resize the icon before the press in a real frame, too.
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().expect("right-click did not open menu");
            let row = m.items.iter().position(|(_, a)| *a == action).expect("menu action missing");
            let x = (m.rect.x + m.rect.w * 0.5) as f64;
            let y = (m.rect.y + 5.0 + (row as f32 + 0.5) * ((m.rect.h - 10.0) / m.items.len() as f32)) as f64;
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
            assert!(s.shell.dock_menu.is_none());
        }
        fn drag(s: &mut FeatherCompositor, target: DockTarget, to: (f64, f64)) {
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_motion(to.0, to.1);
            if s.dock_locked {
                assert!(!s.icon_drag.as_ref().unwrap().active);
                assert!(s.shell.dock_gap.is_none());
                assert!(s.shell.ghost.is_none());
            }
            s.debug_pointer_button(0x110, false);
        }
        let app = state.app_registry.index_of("konsole").unwrap();
        let target = DockTarget::App(app);
        state.unpin_app("konsole");
        at(state, 600, move |s| s.launch_app_instance(app))?;
        at(state, 1000, move |s| s.launch_app_instance(app))?;
        at(state, 2000, move |s| {
            assert!(!s.is_pinned(app), "launch pinned an app");
            assert_eq!(s.windows.iter().filter(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)).count(), 2);
            let (x, y) = point(s, target);
            // A small move within the running section must not silently pin the app.
            drag(s, target, (x + 8.0, y));
            assert!(!s.is_pinned(app), "motion within running apps pinned the app");
            s.execute_shell_action(ShellClickAction::CloseFocused);
        })?;
        at(state, 2800, move |s| {
            assert_eq!(s.windows.iter().filter(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)).count(), 1);
            point(s, target);
            s.execute_shell_action(ShellClickAction::MinimizeFocused);
            assert!(s.build_dock_items().iter().any(|i| i.target == target));
            menu(s, target, DockMenuAction::Quit);
        })?;
        at(state, 3600, move |s| {
            assert!(!s.build_dock_items().iter().any(|i| i.target == target), "unpinned app remained after its last window closed");
            eprintln!("[feather-dock-pins-check] two windows, incidental motion, minimize, last close passed");
            s.launch_app_instance(app);
        })?;
        at(state, 4500, move |s| {
            menu(s, target, DockMenuAction::KeepInDock);
            assert!(s.is_pinned(app));
            menu(s, target, DockMenuAction::Quit);
        })?;
        at(state, 5300, move |s| {
            assert!(!s.windows.iter().any(|w| w.mapped && s.app_key(w) == crate::shell::AppKey::App(app)));
            point(s, target);
            menu(s, target, DockMenuAction::RemoveFromDock);
            assert!(!s.build_dock_items().iter().any(|i| i.target == target));
            s.launch_app_instance(app);
            eprintln!("[feather-dock-pins-check] pin survives last close, unpin removes closed app passed");
        })?;
        at(state, 6200, move |s| {
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(s.dock_locked);
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before);
            menu(s, target, DockMenuAction::KeepInDock);
            assert!(s.is_pinned(app), "manual pin should work while dragging is locked");
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before, "locked drag removed pin");
            let first = DockTarget::App(s.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap());
            let to = point(s, first);
            drag(s, target, to);
            assert_eq!(s.dock_pins, before, "locked drag reordered pins");
            menu(s, target, DockMenuAction::RemoveFromDock);
            assert!(!s.is_pinned(app));
            s.execute_shell_action(ShellClickAction::DockPress(target));
            assert!(s.windows.iter().filter(|w| s.app_key(w) == crate::shell::AppKey::App(app)).all(|w| w.minimized));
            s.execute_shell_action(ShellClickAction::DockPress(target));
            assert!(s.windows.iter().filter(|w| s.app_key(w) == crate::shell::AppKey::App(app)).all(|w| !w.minimized));
            s.toggle_launchpad();
        })?;
        at(state, 7100, move |s| {
            let (app, cell) = s.shell.launchpad.as_ref().unwrap().page_cells().into_iter()
                .find(|(i, _)| !s.is_pinned(*i)).unwrap();
            let before = s.dock_pins.clone();
            s.debug_pointer_motion((cell.x + cell.w * 0.5) as f64, (cell.y + 20.0) as f64);
            s.debug_pointer_button(0x110, true);
            assert!(matches!(s.icon_drag.as_ref().unwrap().source, crate::DragSource::Launchpad { app: i } if i == app));
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            assert!(s.shell.ghost.is_none());
            assert!(s.shell.dock_gap.is_none());
            s.debug_pointer_button(0x110, false);
            assert_eq!(s.dock_pins, before, "locked Launchpad drop pinned an app");
            assert!(s.shell.launchpad.is_some(), "blocked drag turned into a launch");
            s.toggle_launchpad();
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(!s.dock_locked);
            eprintln!("[feather-dock-pins-check] locked Dock out/reorder, Launchpad in, clicks and manual pins passed");
        })?;
        at(state, 8000, move |s| {
            menu(s, target, DockMenuAction::KeepInDock);
            drag(s, target, (20.0, 90.0));
            assert!(!s.is_pinned(app), "unlock did not restore drag out");
            s.toggle_launchpad();
        })?;
        at(state, 8800, move |s| {
            let (app, cell) = s.shell.launchpad.as_ref().unwrap().page_cells().into_iter()
                .find(|(i, _)| !s.is_pinned(*i)).unwrap();
            s.debug_pointer_motion((cell.x + cell.w * 0.5) as f64, (cell.y + 20.0) as f64);
            s.debug_pointer_button(0x110, true);
            let (x, y) = point(s, target);
            s.debug_pointer_motion(x, y);
            assert!(s.shell.ghost.is_some());
            s.debug_pointer_button(0x110, false);
            assert!(s.is_pinned(app), "unlock did not restore Launchpad drop");
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(s.dock_locked);
            eprintln!("[feather-dock-pins-check] unlock restores drag in/out; final lock saved");
        })?;
        at(state, 9500, move |s| {
            let (x, y) = point(s, DockTarget::Trash);
            s.debug_pointer_motion(x, y);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().unwrap();
            assert!(m.items.iter().any(|(label, a)| *a == DockMenuAction::ToggleDockLock && label.starts_with('✓')));
            eprintln!("[feather-dock-pins-check] checked lock in Trash menu passed");
        })?;
        at(state, 11000, |s| {
            s.shell.close_menus();
            let p = (0..s.shell.screen_w).flat_map(|x| (s.shell.screen_h - 100..s.shell.screen_h).map(move |y| (x, y)))
                .find(|&(x, y)| s.shell.dock_contains(x as f64, y as f64) && s.shell.dock_target_at(x as f64, y as f64).is_none())
                .expect("Dock background point");
            s.debug_pointer_motion(p.0 as f64, p.1 as f64);
            s.debug_pointer_button(0x111, true);
            s.debug_pointer_button(0x111, false);
            let m = s.shell.dock_menu.as_ref().expect("right-click Dock background");
            assert_eq!(m.items.len(), 1);
            assert_eq!(m.items[0].1, DockMenuAction::ToggleDockLock);
            s.shell.close_menus();
            s.debug_pointer_motion(20.0, 90.0);
        })?;
        at(state, 12000, move |s| {
            let files = s.app_registry.index_of(crate::apps::AppRegistry::PERMANENT).unwrap();
            let target = DockTarget::App(files);
            menu(s, target, DockMenuAction::ToggleDockLock);
            assert!(!s.dock_locked);
            let to = point(s, DockTarget::App(app));
            drag(s, target, (to.0 + 25.0, to.1));
            assert_eq!(s.pinned_indices().last(), Some(&files), "Files forced back to first position");
            let before = s.dock_pins.clone();
            drag(s, target, (20.0, 90.0));
            assert_eq!(s.dock_pins, before, "Files must remain pinned when dragged out");
            menu(s, target, DockMenuAction::ToggleDockLock);
            let to = point(s, DockTarget::App(s.pinned_indices()[0]));
            drag(s, target, to);
            assert_eq!(s.dock_pins, before, "locked Files moved");
            assert_eq!(s.pinned_indices().last(), Some(&files));
            s.debug_pointer_motion(20.0, 90.0);
            eprintln!("[feather-dock-pins-check] Files keeps chosen position, remains pinned, obeys lock");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_TRASH").is_some() {
        // Empties a trash for real: only ever a throwaway XDG_DATA_HOME under /tmp.
        let data = std::env::var("XDG_DATA_HOME").unwrap_or_default();
        assert!(data.starts_with("/tmp/"), "FEATHER_DEBUG_TRASH needs XDG_DATA_HOME under /tmp");
        at(state, 1500, |s| {
            assert!(s.trash_full, "Dock trash should be full");
            s.confirm_empty_trash();
            assert!(s.keyboard.current_focus().is_none());
        })?;
        at(state, 2600, |s| {
            s.debug_keyboard_key(28, true);
            s.debug_keyboard_key(28, false);
        })?;
        at(state, 3600, |s| {
            assert!(!s.auth.is_open());
            assert!(!s.trash_full, "Dock trash should be empty after confirming");
            assert!(feather_ui::fileops::trash_is_empty());
            eprintln!("[feather-trash-check] full icon, confirmation, Enter empties, icon empty");
        })?;
    }
    if let Ok(root) = std::env::var("FEATHER_DEBUG_POLKIT") {
        let socket = std::path::Path::new(&root).join("helper.socket");
        polkit_fake_helper(&socket)?;
        std::env::set_var("FEATHER_POLKIT_HELPER_SOCKET", &socket);
        let agent = crate::polkit_agent::Agent::detached(&state.loop_handle)?;
        state.polkit = Some(agent.clone());
        let me = unsafe { libc::getuid() };
        at(state, 800, move |_| {
            let user = |uid: u32, name: &str, real: &str| crate::polkit_agent::User { uid, name: name.into(), real_name: real.into() };
            agent.inject(crate::polkit_agent::Request {
                cookie: "debug-1".into(),
                action_id: "org.freedesktop.policykit.exec".into(),
                message: "Для запуска «/usr/bin/gparted» от имени суперпользователя требуется аутентификация.".into(),
                details: vec![
                    ("command_line".into(), "/usr/bin/gparted".into()),
                    ("program".into(), "/usr/bin/gparted".into()),
                ],
                users: vec![user(me, "zumka", "Зумка"), user(me + 1, "admin", "")],
                default_user: 0,
            });
        })?;
        // "wrong" + Enter, then "feather" + Enter (evdev codes, US layout).
        for (i, key) in [17u32, 19, 24, 49, 34, 28].into_iter().enumerate() {
            at(state, 1500 + i as u64 * 60, move |s| {
                s.debug_keyboard_key(key, true);
                s.debug_keyboard_key(key, false);
            })?;
        }
        at(state, 3200, |s| {
            assert!(s.auth.is_open(), "dialog closed after a wrong password");
            assert!(s.keyboard.current_focus().is_none(), "a client has keyboard focus under the dialog");
        })?;
        for (i, key) in [33u32, 18, 30, 20, 35, 18, 19, 28].into_iter().enumerate() {
            at(state, 3600 + i as u64 * 60, move |s| {
                s.debug_keyboard_key(key, true);
                s.debug_keyboard_key(key, false);
            })?;
        }
        at(state, 5000, |s| {
            assert!(!s.auth.is_open(), "dialog still open after the right password");
            eprintln!("[feather-polkit-check] wrong password shakes, right password closes, keyboard captured");
        })?;
    }
    if let Ok(root) = std::env::var("FEATHER_DEBUG_DEFAULTS_ROOT") {
        use crate::system_settings::{Action as A, Page};
        fn click(s: &mut FeatherCompositor, p: impl Fn(&A) -> bool) {
            let w = s
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = s
                .system_settings
                .debug_point(w.width as f32, w.height as f32, &s.settings_context(), p)
                .unwrap();
            let point = (
                w.x as f64 + x as f64,
                w.y as f64 + w.titlebar_h() as f64 + y as f64,
            );
            s.debug_pointer_motion(point.0, point.1);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Defaults))
        })?;
        at(state, 1700, |s| {
            click(s, |a| matches!(a, A::AppPicker(true)))
        })?;
        at(state, 2300, |s| {
            click(
                s,
                |a| matches!(a,A::DefaultApp{browser:true,id} if id=="feather-check-browser.desktop"),
            )
        })?;
        at(state, 3100, |s| {
            assert_eq!(
                s.system_preferences.browser.as_deref(),
                Some("feather-check-browser.desktop")
            );
            click(s, |a| matches!(a, A::AppPicker(false)));
        })?;
        at(state, 4000, |s| {
            click(
                s,
                |a| matches!(a,A::DefaultApp{browser:false,id} if id=="feather-check-terminal.desktop"),
            )
        })?;
        at(state, 4800, |s| {
            assert_eq!(
                s.system_preferences.terminal,
                "feather-check-terminal.desktop"
            );
            s.debug_keyboard_key(125, true);
            s.debug_keyboard_key(20, true);
            s.debug_keyboard_key(20, false);
            s.debug_keyboard_key(125, false);
        })?;
        at(state, 6300, |s| {
            assert!(s
                .windows
                .iter()
                .any(|w| w.app_id == "feather-terminal" && w.mapped));
        })?;
        at(state, 7000, move |s| {
            let exe = std::env::current_exe().unwrap();
            let dir = std::path::Path::new(&root).join("working-dir");
            let command = format!(
                "\"{}\" --open-terminal \"{}\"",
                exe.display(),
                dir.display()
            );
            s.execute_shell_action(ShellClickAction::LaunchApp(
                crate::apps::AppAction::Exec(command),
                "Terminal helper check".into(),
            ));
        })?;
        at(state, 9500, |s| {
            assert_eq!(
                s.windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                2
            );
            s.apply_system_setting(A::Page(
                if std::env::var("FEATHER_DEBUG_DEFAULTS_PAGE").as_deref() == Ok("date") {
                    Page::DateTime
                } else {
                    Page::Defaults
                },
            ));
        })?;
        at(state, 10800, |s| {
            assert_eq!(
                s.system_preferences.terminal,
                "feather-check-terminal.desktop"
            );
            assert_eq!(
                s.system_preferences.browser.as_deref(),
                Some("feather-check-browser.desktop")
            );
            eprintln!("[feather-defaults-check] browser associations, terminal hotkey and folder helper passed");
        })?;
    }
    if let Ok(file) = std::env::var("FEATHER_DEBUG_WALLPAPER_FILE") {
        use crate::system_settings::{Action as A, Page};
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Desktop))
        })?;
        for (ms, index) in [(1700, 0), (3100, 1), (4500, 2)] {
            at(state, ms, move |s| {
                s.apply_system_setting(A::WallpaperImage(Some(
                    crate::wallpaper::ImageSource::Bundled(index),
                )));
            })?;
        }
        at(state, 6000, |s| {
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::Bundled(2))
            );
            let w = s
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = s
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &s.settings_context(),
                    |a| matches!(a, A::ChooseWallpaper),
                )
                .unwrap();
            let point = (
                w.x as f64 + x as f64,
                w.y as f64 + w.titlebar_h() as f64 + y as f64,
            );
            s.debug_pointer_motion(point.0, point.1);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
        })?;
        let file_check = file.clone();
        at(state, 8000, move |s| {
            assert!(!s.system_settings.wallpaper_busy);
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::File(
                    file_check.clone().into()
                ))
            );
            s.set_wallpaper_image(Some(crate::wallpaper::ImageSource::File(
                "/missing-feather-wallpaper.png".into(),
            )));
        })?;
        at(state, 10500, move |s| {
            assert_eq!(
                s.shell.applied_wallpaper_image,
                Some(crate::wallpaper::ImageSource::File(file.clone().into()))
            );
            assert_eq!(s.shell.wallpaper_image, s.shell.applied_wallpaper_image);
            assert!(s.system_settings.error.is_some());
            assert!(s.shell.wallpaper_texture.is_some());
            eprintln!("[feather-wallpaper-check] gallery, picker, custom image, failed file keeps wallpaper passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SETTINGS_RESTORE").is_some() {
        at(state, 1400, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            assert_eq!(
                s.keyboard_config.shortcut,
                crate::settings::Shortcut::CtrlSpace
            );
            assert_eq!(s.scale_factor, 2.0);
            assert!(s.animations.enabled());
            s.debug_keyboard_key(29, true);
            s.debug_keyboard_key(57, true);
            s.debug_keyboard_key(57, false);
            s.debug_keyboard_key(29, false);
            assert_eq!(s.active_layout_name, "DE");
            s.apply_system_setting(crate::system_settings::Action::Page(
                crate::system_settings::Page::Keyboard,
            ));
            eprintln!(
                "[feather-settings-check] restored preferences and XKB shortcut after restart"
            );
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_SETTINGS").is_some() {
        use crate::system_settings::{Action as A, Page};
        fn click(state: &mut FeatherCompositor, predicate: impl Fn(&A) -> bool) {
            let w = state
                .windows
                .iter()
                .find(|w| w.app_id == "builtin.settings")
                .unwrap();
            let (x, y) = state
                .system_settings
                .debug_point(
                    w.width as f32,
                    w.height as f32,
                    &state.settings_context(),
                    predicate,
                )
                .expect("settings control");
            let point = (
                w.x as f64 + x as f64,
                (w.y + w.titlebar_h()) as f64 + y as f64,
            );
            state.debug_pointer_motion(point.0, point.1);
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
        }
        at(state, 900, |s| {
            s.apply_system_setting(A::Page(Page::Keyboard))
        })?;
        at(state, 1300, |s| {
            click(s, |a| matches!(a, A::AddPicker(true)))
        })?;
        at(state, 1700, |s| {
            click(s, |a| matches!(a,A::AddSource(id)if id=="de"))
        })?;
        at(state, 2200, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "ru", "de"]);
            assert!(!s.system_settings.keyboard_busy);
            click(s, |a| matches!(a, A::AddPicker(false)));
        })?;
        at(state, 2500, |s| {
            click(s, |a| matches!(a, A::SelectSource(2)));
            assert_eq!(s.active_layout_name, "DE");
        })?;
        at(state, 2800, |s| {
            let x = (1..s.shell.screen_w)
                .find(|x| s.shell.debug_input_hit(*x as f64))
                .unwrap();
            s.debug_pointer_motion(x as f64 + 2.0, 12.0);
            s.debug_pointer_button(0x110, true);
            s.debug_pointer_button(0x110, false);
            assert_eq!(s.shell.open_menu, Some(crate::shell::TopMenu::Input));
        })?;
        at(state, 3300, |s| {
            s.debug_keyboard_key(108, true);
            s.debug_keyboard_key(108, false);
            s.debug_keyboard_key(28, true);
            s.debug_keyboard_key(28, false);
            assert_eq!(s.active_layout_name, "EN");
        })?;
        at(state, 3500, |s| {
            click(s, |a| matches!(a, A::SelectSource(1)))
        })?;
        at(state, 3700, |s| {
            click(s, |a| matches!(a, A::RemoveSource(1)))
        })?;
        at(state, 4200, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            click(s, |a| {
                matches!(a, A::Shortcut(crate::settings::Shortcut::CtrlSpace))
            });
        })?;
        at(state, 4800, |s| {
            assert_eq!(
                s.keyboard_config.shortcut,
                crate::settings::Shortcut::CtrlSpace
            );
            s.debug_keyboard_key(29, true);
            s.debug_keyboard_key(57, true);
            s.debug_keyboard_key(57, false);
            s.debug_keyboard_key(29, false);
            assert_eq!(
                s.active_layout_name, "DE",
                "configured Ctrl+Space did not switch XKB"
            );
            s.apply_system_setting(A::Page(Page::Appearance));
            click(s, |a| matches!(a, A::Animations));
            assert!(!s.animations.enabled());
        })?;
        at(state, 5300, |s| {
            s.open_or_focus_builtin(crate::apps::BuiltinWindowKind::QuickNotes);
            let w = s.windows.last().unwrap();
            assert!(s.animations.window(w).identity());
            assert!(!s.animations.active());
        })?;
        at(state, 5800, |s| {
            s.windows.retain(|w| w.app_id != "builtin.notes");
            s.apply_system_setting(A::Page(Page::Appearance));
            click(s, |a| matches!(a, A::Animations));
            assert!(s.animations.enabled());
        })?;
        at(state, 6300, |s| {
            s.apply_system_setting(A::Page(Page::Desktop));
            click(s, |a| matches!(a, A::DockZoom));
            assert!(!s.shell.dock_zoom_enabled);
        })?;
        at(state, 6800, |s| {
            click(s, |a| matches!(a, A::DockZoom));
            assert!(s.shell.dock_zoom_enabled);
        })?;
        at(state, 7300, |s| {
            s.apply_system_setting(A::Page(Page::Displays));
            click(s, |a| matches!(a,A::Scale(v)if *v==1.25));
            assert_eq!(s.scale_factor, 1.25);
        })?;
        at(state, 7900, |s| {
            click(s, |a| matches!(a,A::Scale(v)if *v==2.0));
            assert_eq!(s.scale_factor, 2.0);
        })?;
        at(state, 8500, |s| {
            let page = match std::env::var("FEATHER_DEBUG_SETTINGS_PAGE").as_deref() {
                Ok("keyboard") | Ok("input") => Page::Keyboard,
                Ok("displays") => Page::Displays,
                Ok("desktop") => Page::Desktop,
                Ok("defaults") => Page::Defaults,
                Ok("date") => Page::DateTime,
                Ok("power") => Page::Power,
                Ok("notifications") => Page::Notifications,
                _ => Page::Appearance,
            };
            s.apply_system_setting(A::Page(page));
            if std::env::var("FEATHER_DEBUG_SETTINGS_PAGE").as_deref() == Ok("input") {
                let x = (1..s.shell.screen_w)
                    .find(|x| s.shell.debug_input_hit(*x as f64))
                    .unwrap();
                s.debug_pointer_motion(x as f64 + 2.0, 12.0);
                s.debug_pointer_button(0x110, true);
                s.debug_pointer_button(0x110, false);
            } else {
                s.debug_pointer_motion(20.0, 60.0);
            }
        })?;
        at(state, 9400, |s| {
            assert_eq!(s.keyboard_config.layouts, ["us", "de"]);
            assert_eq!(s.active_layout_name, "DE");
            assert!(s.animations.enabled());
            assert!(!s.system_settings.keyboard_busy);
            eprintln!("[feather-settings-check] XKB add/remove/menu/Ctrl+Space, animations, Dock and scale passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_TRAY_CHECK").is_some() {
        at(state, 2500, |state| {
            assert_eq!(
                state.shell.tray_items.len(),
                2,
                "Qt tray items did not register"
            );
            let x = (1..state.shell.screen_w)
                .find(|x| state.shell.tray_hit(*x as f64, 12.0).is_some())
                .unwrap();
            state.debug_pointer_motion(x as f64 + 2.0, 12.0);
            state.debug_pointer_button(0x111, true);
            state.debug_pointer_button(0x111, false);
        })?;
        at(state, 3600, |state| {
            assert!(
                !state.shell.tray_open_ids().is_empty(),
                "tray menu did not arrive"
            );
            state.shell.client_menu_key(1);
            let action = state
                .shell
                .client_menu_key(0)
                .expect("tray menu first action");
            state.execute_shell_action(action);
        })?;
        at(state, 4600, |state| {
            let x = (1..state.shell.screen_w)
                .find(|x| state.shell.tray_hit(*x as f64, 12.0).is_some())
                .unwrap();
            state.debug_pointer_motion(x as f64 + 2.0, 12.0);
            state.debug_pointer_button(0x111, true);
            state.debug_pointer_button(0x111, false);
        })?;
        at(state, 6500, |state| {
            state.shell.close_menus();
            state.needs_redraw = true;
        })?;
        at(state, 8700, |state| {
            assert_eq!(
                state.shell.tray_items.len(),
                1,
                "hidden Qt icon did not disappear"
            );
        })?;
        at(state, 9700, |state| {
            assert_eq!(state.shell.tray_items.len(), 2, "Qt icon did not return");
            eprintln!(
                "[feather-tray-check] Qt icons, native menu, action, removal and return passed"
            );
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_LAUNCHPAD_CLICK").is_some() {
        at(state, 1500, |state| {
            state.toggle_launchpad();
            if let Some(lp) = state.shell.launchpad.as_mut() {
                lp.set_query(&state.app_registry, "Терминал".into());
            }
        })?;
        at(state, 2300, |state| {
            let (_, r) = state
                .shell
                .launchpad
                .as_ref()
                .unwrap()
                .page_cells()
                .into_iter()
                .next()
                .unwrap();
            state.debug_pointer_motion((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.4) as f64);
            state.debug_pointer_button(0x110, true);
            assert!(state.shell.launchpad.is_some());
            state.debug_pointer_button(0x110, false);
            assert!(
                state.shell.launchpad.is_none(),
                "Launchpad did not close on app launch"
            );
        })?;
        at(state, 3400, |state| {
            assert!(state
                .windows
                .iter()
                .any(|w| w.app_id == "feather-terminal" && w.is_visible()));
            eprintln!("[feather-launchpad-check] click closed Launchpad and mapped the app");
        })?;
    }
    if std::env::var("FEATHER_DEBUG_DND").as_deref() == Ok("resize-all") {
        let initial_id = Rc::new(Cell::new(None));
        let origin = Rc::new(Cell::new((0.0, 0.0)));
        // Four edges and four corners, shrinking then growing, with real client
        // dispatch/render opportunities between pointer movements.
        for pass in 0..2 {
            for edge in 0..8 {
                let base = 3000 + (pass * 8 + edge) * 600;
                let id = initial_id.clone();
                let press_origin = origin.clone();
                at(state, base, move |state| {
                    let w = state
                        .windows
                        .iter()
                        .find(|w| w.app_id == "feather-terminal")
                        .expect("terminal disappeared");
                    assert!(w.is_visible(), "terminal hidden before resize");
                    assert_eq!(id.get().unwrap_or(w.id), w.id, "terminal was replaced");
                    id.set(Some(w.id));
                    let (l, r, t, b) = (
                        w.x as f64 + 2.0,
                        (w.x + w.width) as f64 - 2.0,
                        w.y as f64 + 2.0,
                        (w.y + w.total_height()) as f64 - 2.0,
                    );
                    let point = match edge {
                        0 => (r, (t + b) * 0.5),
                        1 => (l, (t + b) * 0.5),
                        2 => ((l + r) * 0.5, t),
                        3 => ((l + r) * 0.5, b),
                        4 => (l, t),
                        5 => (r, t),
                        6 => (l, b),
                        _ => (r, b),
                    };
                    press_origin.set(point);
                    state.debug_pointer_motion(point.0, point.1);
                    state.debug_pointer_button(0x110, true);
                })?;
                for step in 1..=8 {
                    let origin = origin.clone();
                    at(state, base + 80 + step * 30, move |state| {
                        assert!(
                            state.active_drag.is_some(),
                            "client did not start edge resize {edge}"
                        );
                        let direction = if pass == 0 { 1.0 } else { -1.0 };
                        let delta = step as f64 * 8.0 * direction;
                        let (dx, dy) = match edge {
                            0 => (-delta, 0.0),
                            1 => (delta, 0.0),
                            2 => (0.0, delta),
                            3 => (0.0, -delta),
                            4 => (delta, delta),
                            5 => (-delta, delta),
                            6 => (delta, -delta),
                            _ => (-delta, -delta),
                        };
                        let p = origin.get();
                        state.debug_pointer_motion(p.0 + dx, p.1 + dy);
                    })?;
                }
                let id = initial_id.clone();
                at(state, base + 420, move |state| {
                    state.debug_pointer_button(0x110, false);
                    let w = state
                        .windows
                        .iter()
                        .find(|w| Some(w.id) == id.get())
                        .expect("terminal destroyed during resize");
                    assert!(w.is_visible(), "terminal hidden during resize");
                    assert!(
                        state.animations.window(w).identity(),
                        "resize restarted appearance animation"
                    );
                    eprintln!("[feather-resize-check] pass={pass} edge={edge} id={} rect={},{},{}x{} mapped={} minimized={}", w.id,w.x,w.y,w.width,w.height,w.mapped,w.minimized);
                })?;
            }
        }
        at(state, 12800, move |state| {
            let w = state
                .windows
                .iter_mut()
                .find(|w| w.app_id == "feather-terminal")
                .expect("terminal disappeared");
            // At scale 2 this is 2,568,432 bytes, a slot with 16 bytes of padding.
            w.resize_client(733, 219);
        })?;
        at(state, 13600, move |state| {
            let w = state
                .windows
                .iter()
                .find(|w| Some(w.id) == initial_id.get())
                .expect("terminal destroyed at unaligned SHM size");
            assert!(w.is_visible());
            assert_eq!((w.width, w.height), (733, 219));
            eprintln!("[feather-resize-check] unaligned SHM size passed");
        })?;
    }
    if std::env::var_os("FEATHER_DEBUG_DOCK").is_some() {
        let app = state
            .app_registry
            .index_of("konsole")
            .expect("terminal registry entry");
        let slow = std::env::var_os("FEATHER_DEBUG_DOCK_SLOW").is_some();
        if slow {
            state.app_registry.apps[app].action = crate::apps::AppAction::Exec(format!(
                "sleep 0.8; exec {}",
                crate::apps::terminal_command()
            ));
        }
        let point = Rc::new(Cell::new((0.0, 0.0)));
        at(state, 2400, move |state| state.launch_app_instance(app))?;
        at(state, 2500, move |state| {
            assert!(
                state.shell.bounce.is_none(),
                "bounce started before the delay"
            );
        })?;
        at(state, 2800, move |state| {
            assert_eq!(
                state.shell.bounce.is_some(),
                slow,
                "fast/slow launch bounce decision"
            );
            eprintln!(
                "[feather-dock-check] slow={slow} bounce={}",
                state.shell.bounce.is_some()
            );
        })?;
        let p = point.clone();
        at(state, 3400, move |state| {
            let (x, y, size) = state
                .shell
                .dock_slot_global(DockTarget::App(app))
                .expect("terminal Dock slot");
            state.debug_pointer_motion((x + size * 0.5) as f64, (y + size * 0.5) as f64);
        })?;
        at(state, 3600, move |state| {
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            p.set(((x + size * 0.5) as f64, (y + size * 0.5) as f64));
        })?;
        for (ms, minimized) in [(3800, true), (4400, false), (5000, true), (5600, false)] {
            let p = point.clone();
            at(state, ms, move |state| {
                let (x, y) = p.get();
                assert_eq!(
                    state.shell.dock_target_at(x, y).map(|(t, _)| t),
                    Some(DockTarget::App(app)),
                    "Dock moved away from the pointer"
                );
                state.debug_pointer_motion(x, y);
                state.debug_pointer_button(0x110, true);
                state.debug_pointer_button(0x110, false);
                assert!(state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal")
                    .all(|w| w.minimized == minimized));
                eprintln!("[feather-dock-check] same-point click minimized={minimized}");
            })?;
        }
        at(state, 6200, move |state| {
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            state.debug_pointer_motion((x + size * 0.5) as f64, (y + size * 0.5) as f64);
            state.debug_pointer_button(0x112, true);
            state.debug_pointer_button(0x112, false);
        })?;
        at(state, 7200, move |state| {
            assert_eq!(
                state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                2
            );
            state.execute_shell_action(ShellClickAction::DockMenu(
                DockMenuAction::NewWindow,
                DockTarget::App(app),
            ));
        })?;
        at(state, 8200, move |state| {
            assert_eq!(
                state
                    .windows
                    .iter()
                    .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                    .count(),
                3
            );
            let mut visited = Vec::new();
            for _ in 0..3 {
                visited.push(
                    state
                        .windows
                        .iter()
                        .rev()
                        .find(|w| w.is_visible())
                        .unwrap()
                        .id,
                );
                state.execute_shell_action(ShellClickAction::DockPress(DockTarget::App(app)));
            }
            visited.sort_unstable();
            visited.dedup();
            assert_eq!(visited.len(), 3, "Dock click did not cycle all windows");
            let ids: Vec<_> = state
                .windows
                .iter()
                .filter(|w| w.app_id == "feather-terminal" && w.mapped)
                .map(|w| w.id)
                .collect();
            for id in ids {
                state.execute_shell_action(ShellClickAction::DockMenu(
                    DockMenuAction::Window(id),
                    DockTarget::App(app),
                ));
                let top = state.windows.iter().rev().find(|w| w.is_visible()).unwrap();
                assert_eq!(
                    top.id, id,
                    "Dock menu activated the app instead of the selected window"
                );
                assert_eq!(state.keyboard.current_focus().as_ref(), top.wl_surface());
            }
            let (x, y, size) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            state.open_dock_menu_for(DockTarget::App(app), R::new(x, y, size, size));
            let menu = state.shell.dock_menu.as_ref().unwrap();
            assert_eq!(
                menu.items
                    .iter()
                    .filter(|(_, a)| matches!(a, DockMenuAction::Window(_)))
                    .count(),
                3
            );
            eprintln!("[feather-dock-check] three windows, cycling, new-window menu and middle click passed");
        })?;
        at(state, 8400, move |state| {
            let r = state.shell.dock_menu.as_ref().unwrap().rect;
            state.debug_pointer_motion((r.x + r.w * 0.5) as f64, (r.y + r.h * 0.5) as f64);
        })?;
        at(state, 9000, move |state| {
            let (_, y, _) = state.shell.dock_slot_global(DockTarget::App(app)).unwrap();
            let r = state.shell.dock_menu.as_ref().unwrap().rect;
            assert!(
                (r.y + r.h + 10.0 - y).abs() < 0.01,
                "menu detached from Dock icon"
            );
            eprintln!("[feather-dock-check] menu anchored after magnification disappeared");
        })?;
        at(state, 9400, move |state| {
            state.shell.close_menus();
            state.debug_pointer_motion(20.0, 60.0);
            assert!(state.shell.bounce.is_none(), "Dock bounce did not finish");
        })?;
        at(state, 10000, move |state| {
            state.shell.open_switcher(state.switcher_items(), false);
            state.shell.switcher.as_mut().unwrap().visible = true;
            state.shell.switcher_dirty = true;
            state.needs_redraw = true;
        })?;
        at(state, 10500, move |state| {
            let sw = state.shell.switcher.as_ref().unwrap();
            let (index, key) = sw
                .items
                .iter()
                .enumerate()
                .find(|(_, i)| Some(i.key) != state.focused_app())
                .map(|(i, a)| (i, a.key))
                .unwrap();
            let r = state.shell.switcher_rect;
            let cell =
                ((state.shell.screen_w as f32 - 108.0) / sw.items.len() as f32).clamp(40.0, 96.0);
            state.debug_pointer_motion(
                (r.x + 14.0 + cell * (index as f32 + 0.5)) as f64,
                (r.y + 14.0 + cell * 0.5) as f64,
            );
            state.debug_pointer_button(0x110, true);
            state.debug_pointer_button(0x110, false);
            assert!(state.shell.switcher.is_none());
            assert_eq!(state.focused_app(), Some(key));
            eprintln!("[feather-dock-check] Alt+Tab icon click activated the chosen app");
        })?;
    }
    Ok(())
}
