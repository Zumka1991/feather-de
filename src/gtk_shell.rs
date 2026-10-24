//! `gtk_shell1` v1, only for what macOS-style menus need: GTK tells us where its
//! `org.gtk.Menus` menubar and `org.gtk.Actions` live (set_dbus_properties). We
//! announce the global menu bar capability, so GTK hides the in-window menubar.
//! Later versions (present, request_focus, titlebar_gesture) are deliberately not
//! offered: GTK then keeps handling those itself (xdg-activation, its own headerbar).
use smithay::{
    reexports::wayland_server::{
        backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
        GlobalDispatch, New, Resource,
    },
    wayland::compositor::with_states,
};

use crate::{appmenu::{Endpoint, SurfaceMenu}, FeatherCompositor};

#[allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
pub mod protocol {
    use wayland_server;
    use wayland_server::protocol::*;
    pub mod __interfaces {
        use wayland_server::protocol::__interfaces::*;
        use wayland_server::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/gtk-shell.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/gtk-shell.xml");
}
use protocol::{
    gtk_shell1::{self, GtkShell1},
    gtk_surface1::{self, GtkSurface1},
};

/// GTK_SHELL1_CAPABILITY_GLOBAL_MENU_BAR
const GLOBAL_MENU_BAR: u32 = 2;

pub fn create_global(dh: &DisplayHandle) {
    dh.create_global::<FeatherCompositor, GtkShell1, _>(1, ());
}

impl GlobalDispatch<GtkShell1, ()> for FeatherCompositor {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<GtkShell1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let shell = data_init.init(resource, ());
        shell.capabilities(GLOBAL_MENU_BAR);
    }
}

impl Dispatch<GtkShell1, ()> for FeatherCompositor {
    fn request(state: &mut Self, _: &Client, _: &GtkShell1, request: gtk_shell1::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        match request {
            gtk_shell1::Request::GetGtkSurface { gtk_surface, surface } => {data_init.init(gtk_surface, surface);}
            gtk_shell1::Request::SystemBell { surface } => state.system_bell(surface.and_then(|s|s.data::<WlSurface>().cloned())),
            _ => {} // Activation goes through xdg-activation.
        }
    }
}

impl Dispatch<GtkSurface1, WlSurface> for FeatherCompositor {
    fn request(state: &mut Self, _: &Client, resource: &GtkSurface1, request: gtk_surface1::Request, surface: &WlSurface, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let gtk_surface1::Request::SetDbusProperties { menubar_path, window_object_path, application_object_path, unique_bus_name, .. } = request {
            let endpoint = unique_bus_name
                .zip(menubar_path)
                .and_then(|(service, menubar)| Endpoint::gtk(service, menubar, application_object_path, window_object_path));
            with_states(surface, |states| {
                states.data_map.insert_if_missing_threadsafe(|| SurfaceMenu::new(None));
                let mut menu = states.data_map.get::<SurfaceMenu>().unwrap().lock().unwrap();
                // A KDE appmenu address, when a toolkit offers both, keeps priority
                if menu.as_ref().is_none_or(|(id, _)| *id == resource.id()) {
                    *menu = endpoint.map(|e| (resource.id(), e));
                }
            });
            state.sync_appmenu_focus();
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, resource: &GtkSurface1, surface: &WlSurface) {
        if surface.is_alive() {
            with_states(surface, |states| {
                if let Some(data) = states.data_map.get::<SurfaceMenu>() {
                    let mut data = data.lock().unwrap();
                    if data.as_ref().is_some_and(|(id, _)| *id == resource.id()) {
                        *data = None;
                    }
                }
            });
        }
        state.sync_appmenu_focus();
    }
}
