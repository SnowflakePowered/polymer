use crate::{D3d11ImageRing, D3d11RingMode};
use snowglass_protocol::AdapterLuid;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_BOX, D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext,
            ID3D11DeviceContext4, ID3D11Texture2D,
        },
        Dxgi::{IDXGIDevice, IDXGISwapChain},
    },
    core::{Error, HRESULT, Interface},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d11CopySubmission {
    pub slot: u32,
    pub sync_value: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub capture_x: u32,
    pub capture_y: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d11CaptureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct D3d11SwapchainCapture {
    swapchain: IDXGISwapChain,
    context: ID3D11DeviceContext,
    context4: Option<ID3D11DeviceContext4>,
    ring: D3d11ImageRing,
    source_desc: D3D11_TEXTURE2D_DESC,
    next_slot: u32,
    sync_value: u64,
    release_values: Vec<u64>,
    adapter_luid: AdapterLuid,
    source_window: isize,
}

impl D3d11SwapchainCapture {
    pub unsafe fn new(
        swapchain: *mut c_void,
        slots: u32,
        mode: D3d11RingMode,
    ) -> windows::core::Result<Self> {
        if swapchain.is_null() || slots == 0 {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        }
        let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        };
        let swapchain = swapchain.clone();
        let device: ID3D11Device = unsafe { swapchain.GetDevice() }
            .map_err(|error| contextual(error, "IDXGISwapChain::GetDevice"))?;
        let context = unsafe { device.GetImmediateContext() }
            .map_err(|error| contextual(error, "ID3D11Device::GetImmediateContext"))?;

        // If we don't have DeviceContext4, fallback to keyed mutexes.
        let context4 = match context.cast::<ID3D11DeviceContext4>() {
            Ok(context4) => Some(context4),
            Err(_) if mode == D3d11RingMode::KeyedMutexNoFence => None,
            Err(error) => return Err(contextual(error, "ID3D11DeviceContext4 query")),
        };
        let source: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0) }
            .map_err(|error| contextual(error, "IDXGISwapChain::GetBuffer"))?;
        let swapchain_desc = unsafe { swapchain.GetDesc() }
            .map_err(|error| contextual(error, "IDXGISwapChain::GetDesc"))?;

        let adapter_luid = match (device_adapter_luid(&device), mode) {
            (Ok(value), _) => value,
            (Err(error), D3d11RingMode::Fence | D3d11RingMode::KeyedMutexNoFence) => {
                return Err(contextual(error, "D3D11 adapter LUID query"));
            }
            // Vulkan keyed mutexes don't need LUID
            (Err(_), D3d11RingMode::KeyedMutex) => AdapterLuid {
                low_part: 0,
                high_part: 0,
            },
        };
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut source_desc) };
        let ring = unsafe { D3d11ImageRing::new(&device, &source_desc, slots, mode) }
            .map_err(|error| contextual(error, "D3D11 image ring creation"))?;
        Ok(Self {
            swapchain,
            context,
            context4,
            ring,
            source_desc,
            next_slot: 0,
            sync_value: 0,
            release_values: vec![0; slots as usize],
            adapter_luid,
            source_window: swapchain_desc.OutputWindow.0 as isize,
        })
    }

    pub unsafe fn copy_frame(
        &mut self,
        tracked_region: Option<D3d11CaptureRegion>,
    ) -> windows::core::Result<D3d11CopySubmission> {
        let source: ID3D11Texture2D = unsafe { self.swapchain.GetBuffer(0)? };
        let region = (self.source_desc.SampleDesc.Count == 1)
            .then_some(tracked_region)
            .flatten()
            .and_then(|region| {
                clamp_region(region, self.source_desc.Width, self.source_desc.Height)
            })
            .unwrap_or(D3d11CaptureRegion {
                x: 0,
                y: 0,
                width: self.source_desc.Width,
                height: self.source_desc.Height,
            });
        let next_value = self.sync_value.wrapping_add(1).max(1);
        let slot = match self.ring.mode() {
            D3d11RingMode::KeyedMutex | D3d11RingMode::KeyedMutexNoFence => self.next_slot,
            // Fence mode has no mutex to block reuse, so skip slots the
            // consumer has not signaled released yet.
            D3d11RingMode::Fence => {
                let consumer_fence = self
                    .ring
                    .consumer_fence()
                    .expect("fence-mode rings carry a consumer fence");
                let completed = unsafe { consumer_fence.GetCompletedValue() };
                let count = self.ring.desc().slots;
                (0..count)
                    .map(|offset| (self.next_slot + offset) % count)
                    .find(|&index| {
                        let release = self.release_values[index as usize];
                        release == 0 || release <= completed
                    })
                    .ok_or_else(|| {
                        Error::new(
                            HRESULT(0x8007_0102u32 as i32),
                            "the consumer has not released any capture slot",
                        )
                    })?
            }
        };
        let ring_slot = &self.ring.slots()[slot as usize];
        let acquire_key = 0;
        if let Some(keyed_mutex) = ring_slot.keyed_mutex.as_ref() {
            // A zero timeout keeps capture non-blocking if the viewer has not released this slot yet.
            let acquire = unsafe {
                (Interface::vtable(keyed_mutex).AcquireSync)(
                    Interface::as_raw(keyed_mutex),
                    acquire_key,
                    0,
                )
            };
            if acquire.0 != 0 {
                if acquire.0 > 0 {
                    return Err(Error::new(
                        HRESULT(0x8007_0102u32 as i32),
                        format!(
                            "IDXGIKeyedMutex::AcquireSync timed out for slot {slot}, key {acquire_key}"
                        ),
                    ));
                }
                return Err(contextual(
                    Error::from_hresult(acquire),
                    "IDXGIKeyedMutex::AcquireSync",
                ));
            }
        }
        let destination = &ring_slot.texture;
        if self.source_desc.SampleDesc.Count > 1 {
            unsafe {
                self.context
                    .ResolveSubresource(destination, 0, &source, 0, self.source_desc.Format)
            };
        } else {
            let source_box = D3D11_BOX {
                left: region.x,
                top: region.y,
                front: 0,
                right: region.x + region.width,
                bottom: region.y + region.height,
                back: 1,
            };
            unsafe {
                self.context.CopySubresourceRegion(
                    destination,
                    0,
                    0,
                    0,
                    0,
                    &source,
                    0,
                    Some(&raw const source_box),
                )
            };
        }
        if let Some(fence) = self.ring.fence() {
            let context4 = self
                .context4
                .as_ref()
                .expect("fence-carrying rings require ID3D11DeviceContext4");
            if let Err(error) = unsafe { context4.Signal(fence, next_value) } {
                if let Some(keyed_mutex) = ring_slot.keyed_mutex.as_ref() {
                    let _ = unsafe { keyed_mutex.ReleaseSync(acquire_key) };
                }
                return Err(contextual(error, "ID3D11DeviceContext4::Signal"));
            }
        }
        if let Some(keyed_mutex) = ring_slot.keyed_mutex.as_ref() {
            unsafe { keyed_mutex.ReleaseSync(1) }
                .map_err(|error| contextual(error, "IDXGIKeyedMutex::ReleaseSync"))?;
        } else {
            self.release_values[slot as usize] = next_value;
        }
        self.sync_value = next_value;
        self.next_slot = (slot + 1) % self.ring.desc().slots;
        Ok(D3d11CopySubmission {
            slot,
            sync_value: self.sync_value,
            width: region.width,
            height: region.height,
            format: self.source_desc.Format.0 as u32,
            capture_x: region.x,
            capture_y: region.y,
        })
    }

    pub fn ring(&self) -> &D3d11ImageRing {
        &self.ring
    }

    pub fn adapter_luid(&self) -> AdapterLuid {
        self.adapter_luid
    }

    pub fn source_window(&self) -> isize {
        self.source_window
    }
}

fn device_adapter_luid(device: &ID3D11Device) -> windows::core::Result<AdapterLuid> {
    let dxgi_device: IDXGIDevice = device.cast()?;
    let adapter = unsafe { dxgi_device.GetAdapter() }?;
    let desc = unsafe { adapter.GetDesc() }?;
    Ok(AdapterLuid {
        low_part: desc.AdapterLuid.LowPart,
        high_part: desc.AdapterLuid.HighPart,
    })
}

fn clamp_region(region: D3d11CaptureRegion, width: u32, height: u32) -> Option<D3d11CaptureRegion> {
    let left = region.x.min(width);
    let top = region.y.min(height);
    let right = region.x.saturating_add(region.width).min(width);
    let bottom = region.y.saturating_add(region.height).min(height);
    (right > left && bottom > top).then_some(D3d11CaptureRegion {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn contextual(error: Error, operation: &str) -> Error {
    Error::new(error.code(), format!("{operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_region_is_clamped_to_the_backbuffer() {
        assert_eq!(
            clamp_region(
                D3d11CaptureRegion {
                    x: 72,
                    y: 0,
                    width: 800,
                    height: 600,
                },
                788,
                530,
            ),
            Some(D3d11CaptureRegion {
                x: 72,
                y: 0,
                width: 716,
                height: 530,
            })
        );
        assert_eq!(
            clamp_region(
                D3d11CaptureRegion {
                    x: 900,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                788,
                530,
            ),
            None
        );
    }
}
