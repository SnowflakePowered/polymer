//! D3D12 swapchain capture into shared textures and a shared fence.
#![allow(unsafe_code)]

mod capture;
pub mod tracking;

pub use capture::{before_present, before_resize, claim, destroyed};

pub use tracking::{
    Presentation, TrackedResource, before_swapchain_resize, command_reset, detect_device_features,
    enhanced_barriers_supported, record_copy_descriptors_simple, record_descriptor_heap,
    record_draw, record_enhanced_barrier, record_render_target_view, record_render_targets,
    record_resource_barrier, record_root_descriptor_table, record_root_signature, record_scissors,
    record_shader_resource_view, record_viewports, swapchain_presentation,
};

use std::{ffi::c_void, mem::ManuallyDrop};
use windows::{
    Win32::{
        Foundation::{CloseHandle, E_INVALIDARG, E_POINTER, HANDLE},
        Graphics::{
            Direct3D12::*,
            Dxgi::{IDXGISwapChain, IDXGISwapChain3},
        },
    },
    core::Interface,
};

pub(crate) fn d3d12_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    if let Ok(path) = std::env::var("SNOWGLASS_D3D12_LOG_FILE")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

pub struct D3d12SharedSlot {
    resource: ID3D12Resource,
    handle: isize,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    enhanced_list: Option<ID3D12GraphicsCommandList7>,
    release_value: u64,
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
    next_slot: u32,
    next_fence: u64,
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
        capture_source: Option<&ID3D12Resource>,
        use_enhanced_barriers: bool,
    ) -> windows::core::Result<Self> {
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
                Some(source) => source.clone(),
                None => swapchain.GetBuffer(0)?,
            };
            let resource_desc = source.GetDesc();
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
                    release_value: 0,
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
                format: resource_desc.Format.0 as u32,
                slots: slot_count,
            },
            next_slot: 0,
            next_fence: 1,
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
        tracked_source: Option<(&ID3D12Resource, D3d12CaptureSourceState)>,
    ) -> windows::core::Result<Option<D3d12CopySubmission>> {
        unsafe {
            let raw_swapchain = self.swapchain as *mut c_void;
            let swapchain = IDXGISwapChain3::from_raw_borrowed(&raw_swapchain)
                .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            let index = swapchain.GetCurrentBackBufferIndex();
            let source: ID3D12Resource = match tracked_source {
                Some((source, _)) => source.clone(),
                None => swapchain.GetBuffer(index)?,
            };
            let source_desc = source.GetDesc();
            if source_desc.Width != u64::from(self.desc.width)
                || source_desc.Height != self.desc.height
                || source_desc.Format.0 as u32 != self.desc.format
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
            let completed = self.consumer_fence.GetCompletedValue();
            let Some(slot_index) = (0..self.slots.len())
                .map(|offset| (self.next_slot as usize + offset) % self.slots.len())
                .find(|&index| {
                    let release = self.slots[index].release_value;
                    release == 0 || release <= completed
                })
            else {
                return Ok(None);
            };
            let slot = &mut self.slots[slot_index];
            slot.allocator.Reset()?;
            slot.list
                .Reset(&slot.allocator, None::<&ID3D12PipelineState>)?;

            let source_state = tracked_source.map_or(
                D3d12CaptureSourceState::Legacy(D3D12_RESOURCE_STATE_PRESENT),
                |(_, state)| state,
            );
            let enhanced_source = if tracked_source.is_some() {
                enhanced_state(source_state)
            } else {
                Some(EnhancedState::present())
            };
            // The swapchain state at Present is authoritative. A tracked guest
            // intermediate can be recorded and submitted on command lists in a
            // different order, so keep its exact legacy transition path rather
            // than translating that state into an enhanced layout here.
            let use_enhanced = tracked_source.is_none()
                && slot.enhanced_list.is_some()
                && enhanced_source.is_some();
            if use_enhanced {
                let list = slot.enhanced_list.as_ref().expect("checked above");
                record_texture_barriers(
                    list,
                    [
                        texture_barrier(
                            &source,
                            enhanced_source.expect("checked above"),
                            EnhancedState::copy_source(),
                        ),
                        texture_barrier(
                            &slot.resource,
                            EnhancedState::common(),
                            EnhancedState::copy_dest(),
                        ),
                    ],
                );
            } else {
                let D3d12CaptureSourceState::Legacy(source_state) = source_state else {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                };
                let to_copy = transition(&source, source_state, D3D12_RESOURCE_STATE_COPY_SOURCE);
                let destination_to_copy = transition(
                    &slot.resource,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                );
                record_barriers(&slot.list, [to_copy, destination_to_copy]);
            }
            copy_region(&slot.list, &slot.resource, &source, region);
            if use_enhanced {
                let list = slot.enhanced_list.as_ref().expect("checked above");
                record_texture_barriers(
                    list,
                    [
                        texture_barrier(
                            &source,
                            EnhancedState::copy_source(),
                            enhanced_source.expect("checked above"),
                        ),
                        texture_barrier(
                            &slot.resource,
                            EnhancedState::copy_dest(),
                            EnhancedState::common(),
                        ),
                    ],
                );
            } else {
                let D3d12CaptureSourceState::Legacy(source_state) = source_state else {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                };
                let to_present =
                    transition(&source, D3D12_RESOURCE_STATE_COPY_SOURCE, source_state);
                let destination_to_common = transition(
                    &slot.resource,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COMMON,
                );
                record_barriers(&slot.list, [to_present, destination_to_common]);
            }

            slot.list.Close()?;
            let command: ID3D12CommandList = slot.list.cast()?;
            self.queue.ExecuteCommandLists(&[Some(command)]);
            let sync_value = self.next_fence;
            self.queue.Signal(&self.fence, sync_value)?;
            slot.release_value = sync_value;
            self.next_slot = (slot_index as u32 + 1) % self.desc.slots;
            self.next_fence = self.next_fence.wrapping_add(1);
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
            SubresourceIndex: 0,
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
        let last_submission = self.next_fence.saturating_sub(1);
        if last_submission != 0 && unsafe { self.fence.GetCompletedValue() } < last_submission {
            unsafe {
                use windows_sys::Win32::System::Threading::{
                    CreateEventW, INFINITE, WaitForSingleObject,
                };
                let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
                if !event.is_null() {
                    if self
                        .fence
                        .SetEventOnCompletion(last_submission, HANDLE(event))
                        .is_ok()
                    {
                        WaitForSingleObject(event, INFINITE);
                    }
                    CloseHandle(HANDLE(event)).ok();
                }
            }
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
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
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

fn enhanced_state(state: D3d12CaptureSourceState) -> Option<EnhancedState> {
    match state {
        D3d12CaptureSourceState::Enhanced {
            sync,
            access,
            layout,
        } => Some(EnhancedState {
            sync,
            access,
            layout,
        }),
        D3d12CaptureSourceState::Legacy(state)
            if state == D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                || state == D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                || state
                    == D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                        | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE =>
        {
            let mut sync = D3D12_BARRIER_SYNC_NONE;
            if state.contains(D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE) {
                sync |= D3D12_BARRIER_SYNC_PIXEL_SHADING;
            }
            if state.contains(D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE) {
                sync |= D3D12_BARRIER_SYNC_NON_PIXEL_SHADING;
            }
            Some(EnhancedState {
                sync,
                access: D3D12_BARRIER_ACCESS_SHADER_RESOURCE,
                layout: D3D12_BARRIER_LAYOUT_SHADER_RESOURCE,
            })
        }
        D3d12CaptureSourceState::Legacy(state) if state == D3D12_RESOURCE_STATE_RENDER_TARGET => {
            Some(EnhancedState {
                sync: D3D12_BARRIER_SYNC_RENDER_TARGET,
                access: D3D12_BARRIER_ACCESS_RENDER_TARGET,
                layout: D3D12_BARRIER_LAYOUT_RENDER_TARGET,
            })
        }
        D3d12CaptureSourceState::Legacy(state) if state == D3D12_RESOURCE_STATE_COPY_SOURCE => {
            Some(EnhancedState::copy_source())
        }
        D3d12CaptureSourceState::Legacy(state) if state == D3D12_RESOURCE_STATE_COPY_DEST => {
            Some(EnhancedState::copy_dest())
        }
        D3d12CaptureSourceState::Legacy(state) if state == D3D12_RESOURCE_STATE_COMMON => {
            Some(EnhancedState::common())
        }
        _ => None,
    }
}

fn texture_barrier(
    resource: &ID3D12Resource,
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
            IndexOrFirstMipLevel: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
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
        list.ResourceBarrier(&barriers);
        for barrier in &mut barriers {
            let transition = &mut *barrier.Anonymous.Transition;
            ManuallyDrop::drop(&mut transition.pResource);
        }
    }
}
