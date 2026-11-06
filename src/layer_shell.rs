//! wlr-layer-shell: panels, launchers, wallpapers and notifications of other desktops' programs
//! (waybar, rofi, wofi, swaybg, mako…). Without such a client there is no work at all. Smithay
//! arranges the surfaces; here is a plain list of them for the scene, the pointer and the
//! keyboard, rebuilt only when a layer surface commits or a monitor changes.
use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface as MappedLayer, PopupManager, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
    },
};

use crate::monitors::Rect;
use crate::FeatherCompositor;

/// A layer surface as the rest of the compositor sees it
#[derive(Clone, Debug)]
pub struct LayerItem {
    pub layer: Layer,
    pub surface: WlSurface,
    /// Top left corner, global logical coordinates
    pub loc: (i32, i32),
    pub monitor: Rect,
    pub keyboard: KeyboardInteractivity,
}

impl WlrLayerShellHandler for FeatherCompositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(&mut self, surface: LayerSurface, output: Option<WlOutput>, _layer: Layer, namespace: String) {
        let wanted = output.as_ref().and_then(Output::from_resource);
        let Some(output) = wanted.or_else(|| self.primary_monitor().map(|m| m.output.clone())) else {
            surface.send_close();
            return;
        };
        // Our own lock and login screens stay in front whatever a client asks for
        let _ = layer_map_for_output(&output).map_layer(&MappedLayer::new(surface, namespace));
        self.sync_layers();
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        for monitor in &self.monitors {
            let mut map = layer_map_for_output(&monitor.output);
            let found = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = found {
                map.unmap_layer(&layer);
            }
        }
        if self.layer_focus.as_ref() == Some(surface.wl_surface()) {
            self.layer_focus = None;
        }
        self.sync_layers();
        self.refresh_focus();
    }
}
delegate_layer_shell!(FeatherCompositor);

impl FeatherCompositor {
    /// Rearranges and rebuilds the list; the Dock and windows make room for exclusive zones
    pub fn sync_layers(&mut self) {
        let mut items = Vec::new();
        let mut insets = (0, 0, 0, 0);
        for monitor in &self.monitors {
            let rect = monitor.info.rect;
            let mut map = layer_map_for_output(&monitor.output);
            map.arrange();
            if rect.x == 0 && rect.y == 0 {
                let zone = map.non_exclusive_zone();
                insets = (zone.loc.x, rect.w - zone.loc.x - zone.size.w, zone.loc.y, rect.h - zone.loc.y - zone.size.h);
            }
            for layer in [Layer::Overlay, Layer::Top, Layer::Bottom, Layer::Background] {
                for mapped in map.layers_on(layer).rev() {
                    let Some(geometry) = map.layer_geometry(mapped) else { continue };
                    items.push(LayerItem {
                        layer,
                        surface: mapped.wl_surface().clone(),
                        loc: (rect.x + geometry.loc.x, rect.y + geometry.loc.y),
                        monitor: rect,
                        keyboard: mapped.cached_state().keyboard_interactivity,
                    });
                }
            }
        }
        self.shell.layers = items;
        self.needs_redraw = true;
        if crate::theme::set_layer_insets(insets) {
            self.refit_maximized_windows();
        }
    }

    /// A surface committed. True when it was a layer surface.
    pub fn layer_commit(&mut self, surface: &WlSurface) -> bool {
        for monitor in &self.monitors {
            let map = layer_map_for_output(&monitor.output);
            let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() else { continue };
            drop(map);
            self.sync_layers();
            let configured = with_states(surface, |states| states.data_map.get::<LayerSurfaceData>().is_none_or(|d| d.lock().unwrap().initial_configure_sent));
            if !configured {
                layer.layer_surface().send_configure();
            }
            self.refresh_focus();
            return true;
        }
        false
    }

    /// Keyboard: an exclusive surface takes it, an on-demand one once clicked
    pub fn layer_keyboard_target(&self) -> Option<WlSurface> {
        self.shell
            .layers
            .iter()
            .find(|l| l.keyboard == KeyboardInteractivity::Exclusive && matches!(l.layer, Layer::Overlay | Layer::Top))
            .map(|l| l.surface.clone())
            .or_else(|| self.layer_focus.clone().filter(|s| self.shell.layers.iter().any(|l| &l.surface == s)))
    }

    /// The surface of these layers under a point
    pub fn layer_under(&self, layers: &[Layer], px: f64, py: f64) -> Option<(WlSurface, Point<f64, Logical>)> {
        let pos = Point::<f64, Logical>::from((px, py));
        for item in self.shell.layers.iter().filter(|l| layers.contains(&l.layer)) {
            for (popup, offset) in PopupManager::popups_for_surface(&item.surface) {
                let loc = Point::<i32, Logical>::from(item.loc) + offset - popup.geometry().loc;
                if let Some((s, sub)) = smithay::desktop::utils::under_from_surface_tree(popup.wl_surface(), pos, loc, WindowSurfaceType::ALL) {
                    return Some((s, sub.to_f64()));
                }
            }
            if let Some((s, sub)) = smithay::desktop::utils::under_from_surface_tree(&item.surface, pos, item.loc, WindowSurfaceType::ALL) {
                return Some((s, sub.to_f64()));
            }
        }
        None
    }

    /// A press on a panel or launcher in front of everything goes to it, not to our menu bar
    /// or Dock. True when consumed.
    pub fn layer_button(&mut self, px: f64, py: f64) -> Option<WlSurface> {
        if self.shell.layers.is_empty() {
            return None;
        }
        self.layer_under(&[Layer::Overlay, Layer::Top], px, py).map(|(surface, _)| surface)
    }

    /// Pointer press at the top level: gives or takes back an on-demand layer's keyboard
    pub fn layer_focus_on_press(&mut self, target: Option<WlSurface>) {
        let wanted = target.filter(|s| self.shell.layers.iter().any(|l| &l.surface == s && l.keyboard != KeyboardInteractivity::None));
        if wanted != self.layer_focus {
            self.layer_focus = wanted;
            self.refresh_focus();
        }
    }
}
