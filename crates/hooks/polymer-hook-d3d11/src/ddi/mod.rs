//! Driver-supported identity rotation. Application textures, views and command
//! lists remain native; no commands are recorded or translated by Polymer.
mod ffi;
mod installation;
mod registry;
use std::sync::{Arc, atomic::Ordering};
use windows::{
    Win32::Graphics::Dxgi::*,
    core::{Error, HRESULT, Result},
};

use polymer_producer_d3d11::swapchain::rotation::{
    ResourceRotation, RotationCapture, RotationHooks, RotationProvider,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
pub(crate) struct Rotation;
struct CapturedRotation {
    identities: Vec<ResourceIdentity>,
    _textures: Vec<ID3D11Texture2D>,
}
static ROTATION: Rotation = Rotation;
impl RotationProvider for Rotation {
    fn begin(&self) -> Result<Box<dyn RotationCapture>> {
        Ok(Box::new(registry::Capture::begin()?))
    }
}
impl RotationCapture for registry::Capture {
    fn finish(self: Box<Self>, textures: &[ID3D11Texture2D]) -> Result<Box<dyn ResourceRotation>> {
        let identities = (*self).finish();
        Rotation::validate(&identities, textures.len())?;
        Ok(Box::new(CapturedRotation {
            identities,
            _textures: textures.to_vec(),
        }))
    }
}
impl ResourceRotation for CapturedRotation {
    unsafe fn exchange(&self, left: usize, right: usize) -> Result<()> {
        let left = &self.identities[left];
        let right = &self.identities[right];
        if !Arc::ptr_eq(&left.device, &right.device) || !left.device.alive.load(Ordering::Acquire) {
            return Err(Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED));
        }
        let handles = [left.handle, right.handle];
        let mut args = ffi::Rotate {
            device: left.device.handle,
            resources: handles.as_ptr(),
            count: 2,
        };
        unsafe { HRESULT((left.device.rotate)(&mut args)).ok() }
    }
}
pub(crate) struct ResourceIdentity {
    device: Arc<registry::Device>,
    handle: usize,
}
impl Rotation {
    /// Install before the application creates its D3D11 device.
    pub fn install() -> Result<()> {
        installation::Installation::install()?;
        RotationHooks::register(&ROTATION);
        Ok(())
    }

    pub(crate) fn validate(resources: &[ResourceIdentity], expected: usize) -> Result<()> {
        if resources.len() != expected || resources.is_empty() {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "D3D11 device was not captured before resource creation",
            ));
        }
        let device = &resources[0].device;
        if resources
            .iter()
            .any(|resource| !Arc::ptr_eq(&resource.device, device))
        {
            return Err(Error::new(
                DXGI_ERROR_INVALID_CALL,
                "rotation resources belong to different DDI devices",
            ));
        }
        Ok(())
    }
}
