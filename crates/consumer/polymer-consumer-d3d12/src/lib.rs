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
            Direct3D11::{ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Texture2D},
            Direct3D11on12::D3D11On12CreateDevice,
            Direct3D12::{
                D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC,
                D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device,
                ID3D12Fence, ID3D12Resource,
            },
            Dxgi::{
                CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
                IDXGIKeyedMutex,
            },
        },
    },
    core::{HRESULT, IUnknown, Interface},
};

const CONSUMER_ACQUIRE_KEY: u64 = 1;
const CONSUMER_RELEASE_KEY: u64 = 0;

/// HRESULT_FROM_WIN32(WAIT_TIMEOUT); AcquireSync reports timeouts as a
/// success code, so it has to be promoted to an error explicitly.
const WAIT_TIMEOUT_HRESULT: HRESULT = HRESULT(0x8007_0102u32 as i32);

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
        let keyed_mutex = validate_stream(stream, self.adapter_luid)?;
        let mut resources = Vec::<ID3D12Resource>::with_capacity(stream.slot_count as usize);
        unsafe {
            // Open an 11on12 device if we're in keyed mutex mode
            let interop = if keyed_mutex {
                Some(create_11on12_device(&self.device, &self.queue)?)
            } else {
                None
            };
            let mut mutexes = Vec::<IDXGIKeyedMutex>::new();
            let mut textures = Vec::<ID3D11Texture2D>::new();
            for &raw in &stream.image_handles[..stream.slot_count as usize] {
                let handle = HANDLE(raw as *mut c_void);
                let mut resource = None;
                let mut result = self.device.OpenSharedHandle(handle, &mut resource);
                if result.is_ok()
                    && let Some((device11, _)) = interop.as_ref()
                {
                    result = device11
                        .OpenSharedResource1::<ID3D11Texture2D>(handle)
                        .and_then(|texture| {
                            let mutex = texture.cast::<IDXGIKeyedMutex>()?;
                            textures.push(texture);
                            mutexes.push(mutex);
                            Ok(())
                        });
                }
                let _ = CloseHandle(handle);
                result?;
                resources
                    .push(resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?);
            }
            let sync = match interop {
                Some((device, context)) => StreamSync::KeyedMutex(KeyedMutexSync {
                    mutexes,
                    _textures: textures,
                    context,
                    _device: device,
                }),
                None => StreamSync::Fence {
                    producer: open_fence(&self.device, stream.timeline_semaphore_handle)?,
                    consumer: open_fence(&self.device, stream.consumer_semaphore_handle)?,
                },
            };
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
                sync,
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

struct KeyedMutexSync {
    mutexes: Vec<IDXGIKeyedMutex>,
    /// The D3D11On12 views backing the mutexes; released with the stream.
    _textures: Vec<ID3D11Texture2D>,
    context: ID3D11DeviceContext,
    _device: ID3D11Device1,
}

enum StreamSync {
    Fence {
        producer: ID3D12Fence,
        consumer: ID3D12Fence,
    },
    KeyedMutex(KeyedMutexSync),
}

pub struct ImportedD3d12Stream {
    resources: Vec<ID3D12Resource>,
    sync: StreamSync,
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

    pub fn uses_keyed_mutex(&self) -> bool {
        matches!(self.sync, StreamSync::KeyedMutex(_))
    }

    /// Acquire the producer mutex within `timeout_ms` elapses, if using keyed mutexes.
    pub fn acquire_slot(&self, slot: u32, timeout_ms: u32) -> windows::core::Result<()> {
        let StreamSync::KeyedMutex(sync) = &self.sync else {
            return Ok(());
        };
        let mutex = sync
            .mutexes
            .get(slot as usize)
            .ok_or_else(|| windows::core::Error::from_hresult(E_INVALIDARG))?;

        // WAIT_TIMEOUT is a success code, so it's swallowed if we use the windows-rs wrapped call.
        let result = unsafe {
            (Interface::vtable(mutex).AcquireSync)(
                Interface::as_raw(mutex),
                CONSUMER_ACQUIRE_KEY,
                timeout_ms,
            )
        };
        if result.0 != 0 {
            return Err(windows::core::Error::from_hresult(if result.0 > 0 {
                WAIT_TIMEOUT_HRESULT
            } else {
                result
            }));
        }
        Ok(())
    }

    /// Returns an acquired `slot` to the producer.
    pub fn release_slot(&self, slot: u32) -> windows::core::Result<()> {
        let StreamSync::KeyedMutex(sync) = &self.sync else {
            return Ok(());
        };
        let mutex = sync
            .mutexes
            .get(slot as usize)
            .ok_or_else(|| windows::core::Error::from_hresult(E_INVALIDARG))?;
        unsafe { mutex.ReleaseSync(CONSUMER_RELEASE_KEY) }?;
        // D3D11On12 defers sync-object operations until the wrapping device
        // flushes; without this the producer never reacquires the slot.
        unsafe { sync.context.Flush() };
        Ok(())
    }

    /// Cycles a slot whose frame is being discarded unseen so the producer
    /// can write to it again. Only meaningful for keyed-mutex streams.
    pub fn recycle_slot(&self, slot: u32) {
        if self.uses_keyed_mutex() && self.acquire_slot(slot, 0).is_ok() {
            let _ = self.release_slot(slot);
        }
    }

    pub fn wait_on_queue(
        &self,
        queue: &ID3D12CommandQueue,
        value: u64,
    ) -> windows::core::Result<()> {
        match &self.sync {
            StreamSync::Fence { producer, .. } => unsafe { queue.Wait(producer, value) },
            StreamSync::KeyedMutex(_) => Ok(()),
        }
    }

    /// Whether the producer has finished the frame without adding an
    /// unbounded wait to the consumer queue.
    ///
    /// A queue wait cannot be cancelled if the producer exits before signaling
    /// it. Checking first lets the compositor defer that frame while keeping
    /// its own queue usable for stream teardown.
    pub fn producer_ready(&self, value: u64) -> bool {
        match &self.sync {
            StreamSync::Fence { producer, .. } => unsafe { producer.GetCompletedValue() >= value },
            StreamSync::KeyedMutex(_) => true,
        }
    }

    pub fn signal_release(
        &self,
        queue: &ID3D12CommandQueue,
        value: u64,
    ) -> windows::core::Result<()> {
        match &self.sync {
            StreamSync::Fence { consumer, .. } => unsafe { queue.Signal(consumer, value) },
            StreamSync::KeyedMutex(_) => Ok(()),
        }
    }
}

/// Validates the stream shape and returns whether it synchronizes through
/// keyed mutexes instead of shared fences.
fn validate_stream(stream: &StreamReady, adapter_luid: AdapterLuid) -> windows::core::Result<bool> {
    let keyed_mutex = match (stream.image_kind, stream.sync_kind) {
        (
            ExternalImageKind::D3d12Resource
            | ExternalImageKind::OpenGlD3d12Resource
            | ExternalImageKind::VulkanD3d12Resource
            | ExternalImageKind::D3d11D3d12Resource
            | ExternalImageKind::GdiD3d12Resource,
            ExternalSyncKind::D3d12Fence,
        ) => false,
        (ExternalImageKind::D3d11SharedResourceNt, ExternalSyncKind::D3d11KeyedMutex) => true,
        _ => return Err(windows::core::Error::from_hresult(E_INVALIDARG)),
    };
    if stream.adapter_luid != adapter_luid
        || stream.slot_count == 0
        || stream.slot_count as usize > MAX_IMAGE_SLOTS
        || stream.image_handles[..stream.slot_count as usize]
            .iter()
            .any(|&handle| handle == 0 || handle == -1)
        || (!keyed_mutex
            && (stream.timeline_semaphore_handle == 0 || stream.consumer_semaphore_handle == 0))
        || stream.width == 0
        || stream.height == 0
    {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    Ok(keyed_mutex)
}

unsafe fn create_11on12_device(
    device: &ID3D12Device,
    queue: &ID3D12CommandQueue,
) -> windows::core::Result<(ID3D11Device1, ID3D11DeviceContext)> {
    let queues = [Some(queue.cast::<IUnknown>()?)];
    let mut device11: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11On12CreateDevice(
            device,
            0,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            Some(&queues),
            0,
            Some(&mut device11),
            Some(&mut context),
            None,
        )?;
    }
    let device11 = device11.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
    let context = context.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
    Ok((device11.cast()?, context))
}

unsafe fn open_fence(device: &ID3D12Device, raw: i64) -> windows::core::Result<ID3D12Fence> {
    let handle = HANDLE(raw as *mut c_void);
    let mut fence = None;
    let result = unsafe { device.OpenSharedHandle(handle, &mut fence) };
    let _ = unsafe { CloseHandle(handle) };
    result?;
    fence.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}
