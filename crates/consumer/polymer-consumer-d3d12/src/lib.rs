//! Native D3D12 import of Polymer shared image rings and fences.
#![allow(unsafe_code)]

use polymer_ipc::StreamReady;
use polymer_protocol::{AdapterLuid, ExternalImageKind, ExternalSyncKind, MAX_IMAGE_SLOTS};
use std::ffi::c_void;
use windows::{
    Win32::{
        Foundation::{CloseHandle, E_INVALIDARG, E_POINTER, HANDLE, LUID},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D12::{
                D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC,
                D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device,
                ID3D12Fence, ID3D12Resource,
            },
            Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4},
        },
    },
    core::Interface,
};

pub struct D3d12ConsumerDevice {
    factory: IDXGIFactory4,
    adapter: IDXGIAdapter1,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    adapter_luid: AdapterLuid,
}

impl D3d12ConsumerDevice {
    pub fn new_default() -> windows::core::Result<Self> {
        unsafe {
            let factory = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let adapter = factory.EnumAdapters1(0)?;
            Self::from_adapter(factory, adapter)
        }
    }

    pub fn new(adapter_luid: AdapterLuid) -> windows::core::Result<Self> {
        if adapter_luid.low_part == 0 && adapter_luid.high_part == 0 {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        unsafe {
            let factory = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let adapter = factory.EnumAdapterByLuid::<IDXGIAdapter1>(LUID {
                LowPart: adapter_luid.low_part,
                HighPart: adapter_luid.high_part,
            })?;
            Self::from_adapter(factory, adapter)
        }
    }

    /// Adopts retained COM references to a D3D12 device and direct queue that
    /// were created by another graphics frontend on `adapter_luid`.
    ///
    /// # Safety
    ///
    /// `device` and `queue` must each carry one owned COM reference, must be
    /// ABI-compatible `ID3D12Device` and `ID3D12CommandQueue` pointers, and the
    /// queue must belong to the device on `adapter_luid`. Ownership of both
    /// references transfers to the returned consumer.
    pub unsafe fn from_external(
        adapter_luid: AdapterLuid,
        device: *mut c_void,
        queue: *mut c_void,
    ) -> windows::core::Result<Self> {
        if device.is_null() || queue.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        // Adopt the transferred references immediately so every later error
        // path releases them normally.
        let device = unsafe { ID3D12Device::from_raw(device) };
        let queue = unsafe { ID3D12CommandQueue::from_raw(queue) };
        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))? };
        let adapter = unsafe {
            factory.EnumAdapterByLuid::<IDXGIAdapter1>(LUID {
                LowPart: adapter_luid.low_part,
                HighPart: adapter_luid.high_part,
            })?
        };
        let description = unsafe { adapter.GetDesc1()? };
        if description.AdapterLuid.LowPart != adapter_luid.low_part
            || description.AdapterLuid.HighPart != adapter_luid.high_part
        {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        Ok(Self {
            factory,
            adapter,
            device,
            queue,
            adapter_luid,
        })
    }

    unsafe fn from_adapter(
        factory: IDXGIFactory4,
        adapter: IDXGIAdapter1,
    ) -> windows::core::Result<Self> {
        let desc = unsafe { adapter.GetDesc1()? };
        let mut device = None;
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)? };
        let device: ID3D12Device =
            device.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
        let queue = unsafe {
            device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: 0,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            })?
        };
        Ok(Self {
            factory,
            adapter,
            device,
            queue,
            adapter_luid: AdapterLuid {
                low_part: desc.AdapterLuid.LowPart,
                high_part: desc.AdapterLuid.HighPart,
            },
        })
    }

    pub fn import(&self, stream: &StreamReady) -> windows::core::Result<ImportedD3d12Stream> {
        validate_stream(stream, self.adapter_luid)?;
        let mut resources = Vec::<ID3D12Resource>::with_capacity(stream.slot_count as usize);
        unsafe {
            for &raw in &stream.image_handles[..stream.slot_count as usize] {
                let handle = HANDLE(raw as *mut c_void);
                let mut resource = None;
                let result = self.device.OpenSharedHandle(handle, &mut resource);
                let _ = CloseHandle(handle);
                result?;
                resources
                    .push(resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?);
            }
            let producer = open_fence(&self.device, stream.timeline_semaphore_handle)?;
            let consumer = open_fence(&self.device, stream.consumer_semaphore_handle)?;
            for resource in &resources {
                let desc = resource.GetDesc();
                if desc.Width != u64::from(stream.width)
                    || desc.Height != stream.height
                    || desc.Format.0 as u32 != stream.format
                {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                }
            }
            Ok(ImportedD3d12Stream {
                resources,
                producer,
                consumer,
                width: stream.width,
                height: stream.height,
                format: stream.format,
            })
        }
    }

    pub fn factory(&self) -> &IDXGIFactory4 {
        &self.factory
    }

    pub fn adapter(&self) -> &IDXGIAdapter1 {
        &self.adapter
    }

    pub fn device(&self) -> &ID3D12Device {
        &self.device
    }

    pub fn queue(&self) -> &ID3D12CommandQueue {
        &self.queue
    }

    pub fn adapter_luid(&self) -> AdapterLuid {
        self.adapter_luid
    }
}

pub struct ImportedD3d12Stream {
    resources: Vec<ID3D12Resource>,
    producer: ID3D12Fence,
    consumer: ID3D12Fence,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl ImportedD3d12Stream {
    pub fn resource(&self, slot: u32) -> Option<&ID3D12Resource> {
        self.resources.get(slot as usize)
    }

    pub fn resources(&self) -> &[ID3D12Resource] {
        &self.resources
    }

    pub fn wait_on_queue(
        &self,
        queue: &ID3D12CommandQueue,
        value: u64,
    ) -> windows::core::Result<()> {
        unsafe { queue.Wait(&self.producer, value) }
    }

    /// Whether the producer has finished the frame without adding an
    /// unbounded wait to the consumer queue.
    ///
    /// A queue wait cannot be cancelled if the producer exits before signaling
    /// it. Checking first lets the compositor defer that frame while keeping
    /// its own queue usable for stream teardown.
    pub fn producer_ready(&self, value: u64) -> bool {
        unsafe { self.producer.GetCompletedValue() >= value }
    }

    pub fn signal_release(
        &self,
        queue: &ID3D12CommandQueue,
        value: u64,
    ) -> windows::core::Result<()> {
        unsafe { queue.Signal(&self.consumer, value) }
    }
}

/// Validates shared resources and their mandatory ready/release fences.
fn validate_stream(stream: &StreamReady, adapter_luid: AdapterLuid) -> windows::core::Result<()> {
    match (stream.image_kind, stream.sync_kind) {
        (
            ExternalImageKind::D3d12Resource
            | ExternalImageKind::OpenGlD3d12Resource
            | ExternalImageKind::VulkanD3d12Resource
            | ExternalImageKind::D3d11D3d12Resource,
            ExternalSyncKind::D3d12Fence,
        ) => (),
        _ => return Err(windows::core::Error::from_hresult(E_INVALIDARG)),
    };
    if stream.adapter_luid != adapter_luid
        || stream.slot_count == 0
        || stream.slot_count as usize > MAX_IMAGE_SLOTS
        || stream.image_handles[..stream.slot_count as usize]
            .iter()
            .any(|&handle| handle == 0 || handle == -1)
        || stream.timeline_semaphore_handle == 0
        || stream.consumer_semaphore_handle == 0
        || stream.width == 0
        || stream.height == 0
    {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    Ok(())
}

unsafe fn open_fence(device: &ID3D12Device, raw: i64) -> windows::core::Result<ID3D12Fence> {
    let handle = HANDLE(raw as *mut c_void);
    let mut fence = None;
    let result = unsafe { device.OpenSharedHandle(handle, &mut fence) };
    let _ = unsafe { CloseHandle(handle) };
    result?;
    fence.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}
