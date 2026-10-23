//! The two ways Feather reaches the screen: nested in another compositor (winit), or directly
//! on the GPU (DRM/KMS). Everything above this — scene, input handling, shell — is shared.

use smithay::backend::{renderer::gles::GlesRenderer, winit::WinitGraphicsBackend};

pub enum Backend {
    Winit(WinitGraphicsBackend<GlesRenderer>),
    Drm(Box<crate::drm_backend::DrmBackend>),
}

impl Backend {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        match self {
            Backend::Winit(b) => b.renderer(),
            Backend::Drm(d) => &mut d.renderer,
        }
    }

    pub fn is_drm(&self) -> bool {
        matches!(self, Backend::Drm(_))
    }
}
