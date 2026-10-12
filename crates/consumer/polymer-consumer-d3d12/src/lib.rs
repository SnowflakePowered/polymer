//! Native D3D12 import of Polymer shared image rings and fences.
#![allow(unsafe_code)]

use polymer_ipc::{StreamReady, Win32Resources};
use polymer_protocol::{AdapterLuid, ExternalImageKind, MAX_IMAGE_SLOTS};
use std::{
    fmt::Write as _,
    os::windows::io::{AsRawHandle, OwnedHandle},
};
use windows::Win32::{
    Foundation::{CloseHandle, E_INVALIDARG, E_POINTER, HANDLE, LUID, WAIT_OBJECT_0},
    Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{
            D3D12_AUTO_BREADCRUMB_NODE, D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC,
            D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_DRED_ALLOCATION_NODE,
            D3D12_DRED_ENABLEMENT_FORCED_ON, D3D12CreateDevice, D3D12GetDebugInterface,
            ID3D12CommandQueue, ID3D12Device, ID3D12DeviceRemovedExtendedData,
            ID3D12DeviceRemovedExtendedDataSettings, ID3D12Fence, ID3D12Resource,
        },
        Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4},
    },
    System::Threading::CreateEventW,
};
use windows::core::{Interface, PCWSTR, w};

mod fence;
mod timeline;
pub use fence::FenceWaiter;
pub use timeline::ProducerTimeline;

const BREADCRUMB_OPS: [&str; 49] = [
    "SETMARKER",
    "BEGINEVENT",
    "ENDEVENT",
    "DRAWINSTANCED",
    "DRAWINDEXEDINSTANCED",
    "EXECUTEINDIRECT",
    "DISPATCH",
    "COPYBUFFERREGION",
    "COPYTEXTUREREGION",
    "COPYRESOURCE",
    "COPYTILES",
    "RESOLVESUBRESOURCE",
    "CLEARRENDERTARGETVIEW",
    "CLEARUNORDEREDACCESSVIEW",
    "CLEARDEPTHSTENCILVIEW",
    "RESOURCEBARRIER",
    "EXECUTEBUNDLE",
    "PRESENT",
    "RESOLVEQUERYDATA",
    "BEGINSUBMISSION",
    "ENDSUBMISSION",
    "DECODEFRAME",
    "PROCESSFRAMES",
    "ATOMICCOPYBUFFERUINT",
    "ATOMICCOPYBUFFERUINT64",
    "RESOLVESUBRESOURCEREGION",
    "WRITEBUFFERIMMEDIATE",
    "DECODEFRAME1",
    "SETPROTECTEDRESOURCESESSION",
    "DECODEFRAME2",
    "PROCESSFRAMES1",
    "BUILDRAYTRACINGACCELERATIONSTRUCTURE",
    "EMITRAYTRACINGACCELERATIONSTRUCTUREPOSTBUILDINFO",
    "COPYRAYTRACINGACCELERATIONSTRUCTURE",
    "DISPATCHRAYS",
    "INITIALIZEMETACOMMAND",
    "EXECUTEMETACOMMAND",
    "ESTIMATEMOTION",
    "RESOLVEMOTIONVECTORHEAP",
    "SETPIPELINESTATE1",
    "INITIALIZEEXTENSIONCOMMAND",
    "EXECUTEEXTENSIONCOMMAND",
    "DISPATCHMESH",
    "ENCODEFRAME",
    "RESOLVEENCODEROUTPUTMETADATA",
    "BARRIER",
    "BEGIN_COMMAND_LIST",
    "DISPATCHGRAPH",
    "SETPROGRAM",
];

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

    unsafe fn from_adapter(
        factory: IDXGIFactory4,
        adapter: IDXGIAdapter1,
    ) -> windows::core::Result<Self> {
        let desc = unsafe { adapter.GetDesc1()? };
        Self::enable_removal_diagnostics();
        let mut device = None;
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)? };
        let device: ID3D12Device =
            device.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
        let queue: ID3D12CommandQueue = unsafe {
            device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: 0,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            })?
        };
        unsafe { queue.SetName(w!("Polymer consumer queue"))? };
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

    /// Describes why the device was removed: the removal reason and, when DRED is enabled in
    /// debug builds, the command lists that did not finish and any page-faulting allocation.
    pub fn removal_report(&self) -> String {
        Self::removal_report_for(&self.device)
    }

    /// Enables DRED breadcrumbs and page-fault tracking for D3D12 devices created afterwards,
    /// in debug builds. The setting is process-wide, so call it before creating any device.
    pub fn enable_removal_diagnostics() {
        if !cfg!(debug_assertions) {
            return;
        }
        let mut settings: Option<ID3D12DeviceRemovedExtendedDataSettings> = None;
        if unsafe { D3D12GetDebugInterface(&mut settings) }.is_ok()
            && let Some(settings) = settings
        {
            unsafe {
                settings.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
                settings.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            }
        }
    }

    /// [`Self::removal_report`] for any D3D12 device, such as one owned by wgpu.
    pub fn removal_report_for(device: &ID3D12Device) -> String {
        let mut report = match unsafe { device.GetDeviceRemovedReason() } {
            Ok(()) => "device is not removed".to_owned(),
            Err(error) => format!("reason {error}"),
        };
        let Ok(dred) = device.cast::<ID3D12DeviceRemovedExtendedData>() else {
            return report;
        };
        let name = |name: PCWSTR| {
            if name.is_null() {
                "unnamed".to_owned()
            } else {
                unsafe { name.to_string() }.unwrap_or_default()
            }
        };
        if let Ok(output) = unsafe { dred.GetAutoBreadcrumbsOutput() } {
            let mut node = output.pHeadAutoBreadcrumbNode;
            while let Some(breadcrumbs) = unsafe { node.as_ref() } {
                let completed = unsafe { breadcrumbs.pLastBreadcrumbValue.as_ref() }
                    .copied()
                    .unwrap_or(0);
                if completed < breadcrumbs.BreadcrumbCount {
                    let history = unsafe {
                        std::slice::from_raw_parts(
                            breadcrumbs.pCommandHistory,
                            breadcrumbs.BreadcrumbCount as usize,
                        )
                    };
                    let operations = history.iter().enumerate().map(|(index, op)| {
                        let label = BREADCRUMB_OPS
                            .get(op.0 as usize)
                            .copied()
                            .unwrap_or("UNKNOWN");
                        if index == completed as usize {
                            format!(">>{label}<<")
                        } else {
                            label.to_owned()
                        }
                    });
                    let _ = write!(
                        report,
                        "; command list {} on queue {} stopped after {completed} of {}: [{}]",
                        name(breadcrumbs.pCommandListDebugNameW),
                        name(breadcrumbs.pCommandQueueDebugNameW),
                        breadcrumbs.BreadcrumbCount,
                        operations.collect::<Vec<_>>().join(", "),
                    );
                }
                node = breadcrumbs.pNext as *const D3D12_AUTO_BREADCRUMB_NODE;
            }
        }
        if let Ok(fault) = unsafe { dred.GetPageFaultAllocationOutput() }
            && fault.PageFaultVA != 0
        {
            let _ = write!(report, "; page fault at {:#x}", fault.PageFaultVA);
            for (label, mut node) in [
                ("existing", fault.pHeadExistingAllocationNode),
                ("freed", fault.pHeadRecentFreedAllocationNode),
            ] {
                while let Some(allocation) = unsafe { node.as_ref() } {
                    let _ = write!(
                        report,
                        ", {label} {} ({:?})",
                        name(allocation.ObjectNameW),
                        allocation.AllocationType
                    );
                    node = allocation.pNext as *const D3D12_DRED_ALLOCATION_NODE;
                }
            }
        }
        report
    }

    pub fn import(
        &self,
        stream: &StreamReady,
        shared: &Win32Resources,
    ) -> windows::core::Result<ImportedD3d12Stream> {
        validate_stream(stream, self.adapter_luid)?;
        let mut resources = Vec::<ID3D12Resource>::with_capacity(shared.images.len());
        unsafe {
            for handle in &shared.images {
                let mut resource = None;
                self.device
                    .OpenSharedHandle(HANDLE(handle.as_raw_handle()), &mut resource)?;
                resources
                    .push(resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?);
            }
            let consumer = open_fence(&self.device, &shared.released)?;
            for resource in &resources {
                let desc = resource.GetDesc();
                if desc.Width != u64::from(stream.width)
                    || desc.Height != stream.height
                    || desc.Format.0 as u32 != stream.format
                {
                    return Err(windows::core::Error::from_hresult(E_INVALIDARG));
                }
            }
            Ok(ImportedD3d12Stream::new(
                resources,
                consumer,
                stream.width,
                stream.height,
                stream.format,
            ))
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
    consumer: ID3D12Fence,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl ImportedD3d12Stream {
    pub fn new(
        resources: Vec<ID3D12Resource>,
        consumer: ID3D12Fence,
        width: u32,
        height: u32,
        format: u32,
    ) -> Self {
        Self {
            resources,
            consumer,
            width,
            height,
            format,
        }
    }

    pub fn resource(&self, slot: u32) -> Option<&ID3D12Resource> {
        self.resources.get(slot as usize)
    }

    pub fn resources(&self) -> &[ID3D12Resource] {
        &self.resources
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
    if !matches!(
        stream.image_kind,
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d11Resource
    ) {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    if stream.adapter_luid != adapter_luid
        || stream.slot_count == 0
        || stream.slot_count as usize > MAX_IMAGE_SLOTS
        || stream.width == 0
        || stream.height == 0
    {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    Ok(())
}

unsafe fn open_fence(
    device: &ID3D12Device,
    handle: &OwnedHandle,
) -> windows::core::Result<ID3D12Fence> {
    let mut fence = None;
    unsafe { device.OpenSharedHandle(HANDLE(handle.as_raw_handle()), &mut fence)? };
    fence.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}
