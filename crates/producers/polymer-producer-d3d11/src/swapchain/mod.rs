use crate::{D3d11ImageRing, D3d11RingMode, TrackedResource};
use polymer_protocol::AdapterLuid;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_BOX, D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext,
            ID3D11DeviceContext4, ID3D11Texture2D,
        },
        Dxgi::{Common::DXGI_FORMAT, IDXGIDevice, IDXGISwapChain},
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
    // Borrowed from the swapchain hook owner. Owning a COM reference here
    // would keep that owner alive through its own capture state.
    swapchain: usize,
    context: ID3D11DeviceContext,
    context4: Option<ID3D11DeviceContext4>,
    ring: D3d11ImageRing,
    source_desc: D3D11_TEXTURE2D_DESC,
    resolved_source: Option<ID3D11Texture2D>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    sync_value: u64,
    release_values: Vec<u64>,
    submitted_values: Vec<u64>,
    submitted_sources: Vec<Option<ID3D11Texture2D>>,
    pending_signal: Option<u64>,
    adapter_luid: AdapterLuid,
    source_window: isize,
}

impl D3d11SwapchainCapture {
    /// # Safety
    /// `swapchain` must be a live IDXGISwapChain for this call and every later
    /// `copy_frame` call. The capture does not own the swapchain's lifetime.
    pub unsafe fn new(
        swapchain: *mut c_void,
        slots: u32,
        mode: D3d11RingMode,
        tracked_source: Option<&TrackedResource>,
    ) -> windows::core::Result<Self> {
        if swapchain.is_null() || slots == 0 {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        }
        let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        };
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
        let source: ID3D11Texture2D = match tracked_source {
            Some(source) => source.texture.clone(),
            None => unsafe { swapchain.GetBuffer(0) }
                .map_err(|error| contextual(error, "IDXGISwapChain::GetBuffer"))?,
        };
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
        let mut resolved_source = None;
        if source_desc.SampleDesc.Count > 1 {
            let mut resolved_desc = source_desc;
            resolved_desc.MipLevels = 1;
            resolved_desc.ArraySize = 1;
            resolved_desc.SampleDesc.Count = 1;
            resolved_desc.SampleDesc.Quality = 0;
            resolved_desc.Usage = windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT;
            resolved_desc.BindFlags = 0;
            resolved_desc.MiscFlags = 0;
            resolved_desc.CPUAccessFlags = 0;
            resolved_desc.Format = DXGI_FORMAT(polymer_protocol::transport_unorm_dxgi_format(
                source_desc.Format.0 as u32,
            ) as i32);
            unsafe { device.CreateTexture2D(&resolved_desc, None, Some(&mut resolved_source)) }?;
        }
        if let Some(source) = tracked_source {
            source_desc.Width = source.width;
            source_desc.Height = source.height;
            source_desc.MipLevels = 1;
            source_desc.ArraySize = 1;
        }
        let ring = unsafe { D3d11ImageRing::new(&device, &source_desc, slots, mode) }
            .map_err(|error| contextual(error, "D3D11 image ring creation"))?;
        Ok(Self {
            swapchain: swapchain.as_raw() as usize,
            context,
            context4,
            ring,
            source_desc,
            resolved_source,
            scheduler: Default::default(),
            sync_value: 0,
            release_values: vec![0; slots as usize],
            submitted_values: vec![0; slots as usize],
            submitted_sources: vec![None; slots as usize],
            pending_signal: None,
            adapter_luid,
            source_window: swapchain_desc.OutputWindow.0 as isize,
        })
    }

    /// # Safety
    /// The swapchain supplied at construction and any tracked source must still
    /// be live. Call on the application's immediate-context thread.
    pub unsafe fn copy_frame(
        &mut self,
        tracked_region: Option<D3d11CaptureRegion>,
        tracked_source: Option<&TrackedResource>,
    ) -> windows::core::Result<D3d11CopySubmission> {
        if let Some(value) = self.pending_signal {
            let context = self
                .context4
                .as_ref()
                .expect("pending fence signal requires Context4");
            let fence = self.ring.fence().expect("pending producer fence");
            unsafe { context.Signal(fence, value) }
                .map_err(|error| contextual(error, "retry ID3D11DeviceContext4::Signal"))?;
            self.pending_signal = None;
        }
        let (source, source_subresource) = match tracked_source {
            Some(source) => (source.texture.clone(), source.subresource),
            None => {
                let pointer = self.swapchain as *mut c_void;
                let swapchain = unsafe { IDXGISwapChain::from_raw_borrowed(&pointer) }
                    .expect("capture was constructed with a non-null swapchain");
                (unsafe { swapchain.GetBuffer(0)? }, 0)
            }
        };
        let region = tracked_source
            .is_none()
            .then_some(tracked_region)
            .flatten()
            .and_then(|region| {
                clamp_region(region, self.source_desc.Width, self.source_desc.Height)
            })
            .unwrap_or(D3d11CaptureRegion {
                x: tracked_source.map_or(0, |source| source.x),
                y: tracked_source.map_or(0, |source| source.y),
                width: self.source_desc.Width,
                height: self.source_desc.Height,
            });
        let next_value = self.sync_value.wrapping_add(1).max(1);
        let producer_completed = self
            .ring
            .fence()
            .map_or(u64::MAX, |fence| unsafe { fence.GetCompletedValue() });
        let consumer_completed = self
            .ring
            .consumer_fence()
            .map_or(u64::MAX, |fence| unsafe { fence.GetCompletedValue() });
        for (source, submitted) in self
            .submitted_sources
            .iter_mut()
            .zip(&self.submitted_values)
        {
            if *submitted <= producer_completed {
                *source = None;
            }
        }
        let reservation = self
            .scheduler
            .try_acquire(self.ring.slots().len(), |index| {
                let release = self.release_values[index];
                if self.submitted_values[index] > producer_completed {
                    return None;
                }
                if let Some(mutex) = self.ring.slots()[index].keyed_mutex.as_ref() {
                    unsafe { crate::KeyedMutexLease::try_acquire(mutex, 0, 1) }
                        .ok()
                        .flatten()
                        .map(Some)
                } else {
                    (release <= consumer_completed).then_some(None)
                }
            })
            .ok_or_else(|| {
                Error::new(
                    HRESULT(0x8007_0102u32 as i32),
                    "no capture slot has completed and been released",
                )
            })?;
        let slot = reservation.index() as u32;
        let ring_slot = &self.ring.slots()[slot as usize];
        let destination = &ring_slot.texture;
        let (copy_source, copy_subresource) = if let Some(resolved) = &self.resolved_source {
            let transport_format = DXGI_FORMAT(self.ring.desc().format as i32);
            unsafe {
                self.context.ResolveSubresource(
                    resolved,
                    0,
                    &source,
                    source_subresource,
                    transport_format,
                )
            };
            (resolved, 0)
        } else {
            (&source, source_subresource)
        };
        {
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
                    copy_source,
                    copy_subresource,
                    Some(&raw const source_box),
                )
            };
        }
        // The copy is already queued even if signaling or ownership handoff
        // fails below. Do not reuse its synchronization value or output slot.
        self.sync_value = next_value;
        self.submitted_values[slot as usize] = next_value;
        if let Some(fence) = self.ring.fence() {
            self.submitted_sources[slot as usize] = Some(source);
            self.pending_signal = Some(next_value);
            let context4 = self
                .context4
                .as_ref()
                .expect("fence-carrying rings require ID3D11DeviceContext4");
            if let Err(error) = unsafe { context4.Signal(fence, next_value) } {
                return Err(contextual(error, "ID3D11DeviceContext4::Signal"));
            }
            self.pending_signal = None;
        }
        reservation
            .commit()
            .map_err(|error| contextual(error, "IDXGIKeyedMutex::ReleaseSync"))?;
        self.release_values[slot as usize] = next_value;
        Ok(D3d11CopySubmission {
            slot,
            sync_value: self.sync_value,
            width: region.width,
            height: region.height,
            format: self.ring.desc().format,
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
    mod failure;

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
