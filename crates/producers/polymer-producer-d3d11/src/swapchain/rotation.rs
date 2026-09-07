//! Boundary between native D3D11 hook capture and producer buffer ownership.
use std::sync::OnceLock;
use windows::{
    Win32::Graphics::{Direct3D11::ID3D11Texture2D, Dxgi::DXGI_ERROR_UNSUPPORTED},
    core::{Error, Result},
};

pub trait ResourceRotation: Send + Sync {
    /// # Safety
    /// The caller serializes the immediate context and owns the allocation
    /// selected for the next render lease. Rotation must precede guest access.
    unsafe fn exchange(&self, left: usize, right: usize) -> Result<()>;
}

/// Thread-affine capture scope; dropping it must cancel an unfinished capture.
pub trait RotationCapture {
    fn finish(self: Box<Self>, textures: &[ID3D11Texture2D]) -> Result<Box<dyn ResourceRotation>>;
}

pub trait RotationProvider: Send + Sync {
    fn begin(&self) -> Result<Box<dyn RotationCapture>>;
}

static PROVIDER: OnceLock<&'static dyn RotationProvider> = OnceLock::new();
pub struct RotationHooks;
impl RotationHooks {
    pub fn register(provider: &'static dyn RotationProvider) {
        let _ = PROVIDER.set(provider);
    }
    pub(crate) fn begin() -> Result<Box<dyn RotationCapture>> {
        PROVIDER
            .get()
            .ok_or_else(|| {
                Error::new(
                    DXGI_ERROR_UNSUPPORTED,
                    "D3D11 rotation hooks were not installed before device creation",
                )
            })?
            .begin()
    }
}
