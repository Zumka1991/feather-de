//! Window-scoped input, valid for one action after each observation.

use std::time::{Duration, Instant};

use feather_api::Error;
use smithay::{
    backend::input::{Axis, AxisSource, ButtonState, KeyState},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Physical, Rectangle, SERIAL_COUNTER},
};

use super::{unavailable, Responder};
use crate::{window::ManagedWindow, FeatherCompositor};

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Click { x: u32, y: u32, button: u32, count: u32 },
    Scroll { x: u32, y: u32, steps: i32 },
    Key(Vec<u32>),
    Paste(String, Vec<u32>),
}

#[derive(Clone, Copy)]
pub(super) struct View {
    id: u64,
    token: u64,
    captured: Instant,
    frame: Rectangle<i32, Logical>,
    region: Rectangle<i32, Physical>,
    scale: f64,
}

pub(super) fn validate_keys(keys: &[u32]) -> Result<(), Error> {
    if keys.is_empty() || keys.len() > 4
        || keys.iter().any(|key| !(1..=255).contains(key))
        || keys.iter().enumerate().any(|(i, key)| keys[..i].contains(key))
    {
        return Err(Error::InvalidArgs("Нужно от одного до четырёх разных кодов evdev".into()));
    }
    Ok(())
}

impl FeatherCompositor {
    fn automation_window(&self, id: u64) -> Result<&ManagedWindow, Error> {
        if self.larvis_manual_input {
            return Err(unavailable("Рабочий стол в ручном режиме. Нажмите Cmd + Shift + U, чтобы вернуть управление Ларвису."));
        }
        if id == 0 {
            return Err(Error::InvalidArgs("Укажите конкретный ID окна".into()));
        }
        if !self.api.as_ref().is_some_and(|api| api.automation_enabled) {
            return Err(unavailable("Управление интерфейсом Feather выключено (FEATHER_AUTOMATION=1)"));
        }
        if self.lock.active || self.auth.is_open() || self.mission_open()
            || self.shell.launchpad.is_some() || self.shell.spotlight_open
            || self.shell.control_center_open || self.shell.center_open
            || self.shell.open_menu.is_some() || self.shell.dock_menu.is_some()
            || self.shell.switcher.as_ref().is_some_and(|s| s.visible)
            || self.screenshot_select.is_some() || self.active_drag.is_some()
        {
            return Err(unavailable("Сначала закройте системный диалог или оверлей"));
        }
        let win = &self.windows[self.api_window(id)?];
        let surface = win.wl_surface().ok_or_else(|| unavailable("Нужно окно приложения Wayland или XWayland"))?;
        let focus = self.keyboard.current_focus();
        if !win.is_visible() || !win.activated
            || focus.as_ref().and_then(|focus| self.owner_of_surface(focus).0).as_ref() != Some(surface)
        {
            return Err(unavailable("Окно должно быть на переднем плане и иметь фокус"));
        }
        Ok(win)
    }

    pub(super) fn api_observe(&mut self, id: u64, reply: Responder) {
        if let Err(error) = self.automation_window(id) {
            return reply.send(Err(error));
        }
        let Some(target) = self.window_target(|w| w.id == id) else {
            return reply.send(Err(unavailable("Окно не видно")));
        };
        let Some(api) = self.api.as_mut() else { return };
        if api.last_observation.is_some_and(|time| time.elapsed() < Duration::from_millis(500)) {
            return reply.send(Err(unavailable("Подождите перед следующим снимком")));
        }
        api.last_observation = Some(Instant::now());
        api.next_view = api.next_view.wrapping_add(1);
        let token = api.next_view;
        self.capture_window_for(target, id, token, reply);
    }

    pub(crate) fn api_record_view(&mut self, id: u64, token: u64, region: Rectangle<i32, Physical>) {
        let Ok(win) = self.automation_window(id) else { return };
        let frame = win.frame_rect();
        if let Some(api) = self.api.as_mut() {
            api.view = Some(View { id, token, captured: Instant::now(), frame, region, scale: self.scale_factor });
        }
    }

    pub(super) fn api_window_input(&mut self, token: u64, input: Input) -> Result<(), Error> {
        let api = self.api.as_mut().ok_or_else(stale)?;
        let view = api.view.as_ref().filter(|v| v.token == token).ok_or_else(stale)?;
        let view = *view;
        api.view = None;
        let win = self.automation_window(view.id)?;
        if view.captured.elapsed() > Duration::from_secs(90)
            || view.frame != win.frame_rect() || view.scale != self.scale_factor
        {
            return Err(stale());
        }
        if !self.pressed_buttons.is_empty() || !self.keyboard.pressed_keys().is_empty() || self.pointer_locked() {
            return Err(unavailable("Окно уже получает ввод. Снова осмотрите окно."));
        }
        let time = self.start_time.elapsed().as_millis() as u32;
        match input {
            Input::Click { x, y, button, count } => {
                self.automation_pointer(&view, x, y, time)?;
                let button = [0x110, 0x111, 0x112][(button - 1) as usize];
                let pointer = self.pointer.clone();
                for _ in 0..count {
                    for state in [ButtonState::Pressed, ButtonState::Released] {
                        pointer.button(self, &ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time });
                        pointer.frame(self);
                    }
                }
            }
            Input::Scroll { x, y, steps } => {
                self.automation_pointer(&view, x, y, time)?;
                let pointer = self.pointer.clone();
                let frame = AxisFrame::new(time).source(AxisSource::Wheel)
                    .value(Axis::Vertical, f64::from(steps) * 15.0).v120(Axis::Vertical, steps * 120);
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            Input::Key(keys) => self.automation_keys(&keys, time),
            Input::Paste(text, keys) => {
                let bytes = text.into_bytes();
                self.set_clipboard(vec![
                    ("text/plain;charset=utf-8".into(), bytes.clone()),
                    ("UTF8_STRING".into(), bytes.clone()),
                    ("text/plain".into(), bytes),
                ]);
                self.automation_keys(&keys, time);
            }
        }
        Ok(())
    }

    fn automation_pointer(&mut self, view: &View, x: u32, y: u32, time: u32) -> Result<(), Error> {
        if x >= 1000 || y >= 1000 {
            return Err(Error::InvalidArgs("Координаты на снимке должны быть от 0 до 999".into()));
        }
        if self.pointer.is_grabbed() || self.pointer_capture_suspended && !self.debug_input_only {
            return Err(unavailable("Указатель сейчас захвачен или сеанс потерял фокус"));
        }
        let pos = (
            (f64::from(view.region.loc.x) + (f64::from(x) + 0.5) * f64::from(view.region.size.w) / 1000.0) / view.scale,
            (f64::from(view.region.loc.y) + (f64::from(y) + 0.5) * f64::from(view.region.size.h) / 1000.0) / view.scale,
        ).into();
        let pos: smithay::utils::Point<f64, Logical> = pos;
        if !self.monitor_at(pos.x, pos.y).contains(pos.x, pos.y) {
            return Err(unavailable("Точка за пределами видимых экранов"));
        }
        let win = self.automation_window(view.id)?;
        let under = self.surface_under_pointer(pos.x, pos.y).filter(|(surface, _)| {
            self.owner_of_surface(surface).0.as_ref() == win.wl_surface()
        }).ok_or_else(|| unavailable("Точка перекрыта или находится вне интерфейса окна"))?;
        self.pointer_location = pos;
        self.needs_redraw |= self.larvis_desktop;
        let pointer = self.pointer.clone();
        pointer.motion(self, Some(under), &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        Ok(())
    }

    fn automation_keys(&mut self, keys: &[u32], time: u32) {
        let keyboard = self.keyboard.clone();
        for (state, codes) in [(KeyState::Pressed, keys.to_vec()), (KeyState::Released, keys.iter().rev().copied().collect())] {
            for code in codes {
                // Use the client's focus directly. Shell shortcuts must never run here.
                keyboard.input::<(), _>(self, (code + 8).into(), state, SERIAL_COUNTER.next_serial(), time,
                    |_, _, _| FilterResult::Forward);
            }
        }
    }
}

fn stale() -> Error {
    unavailable("Снимок устарел. Снова осмотрите окно перед вводом.")
}
