//! D3D12 swapchain capture into shared textures and a shared fence.
#![allow(unsafe_code)]

mod capture;
mod leases;
mod retirement;
pub mod tracking;

pub use capture::{SwapchainCapture, claim};

pub use tracking::{Presentation, TrackedResource, Tracking};

use polymer_protocol::transport_unorm_dxgi_format;
use std::{ffi::c_void, mem::ManuallyDrop};
use windows::{
    Win32::{
        Foundation::{CloseHandle, E_INVALIDARG, E_POINTER, HANDLE},
        Graphics::{
            Direct3D12::*,
            Dxgi::{Common::DXGI_FORMAT, IDXGISwapChain, IDXGISwapChain3},
        },
    },
    core::Interface,
};

pub struct D3d12SharedSlot {
    resource: ID3D12Resource,
    handle: isize,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    enhanced_list: Option<ID3D12GraphicsCommandList7>,
    state: leases::SlotState,
    source: parking_lot::Mutex<Option<ID3D12Resource>>,
}

impl D3d12SharedSlot {
    pub fn raw_handle(&self) -> isize {
        self.handle
    }
}

impl Drop for D3d12SharedSlot {
    fn drop(&mut self) {
        if self.handle != 0 && self.handle != -1 {
            unsafe { CloseHandle(HANDLE(self.handle as *mut c_void)) }.ok();
        }
    }
}

#[derive(Clone, Copy)]
pub struct D3d12RingDesc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub slots: u32,
}

pub struct D3d12CopySubmission {
    pub slot: u32,
    pub width: u32,
    pub height: u32,
    pub sync_value: u64,
    pub capture_x: u32,
    pub capture_y: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum D3d12CaptureSourceState {
    Legacy(D3D12_RESOURCE_STATES),
    Enhanced {
        sync: D3D12_BARRIER_SYNC,
        access: D3D12_BARRIER_ACCESS,
        layout: D3D12_BARRIER_LAYOUT,
    },
}

impl D3d12CaptureSourceState {
    fn capture_supported(self, enhanced: bool) -> bool {
        matches!(self, Self::Legacy(_)) || (enhanced && EnhancedState::tracked(self).is_some())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d12CaptureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct D3d12SwapchainCapture {
    swapchain: usize,
    queue: ID3D12CommandQueue,
    slots: Vec<D3d12SharedSlot>,
    fence: ID3D12Fence,
    fence_handle: isize,
    consumer_fence: ID3D12Fence,
    consumer_fence_handle: isize,
    desc: D3d12RingDesc,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    next_fence: u64,
    pending_signal: Option<u64>,
    source_window: isize,
    adapter_luid: (u32, i32),
}

impl D3d12SwapchainCapture {
    /// Creates capture resources for a D3D12 swapchain and its presenting queue.
    ///
    /// # Safety
    /// Both pointers must identify live COM interfaces for the duration of the call.
    pub unsafe fn new(
        swapchain: *mut c_void,
        queue: *mut c_void,
        slot_count: u32,
        capture_source: Option<(&ID3D12Resource, u32)>,
        use_enhanced_barriers: bool,
    ) -> windows::core::Result<Self> {
        retirement::RetiredRing::poll();
        if swapchain.is_null() || queue.is_null() || slot_count == 0 {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        let (swapchain, queue, device, swapchain_desc, resource_desc, adapter_luid) = unsafe {
            let swapchain = IDXGISwapChain::from_raw_borrowed(&swapchain)
                .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?
                .cast::<IDXGISwapChain3>()?;
            let queue = ID3D12CommandQueue::from_raw_borrowed(&queue)
                .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?
                .clone();
            let mut device = None;
            queue.GetDevice(&mut device)?;
            let device: ID3D12Device =
                device.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            let swapchain_desc = swapchain.GetDesc()?;
            let source: ID3D12Resource = match capture_source {
                Some((source, _)) => source.clone(),
                None => swapchain.GetBuffer(0)?,
            };
            let mut resource_desc = source.GetDesc();
            let subresource = capture_source.map_or(0, |(_, index)| index);
            let mip = subresource % u32::from(resource_desc.MipLevels).max(1);
            resource_desc.Width = (resource_desc.Width >> mip).max(1);
            resource_desc.Height = (resource_desc.Height >> mip).max(1);
            resource_desc.MipLevels = 1;
            resource_desc.DepthOrArraySize = 1;
            let adapter_luid = device.GetAdapterLuid();
            (
                swapchain,
                queue,
                device,
                swapchain_desc,
                resource_desc,
                (adapter_luid.LowPart, adapter_luid.HighPart),
            )
        };
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let mut shared_desc = resource_desc;
        let transport_format = transport_unorm_dxgi_format(resource_desc.Format.0 as u32);
        shared_desc.Format = DXGI_FORMAT(transport_format as i32);
        shared_desc.Flags = D3D12_RESOURCE_FLAG_NONE;
        let mut slots = Vec::with_capacity(slot_count as usize);
        unsafe {
            for _ in 0..slot_count {
                let mut resource = None;
                device.CreateCommittedResource(
                    &heap,
                    D3D12_HEAP_FLAG_SHARED,
                    &shared_desc,
                    D3D12_RESOURCE_STATE_COMMON,
                    None,
                    &mut resource,
                )?;
                let resource =
                    resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
                let handle = device.CreateSharedHandle(&resource, None, 0x1000_0000, None)?;
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None::<&ID3D12PipelineState>,
                )?;
                let enhanced_list = if use_enhanced_barriers {
                    Some(list.cast::<ID3D12GraphicsCommandList7>()?)
                } else {
                    None
                };
                list.Close()?;
                slots.push(D3d12SharedSlot {
                    resource,
                    handle: handle.0 as isize,
                    allocator,
                    list,
                    enhanced_list,
                    state: Default::default(),
                    source: Default::default(),
                });
            }
        }
        let (fence, fence_handle, consumer_fence, consumer_fence_handle) = unsafe {
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED)?;
            let fence_handle = device.CreateSharedHandle(&fence, None, 0x1000_0000, None)?;
            let consumer_fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED)?;
            let consumer_fence_handle =
                device.CreateSharedHandle(&consumer_fence, None, 0x1000_0000, None)?;
            (
                fence,
                fence_handle.0 as isize,
                consumer_fence,
                consumer_fence_handle.0 as isize,
            )
        };
        Ok(Self {
            swapchain: swapchain.as_raw() as usize,
            queue,
            slots,
            fence,
            fence_handle,
            consumer_fence,
            consumer_fence_handle,
            desc: D3d12RingDesc {
                width: resource_desc.Width as u32,
                height: resource_desc.Height,
                format: transport_format,
                slots: slot_count,
            },
            scheduler: Default::default(),
            next_fence: 1,
            pending_signal: None,
            source_window: swapchain_desc.OutputWindow.0 as isize,
            adapter_luid,
        })
    }

    pub fn desc(&self) -> D3d12RingDesc {
        self.desc
    }
    pub fn slots(&self) -> &[D3d12SharedSlot] {
        &self.slots
    }
    pub fn raw_fence_handle(&self) -> isize {
        self.fence_handle
    }
    pub fn raw_consumer_fence_handle(&self) -> isize {
        self.consumer_fence_handle
    }
    pub fn source_window(&self) -> isize {
        self.source_window
    }
    pub fn adapter_luid(&self) -> (u32, i32) {
        self.adapter_luid
    }

    /// Records and submits the selected backbuffer region on the presenting queue.
    ///
    /// # Safety
    /// The application must have submitted rendering before this call. A tracked
    /// source state must describe the resource state after that submitted work.
    pub unsafe fn copy_frame(
        &mut self,
        tracked_region: Option<D3d12CaptureRegion>,
        tracked_source: Option<(&ID3D12Resource, D3d12CaptureSourceState, u32)>,
    ) -> windows::core::Result<Option<D3d12CopySubmission>> {
        retirement::RetiredRing::poll();
        unsafe {
            if let Some(value) = self.pending_signal {
                self.queue.Signal(&self.fence, value)?;
                self.pending_signal = None;
            }
            let raw_swapchain = self.swapchain as *mut c_void;
            let swapchain = IDXGISwapChain3::from_raw_borrowed(&raw_swapchain)
                .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            let index = swapchain.GetCurrentBackBufferIndex();
            let source: ID3D12Resource = match tracked_source {
                Some((source, _, _)) => source.clone(),
                None => swapchain.GetBuffer(index)?,
            };
            let mut source_desc = source.GetDesc();
            let subresource = tracked_source.map_or(0, |(_, _, index)| index);
            let mip = subresource % u32::from(source_desc.MipLevels).max(1);
            source_desc.Width = (source_desc.Width >> mip).max(1);
            source_desc.Height = (source_desc.Height >> mip).max(1);
            if source_desc.Width != u64::from(self.desc.width)
                || source_desc.Height != self.desc.height
                || transport_unorm_dxgi_format(source_desc.Format.0 as u32) != self.desc.format
            {
                return Err(windows::core::Error::from_hresult(E_INVALIDARG));
            }
            let region = tracked_region
                .and_then(|region| clamp_region(region, self.desc.width, self.desc.height))
                .unwrap_or(D3d12CaptureRegion {
                    x: 0,
                    y: 0,
                    width: self.desc.width,
                    height: self.desc.height,
                });
            let producer_completed = self.fence.GetCompletedValue();
            let consumer_completed = self.consumer_fence.GetCompletedValue();
            let sync_value = self.next_fence;
            let Some(reservation) = self.scheduler.try_acquire(self.slots.len(), |index| {
                self.slots[index].state.try_acquire(
                    producer_completed,
                    consumer_completed,
                    sync_value,
                )
            }) else {
                return Ok(None);
            };
            let slot_index = reservation.index();
            let slot = &self.slots[slot_index];
            *slot.source.lock() = Some(source.clone());
            slot.allocator.Reset()?;
            slot.list
                .Reset(&slot.allocator, None::<&ID3D12PipelineState>)?;

            let source_state = tracked_source.map_or(
                D3d12CaptureSourceState::Legacy(D3D12_RESOURCE_STATE_PRESENT),
                |(_, state, _)| state,
            );
            let enhanced_source = match tracked_source {
                None => Some(EnhancedState::present()),
                Some((_, state, _)) => EnhancedState::tracked(state),
            };
            // Preserve the submitted barrier model. Legacy intermediates keep
            // legacy barriers; enhanced intermediates retain their exact layout.
            let use_enhanced = slot.enhanced_list.is_some() && enhanced_source.is_some();
            if use_enhanced {
                let list = slot.enhanced_list.as_ref().expect("checked above");
                record_texture_barriers(
                    list,
                    [
                        texture_barrier(
                            &source,
                            subresource,
                            enhanced_source.expect("checked above"),
                            EnhancedState::copy_source(),
                        ),
                        texture_barrier(
                            &slot.resource,
                            0,
                            EnhancedState::common(),
                            EnhancedState::copy_dest(),
                        ),
                    ],
                );
            } else {
                let D3d12CaptureSourceState::Legacy(source_state) = source_state else {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                };
                let to_copy = transition(
                    &source,
                    source_state,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    subresource,
                );
                let destination_to_copy = transition(
                    &slot.resource,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    0,
                );
                record_barriers(&slot.list, [to_copy, destination_to_copy]);
            }
            copy_region(&slot.list, &slot.resource, &source, subresource, region);
            if use_enhanced {
                let list = slot.enhanced_list.as_ref().expect("checked above");
                record_texture_barriers(
                    list,
                    [
                        texture_barrier(
                            &source,
                            subresource,
                            EnhancedState::copy_source(),
                            enhanced_source.expect("checked above"),
                        ),
                        texture_barrier(
                            &slot.resource,
                            0,
                            EnhancedState::copy_dest(),
                            EnhancedState::common(),
                        ),
                    ],
                );
            } else {
                let D3d12CaptureSourceState::Legacy(source_state) = source_state else {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                };
                let to_present = transition(
                    &source,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    source_state,
                    subresource,
                );
                let destination_to_common = transition(
                    &slot.resource,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COMMON,
                    0,
                );
                record_barriers(&slot.list, [to_present, destination_to_common]);
            }

            slot.list.Close()?;
            let command: ID3D12CommandList = slot.list.cast()?;
            self.queue.ExecuteCommandLists(&[Some(command)]);
            slot.state.submitted(sync_value);
            self.next_fence = self.next_fence.wrapping_add(1);
            self.pending_signal = Some(sync_value);
            self.queue.Signal(&self.fence, sync_value)?;
            self.pending_signal = None;
            reservation.commit().unwrap();
            Ok(Some(D3d12CopySubmission {
                slot: slot_index as u32,
                width: region.width,
                height: region.height,
                sync_value,
                capture_x: region.x,
                capture_y: region.y,
            }))
        }
    }
}

fn clamp_region(region: D3d12CaptureRegion, width: u32, height: u32) -> Option<D3d12CaptureRegion> {
    let left = region.x.min(width);
    let top = region.y.min(height);
    let right = region.x.saturating_add(region.width).min(width);
    let bottom = region.y.saturating_add(region.height).min(height);
    (right > left && bottom > top).then_some(D3d12CaptureRegion {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

unsafe fn copy_region(
    list: &ID3D12GraphicsCommandList,
    destination: &ID3D12Resource,
    source: &ID3D12Resource,
    subresource: u32,
    region: D3d12CaptureRegion,
) {
    let mut destination_location = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(destination.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let mut source_location = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(source.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: subresource,
        },
    };
    let source_box = D3D12_BOX {
        left: region.x,
        top: region.y,
        front: 0,
        right: region.x + region.width,
        bottom: region.y + region.height,
        back: 1,
    };
    unsafe {
        list.CopyTextureRegion(
            &raw const destination_location,
            0,
            0,
            0,
            &raw const source_location,
            Some(&raw const source_box),
        );
        ManuallyDrop::drop(&mut destination_location.pResource);
        ManuallyDrop::drop(&mut source_location.pResource);
    }
}

impl Drop for D3d12SwapchainCapture {
    fn drop(&mut self) {
        let mut retired = retirement::RetiredRing::new(self);
        if !retired.wait() {
            retired.defer();
        }
        if self.fence_handle != 0 && self.fence_handle != -1 {
            unsafe { CloseHandle(HANDLE(self.fence_handle as *mut c_void)) }.ok();
        }
        if self.consumer_fence_handle != 0 && self.consumer_fence_handle != -1 {
            unsafe { CloseHandle(HANDLE(self.consumer_fence_handle as *mut c_void)) }.ok();
        }
    }
}

fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
    subresource: u32,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: subresource,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

#[derive(Clone, Copy)]
struct EnhancedState {
    sync: D3D12_BARRIER_SYNC,
    access: D3D12_BARRIER_ACCESS,
    layout: D3D12_BARRIER_LAYOUT,
}

impl EnhancedState {
    fn common() -> Self {
        Self {
            sync: D3D12_BARRIER_SYNC_NONE,
            access: D3D12_BARRIER_ACCESS_NO_ACCESS,
            layout: D3D12_BARRIER_LAYOUT_COMMON,
        }
    }

    fn present() -> Self {
        Self {
            layout: D3D12_BARRIER_LAYOUT_PRESENT,
            ..Self::common()
        }
    }

    fn copy_source() -> Self {
        Self {
            sync: D3D12_BARRIER_SYNC_COPY,
            access: D3D12_BARRIER_ACCESS_COPY_SOURCE,
            layout: D3D12_BARRIER_LAYOUT_COPY_SOURCE,
        }
    }

    fn copy_dest() -> Self {
        Self {
            sync: D3D12_BARRIER_SYNC_COPY,
            access: D3D12_BARRIER_ACCESS_COPY_DEST,
            layout: D3D12_BARRIER_LAYOUT_COPY_DEST,
        }
    }
}

impl EnhancedState {
    fn tracked(state: D3d12CaptureSourceState) -> Option<Self> {
        match state {
            D3d12CaptureSourceState::Enhanced {
                sync,
                access,
                layout,
            } if !sync.contains(D3D12_BARRIER_SYNC_SPLIT)
                && layout != D3D12_BARRIER_LAYOUT_UNDEFINED =>
            {
                Some(Self {
                    sync,
                    access,
                    layout,
                })
            }
            _ => None,
        }
    }
}

fn texture_barrier(
    resource: &ID3D12Resource,
    subresource: u32,
    before: EnhancedState,
    after: EnhancedState,
) -> D3D12_TEXTURE_BARRIER {
    D3D12_TEXTURE_BARRIER {
        SyncBefore: before.sync,
        SyncAfter: after.sync,
        AccessBefore: before.access,
        AccessAfter: after.access,
        LayoutBefore: before.layout,
        LayoutAfter: after.layout,
        pResource: ManuallyDrop::new(Some(resource.clone())),
        Subresources: D3D12_BARRIER_SUBRESOURCE_RANGE {
            IndexOrFirstMipLevel: subresource,
            ..Default::default()
        },
        Flags: D3D12_TEXTURE_BARRIER_FLAG_NONE,
    }
}

unsafe fn record_texture_barriers<const N: usize>(
    list: &ID3D12GraphicsCommandList7,
    mut barriers: [D3D12_TEXTURE_BARRIER; N],
) {
    let group = D3D12_BARRIER_GROUP {
        Type: D3D12_BARRIER_TYPE_TEXTURE,
        NumBarriers: N as u32,
        Anonymous: D3D12_BARRIER_GROUP_0 {
            pTextureBarriers: barriers.as_ptr(),
        },
    };
    unsafe {
        list.Barrier(std::slice::from_ref(&group));
        for barrier in &mut barriers {
            ManuallyDrop::drop(&mut barrier.pResource);
        }
    }
}

unsafe fn record_barriers<const N: usize>(
    list: &ID3D12GraphicsCommandList,
    mut barriers: [D3D12_RESOURCE_BARRIER; N],
) {
    unsafe {
        let effective: Vec<_> = barriers
            .iter()
            .filter(|barrier| {
                let transition = &*barrier.Anonymous.Transition;
                transition.StateBefore != transition.StateAfter
            })
            .cloned()
            .collect();
        if !effective.is_empty() {
            list.ResourceBarrier(&effective);
        }
        for barrier in &mut barriers {
            let transition = &mut *barrier.Anonymous.Transition;
            ManuallyDrop::drop(&mut transition.pResource);
        }
    }
}
