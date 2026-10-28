//! Window-manager requests that xwayland-satellite drops.
//!
//! Satellite (0.8.3) is the X window manager but only understands fullscreen: the minimize
//! and maximize buttons of programs that draw their own titlebar (Steam, Chromium-based
//! X11 apps, Wine) send `WM_CHANGE_STATE` / `_NET_WM_STATE` to the root window and nothing
//! happens. A second X connection selects SubstructureNotify on the root (any client may,
//! only SubstructureRedirect is exclusive), so those ClientMessages reach us too.
//!
//! It runs in its own thread blocked in `wait_for_event` (no work when idle) and ends
//! when satellite exits. Requests go to the event loop over a channel and are matched to
//! satellite's toplevels by title and WM_CLASS, the same values satellite sends as
//! title/app_id.
use crate::FeatherCompositor;
use smithay::reexports::calloop::{channel, LoopHandle};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, PropMode, Window},
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// ICCCM IconicState in WM_CHANGE_STATE
const ICONIC_STATE: u32 = 3;
const NET_WM_STATE_REMOVE: u32 = 0;
const NET_WM_STATE_ADD: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Minimize,
    /// None: toggle
    Maximize(Option<bool>),
}

#[derive(Debug)]
pub struct Request {
    pub action: Action,
    pub title: String,
    /// WM_CLASS instance and class
    pub classes: Vec<String>,
}

struct Atoms {
    wm_change_state: u32,
    net_wm_state: u32,
    max_vert: u32,
    max_horz: u32,
    hidden: u32,
    net_supported: u32,
    net_wm_name: u32,
    utf8_string: u32,
}

/// Decodes a root ClientMessage into a request for `Action`, if it is one we handle
fn action_for(atoms: &Atoms, type_: u32, data: [u32; 5]) -> Option<Action> {
    if type_ == atoms.wm_change_state {
        return (data[0] == ICONIC_STATE).then_some(Action::Minimize);
    }
    if type_ != atoms.net_wm_state {
        return None;
    }
    let props = [data[1], data[2]];
    if props.contains(&atoms.hidden) && data[0] != NET_WM_STATE_REMOVE {
        return Some(Action::Minimize);
    }
    if props.contains(&atoms.max_vert) || props.contains(&atoms.max_horz) {
        return Some(Action::Maximize(match data[0] {
            NET_WM_STATE_REMOVE => Some(false),
            NET_WM_STATE_ADD => Some(true),
            _ => None,
        }));
    }
    None
}

fn intern(conn: &RustConnection, name: &str) -> Option<u32> {
    Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
}

fn property(conn: &RustConnection, window: Window, atom: u32, type_: u32) -> Vec<u8> {
    conn.get_property(false, window, atom, type_, 0, 256)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.value)
        .unwrap_or_default()
}

/// Satellite rewrites _NET_SUPPORTED when it starts; programs that check it before sending
/// (Chromium does) must see the states we handle
fn advertise(conn: &RustConnection, root: Window, atoms: &Atoms) {
    let current: Vec<u32> = conn
        .get_property(false, root, atoms.net_supported, AtomEnum::ATOM, 0, 1024)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(|v| v.collect()))
        .unwrap_or_default();
    // Until satellite has published its list, appending would only be overwritten
    if !current.contains(&atoms.net_wm_state) {
        return;
    }
    let missing: Vec<u32> = [atoms.max_vert, atoms.max_horz, atoms.hidden]
        .into_iter()
        .filter(|a| !current.contains(a))
        .collect();
    if !missing.is_empty() {
        let _ = conn.change_property32(PropMode::APPEND, root, atoms.net_supported, AtomEnum::ATOM, &missing);
        let _ = conn.flush();
    }
}

fn run(display: &str, tx: &channel::Sender<Request>) -> Option<()> {
    let (conn, screen) = RustConnection::connect(Some(display)).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atoms = Atoms {
        wm_change_state: intern(&conn, "WM_CHANGE_STATE")?,
        net_wm_state: intern(&conn, "_NET_WM_STATE")?,
        max_vert: intern(&conn, "_NET_WM_STATE_MAXIMIZED_VERT")?,
        max_horz: intern(&conn, "_NET_WM_STATE_MAXIMIZED_HORZ")?,
        hidden: intern(&conn, "_NET_WM_STATE_HIDDEN")?,
        net_supported: intern(&conn, "_NET_SUPPORTED")?,
        net_wm_name: intern(&conn, "_NET_WM_NAME")?,
        utf8_string: intern(&conn, "UTF8_STRING")?,
    };
    let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE;
    conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(mask)).ok()?;
    advertise(&conn, root, &atoms);
    conn.flush().ok()?;
    loop {
        match conn.wait_for_event().ok()? {
            Event::PropertyNotify(p) if p.window == root && p.atom == atoms.net_supported => advertise(&conn, root, &atoms),
            Event::ClientMessage(m) => {
                let Some(action) = action_for(&atoms, m.type_, m.data.as_data32()) else { continue };
                let mut title = property(&conn, m.window, atoms.net_wm_name, atoms.utf8_string);
                if title.is_empty() {
                    title = property(&conn, m.window, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into());
                }
                let class = property(&conn, m.window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
                let request = Request {
                    action,
                    title: String::from_utf8_lossy(&title).into_owned(),
                    classes: class
                        .split(|b| *b == 0)
                        .filter(|s| !s.is_empty())
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .collect(),
                };
                tracing::debug!(?request, "X11 WM request");
                tx.send(request).ok()?;
            }
            _ => {}
        }
    }
}

/// Starts listening on `display` (satellite was just started for it)
pub fn start(display: String, handle: &LoopHandle<'static, FeatherCompositor>) {
    let (tx, rx) = channel::channel::<Request>();
    let Ok(token) = handle.insert_source(rx, |event, _, state| {
        if let channel::Event::Msg(request) = event {
            state.x11_wm_request(request);
        }
    }) else {
        return;
    };
    let remove = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("feather-x11-wm".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let _ = run(&display, &tx);
        });
    if spawned.is_err() {
        remove.remove(token);
    }
}

impl FeatherCompositor {
    fn x11_wm_request(&mut self, request: Request) {
        let x11 = |w: &crate::window::ManagedWindow| w.mapped && w.wl_surface().is_some_and(crate::xwayland::is_xwayland_surface);
        let class_match = |w: &crate::window::ManagedWindow| request.classes.iter().any(|c| c.eq_ignore_ascii_case(&w.app_id));
        // Topmost first: the window whose button was just clicked is normally in front
        let order: Vec<usize> = (0..self.windows.len()).rev().filter(|&i| x11(&self.windows[i])).collect();
        let by_class: Vec<usize> = order.iter().copied().filter(|&i| class_match(&self.windows[i])).collect();
        let found = order
            .iter()
            .copied()
            .find(|&i| self.windows[i].title == request.title && class_match(&self.windows[i]))
            .or_else(|| order.iter().copied().find(|&i| self.windows[i].title == request.title))
            .or_else(|| (by_class.len() == 1).then(|| by_class[0]));
        let Some(i) = found else {
            tracing::debug!(title = request.title, "X11 WM request for an unknown window");
            return;
        };
        match request.action {
            Action::Minimize => self.minimize_window(i),
            Action::Maximize(on) => {
                let on = on.unwrap_or(!self.windows[i].maximized);
                let r = self.window_monitor(i);
                self.windows[i].resize_anchor = None;
                self.windows[i].maximize_on(on, r);
                if let Some(t) = self.windows[i].toplevel() {
                    if t.is_initial_configure_sent() {
                        t.send_pending_configure();
                    }
                }
                self.needs_redraw = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_steam_buttons() {
        let atoms = Atoms { wm_change_state: 1, net_wm_state: 2, max_vert: 3, max_horz: 4, hidden: 5, net_supported: 6, net_wm_name: 7, utf8_string: 8 };
        assert_eq!(action_for(&atoms, 1, [ICONIC_STATE, 0, 0, 0, 0]), Some(Action::Minimize));
        assert_eq!(action_for(&atoms, 1, [1, 0, 0, 0, 0]), None, "NormalState is not a minimize");
        assert_eq!(action_for(&atoms, 2, [1, 3, 4, 1, 0]), Some(Action::Maximize(Some(true))));
        assert_eq!(action_for(&atoms, 2, [0, 4, 3, 1, 0]), Some(Action::Maximize(Some(false))));
        assert_eq!(action_for(&atoms, 2, [2, 3, 4, 1, 0]), Some(Action::Maximize(None)));
        assert_eq!(action_for(&atoms, 2, [1, 5, 0, 1, 0]), Some(Action::Minimize));
        assert_eq!(action_for(&atoms, 2, [1, 99, 0, 1, 0]), None, "fullscreen stays with satellite");
        assert_eq!(action_for(&atoms, 9, [3, 0, 0, 0, 0]), None);
    }
}
